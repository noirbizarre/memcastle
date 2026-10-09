//! Read-only diagnostics for configuration, local prerequisites and the running daemon.
//!
//! The CLI may run this even when `Config::load` fails. Storage is only inspected through
//! `DaemonClient`: opening an embedded palace would create files and contend with its writer.

use std::path::Path;

use serde::Serialize;

use crate::app::MinerState;
use crate::client::DaemonClient;
use crate::config::{self, Config, EmbeddingProvider, ExtractionProvider, StoreConfig};
use crate::domain::{CredentialRef, MemoryMode, SourceState, TriggerStatus};
use crate::error::Error;
use crate::term::Painter;

/// Severity of a check; only `Error` changes the process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// The prerequisite is satisfied.
    Ok,
    /// Something needs attention but does not prevent local operation.
    Warning,
    /// A required prerequisite is broken.
    Error,
    /// The prerequisite cannot safely be verified right now.
    Skipped,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
            Self::Skipped => "SKIPPED",
        }
    }
}

/// One diagnostic, constructed from trusted text rather than unfiltered configuration or HTTP replies.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// The category to group this check under.
    pub area: &'static str,
    /// Stable name for scripting.
    pub check: String,
    /// Whether this is healthy, actionable, or unavailable.
    pub status: CheckStatus,
    /// A safe explanation of the result.
    pub summary: String,
    /// An action to take, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// The full doctor result, including findings when configuration is invalid.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    /// Checks in presentation order, grouped by area.
    pub findings: Vec<Finding>,
}

impl Report {
    fn add(
        &mut self,
        area: &'static str,
        check: impl Into<String>,
        status: CheckStatus,
        summary: impl Into<String>,
        remediation: Option<&str>,
    ) {
        self.findings.push(Finding {
            area,
            check: check.into(),
            status,
            summary: summary.into(),
            remediation: remediation.map(str::to_owned),
        });
    }

    /// Nonzero only when a blocking prerequisite failed.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        u8::from(
            self.findings
                .iter()
                .any(|finding| finding.status == CheckStatus::Error),
        )
    }

    /// Grouped terminal report; the same sanitized findings are used by JSON output.
    #[must_use]
    pub fn render(&self, painter: Painter) -> String {
        let mut lines = vec![painter.heading("MemCastle doctor")];
        let mut last_area = "";
        for finding in &self.findings {
            if finding.area != last_area {
                lines.push(format!("\n{}", painter.heading(finding.area)));
                last_area = finding.area;
            }
            let label = match finding.status {
                CheckStatus::Ok => painter.ok(finding.status.label()),
                CheckStatus::Warning => painter.warn(finding.status.label()),
                CheckStatus::Error => painter.error(finding.status.label()),
                CheckStatus::Skipped => painter.dim(finding.status.label()),
            };
            lines.push(format!(
                "  [{label}] {}: {}",
                finding.check, finding.summary
            ));
            if let Some(remediation) = &finding.remediation {
                lines.push(format!("        Fix: {remediation}"));
            }
        }
        lines.join("\n")
    }
}

/// Collect independent local checks, then use only read-only HTTP calls for runtime state.
pub async fn run(
    path: Option<&Path>,
    mode: Option<MemoryMode>,
    loaded: Result<Config, Error>,
) -> Report {
    let mut report = Report::default();
    let file = path
        .map(Path::to_path_buf)
        .or_else(config::paths::default_config_file);
    if let Some(file) = file.as_deref() {
        match std::fs::metadata(file) {
            Ok(metadata) if metadata.is_file() => report.add(
                "Configuration",
                "file",
                CheckStatus::Ok,
                "Configuration file is present.",
                None,
            ),
            Ok(_) => report.add(
                "Configuration",
                "file",
                CheckStatus::Error,
                "Configuration path is not a file.",
                Some("Point --config at a readable TOML file."),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && path.is_none() => report
                .add(
                    "Configuration",
                    "file",
                    CheckStatus::Ok,
                    "No default file; built-in defaults apply.",
                    None,
                ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => report.add(
                "Configuration",
                "file",
                CheckStatus::Error,
                "Explicit configuration file is missing.",
                Some("Fix --config/MEMCASTLE_CONFIG or create the file."),
            ),
            Err(_) => report.add(
                "Configuration",
                "file",
                CheckStatus::Error,
                "Configuration file cannot be inspected.",
                Some("Check the file and parent directory permissions."),
            ),
        }
    } else {
        report.add(
            "Configuration",
            "file",
            CheckStatus::Ok,
            "Built-in defaults apply.",
            None,
        );
    }

    let config = match loaded {
        Ok(config) => {
            report.add(
                "Configuration",
                "effective",
                CheckStatus::Ok,
                "Effective configuration passes validation.",
                None,
            );
            Some(config)
        }
        Err(error) => {
            // Config errors can contain a password, URL, or the offending environment value.
            // Select only known field names; never forward the original error to either renderer.
            let field = safe_config_field(&error);
            report.add(
                "Configuration",
                "effective",
                CheckStatus::Error,
                field.map_or_else(
                    || "Configuration cannot be parsed or validated.".to_string(),
                    |field| format!("Invalid setting: {field}."),
                ),
                Some("Fix the named configuration file or MEMCASTLE_* override and rerun doctor."),
            );
            None
        }
    };
    if let Some(file) = file.as_deref().filter(|file| file.is_file()) {
        unknown_keys(&mut report, file);
    }
    let Some(config) = config else {
        for area in [
            "Paths",
            "Providers",
            "Daemon and storage",
            "Sources and miners",
        ] {
            report.add(
                area,
                "dependent checks",
                CheckStatus::Skipped,
                "Requires a valid effective configuration.",
                None,
            );
        }
        return report;
    };

    paths(&mut report, &config);
    providers(&mut report, &config);
    online(&mut report, &config, mode).await;
    offline_miners(&mut report, &config);
    report
}

fn safe_config_field(error: &Error) -> Option<&'static str> {
    let Error::Config { message } = error else {
        return None;
    };
    // Do not derive a field name from arbitrary input: a typo or a URL could itself be a secret.
    const FIELDS: &[&str] = &[
        "palace.path",
        "assets.dir",
        "jobs.max_concurrency",
        "jobs.background_concurrency",
        "jobs.drain_timeout_secs",
        "jobs.lease_ttl_secs",
        "db.bind",
        "db.port",
        "auth.token",
        "embeddings.command",
        "embeddings.url",
        "embeddings.model",
        "embeddings.timeout_secs",
        "embeddings.batch_size",
        "extraction.command",
        "extraction.url",
        "extraction.model",
        "extraction.timeout_secs",
        "extraction.batch_size",
        "extraction.max_entities",
        "extraction.max_relations",
        "extraction.min_confidence",
        "mining.chunk_chars",
        "mining.max_file_bytes",
        "mining.max_documents",
        "mining.sources_dir",
        "mining.bundled_dir",
        "mining.registries",
        "mining.github_api_url",
        "mining.trusted_keys",
        "mining.trust",
        "mining.source_memory_mib",
        "mining.source_timeout_secs",
        "credentials.dir",
        "credentials.backend",
        "dedup.near_threshold",
        "webhook.bind",
        "webhook.port",
        "webhook.max_body_bytes",
        "[[miners]]",
        "[[triggers]]",
    ];
    FIELDS
        .iter()
        .copied()
        .find(|field| message.starts_with(field))
}

fn unknown_keys(report: &mut Report, file: &Path) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let mut unknown = Vec::new();
    let Ok(deserializer) = toml::de::Deserializer::parse(&text) else {
        return;
    };
    let parsed: Result<Config, _> =
        serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()));
    if parsed.is_ok() {
        for key in unknown {
            // A TOML key is user-controlled. Only ordinary short identifiers can be echoed safely;
            // the rest still get a warning without reflecting arbitrary text.
            let safe = key.len() <= 100
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-[]".contains(c));
            report.add(
                "Configuration",
                "unknown key",
                CheckStatus::Warning,
                if safe {
                    format!("Unrecognized setting `{key}` is ignored.")
                } else {
                    "An unrecognized setting is ignored.".to_string()
                },
                Some("Check the setting's spelling in docs/configuration.md."),
            );
        }
    }
}

fn directory(report: &mut Report, area: &'static str, check: &str, path: &Path, required: bool) {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {
            if std::fs::read_dir(path).is_ok() {
                report.add(
                    area,
                    check,
                    CheckStatus::Ok,
                    "Directory can be listed.",
                    None,
                );
            } else {
                report.add(
                    area,
                    check,
                    CheckStatus::Error,
                    "Directory cannot be listed.",
                    Some("Check directory access permissions."),
                );
            }
        }
        Ok(_) => report.add(
            area,
            check,
            CheckStatus::Error,
            "Expected a directory but found another file type.",
            Some("Correct the configured directory path."),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => report.add(
            area,
            check,
            CheckStatus::Skipped,
            "Optional directory does not exist yet.",
            None,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => report.add(
            area,
            check,
            CheckStatus::Error,
            "Required directory is missing.",
            Some("Create the directory or correct the configured path."),
        ),
        Err(_) => report.add(
            area,
            check,
            CheckStatus::Error,
            "Directory cannot be inspected.",
            Some("Check parent directory access permissions."),
        ),
    }
}

fn paths(report: &mut Report, config: &Config) {
    directory(report, "Paths", "palace.path", &config.palace.path, false);
    if let StoreConfig::Embedded { .. } = &config.store {
        directory(
            report,
            "Paths",
            "embedded database",
            &config.palace.path.join("db"),
            false,
        );
    } else {
        report.add(
            "Paths",
            "embedded database",
            CheckStatus::Skipped,
            "Remote storage is configured.",
            None,
        );
    }
    if let Some(dir) = &config.assets.dir {
        directory(report, "Paths", "assets.dir", dir, true);
    }
    if let Some(dir) = &config.mining.bundled_dir {
        directory(report, "Paths", "mining.bundled_dir", dir, true);
    }
    if config.credentials.backend == config::CredentialBackend::File {
        directory(
            report,
            "Paths",
            "credentials.dir",
            &config.credentials.dir(),
            false,
        );
    }
    directory(
        report,
        "Paths",
        "mining.sources_dir",
        &config.mining.sources_dir(),
        config.mining.sources_dir.is_some(),
    );
    // A metadata check cannot prove future writes will succeed, so never claim writability.
    report.add(
        "Paths",
        "write access",
        CheckStatus::Skipped,
        "Write access is not tested without creating files.",
        None,
    );
}

fn providers(report: &mut Report, config: &Config) {
    let checks = [
        (
            "embeddings",
            config.embeddings.provider == EmbeddingProvider::Command,
            config.embeddings.provider == EmbeddingProvider::Http,
            &config.embeddings.command,
            config.embeddings.api_key.is_some(),
        ),
        (
            "extraction",
            config.extraction.provider == ExtractionProvider::Command,
            config.extraction.provider == ExtractionProvider::Http,
            &config.extraction.command,
            config.extraction.api_key.is_some(),
        ),
    ];
    for (name, command, http, args, credential) in checks {
        if command {
            let found = args
                .first()
                .is_some_and(|program| executable_present(program));
            report.add(
                "Providers",
                name,
                if found {
                    CheckStatus::Ok
                } else {
                    CheckStatus::Error
                },
                if found {
                    "Configured command is present; protocol not tested."
                } else {
                    "Configured command cannot be found."
                },
                (!found).then_some("Install the program or correct the provider command/PATH."),
            );
        } else if http {
            report.add(
                "Providers",
                name,
                CheckStatus::Ok,
                "HTTP provider and model are configured; availability not tested.",
                None,
            );
            report.add(
                "Providers",
                format!("{name} API key"),
                if credential {
                    CheckStatus::Ok
                } else {
                    CheckStatus::Skipped
                },
                if credential {
                    "API key is configured."
                } else {
                    "No API key is configured; this may be valid for a local provider."
                },
                None,
            );
        } else {
            report.add(
                "Providers",
                name,
                CheckStatus::Ok,
                "No external provider is selected.",
                None,
            );
        }
        if command || http {
            report.add(
                "Providers",
                format!("{name} availability"),
                CheckStatus::Skipped,
                "Provider/model is not contacted by doctor.",
                None,
            );
        }
    }
}

fn executable_present(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return is_executable(path);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| is_executable(&dir.join(program)))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn offline_miners(report: &mut Report, config: &Config) {
    for (index, miner) in config.miners.iter().enumerate() {
        let check = format!("miner #{}", index + 1);
        if !miner.enabled {
            report.add(
                "Sources and miners",
                check,
                CheckStatus::Skipped,
                "Miner is disabled; runtime prerequisites are not required.",
                None,
            );
            continue;
        }
        if let Some(credential) = &miner.credential {
            let available = match credential {
                CredentialRef::Env { name } => {
                    std::env::var_os(name).is_some_and(|value| !value.is_empty())
                }
                CredentialRef::File { path } => std::fs::File::open(path).is_ok(),
                CredentialRef::Oauth => {
                    report.add(
                        "Sources and miners",
                        format!("{check} credential"),
                        CheckStatus::Skipped,
                        "OAuth sign-in is checked by the daemon when available.",
                        None,
                    );
                    true
                }
            };
            if !available {
                report.add(
                    "Sources and miners",
                    format!("{check} credential"),
                    CheckStatus::Error,
                    "Required credential is unavailable.",
                    Some("Set the referenced environment variable or restore the credential file."),
                );
            }
        }
        if miner.source == "directory" {
            if let Some(locator) = &miner.locator {
                directory(
                    report,
                    "Sources and miners",
                    &format!("{check} directory"),
                    Path::new(locator),
                    true,
                );
            } else {
                report.add(
                    "Sources and miners",
                    format!("{check} locator"),
                    CheckStatus::Error,
                    "Enabled directory miner has no directory locator.",
                    Some("Set the miner's absolute locator or disable the miner."),
                );
            }
        }
    }
    for (index, trigger) in config.triggers.iter().enumerate() {
        if trigger.enabled
            && !config
                .miners
                .iter()
                .any(|miner| miner.name == trigger.miner && miner.enabled)
        {
            report.add(
                "Sources and miners",
                format!("trigger #{}", index + 1),
                CheckStatus::Error,
                "Enabled trigger has no enabled miner.",
                Some("Configure and enable its miner, or disable the trigger."),
            );
        }
        if trigger.enabled
            && trigger.kind == crate::domain::TriggerMechanism::Webhook
            && !config.webhook.enable
        {
            report.add(
                "Sources and miners",
                format!("trigger #{} webhook", index + 1),
                CheckStatus::Error,
                "Enabled webhook trigger has no webhook listener configured.",
                Some("Set webhook.enable = true or disable the trigger."),
            );
        }
    }
}

async fn online(report: &mut Report, config: &Config, mode: Option<MemoryMode>) {
    let client = DaemonClient::discover(&config.palace.path, config.server.socket_addr())
        .with_token(config.auth.token.clone());
    let client = match mode {
        Some(mode) => client.with_mode(mode),
        None => client,
    };
    let status = match client.status().await {
        Ok(status) => status,
        Err(Error::DaemonNotRunning) => {
            report.add(
                "Daemon and storage",
                "daemon",
                CheckStatus::Warning,
                "No daemon is reachable for this palace.",
                Some("Start it with `memcastle daemon start` to run online checks."),
            );
            skip_online(report);
            return;
        }
        Err(error) => {
            let auth = matches!(
                error,
                Error::Remote {
                    status: 401 | 403,
                    ..
                }
            );
            report.add(
                "Daemon and storage",
                "daemon",
                CheckStatus::Error,
                if auth {
                    "Daemon rejected the configured credential."
                } else {
                    "Daemon status request failed or timed out."
                },
                Some(if auth {
                    "Configure the daemon's auth token and retry."
                } else {
                    "Check `memcastle status` and daemon logs, then retry."
                }),
            );
            skip_online(report);
            return;
        }
    };
    report.add(
        "Daemon and storage",
        "daemon",
        CheckStatus::Ok,
        "Daemon answered authenticated status request.",
        None,
    );
    if !status.palace_path.is_empty() && status.palace_path != config.palace.path.to_string_lossy()
    {
        report.add(
            "Daemon and storage",
            "palace",
            CheckStatus::Error,
            "Daemon serves a different palace than this configuration.",
            Some("Check --palace, the daemon registry and the daemon's configuration."),
        );
    }
    if status.version != env!("CARGO_PKG_VERSION") {
        report.add(
            "Daemon and storage",
            "version",
            CheckStatus::Warning,
            "Daemon and CLI versions differ.",
            Some("Restart the daemon with the current MemCastle binary."),
        );
    }
    if status.datastore.ok {
        report.add(
            "Daemon and storage",
            "storage",
            CheckStatus::Ok,
            "Daemon can reach storage.",
            None,
        );
        report.add("Daemon and storage", "migrations", if status.datastore.pending.is_empty() { CheckStatus::Ok } else { CheckStatus::Error },
            if status.datastore.pending.is_empty() { "Data migrations are up to date." } else { "Data migrations are pending." },
            (!status.datastore.pending.is_empty()).then_some("Run `memcastle migrate` while the daemon is stopped, or restart it to apply migrations."));
    } else {
        report.add(
            "Daemon and storage",
            "storage",
            CheckStatus::Error,
            "Daemon reports storage unavailable.",
            Some("Check the storage configuration, permissions and daemon logs."),
        );
        report.add(
            "Daemon and storage",
            "migrations",
            CheckStatus::Skipped,
            "Migration state cannot be read without storage.",
            None,
        );
        report.add(
            "Sources and miners",
            "runtime prerequisites",
            CheckStatus::Skipped,
            "Source, miner and trigger availability requires healthy storage.",
            None,
        );
        return;
    }
    match client.list_sources().await {
        Ok(sources) => {
            for (index, source) in sources.adapters.iter().enumerate() {
                if source.state == SourceState::Unavailable {
                    report.add("Sources and miners", format!("source #{}", index + 1), CheckStatus::Warning, "Installed source is unavailable.", Some("Check its installed component, version and permissions with `memcastle sources`."));
                }
            }
            report.add(
                "Sources and miners",
                "sources",
                CheckStatus::Ok,
                "Source availability was inspected.",
                None,
            );
        }
        Err(_) => report.add(
            "Sources and miners",
            "sources",
            CheckStatus::Skipped,
            "Daemon could not report source availability.",
            Some("Check `memcastle sources` and the daemon logs."),
        ),
    }
    match client.list_miners().await {
        Ok(miners) => {
            if miners.error.is_some() {
                report.add(
                    "Sources and miners",
                    "miner configuration",
                    CheckStatus::Error,
                    "Daemon is using a last-good miner configuration after a failed reload.",
                    Some("Fix the miners in the daemon's configuration file."),
                );
            }
            for (index, miner) in miners.miners.iter().enumerate() {
                if miner.enabled && miner.state == MinerState::Unavailable {
                    report.add(
                        "Sources and miners",
                        format!("miner #{} runtime", index + 1),
                        CheckStatus::Error,
                        "Enabled miner is unavailable.",
                        Some(
                            "Check its source, locator and credential with `memcastle miner list`.",
                        ),
                    );
                }
            }
            report.add(
                "Sources and miners",
                "miners",
                CheckStatus::Ok,
                "Miner readiness was inspected.",
                None,
            );
        }
        Err(_) => report.add(
            "Sources and miners",
            "miners",
            CheckStatus::Skipped,
            "Daemon could not report miners.",
            Some("Check `memcastle miner list` and the daemon logs."),
        ),
    }
    match client.list_triggers().await {
        Ok(triggers) => {
            if triggers.error.is_some() {
                report.add(
                    "Sources and miners",
                    "trigger configuration",
                    CheckStatus::Error,
                    "Daemon is using a last-good trigger configuration after a failed reload.",
                    Some("Fix the triggers in the daemon's configuration file."),
                );
            }
            for (index, trigger) in triggers.triggers.iter().enumerate() {
                if trigger.enabled
                    && matches!(
                        trigger.status,
                        TriggerStatus::Unavailable | TriggerStatus::Failing
                    )
                {
                    report.add(
                        "Sources and miners",
                        format!("trigger #{} runtime", index + 1),
                        CheckStatus::Error,
                        "Enabled trigger is unavailable or failing.",
                        Some("Check its prerequisites with `memcastle trigger list`."),
                    );
                }
            }
            report.add(
                "Sources and miners",
                "triggers",
                CheckStatus::Ok,
                "Trigger readiness was inspected.",
                None,
            );
        }
        Err(_) => report.add(
            "Sources and miners",
            "triggers",
            CheckStatus::Skipped,
            "Daemon could not report triggers.",
            Some("Check `memcastle trigger list` and the daemon logs."),
        ),
    }
}

fn skip_online(report: &mut Report) {
    for (area, check) in [
        ("Daemon and storage", "storage and migrations"),
        ("Sources and miners", "runtime prerequisites"),
    ] {
        report.add(
            area,
            check,
            CheckStatus::Skipped,
            "Requires an authenticated running daemon.",
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Secret;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;

    #[test]
    fn only_blocking_errors_change_the_exit_code() {
        let mut report = Report::default();
        report.add(
            "Daemon and storage",
            "daemon",
            CheckStatus::Warning,
            "No daemon is running.",
            None,
        );
        report.add(
            "Daemon and storage",
            "storage",
            CheckStatus::Skipped,
            "Offline.",
            None,
        );
        assert_eq!(report.exit_code(), 0);
        report.add(
            "Configuration",
            "effective",
            CheckStatus::Error,
            "Invalid configuration.",
            None,
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[tokio::test]
    async fn an_invalid_configuration_still_produces_a_redacted_report() {
        let canary = "unprintable-secret-canary";
        let report = run(
            None,
            None,
            Err(Error::config(format!(
                "mining.github_api_url {canary} is invalid"
            ))),
        )
        .await;
        assert_eq!(report.exit_code(), 1);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.status == CheckStatus::Skipped)
        );
        assert!(!report.render(Painter::PLAIN).contains(canary));
        assert!(!serde_json::to_string(&report).unwrap().contains(canary));
    }

    #[tokio::test]
    async fn an_offline_palace_is_healthy_without_creating_storage() {
        let temp = tempfile::tempdir().unwrap();
        let palace = temp.path().join("new-palace");
        let mut config = Config::default();
        config.palace.path = palace.clone();
        config.server.port = 0;
        config.auth.token = Some(Secret::new("secret-redaction-canary"));
        let report = run(None, None, Ok(config)).await;
        assert_eq!(report.exit_code(), 0, "{}", report.render(Painter::PLAIN));
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "storage and migrations" && f.status == CheckStatus::Skipped)
        );
        assert!(!palace.exists());
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("secret-redaction-canary")
        );
    }

    #[test]
    fn only_enabled_miners_require_their_credential_and_directory() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.miners.push(crate::domain::MinerDefinition {
            name: "missing-directory".into(),
            source: "directory".into(),
            enabled: false,
            locator: Some(temp.path().join("absent").display().to_string()),
            wing: None,
            credential: Some(CredentialRef::File {
                path: temp.path().join("missing-secret").display().to_string(),
            }),
            options: Default::default(),
        });
        let mut report = Report::default();
        offline_miners(&mut report, &config);
        assert_eq!(report.exit_code(), 0);
        config.miners[0].enabled = true;
        let mut report = Report::default();
        offline_miners(&mut report, &config);
        assert_eq!(report.exit_code(), 1);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.check.contains("credential")
                    && finding.status == CheckStatus::Error)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.check.contains("directory")
                    && finding.status == CheckStatus::Error)
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("missing-secret")
        );
    }

    #[test]
    fn an_unknown_nested_setting_is_a_warning_without_reflecting_its_value() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("config.toml");
        std::fs::write(
            &file,
            "[embeddings]\nprovider = 'none'\nunused_setting = 'secret-redaction-canary'\n",
        )
        .unwrap();
        let mut report = Report::default();
        unknown_keys(&mut report, &file);
        assert_eq!(report.exit_code(), 0);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.status == CheckStatus::Warning && f.summary.contains("unused_setting"))
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("secret-redaction-canary")
        );
    }

    #[tokio::test]
    async fn a_daemon_rejecting_authentication_is_an_error_not_an_absence() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/api/status",
                    get(|| async {
                        (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error": "secret-redaction-canary"})),
                        )
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.palace.path = temp.path().join("palace");
        config.server.port = port;
        config.auth.token = Some(Secret::new("secret-redaction-canary"));
        let report = run(None, None, Ok(config)).await;
        server.abort();
        assert_eq!(report.exit_code(), 1);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.check == "daemon" && finding.status == CheckStatus::Error)
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("secret-redaction-canary")
        );
    }

    #[tokio::test]
    async fn an_online_daemon_reports_storage_and_pending_migration_failures() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let temp = tempfile::tempdir().unwrap();
        let palace = temp.path().join("palace");
        let palace_for_server = palace.display().to_string();
        let server = tokio::spawn(async move {
            let app = Router::new().route("/api/status", get(move || {
                let palace = palace_for_server.clone();
                async move { Json(json!({
                    "version": env!("CARGO_PKG_VERSION"), "uptime_secs": 1,
                    "palace_name": "default", "drawer_count": 0,
                    "jobs_queued": 0, "jobs_running": 0, "jobs_paused": 0,
                    "mode": "full", "palace_path": palace,
                    "datastore": {"ok": true, "backend": "embedded", "location": "hidden",
                        "error": null, "migration_version": 0, "latest_version": 1, "pending": ["a migration"]}
                })) }
            }));
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = Config::default();
        config.palace.path = palace;
        config.server.port = port;
        let report = run(None, None, Ok(config)).await;
        server.abort();
        assert_eq!(report.exit_code(), 1, "{}", report.render(Painter::PLAIN));
        assert!(
            report.findings.iter().any(
                |finding| finding.check == "migrations" && finding.status == CheckStatus::Error
            )
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.check == "storage" && finding.status == CheckStatus::Ok)
        );
        assert!(!serde_json::to_string(&report).unwrap().contains("hidden"));
    }

    #[test]
    fn paths_distinguish_optional_uncreated_directories_from_required_missing_ones() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.palace.path = temp.path().join("future-palace");
        config.assets.dir = Some(temp.path().join("missing-assets"));
        config.mining.sources_dir = Some(temp.path().join("missing-sources"));
        config.mining.bundled_dir = Some(temp.path().join("missing-bundle"));
        config.credentials.backend = config::CredentialBackend::File;
        config.credentials.dir = Some(temp.path().join("future-credentials"));
        let mut report = Report::default();
        paths(&mut report, &config);
        assert_eq!(report.exit_code(), 1);
        for check in [
            "palace.path",
            "embedded database",
            "credentials.dir",
            "write access",
        ] {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|f| f.check == check && f.status == CheckStatus::Skipped),
                "{check}"
            );
        }
        for check in ["assets.dir", "mining.sources_dir", "mining.bundled_dir"] {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|f| f.check == check && f.status == CheckStatus::Error),
                "{check}"
            );
        }
        assert!(!config.palace.path.exists());
    }

    #[test]
    fn provider_checks_only_inspect_executables_and_never_contact_models() {
        let mut config = Config::default();
        config.embeddings.provider = EmbeddingProvider::Command;
        config.embeddings.command = vec!["/not-a-real-provider".into()];
        config.extraction.provider = ExtractionProvider::Http;
        config.extraction.url = Some("https://secret-redaction-canary.invalid/v1".into());
        config.extraction.model = Some("private-model".into());
        config.extraction.api_key = Some(Secret::new("secret-redaction-canary"));
        let mut report = Report::default();
        providers(&mut report, &config);
        assert_eq!(report.exit_code(), 1);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "embeddings" && f.status == CheckStatus::Error)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "extraction API key" && f.status == CheckStatus::Ok)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "extraction availability" && f.status == CheckStatus::Skipped)
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("secret-redaction-canary")
        );

        config.embeddings.command = vec![std::env::current_exe().unwrap().display().to_string()];
        config.extraction.api_key = None;
        let mut report = Report::default();
        providers(&mut report, &config);
        assert_eq!(report.exit_code(), 0);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "embeddings" && f.status == CheckStatus::Ok)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "extraction API key" && f.status == CheckStatus::Skipped)
        );
    }

    #[test]
    fn enabled_triggers_without_setup_report_errors_while_disabled_ones_do_not() {
        let mut config = Config::default();
        config.triggers.push(crate::domain::TriggerDefinition {
            name: "delivery".into(),
            miner: "absent-miner".into(),
            kind: crate::domain::TriggerMechanism::Webhook,
            enabled: false,
            credential: None,
            settings: Default::default(),
        });
        let mut report = Report::default();
        offline_miners(&mut report, &config);
        assert_eq!(report.exit_code(), 0);
        config.triggers[0].enabled = true;
        let mut report = Report::default();
        offline_miners(&mut report, &config);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.summary.contains("no enabled miner") && f.status == CheckStatus::Error)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.summary.contains("webhook listener") && f.status == CheckStatus::Error)
        );
    }

    #[tokio::test]
    async fn a_running_daemon_reports_unavailable_sources_miners_and_triggers_without_leaking_reasons()
     {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.palace.path = temp.path().join("palace");
        config.server.port = port;
        let app = Router::new()
            .route("/api/status", get(|| async { Json(json!({
                "version": "older", "uptime_secs": 1, "palace_name": "default", "drawer_count": 0,
                "jobs_queued": 0, "jobs_running": 0, "jobs_paused": 0, "mode": "full",
                "palace_path": "/different-palace",
                "datastore": {"ok": true, "backend": "embedded", "location": "secret-redaction-canary",
                    "error": null, "migration_version": 1, "latest_version": 1, "pending": []}
            })) }))
            .route("/api/sources", get(|| async { Json(json!({
                "adapters": [{"name": "unavailable-source", "description": "source", "capabilities": {},
                    "state": "unavailable", "unavailable_reason": "secret-redaction-canary"}], "sources": []
            })) }))
            .route("/api/miners", get(|| async { Json(json!({
                "miners": [{"name": "blocked", "source": "unavailable-source", "enabled": true,
                    "state": "unavailable", "reason": "secret-redaction-canary"}],
                "error": "secret-redaction-canary"
            })) }))
            .route("/api/triggers", get(|| async { Json(json!({
                "triggers": [{"name": "hook", "miner": "blocked", "type": "webhook", "enabled": true,
                    "status": "failing", "reason": "secret-redaction-canary"}],
                "webhook": {"enabled": true, "bind": "127.0.0.1", "port": 8787, "allow_remote": false},
                "error": "secret-redaction-canary"
            })) }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let report = run(None, None, Ok(config)).await;
        server.abort();
        assert_eq!(report.exit_code(), 1, "{}", report.render(Painter::PLAIN));
        for check in [
            "palace",
            "version",
            "source #1",
            "miner configuration",
            "miner #1 runtime",
            "trigger configuration",
            "trigger #1 runtime",
        ] {
            assert!(
                report.findings.iter().any(|f| f.check == check),
                "{check}: {}",
                report.render(Painter::PLAIN)
            );
        }
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("secret-redaction-canary")
        );
    }

    #[tokio::test]
    async fn unavailable_storage_skips_dependent_runtime_checks() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().route("/api/status", get(|| async { Json(json!({
            "version": env!("CARGO_PKG_VERSION"), "uptime_secs": 1, "palace_name": "default",
            "drawer_count": 0, "jobs_queued": 0, "jobs_running": 0, "jobs_paused": 0,
            "mode": "full", "datastore": {"ok": false, "backend": "embedded",
                "location": "hidden", "error": "secret-redaction-canary", "migration_version": 0,
                "latest_version": 1, "pending": []}
        })) }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.palace.path = temp.path().join("palace");
        config.server.port = port;
        let report = run(None, None, Ok(config)).await;
        server.abort();
        assert_eq!(report.exit_code(), 1);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "storage" && f.status == CheckStatus::Error)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "runtime prerequisites" && f.status == CheckStatus::Skipped)
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("secret-redaction-canary")
        );
    }
}
