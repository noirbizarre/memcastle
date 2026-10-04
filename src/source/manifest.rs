//! Reading and validating `memcastle-source.toml`.

use crate::domain::{SourceManifest, contract_compatibility, contract_version};
use crate::error::{Error, Result};

/// The longest a source name may be: it is a path segment, a CLI argument and a column in a table.
const MAX_NAME_LEN: usize = 48;

/// Parse and validate a manifest.
///
/// `reserved` are names a source may not take, so that an installed package cannot shadow a built-in source.
///
/// # Errors
///
/// [`Error::SourceManifestInvalid`] naming the first thing wrong.
pub fn parse(text: &str, reserved: &[&str]) -> Result<SourceManifest> {
    let manifest: SourceManifest =
        toml::from_str(text).map_err(|source| Error::SourceManifestInvalid {
            message: source.message().to_string(),
        })?;
    validate(&manifest, reserved)?;
    Ok(manifest)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::SourceManifestInvalid {
        message: message.into(),
    }
}

/// Check every rule a parse cannot.
///
/// # Errors
///
/// [`Error::SourceManifestInvalid`] naming the first thing wrong.
pub fn validate(manifest: &SourceManifest, reserved: &[&str]) -> Result<()> {
    let name = &manifest.source.name;
    // A name is a directory under the sources directory: restricting its alphabet is what keeps `../x` out of it.
    let name_ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !name_ok {
        return Err(invalid(format!(
            "source.name `{name}` must be 1 to {MAX_NAME_LEN} lowercase letters, digits or `-`, and not start or end with `-`"
        )));
    }
    if reserved.contains(&name.as_str()) {
        return Err(invalid(format!(
            "source.name `{name}` is a built-in source; choose another name"
        )));
    }
    if semver::Version::parse(&manifest.source.version).is_err() {
        return Err(invalid(format!(
            "source.version `{}` is not a semantic version like `0.1.0`",
            manifest.source.version
        )));
    }
    let description = manifest.source.description.trim();
    if description.is_empty() || description.contains('\n') {
        return Err(invalid("source.description must be one non-empty line"));
    }
    if semver::VersionReq::parse(&manifest.compatibility.memcastle).is_err() {
        return Err(invalid(format!(
            "compatibility.memcastle `{}` is not a version requirement like `>=0.3, <0.4`",
            manifest.compatibility.memcastle
        )));
    }
    // The format of `contract` is checked now; whether this MemCastle implements it is checked when it runs.
    if let Err(reason) = contract_version(&manifest.compatibility.contract) {
        return Err(invalid(format!("compatibility.contract: {reason}")));
    }
    for entry in &manifest.permissions.filesystem.read {
        let ok = entry == "locator"
            || entry.starts_with("~/")
            || std::path::Path::new(entry).is_absolute();
        if !ok {
            return Err(invalid(format!(
                "permissions.filesystem.read `{entry}` must be `locator`, an absolute path, or start with `~/`"
            )));
        }
    }
    for program in &manifest.permissions.process {
        // An exact program name, never a path or a command line: a path would let a manifest name any file.
        if program.is_empty()
            || program.contains(['/', '\\'])
            || program.contains(char::is_whitespace)
        {
            return Err(invalid(format!(
                "permissions.process `{program}` must be a bare program name such as `git`"
            )));
        }
    }
    for variable in &manifest.permissions.env {
        let ok = !variable.is_empty()
            && variable
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(invalid(format!(
                "permissions.env `{variable}` must be an environment variable name"
            )));
        }
    }
    if manifest.limits.memory_mib == Some(0) || manifest.limits.timeout_secs == Some(0) {
        return Err(invalid("limits must be greater than zero when set"));
    }
    if let Some(build) = &manifest.build
        && (build.command.is_empty() || build.output.is_empty())
    {
        return Err(invalid("build.command and build.output must not be empty"));
    }
    Ok(())
}

/// Whether this MemCastle can run `manifest`: it implements the contract the source was built for, and the
/// source's requirement on MemCastle's version holds.
///
/// # Errors
///
/// [`Error::SourceIncompatible`] saying which of the two fails.
pub fn check_compatible(manifest: &SourceManifest) -> Result<()> {
    let incompatible = |reason: String| Error::SourceIncompatible {
        name: manifest.source.name.clone(),
        reason,
    };
    contract_compatibility(&manifest.compatibility.contract).map_err(incompatible)?;
    let requirement = semver::VersionReq::parse(&manifest.compatibility.memcastle)
        .map_err(|e| incompatible(e.to_string()))?;
    let running = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| incompatible(e.to_string()))?;
    // A pre-release of the running version (`0.3.0-rc.1`) is held to the release's requirement: otherwise every
    // release candidate would be incompatible with a source built for the release.
    let release = semver::Version::new(running.major, running.minor, running.patch);
    if requirement.matches(&release) {
        Ok(())
    } else {
        Err(incompatible(format!(
            "it requires MemCastle {}, and this is {running}",
            manifest.compatibility.memcastle
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[source]
name = "demo"
version = "0.1.0"
description = "demo documents"

[compatibility]
contract = "0.1"
memcastle = ">=0.1"

[permissions.filesystem]
read = ["locator"]
"#;

    fn with(edit: impl Fn(&str) -> String) -> Result<SourceManifest> {
        parse(&edit(GOOD), &["directory"])
    }

    #[test]
    fn a_complete_manifest_parses_with_defaults_for_what_it_leaves_out() {
        let manifest = parse(GOOD, &[]).unwrap();
        assert_eq!(manifest.source.name, "demo");
        assert!(!manifest.capabilities.needs_credentials);
        assert!(!manifest.permissions.network);
        assert_eq!(manifest.permissions.filesystem.read, ["locator"]);
    }

    #[test]
    fn every_rule_names_the_field_it_is_about() {
        let cases = [
            ("name = \"demo\"", "name = \"Demo\"", "source.name"),
            ("name = \"demo\"", "name = \"../x\"", "source.name"),
            ("name = \"demo\"", "name = \"directory\"", "built-in"),
            ("version = \"0.1.0\"", "version = \"one\"", "source.version"),
            ("\">=0.1\"", "\"lots\"", "compatibility.memcastle"),
            (
                "\"locator\"",
                "\"relative/dir\"",
                "permissions.filesystem.read",
            ),
        ];
        for (from, to, expected) in cases {
            let error = with(|text| text.replace(from, to)).unwrap_err().to_string();
            assert!(error.contains(expected), "{to}: {error}");
        }
    }

    #[test]
    fn a_process_permission_must_be_a_bare_name_never_a_path_or_a_command_line() {
        for bad in ["/usr/bin/git", "git status", "..\\\\git", ""] {
            let text = format!("{GOOD}\n[permissions]\nprocess = [\"{bad}\"]\n")
                .replace("[permissions.filesystem]\nread = [\"locator\"]\n", "");
            assert!(parse(&text, &[]).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn an_unknown_key_anywhere_is_an_error_not_a_silent_omission() {
        assert!(with(|text| format!("{text}\n[permission]\nnetwork = true\n")).is_err());
        assert!(with(|text| text.replace("version =", "versions =")).is_err());
    }

    #[test]
    fn a_manifest_for_another_contract_is_valid_but_incompatible() {
        let manifest = with(|text| text.replace("\"0.1\"", "\"0.9\"")).unwrap();
        let error = check_compatible(&manifest).unwrap_err();
        assert!(matches!(error, Error::SourceIncompatible { .. }), "{error}");
    }

    #[test]
    fn a_version_requirement_this_memcastle_does_not_meet_is_incompatible() {
        let manifest = with(|text| text.replace("\">=0.1\"", "\">=99\"")).unwrap();
        let error = check_compatible(&manifest).unwrap_err().to_string();
        assert!(error.contains(">=99"), "{error}");
        assert!(check_compatible(&parse(GOOD, &[]).unwrap()).is_ok());
    }
}
