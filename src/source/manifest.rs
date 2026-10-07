//! Reading and validating `memcastle-source.toml`.

use crate::domain::{SourceManifest, contract_version};
use crate::error::{Error, Result};

use crate::domain::{
    MANIFEST_FORMAT, MAX_SOURCE_NAME_LEN as MAX_NAME_LEN, is_valid_source_name,
    version_compatibility,
};

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
    if !is_valid_source_name(name) {
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
    if manifest.format == 0 {
        return Err(invalid("format must be 1 or more"));
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
    for (key, option) in &manifest.options {
        // The key is typed on a command line next to paths, so the CLI tells the two apart by this very rule.
        if !crate::domain::is_option_key(key) {
            return Err(invalid(format!(
                "options.{key} must be lowercase letters, digits, `-` or `_`, starting with a letter"
            )));
        }
        if option.description.trim().is_empty() || option.description.contains('\n') {
            return Err(invalid(format!(
                "options.{key}.description must be one non-empty line"
            )));
        }
    }
    for (key, trigger) in &manifest.triggers {
        // Only the mechanisms a source has to speak for: a timetable and a poll are the host's, for every source.
        match crate::domain::TriggerMechanism::parse(key) {
            Some(kind) if !kind.is_host_provided() => {}
            Some(_) => {
                return Err(invalid(format!(
                    "triggers.{key} is not declared: every source can be scheduled and polled, so only `webhook` and \
                     `watch` are listed here"
                )));
            }
            None => {
                return Err(invalid(format!(
                    "triggers.{key} is not a trigger mechanism; declare `webhook` or `watch`"
                )));
            }
        }
        if trigger.description.trim().is_empty() || trigger.description.contains('\n') {
            return Err(invalid(format!(
                "triggers.{key}.description must be one non-empty line"
            )));
        }
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
    if let Some(oauth) = &manifest.permissions.oauth {
        oauth.validate().map_err(invalid)?;
        // The flag is what `source list` and a miner's activation check read, so a source that signs in must say so.
        if !manifest.capabilities.needs_credentials {
            return Err(invalid(
                "permissions.oauth is set, so capabilities.needs_credentials must be true",
            ));
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
    // A format this MemCastle does not know may mean something it would silently ignore, so it is refused whole.
    if manifest.format > MANIFEST_FORMAT {
        return Err(incompatible(format!(
            "its manifest is format {}, and this MemCastle reads up to format {MANIFEST_FORMAT}; upgrade MemCastle",
            manifest.format
        )));
    }
    let running = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| incompatible(e.to_string()))?;
    version_compatibility(
        &manifest.compatibility.contract,
        &manifest.compatibility.memcastle,
        &running,
    )
    .map_err(incompatible)
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
contract = "0.4"
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
        let manifest = with(|text| text.replace("\"0.4\"", "\"0.9\"")).unwrap();
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

    #[test]
    fn the_remaining_rules_each_name_what_they_refuse() {
        let cases = [
            (
                GOOD.replace("description = \"demo documents\"", "description = \"  \""),
                "source.description",
            ),
            (
                GOOD.replace(
                    "description = \"demo documents\"",
                    "description = \"a\\nb\"",
                ),
                "source.description",
            ),
            (
                GOOD.replace("contract = \"0.4\"", "contract = \"x\""),
                "compatibility.contract",
            ),
            (
                format!("{GOOD}\n[permissions]\nenv = [\"NOT-A-NAME\"]\n"),
                "permissions.env",
            ),
            (
                format!("{GOOD}\n[permissions]\nenv = [\"\"]\n"),
                "permissions.env",
            ),
            (format!("{GOOD}\n[limits]\nmemory_mib = 0\n"), "limits"),
            (format!("{GOOD}\n[limits]\ntimeout_secs = 0\n"), "limits"),
            (
                format!("{GOOD}\n[build]\ncommand = []\noutput = \"x\"\n"),
                "build.command",
            ),
            (
                format!("{GOOD}\n[build]\ncommand = [\"make\"]\noutput = \"\"\n"),
                "build.output",
            ),
        ];
        for (text, expected) in cases {
            let error = parse(&text, &[]).unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "expected `{expected}` in `{error}`"
            );
        }
    }

    #[test]
    fn valid_environment_names_programs_and_limits_are_accepted() {
        let text = format!(
            "{GOOD}\n[permissions]\nenv = [\"GITHUB_TOKEN\"]\nprocess = [\"git\"]\nnetwork = true\n\n[limits]\nmemory_mib = 64\ntimeout_secs = 5\n"
        );
        let manifest = parse(&text, &[]).unwrap();
        assert_eq!(manifest.permissions.env, ["GITHUB_TOKEN"]);
        assert_eq!(manifest.limits.memory_mib, Some(64));
    }

    const OAUTH: &str = "\n[capabilities]\nneeds_credentials = true\n\n[permissions.oauth]\nclient_id = \"abc\"\nscopes = [\"read\"]\ntoken_url = \"https://auth.example.com/token\"\ndevice_authorization_url = \"https://auth.example.com/device\"\n";

    #[test]
    fn a_source_that_signs_in_declares_its_endpoints_and_says_it_needs_credentials() {
        let manifest = parse(&format!("{GOOD}{OAUTH}"), &[]).unwrap();
        let oauth = manifest.permissions.oauth.unwrap();
        assert_eq!(oauth.client_id, "abc");
        assert!(oauth.supports_device() && !oauth.supports_browser());
    }

    #[test]
    fn a_sign_in_without_the_credentials_flag_or_with_an_unsafe_endpoint_is_refused() {
        let unflagged = OAUTH.replace("needs_credentials = true", "needs_credentials = false");
        let error = parse(&format!("{GOOD}{unflagged}"), &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs_credentials"), "{error}");

        let insecure = OAUTH.replace(
            "https://auth.example.com/token",
            "http://auth.example.com/token",
        );
        let error = parse(&format!("{GOOD}{insecure}"), &[])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("token_url") && error.contains("https"),
            "{error}"
        );

        let secret = format!("{GOOD}{OAUTH}client_secret = \"nope\"\n");
        assert!(
            parse(&secret, &[]).is_err(),
            "a client secret has no place in a manifest"
        );
    }

    #[test]
    fn a_manifest_without_a_format_is_format_one_and_a_newer_format_is_refused_as_incompatible() {
        assert_eq!(parse(GOOD, &[]).unwrap().format, MANIFEST_FORMAT);

        let newer = parse(&format!("format = 2\n{GOOD}"), &[]).unwrap();
        let error = check_compatible(&newer).unwrap_err().to_string();
        assert!(
            error.contains("format 2") && error.contains("upgrade MemCastle"),
            "{error}"
        );

        let error = parse(&format!("format = 0\n{GOOD}"), &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("format"), "{error}");
    }

    #[test]
    fn provenance_fields_are_optional_and_kept() {
        let text = GOOD.replace(
            "description = \"demo documents\"",
            "description = \"demo documents\"\nlicense = \"MIT\"\nhomepage = \"https://example.org\"\nrepository = \"https://example.org/git\"",
        );
        let manifest = parse(&text, &[]).unwrap();
        assert_eq!(manifest.source.license.as_deref(), Some("MIT"));
        assert_eq!(
            manifest.source.repository.as_deref(),
            Some("https://example.org/git")
        );
        assert!(parse(GOOD, &[]).unwrap().source.license.is_none());
    }

    #[test]
    fn a_source_that_declares_no_trigger_is_complete_and_is_still_schedulable_and_pollable() {
        let manifest = parse(GOOD, &[]).unwrap();
        assert!(manifest.triggers.is_empty());
        let kinds: Vec<_> = manifest
            .trigger_specs()
            .iter()
            .map(|t| t.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            ["schedule", "poll"],
            "the host gives every source a timetable and a poll"
        );
    }

    #[test]
    fn declared_triggers_are_capabilities_and_change_nothing_a_user_consents_to() {
        let plain = parse(GOOD, &[]).unwrap();
        let declared = parse(
            &format!(
                "{GOOD}\n[triggers.watch]\ndescription = \"a file changes\"\n\n[triggers.webhook]\ndescription = \"a hook\"\n"
            ),
            &[],
        )
        .unwrap();
        let kinds: Vec<_> = declared
            .trigger_specs()
            .iter()
            .map(|t| t.kind.as_str())
            .collect();
        assert_eq!(kinds, ["schedule", "poll", "webhook", "watch"]);
        assert_eq!(
            plain.permissions.consent_digest("demo"),
            declared.permissions.consent_digest("demo"),
            "declaring a trigger asks for no permission and so cannot change what was agreed to"
        );
        assert_eq!(
            declared.format, MANIFEST_FORMAT,
            "no new manifest format is needed"
        );
    }

    #[test]
    fn a_trigger_that_is_the_hosts_or_unknown_or_undescribed_is_refused() {
        for (declaration, wants) in [
            ("[triggers.poll]\ndescription = \"x\"\n", "every source"),
            ("[triggers.schedule]\ndescription = \"x\"\n", "every source"),
            (
                "[triggers.cron]\ndescription = \"x\"\n",
                "not a trigger mechanism",
            ),
            ("[triggers.watch]\ndescription = \"\"\n", "description"),
        ] {
            let error = parse(&format!("{GOOD}\n{declaration}"), &[])
                .unwrap_err()
                .to_string();
            assert!(error.contains(wants), "{declaration}: {error}");
        }
    }

    #[test]
    fn the_reference_sources_declare_the_triggers_their_documentation_promises() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sources");
        for (name, expected) in [
            ("directory", vec!["schedule", "poll", "webhook", "watch"]),
            ("pi", vec!["schedule", "poll", "watch"]),
            ("opencode", vec!["schedule", "poll", "watch"]),
        ] {
            let text = std::fs::read_to_string(root.join(name).join("memcastle-source.toml"))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let manifest = parse(&text, &[]).unwrap_or_else(|e| panic!("{name}: {e}"));
            let kinds: Vec<_> = manifest
                .trigger_specs()
                .iter()
                .map(|t| t.kind.as_str())
                .collect();
            assert_eq!(kinds, expected, "{name}");
        }
    }
}
