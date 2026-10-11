//! Deterministic multi-module archives with bounded, manifest-checked extraction.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::domain::{PluginManifest, PluginModuleKind, sha256_hex};
use crate::error::{Error, Result};

use super::{MANIFEST_FILE, manifest};

/// Maximum compressed plugin upload/download size.
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum expanded plugin size; total includes files the manifest does not name.
pub const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// Validated contents of a plugin release.
#[derive(Debug, Clone)]
pub struct PluginPackage {
    /// Parsed parent contract.
    pub manifest: PluginManifest,
    /// Original manifest bytes, retained for reproducible installation.
    pub manifest_text: String,
    /// Every regular archive file under its checked path.
    pub files: BTreeMap<String, Vec<u8>>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::PluginPackageInvalid {
        message: message.into(),
    }
}

/// Pack validated plugin files in lexicographic order with fixed metadata.
///
/// # Errors
///
/// [`Error::PluginPackageInvalid`] if the input is incomplete or the archive cannot be written.
pub fn pack(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    validate_files(files)?;
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tarball = tar::Builder::new(encoder);
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        tarball
            .append_data(&mut header, path, bytes.as_slice())
            .map_err(|e| invalid(format!("cannot archive `{path}`: {e}")))?;
    }
    tarball
        .into_inner()
        .and_then(flate2::write::GzEncoder::finish)
        .map_err(|e| invalid(format!("cannot finish the archive: {e}")))
}

/// Build an archive from a local plugin project, reading only declared module artifacts.
///
/// # Errors
///
/// Refuses symlinks, missing artifacts and oversized projects; ignores build outputs not declared by the manifest.
pub fn pack_project(root: &Path) -> Result<Vec<u8>> {
    let mut files = BTreeMap::new();
    let manifest = read_project_file(root, MANIFEST_FILE)?;
    let text = std::str::from_utf8(&manifest).map_err(|_| invalid("plugin.toml is not UTF-8"))?;
    let plugin = manifest::parse(text)?;
    files.insert(MANIFEST_FILE.to_string(), manifest);
    for module in &plugin.modules {
        files.insert(
            module.manifest.clone(),
            read_project_file(root, &module.manifest)?,
        );
        files.insert(
            module.entry.clone(),
            read_project_file(root, &module.entry)?,
        );
        if module.kind == PluginModuleKind::Integration {
            let text = std::str::from_utf8(&files[&module.manifest]).map_err(|_| {
                invalid(format!("integration `{}` manifest is not UTF-8", module.id))
            })?;
            let integration = crate::integration::manifest::parse(text)?;
            let prefix = module.manifest.rsplit_once('/').map_or("", |(dir, _)| dir);
            for asset in &integration.assets {
                let path = format!("{prefix}/{}", asset.from);
                collect_project_files(root, &path, &mut files)?;
            }
            for skill in &integration.skills {
                let path = if skill.local {
                    format!("{prefix}/skills/{}", skill.name)
                } else {
                    format!("skills/{}", skill.name)
                };
                collect_project_files(root, &path, &mut files)?;
            }
        }
    }
    pack(&files)
}

fn checked_project_path(root: &Path, name: &str) -> Result<PathBuf> {
    if !manifest::safe_path(name) || name.split('/').count() > 16 {
        return Err(invalid(format!("unsafe project file `{name}`")));
    }
    let mut path = root.to_path_buf();
    for part in Path::new(name).components() {
        path.push(part);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|e| invalid(format!("cannot read `{}`: {e}", path.display())))?;
        if metadata.file_type().is_symlink() {
            return Err(invalid(format!(
                "project file `{name}` traverses a symbolic link"
            )));
        }
    }
    Ok(path)
}

fn read_project_file(root: &Path, name: &str) -> Result<Vec<u8>> {
    let path = checked_project_path(root, name)?;
    let metadata =
        std::fs::metadata(&path).map_err(|e| invalid(format!("cannot read `{name}`: {e}")))?;
    if !metadata.is_file() || metadata.len() > MAX_UNPACKED_BYTES {
        return Err(invalid(format!(
            "`{name}` is not a regular file within the size limit"
        )));
    }
    std::fs::read(path).map_err(|e| invalid(format!("cannot read `{name}`: {e}")))
}

fn collect_project_files(
    root: &Path,
    name: &str,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if files.len() > 1024 {
        return Err(invalid("plugin project has more than 1024 files"));
    }
    let path = checked_project_path(root, name)?;
    if path.is_dir() {
        let entries =
            std::fs::read_dir(&path).map_err(|e| invalid(format!("cannot list `{name}`: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| invalid(format!("cannot list `{name}`: {e}")))?;
            let child = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid(format!("non-UTF-8 file under `{name}`")))?;
            collect_project_files(root, &format!("{name}/{child}"), files)?;
        }
    } else {
        files.insert(name.to_string(), read_project_file(root, name)?);
    }
    Ok(())
}

/// Read a release, rejecting links, duplicate paths and oversized or unlisted files.
///
/// # Errors
///
/// [`Error::PluginPackageInvalid`] for an unsafe/incomplete archive, or a manifest error.
pub fn unpack(archive: &[u8]) -> Result<PluginPackage> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(invalid("the compressed archive exceeds 64 MiB"));
    }
    let reader = flate2::read::GzDecoder::new(archive);
    let mut tarball = tar::Archive::new(reader);
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for entry in tarball
        .entries()
        .map_err(|e| invalid(format!("not a gzip tar archive: {e}")))?
    {
        let mut entry = entry.map_err(|e| invalid(format!("damaged tar entry: {e}")))?;
        if !entry.header().entry_type().is_file() {
            return Err(invalid(
                "only regular files are accepted; links and special files are refused",
            ));
        }
        let path = entry
            .path()
            .map_err(|e| invalid(format!("unreadable archive path: {e}")))?
            .into_owned();
        let path = path
            .to_str()
            .ok_or_else(|| invalid("archive paths must be UTF-8"))?;
        if !manifest::safe_path(path) {
            return Err(invalid(format!("unsafe archive path `{path}`")));
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(invalid("the expanded archive exceeds 256 MiB"));
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(format!("cannot read `{path}`: {e}")))?;
        if files.insert(path.to_string(), bytes).is_some() {
            return Err(invalid(format!("duplicate archive path `{path}`")));
        }
    }
    validate_files(&files)
}

fn validate_files(files: &BTreeMap<String, Vec<u8>>) -> Result<PluginPackage> {
    let total: u64 = files.values().map(|bytes| bytes.len() as u64).sum();
    if total > MAX_UNPACKED_BYTES {
        return Err(invalid("the expanded archive exceeds 256 MiB"));
    }
    for path in files.keys() {
        if !manifest::safe_path(path) {
            return Err(invalid(format!("unsafe archive path `{path}`")));
        }
    }
    let text = std::str::from_utf8(
        files
            .get(MANIFEST_FILE)
            .ok_or_else(|| invalid("missing plugin.toml"))?,
    )
    .map_err(|_| invalid("plugin.toml is not UTF-8"))?;
    let root = manifest::parse(text)?;
    let mut allowed = HashSet::from([MANIFEST_FILE]);
    for module in &root.modules {
        for (path, expected) in [
            (&module.manifest, &module.manifest_sha256),
            (&module.entry, &module.sha256),
        ] {
            let bytes = files
                .get(path)
                .ok_or_else(|| invalid(format!("module `{}` is missing `{path}`", module.id)))?;
            if sha256_hex(bytes) != *expected {
                return Err(invalid(format!(
                    "module `{}`: `{path}` does not match the declared SHA-256",
                    module.id
                )));
            }
            allowed.insert(path.as_str());
        }
        let text = std::str::from_utf8(&files[&module.manifest])
            .map_err(|_| invalid(format!("module `{}` manifest is not UTF-8", module.id)))?;
        match module.kind {
            PluginModuleKind::Source => {
                let source =
                    crate::source::manifest::parse(text, &crate::mining::registry::BUILTIN_NAMES)?;
                if source.source.name != module.id || source.source.version != module.version {
                    return Err(invalid(format!(
                        "source module `{}` identity/version differs from its own manifest",
                        module.id
                    )));
                }
                if module.contract.as_deref() != Some(source.compatibility.contract.as_str()) {
                    return Err(invalid(format!(
                        "source module `{}` contract differs from its source manifest",
                        module.id
                    )));
                }
                if !files[&module.entry].starts_with(b"\0asm") {
                    return Err(invalid(format!(
                        "source module `{}` is not WebAssembly",
                        module.id
                    )));
                }
            }
            PluginModuleKind::Integration => {
                let integration = crate::integration::manifest::parse(text)?;
                if integration.integration.id != module.id
                    || integration.integration.version != module.version
                {
                    return Err(invalid(format!(
                        "integration module `{}` identity/version differs from its own manifest",
                        module.id
                    )));
                }
                if module.contract.is_some() {
                    return Err(invalid(format!(
                        "integration module `{}` has no WASM contract",
                        module.id
                    )));
                }
                let prefix = module.manifest.rsplit_once('/').map_or("", |(dir, _)| dir);
                let under = |path: &str| format!("{prefix}/{path}");
                for asset in &integration.assets {
                    let path = under(&asset.from);
                    let members: Vec<_> = files
                        .keys()
                        .filter(|name| **name == path || name.starts_with(&format!("{path}/")))
                        .collect();
                    if members.is_empty() {
                        return Err(invalid(format!(
                            "integration module `{}` is missing asset `{}`",
                            module.id, asset.from
                        )));
                    }
                    allowed.extend(members.into_iter().map(String::as_str));
                }
                for skill in &integration.skills {
                    let path = if skill.local {
                        under(&format!("skills/{}", skill.name))
                    } else {
                        format!("skills/{}", skill.name)
                    };
                    let members: Vec<_> = files
                        .keys()
                        .filter(|name| name.starts_with(&format!("{path}/")))
                        .collect();
                    if members.is_empty() {
                        return Err(invalid(format!(
                            "integration module `{}` is missing skill `{}`",
                            module.id, skill.name
                        )));
                    }
                    allowed.extend(members.into_iter().map(String::as_str));
                }
            }
            PluginModuleKind::EmbeddingProvider => {
                // The metadata can be discovered, but #256 defines how its code is run.
            }
        }
    }
    if let Some(path) = files.keys().find(|path| !allowed.contains(path.as_str())) {
        return Err(invalid(format!("unlisted plugin file `{path}`")));
    }
    Ok(PluginPackage {
        manifest: root,
        manifest_text: text.to_string(),
        files: files.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PluginInfo, PluginModule, PluginModuleKind};

    fn fixture() -> BTreeMap<String, Vec<u8>> {
        let source = b"format = 1\n[source]\nname = 'one'\nversion = '0.1.0'\ndescription = 'one'\n[compatibility]\ncontract = '0.4.0'\nmemcastle = '>=0.4.0'\n";
        let second = b"format = 1\n[source]\nname = 'two'\nversion = '0.1.0'\ndescription = 'two'\n[compatibility]\ncontract = '0.4.0'\nmemcastle = '>=0.4.0'\n";
        let component = b"\0asm\x01\0\0\0";
        let modules = [("one", source.as_slice()), ("two", second.as_slice())]
            .into_iter()
            .map(|(id, bytes)| PluginModule {
                id: id.to_string(),
                kind: PluginModuleKind::Source,
                version: "0.1.0".to_string(),
                manifest: format!("modules/{id}/memcastle-source.toml"),
                manifest_sha256: sha256_hex(bytes),
                entry: format!("modules/{id}/source.wasm"),
                sha256: sha256_hex(component),
                optional: false,
                contract: Some("0.4.0".to_string()),
                config_schema: None,
            })
            .collect::<Vec<_>>();
        let manifest = PluginManifest {
            format: 1,
            plugin: PluginInfo {
                id: "example".to_string(),
                version: "0.1.0".to_string(),
                provider: "example".to_string(),
                description: "Two independent sources".to_string(),
                repository: "https://github.com/example/example".to_string(),
                license: "MIT".to_string(),
            },
            memcastle: ">=0.4.0".to_string(),
            dependencies: Vec::new(),
            shared_config_schema: None,
            authentication: None,
            modules,
        };
        let mut files = BTreeMap::from([(
            MANIFEST_FILE.to_string(),
            toml::to_string(&manifest).unwrap().into_bytes(),
        )]);
        for (id, bytes) in [("one", source.as_slice()), ("two", second.as_slice())] {
            files.insert(
                format!("modules/{id}/memcastle-source.toml"),
                bytes.to_vec(),
            );
            files.insert(format!("modules/{id}/source.wasm"), component.to_vec());
        }
        files
    }

    #[test]
    fn two_sources_are_packaged_together_without_activation_state() {
        let files = fixture();
        let bytes = pack(&files).unwrap();
        assert_eq!(pack(&files).unwrap(), bytes);
        let package = unpack(&bytes).unwrap();
        assert_eq!(package.manifest.modules.len(), 2);
        assert_eq!(package.files, files);
    }

    #[test]
    fn an_artifact_changed_after_manifest_creation_is_refused() {
        let mut files = fixture();
        files.insert(
            "modules/two/source.wasm".to_string(),
            b"\0asmreplaced".to_vec(),
        );
        assert!(matches!(
            pack(&files),
            Err(Error::PluginPackageInvalid { .. })
        ));
    }

    #[test]
    fn an_extra_file_or_path_escape_is_refused() {
        let mut files = fixture();
        files.insert("../outside".to_string(), Vec::new());
        assert!(matches!(
            pack(&files),
            Err(Error::PluginPackageInvalid { .. })
        ));
        files.remove("../outside");
        files.insert("modules/unreviewed/source.wasm".to_string(), Vec::new());
        assert!(matches!(
            pack(&files),
            Err(Error::PluginPackageInvalid { .. })
        ));
    }

    #[test]
    fn a_module_cannot_point_at_another_modules_artifact_directory() {
        let files = fixture();
        let mut manifest =
            manifest::parse(std::str::from_utf8(&files[MANIFEST_FILE]).unwrap()).unwrap();
        manifest.modules[0].entry = "modules/two/renamed-source.wasm".into();
        assert!(matches!(
            manifest::validate(&manifest),
            Err(Error::PluginManifestInvalid { .. })
        ));
    }

    #[test]
    fn an_archive_link_is_refused_before_following_any_path() {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tarball = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name("../../outside").unwrap();
        header.set_size(0);
        header.set_cksum();
        tarball
            .append_data(&mut header, "modules/evil/source.wasm", &[][..])
            .unwrap();
        let archive = tarball.into_inner().unwrap().finish().unwrap();
        assert!(matches!(
            unpack(&archive),
            Err(Error::PluginPackageInvalid { .. })
        ));
    }

    #[test]
    fn an_empty_plugin_has_no_implicitly_activated_modules() {
        let mut files = fixture();
        let mut manifest =
            manifest::parse(std::str::from_utf8(&files[MANIFEST_FILE]).unwrap()).unwrap();
        manifest.modules.clear();
        files.retain(|name, _| name == MANIFEST_FILE);
        files.insert(
            MANIFEST_FILE.to_string(),
            toml::to_string(&manifest).unwrap().into_bytes(),
        );
        assert!(
            unpack(&pack(&files).unwrap())
                .unwrap()
                .manifest
                .modules
                .is_empty()
        );
    }

    #[test]
    fn a_source_and_integration_share_a_package_but_remain_distinct_modules() {
        let mut files = fixture();
        let mut manifest =
            manifest::parse(std::str::from_utf8(&files[MANIFEST_FILE]).unwrap()).unwrap();
        manifest.modules.retain(|module| module.id == "one");
        files.remove("modules/two/memcastle-source.toml");
        files.remove("modules/two/source.wasm");
        let integration = b"format = 1\n[integration]\nid = 'assistant'\nversion = '0.1.0'\ndescription = 'An assistant integration'\n[compatibility]\nmemcastle = '>=0.4.0'\n[agent]\nkind = 'opencode'\nentry = 'dist/plugin.js'\n[[assets]]\nfrom = 'dist'\nto = '.'\n";
        let entry = b"export default () => {}";
        manifest.modules.push(PluginModule {
            id: "assistant".to_string(),
            kind: PluginModuleKind::Integration,
            version: "0.1.0".to_string(),
            manifest: "modules/assistant/memcastle-integration.toml".to_string(),
            manifest_sha256: sha256_hex(integration),
            entry: "modules/assistant/dist/plugin.js".to_string(),
            sha256: sha256_hex(entry),
            optional: false,
            contract: None,
            config_schema: None,
        });
        files.insert(
            "modules/assistant/memcastle-integration.toml".to_string(),
            integration.to_vec(),
        );
        files.insert(
            "modules/assistant/dist/plugin.js".to_string(),
            entry.to_vec(),
        );
        files.insert(
            MANIFEST_FILE.to_string(),
            toml::to_string(&manifest).unwrap().into_bytes(),
        );
        let package = unpack(&pack(&files).unwrap()).unwrap();
        assert_eq!(package.manifest.modules[0].kind, PluginModuleKind::Source);
        assert_eq!(
            package.manifest.modules[1].kind,
            PluginModuleKind::Integration
        );
    }

    #[test]
    fn an_embedding_module_is_discoverable_without_pretending_to_be_a_source_adapter() {
        let mut files = fixture();
        let mut manifest =
            manifest::parse(std::str::from_utf8(&files[MANIFEST_FILE]).unwrap()).unwrap();
        let entry = b"future embedding contract";
        let config = b"{\"format\":1}";
        manifest.modules.push(PluginModule {
            id: "embeddings".into(),
            kind: PluginModuleKind::EmbeddingProvider,
            version: "0.1.0".into(),
            manifest: "modules/embeddings/manifest.json".into(),
            manifest_sha256: sha256_hex(config),
            entry: "modules/embeddings/provider.bin".into(),
            sha256: sha256_hex(entry),
            optional: true,
            contract: None,
            config_schema: None,
        });
        files.insert("modules/embeddings/manifest.json".into(), config.to_vec());
        files.insert("modules/embeddings/provider.bin".into(), entry.to_vec());
        files.insert(
            MANIFEST_FILE.into(),
            toml::to_string(&manifest).unwrap().into_bytes(),
        );
        let inspected = unpack(&pack(&files).unwrap()).unwrap();
        assert_eq!(
            inspected.manifest.modules[2].kind,
            PluginModuleKind::EmbeddingProvider
        );
    }

    #[test]
    fn a_local_plugin_project_packages_only_its_declared_artifacts() {
        let root = tempfile::tempdir().unwrap();
        for (path, bytes) in fixture() {
            let target = root.path().join(&path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
        std::fs::write(root.path().join("Cargo.toml"), b"build-only metadata").unwrap();
        let packed = pack_project(root.path()).unwrap();
        assert!(!unpack(&packed).unwrap().files.contains_key("Cargo.toml"));
    }
}
