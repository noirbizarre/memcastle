//! Parse and validate the parent manifest before touching its artifacts.

use std::collections::HashSet;
use std::path::{Component, Path};

use crate::domain::{PluginManifest, PluginModuleKind, is_valid_source_name};
use crate::error::{Error, Result};

fn invalid(message: impl Into<String>) -> Error {
    Error::PluginManifestInvalid {
        message: message.into(),
    }
}

/// Reject traversal and ambiguous spellings even before an archive is read.
#[must_use]
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Read the strictly versioned plugin contract.
///
/// # Errors
///
/// [`Error::PluginManifestInvalid`] identifies the first malformed field.
pub fn parse(text: &str) -> Result<PluginManifest> {
    let manifest: PluginManifest =
        toml::from_str(text).map_err(|e| invalid(format!("plugin.toml: {}", e.message())))?;
    validate(&manifest)?;
    Ok(manifest)
}

/// Check identities, version requirements, paths and artifact digests.
///
/// # Errors
///
/// [`Error::PluginManifestInvalid`] identifies the first malformed field.
pub fn validate(manifest: &PluginManifest) -> Result<()> {
    if manifest.format != crate::domain::plugin::PLUGIN_FORMAT {
        return Err(invalid(format!(
            "format {} is unsupported; upgrade MemCastle or use format {}",
            manifest.format,
            crate::domain::plugin::PLUGIN_FORMAT
        )));
    }
    let info = &manifest.plugin;
    if manifest.modules.len() > 64 || manifest.dependencies.len() > 64 {
        return Err(invalid(
            "a plugin may declare at most 64 modules and 64 dependencies",
        ));
    }
    for (field, value) in [("plugin.id", &info.id), ("plugin.provider", &info.provider)] {
        if !is_valid_source_name(value) {
            return Err(invalid(format!(
                "{field} `{value}` must be a lowercase hyphenated identifier"
            )));
        }
    }
    version(&info.version, "plugin.version")?;
    requirement(&manifest.memcastle, "memcastle")?;
    if manifest
        .shared_config_schema
        .as_ref()
        .is_some_and(|schema| !schema.is_object())
    {
        return Err(invalid("shared_config_schema must be a JSON Schema object"));
    }
    if let Some(authentication) = &manifest.authentication
        && (!matches!(
            authentication.kind.as_str(),
            "oauth_device" | "oauth_browser"
        ) || !crate::domain::is_valid_source_name(&authentication.credential))
    {
        return Err(invalid(
            "authentication must name a supported public scheme and a credential reference, never a token",
        ));
    }
    if info.description.trim().is_empty() || info.description.contains('\n') {
        return Err(invalid("plugin.description must be one non-empty line"));
    }
    if !crate::domain::is_secure_endpoint(&info.repository) || info.license.trim().is_empty() {
        return Err(invalid(
            "plugin.repository must be an HTTPS repository URL (or loopback HTTP), and plugin.license is required",
        ));
    }
    let mut dependencies = HashSet::new();
    for dependency in &manifest.dependencies {
        if !is_valid_source_name(&dependency.id)
            || dependency.id == info.id
            || !dependencies.insert(&dependency.id)
        {
            return Err(invalid(format!(
                "dependency `{}` is invalid, duplicated or self-referential",
                dependency.id
            )));
        }
        requirement(&dependency.version, "dependency.version")?;
    }
    let mut modules = HashSet::new();
    let mut paths = HashSet::new();
    for module in &manifest.modules {
        if !is_valid_source_name(&module.id) || !modules.insert(&module.id) {
            return Err(invalid(format!(
                "module `{}` is invalid or duplicated",
                module.id
            )));
        }
        version(&module.version, "module.version")?;
        if module
            .config_schema
            .as_ref()
            .is_some_and(|schema| !schema.is_object())
        {
            return Err(invalid(format!(
                "module `{}` config_schema must be a JSON Schema object",
                module.id
            )));
        }
        for path in [&module.manifest, &module.entry] {
            if !safe_path(path) || !paths.insert(path) {
                return Err(invalid(format!(
                    "module `{}` has an unsafe or duplicated artifact path `{path}`",
                    module.id
                )));
            }
            if !path.starts_with(&format!("modules/{}/", module.id)) {
                return Err(invalid(format!(
                    "module `{}` artifact `{path}` must stay under modules/{}/",
                    module.id, module.id
                )));
            }
        }
        for digest in [&module.manifest_sha256, &module.sha256] {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid(format!(
                    "module `{}` needs lowercase SHA-256 digests for its artifacts",
                    module.id
                )));
            }
        }
        match module.kind {
            PluginModuleKind::Source
                if !module.manifest.ends_with("/memcastle-source.toml")
                    || !module.entry.ends_with("/source.wasm") =>
            {
                return Err(invalid(format!(
                    "source module `{}` must declare its source manifest and source.wasm",
                    module.id
                )));
            }
            PluginModuleKind::Integration
                if !module.manifest.ends_with("/memcastle-integration.toml") =>
            {
                return Err(invalid(format!(
                    "integration module `{}` must declare its integration manifest",
                    module.id
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn version(value: &str, field: &str) -> Result<()> {
    semver::Version::parse(value)
        .map(|_| ())
        .map_err(|e| invalid(format!("{field} `{value}` is not a semantic version: {e}")))
}

fn requirement(value: &str, field: &str) -> Result<()> {
    semver::VersionReq::parse(value).map(|_| ()).map_err(|e| {
        invalid(format!(
            "{field} `{value}` is not a version requirement: {e}"
        ))
    })
}
