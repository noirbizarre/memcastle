//! The distributable form of a source: a gzip-compressed tarball, and where an installed one lives on disk.
//!
//! An archive holds `memcastle-source.toml`, `source.wasm`, and optionally a `README.md` and a `LICENSE`; nothing
//! else is ever read from it, and every entry is matched by its exact top-level name, so an archive cannot place a
//! file anywhere (`../x`, an absolute path, a link) however it is crafted.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::domain::{SourceManifest, sha256_hex};
use crate::error::{Error, Result};

use super::{COMPONENT_FILE, MANIFEST_FILE, manifest};

/// The most an archive may expand to. A source is a program and a manifest; this bounds a decompression bomb, not a
/// real package.
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// The optional files an archive may carry, which are kept next to the component for the person who installed it.
const EXTRAS: [&str; 3] = ["README.md", "LICENSE", "LICENSE.md"];

/// A package as read from an archive.
#[derive(Debug, Clone)]
pub struct SourcePackage {
    /// The validated manifest.
    pub manifest: SourceManifest,
    /// The manifest exactly as written, kept so an install is reproducible.
    pub manifest_text: String,
    /// The WebAssembly component.
    pub component: Vec<u8>,
    /// SHA-256 of [`SourcePackage::component`].
    pub digest: String,
    /// The optional documentation files, by name.
    pub extras: Vec<(String, Vec<u8>)>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::SourcePackageInvalid {
        message: message.into(),
    }
}

/// Write an archive of a manifest, a component and optional extras.
///
/// The archive is deterministic (no timestamps, fixed modes), so packaging the same source twice gives the same
/// bytes and a digest can be published.
///
/// # Errors
///
/// [`Error::SourcePackageInvalid`] when the component is not WebAssembly or an archive cannot be written.
pub fn pack(
    manifest_text: &str,
    component: &[u8],
    extras: &[(String, Vec<u8>)],
) -> Result<Vec<u8>> {
    if !component.starts_with(b"\0asm") {
        return Err(invalid(format!(
            "`{COMPONENT_FILE}` is not a WebAssembly binary; run `memcastle source build` first"
        )));
    }
    let io = |source: std::io::Error| invalid(format!("writing the archive failed: {source}"));
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let mut add = |name: &str, bytes: &[u8]| -> Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        archive.append_data(&mut header, name, bytes).map_err(io)
    };
    add(MANIFEST_FILE, manifest_text.as_bytes())?;
    add(COMPONENT_FILE, component)?;
    for (name, bytes) in extras {
        if EXTRAS.contains(&name.as_str()) {
            add(name, bytes)?;
        }
    }
    let encoder = archive.into_inner().map_err(io)?;
    encoder.finish().map_err(io)
}

/// Read and validate an archive.
///
/// `reserved` are the names an installed source may not take (see [`manifest::parse`]).
///
/// # Errors
///
/// [`Error::SourcePackageInvalid`] when the archive is unreadable or lacks the manifest or the component, and
/// [`Error::SourceManifestInvalid`] when the manifest is wrong.
pub fn unpack(archive: &[u8], reserved: &[&str]) -> Result<SourcePackage> {
    let decoder = flate2::read::GzDecoder::new(archive).take(MAX_UNPACKED_BYTES + 1);
    let mut tarball = tar::Archive::new(decoder);
    let entries = tarball
        .entries()
        .map_err(|source| invalid(format!("not a gzip-compressed tar archive: {source}")))?;

    let mut manifest_text = None;
    let mut component = None;
    let mut extras = Vec::new();
    let mut total = 0_u64;
    for entry in entries {
        let mut entry =
            entry.map_err(|source| invalid(format!("the archive is damaged: {source}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|source| invalid(format!("an entry has an unreadable name: {source}")))?
            .into_owned();
        // `./name` is how some tools write a top-level file; anything deeper or absolute is not ours to read.
        let Some(name) = path
            .strip_prefix(".")
            .unwrap_or(&path)
            .to_str()
            .filter(|name| !name.contains(['/', '\\']))
            .map(str::to_string)
        else {
            continue;
        };
        let wanted =
            name == MANIFEST_FILE || name == COMPONENT_FILE || EXTRAS.contains(&name.as_str());
        if !wanted {
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(invalid("the archive expands to more than 256 MiB"));
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|source| invalid(format!("`{name}` cannot be read: {source}")))?;
        match name.as_str() {
            MANIFEST_FILE => manifest_text = Some(bytes),
            COMPONENT_FILE => component = Some(bytes),
            _ => extras.push((name, bytes)),
        }
    }

    let manifest_bytes = manifest_text.ok_or_else(|| {
        invalid(format!(
            "the archive has no `{MANIFEST_FILE}` at its top level"
        ))
    })?;
    let manifest_text = String::from_utf8(manifest_bytes)
        .map_err(|_| invalid(format!("`{MANIFEST_FILE}` is not UTF-8 text")))?;
    let component = component.ok_or_else(|| {
        invalid(format!(
            "the archive has no `{COMPONENT_FILE}` at its top level"
        ))
    })?;
    if !component.starts_with(b"\0asm") {
        return Err(invalid(format!(
            "`{COMPONENT_FILE}` is not a WebAssembly binary"
        )));
    }
    let manifest = manifest::parse(&manifest_text, reserved)?;
    Ok(SourcePackage {
        digest: sha256_hex(&component),
        manifest,
        manifest_text,
        component,
        extras,
    })
}

/// [`unpack`] with the names MemCastle reserves: what installing, and `source package`'s check, mean by "valid".
///
/// # Errors
///
/// As [`unpack`].
pub fn inspect(archive: &[u8]) -> Result<SourcePackage> {
    unpack(archive, &crate::mining::registry::BUILTIN_NAMES)
}

/// Where the installed source `name` lives under `sources_dir`.
///
/// `name` is a validated source name (lowercase letters, digits and `-`), so it cannot leave `sources_dir`.
#[must_use]
pub fn installed_dir(sources_dir: &Path, name: &str) -> PathBuf {
    sources_dir.join(name)
}

/// Write `package` under `sources_dir`, replacing an earlier install of the same name.
///
/// The component is written to a temporary file and renamed, so a daemon that loads it at that moment sees the old
/// file or the new one, never half of one.
///
/// # Errors
///
/// [`Error::Io`] when the directory cannot be written.
pub fn install(sources_dir: &Path, package: &SourcePackage) -> Result<()> {
    let dir = installed_dir(sources_dir, &package.manifest.source.name);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(dir.display().to_string(), e))?;
    let write = |name: &str, bytes: &[u8]| -> Result<()> {
        let target = dir.join(name);
        let temporary = dir.join(format!(".{name}.tmp"));
        std::fs::write(&temporary, bytes)
            .and_then(|()| std::fs::rename(&temporary, &target))
            .map_err(|e| Error::io(target.display().to_string(), e))
    };
    write(COMPONENT_FILE, &package.component)?;
    write(MANIFEST_FILE, package.manifest_text.as_bytes())?;
    for (name, bytes) in &package.extras {
        write(name, bytes)?;
    }
    Ok(())
}

/// The installed component of `name`.
///
/// # Errors
///
/// [`Error::Io`] when it is missing or unreadable.
pub fn read_installed_component(sources_dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = installed_dir(sources_dir, name).join(COMPONENT_FILE);
    std::fs::read(&path).map_err(|e| Error::io(path.display().to_string(), e))
}

/// Delete the installed files of `name`. Missing files are not an error: the goal is that they are gone.
///
/// # Errors
///
/// [`Error::Io`] when the directory exists but cannot be removed.
pub fn remove(sources_dir: &Path, name: &str) -> Result<()> {
    let dir = installed_dir(sources_dir, name);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::io(dir.display().to_string(), source)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
[source]
name = "demo"
version = "0.1.0"
description = "demo"

[compatibility]
contract = "0.2"
memcastle = ">=0.1"
"#;

    const WASM: &[u8] = b"\0asm\x0d\0\x01\0";

    #[test]
    fn an_archive_round_trips_its_manifest_component_and_digest() {
        let bytes = pack(MANIFEST, WASM, &[("README.md".into(), b"hi".to_vec())]).unwrap();
        let package = unpack(&bytes, &[]).unwrap();

        assert_eq!(package.manifest.source.name, "demo");
        assert_eq!(package.component, WASM);
        assert_eq!(package.digest, sha256_hex(WASM));
        assert_eq!(package.extras, [("README.md".to_string(), b"hi".to_vec())]);
    }

    #[test]
    fn packing_the_same_source_twice_gives_the_same_bytes() {
        assert_eq!(
            pack(MANIFEST, WASM, &[]).unwrap(),
            pack(MANIFEST, WASM, &[]).unwrap()
        );
    }

    #[test]
    fn a_component_that_is_not_webassembly_is_not_packed() {
        assert!(matches!(
            pack(MANIFEST, b"#!/bin/sh", &[]),
            Err(Error::SourcePackageInvalid { .. })
        ));
    }

    #[test]
    fn an_archive_missing_the_component_or_the_manifest_says_which() {
        let only_manifest = {
            let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::fast(),
            ));
            let mut header = tar::Header::new_gnu();
            header.set_size(MANIFEST.len() as u64);
            header.set_mode(0o644);
            archive
                .append_data(&mut header, MANIFEST_FILE, MANIFEST.as_bytes())
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap()
        };
        let error = unpack(&only_manifest, &[]).unwrap_err().to_string();
        assert!(error.contains(COMPONENT_FILE), "{error}");
        assert!(unpack(b"not an archive", &[]).is_err());
    }

    #[test]
    fn entries_outside_the_top_level_are_never_read() {
        // A nested manifest must not stand in for a missing top-level one.
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for name in ["sub/memcastle-source.toml", "sub/source.wasm"] {
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            archive
                .append_data(&mut header, name, &b"\0asm"[..])
                .unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        assert!(unpack(&bytes, &[]).is_err());
    }

    #[test]
    fn a_reserved_name_is_refused_when_unpacking() {
        let bytes = pack(MANIFEST, WASM, &[]).unwrap();
        assert!(unpack(&bytes, &["demo"]).is_err());
    }

    #[test]
    fn installing_writes_the_component_and_manifest_and_removing_deletes_them() {
        let dir = tempfile::tempdir().unwrap();
        let package = unpack(&pack(MANIFEST, WASM, &[]).unwrap(), &[]).unwrap();
        install(dir.path(), &package).unwrap();

        assert_eq!(read_installed_component(dir.path(), "demo").unwrap(), WASM);
        remove(dir.path(), "demo").unwrap();
        assert!(read_installed_component(dir.path(), "demo").is_err());
        // Removing what is not there is fine.
        remove(dir.path(), "demo").unwrap();
    }

    fn archive_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            archive.append_data(&mut header, name, *bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn files_that_are_not_part_of_a_package_and_directories_are_ignored_not_extracted() {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut dir = tar::Header::new_gnu();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_size(0);
        dir.set_mode(0o755);
        archive
            .append_data(&mut dir, "somewhere/", std::io::empty())
            .unwrap();
        for (name, bytes) in [
            (MANIFEST_FILE, MANIFEST.as_bytes()),
            (COMPONENT_FILE, WASM),
            ("install.sh", b"rm -rf /"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            archive.append_data(&mut header, name, bytes).unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();

        let package = unpack(&bytes, &[]).unwrap();

        assert!(
            package.extras.is_empty(),
            "nothing but the listed files is ever read"
        );
    }

    #[test]
    fn a_component_entry_that_is_not_webassembly_is_refused_when_unpacking() {
        let bytes = archive_of(&[
            (MANIFEST_FILE, MANIFEST.as_bytes()),
            (COMPONENT_FILE, b"#!/bin/sh"),
        ]);
        let error = unpack(&bytes, &[]).unwrap_err().to_string();
        assert!(error.contains("not a WebAssembly binary"), "{error}");
    }

    #[test]
    fn an_archive_that_claims_to_expand_past_the_ceiling_is_refused_before_it_is_read() {
        use std::io::Write;
        // Only the header of a 300 MiB entry: the size is checked before a byte of it is read, which is what makes a
        // decompression bomb cheap to refuse.
        let mut header = tar::Header::new_gnu();
        header.set_path(COMPONENT_FILE).unwrap();
        header.set_size(300 * 1024 * 1024);
        header.set_mode(0o644);
        header.set_cksum();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(header.as_bytes()).unwrap();
        let bytes = gzip.finish().unwrap();

        let error = unpack(&bytes, &[]).unwrap_err().to_string();

        assert!(error.contains("256 MiB"), "{error}");
    }

    #[test]
    fn removing_something_that_cannot_be_removed_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the source's directory should be: it exists, and is not a directory.
        std::fs::write(dir.path().join("demo"), "in the way").unwrap();
        assert!(matches!(remove(dir.path(), "demo"), Err(Error::Io { .. })));
    }

    #[test]
    fn inspecting_applies_the_reserved_built_in_names() {
        let bytes = pack(&MANIFEST.replace("demo", "directory"), WASM, &[]).unwrap();
        assert!(
            inspect(&bytes).is_err(),
            "a package may not take a built-in name"
        );
        assert!(inspect(&pack(MANIFEST, WASM, &[]).unwrap()).is_ok());
    }
}
