//! `memcastle status`: one answer to "is the daemon up, where, serving what,
//! and is its datastore healthy?" — including when the answer is "no daemon".
//!
//! Built from the same two inputs every other command uses to find the
//! daemon (the registry file, then the configured address) plus one
//! `GET /api/status`; there is no separate discovery mechanism.

use std::net::SocketAddr;
use std::path::Path;

use serde::Serialize;

use super::{DaemonClient, EndpointSource};
use crate::app::StatusReport;
use crate::domain::MemoryMode;
use crate::error::{Error, Result};
use crate::server::lifecycle::{self, Registry};

/// Exit code for a daemon that is up and whose datastore is healthy.
pub const EXIT_HEALTHY: u8 = 0;

/// Exit code for a daemon that answers but is degraded (datastore unreachable
/// or migrations pending). Also the generic failure code, on purpose: to a
/// script a daemon that cannot serve is a failure, not an absence.
pub const EXIT_DEGRADED: u8 = 1;

/// Exit code for "no daemon is running". Distinct from [`EXIT_DEGRADED`] so
/// `memcastle status || memcastle serve` style scripts can tell a stopped
/// daemon from a broken one; 3 is what `systemctl status` uses for "inactive".
pub const EXIT_NOT_RUNNING: u8 = 3;

/// What the registry file said, for the report.
#[derive(Debug, Clone, Serialize)]
pub struct RegistryView {
    /// `absent`, `live` or `stale`.
    pub state: &'static str,
    /// The PID the file records, if there is a file.
    pub pid: Option<u32>,
    /// The address the file records, if there is a file.
    pub bind_addr: Option<String>,
}

impl From<&Registry> for RegistryView {
    fn from(registry: &Registry) -> Self {
        Self {
            state: registry.as_str(),
            pid: registry.info().map(|info| info.pid),
            bind_addr: registry.info().map(|info| info.bind_addr.clone()),
        }
    }
}

/// Everything `memcastle status` reports. Serialized as-is by `--json`, so
/// its field names are a scripting contract.
#[derive(Debug, Clone, Serialize)]
pub struct StatusView {
    /// Whether a daemon answered.
    pub running: bool,
    /// The base URL the CLI dialed.
    pub endpoint: String,
    /// Whether `endpoint` came from a live registry file or the configuration.
    pub endpoint_source: EndpointSource,
    /// Where an MCP client connects.
    pub mcp_url: String,
    /// The palace directory this configuration points at. Present even when
    /// no daemon runs, which is when "which palace would it serve" matters.
    pub palace_path: String,
    /// The registry file's state.
    pub registry: RegistryView,
    /// The daemon's own report; `None` when it is not running.
    pub daemon: Option<StatusReport>,
}

impl StatusView {
    /// Ask the daemon for `palace_path`'s configuration and describe what
    /// answered — or that nothing did.
    ///
    /// # Errors
    ///
    /// A daemon that is unreachable is *not* an error (it is `running:
    /// false`). Anything else that goes wrong talking to one — a timeout, a
    /// 5xx, an unreadable answer — is, because it is not evidence of absence.
    pub async fn collect(
        palace_path: &Path,
        configured_bind: SocketAddr,
        mode: Option<MemoryMode>,
    ) -> Result<Self> {
        let registry = lifecycle::inspect(palace_path);
        let daemon = DaemonClient::discover(palace_path, configured_bind);
        let daemon = match mode {
            Some(mode) => daemon.with_mode(mode),
            None => daemon,
        };
        let report = match daemon.status().await {
            Ok(report) => Some(report),
            Err(Error::DaemonNotRunning) => None,
            Err(other) => return Err(other),
        };
        Ok(Self {
            running: report.is_some(),
            endpoint: daemon.base_url().to_string(),
            endpoint_source: daemon.endpoint_source(),
            mcp_url: format!("{}/mcp", daemon.base_url()),
            palace_path: palace_path.display().to_string(),
            registry: RegistryView::from(&registry),
            daemon: report,
        })
    }

    /// The process exit code that summarises this view.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match &self.daemon {
            None => EXIT_NOT_RUNNING,
            Some(report) if report.datastore.is_healthy() => EXIT_HEALTHY,
            Some(_) => EXIT_DEGRADED,
        }
    }

    /// A human-readable rendering: plain text, no colour, stable enough to
    /// read but not to parse (that is what `--json` is for).
    #[must_use]
    pub fn render_human(&self) -> String {
        match &self.daemon {
            Some(report) => self.render_running(report),
            None => self.render_stopped(),
        }
    }

    fn render_running(&self, report: &StatusReport) -> String {
        let datastore = &report.datastore;
        let mut lines = vec![
            if datastore.is_healthy() {
                "MemCastle is running".to_string()
            } else {
                "MemCastle is running, but DEGRADED".to_string()
            },
            format!(
                "  version    {} (pid {}, up {})",
                report.version,
                report.pid,
                format_uptime(report.uptime_secs)
            ),
            format!(
                "  endpoint   {} ({})",
                self.endpoint,
                source_label(self.endpoint_source)
            ),
            format!("  mcp        {}", self.mcp_url),
            format!(
                "  palace     {} ({})",
                report.palace_name,
                non_empty(&report.palace_path, &self.palace_path)
            ),
        ];
        if datastore.ok {
            lines.push(format!(
                "  datastore  ok - {} {}, migrations {}/{}",
                non_empty(&datastore.backend, "unknown"),
                datastore.location,
                datastore.migration_version,
                datastore.latest_version
            ));
        } else {
            lines.push(format!(
                "  datastore  UNAVAILABLE - {} {}: {}",
                non_empty(&datastore.backend, "unknown"),
                datastore.location,
                datastore.error.as_deref().unwrap_or("no detail reported")
            ));
        }
        if !datastore.pending.is_empty() {
            lines.push(format!(
                "  migrations pending: {} (run `memcastle migrate`)",
                datastore.pending.join(", ")
            ));
        }
        lines.push(format!("  drawers    {}", report.drawer_count));
        lines.push(format!(
            "  jobs       {} queued, {} running, {} paused",
            report.jobs_queued, report.jobs_running, report.jobs_paused
        ));
        lines.push(format!("  mode       {}", report.mode.as_str()));
        lines.push("Restart with `memcastle restart`, stop with `memcastle stop`.".to_string());
        lines.join("\n")
    }

    fn render_stopped(&self) -> String {
        let mut lines = vec![
            "MemCastle is not running".to_string(),
            format!(
                "  endpoint   {} ({}, nothing answered)",
                self.endpoint,
                source_label(self.endpoint_source)
            ),
            format!("  palace     {}", self.palace_path),
        ];
        match (self.registry.state, self.registry.pid) {
            ("stale", Some(pid)) => lines.push(format!(
                "  registry   stale: it names pid {pid}, which is gone (the daemon was killed or crashed); it is ignored"
            )),
            ("live", Some(pid)) => lines.push(format!(
                "  registry   names pid {pid}, which is alive, but it did not answer at {} (starting up, or a reused pid?)",
                self.registry.bind_addr.as_deref().unwrap_or("its address")
            )),
            _ => {}
        }
        lines.push("Start it with `memcastle serve`.".to_string());
        lines.join("\n")
    }
}

/// `value`, or `fallback` when the daemon left it empty (an older daemon).
fn non_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn source_label(source: EndpointSource) -> &'static str {
    match source {
        EndpointSource::Registry => "from the daemon's registry file",
        EndpointSource::Config => "from the configuration",
    }
}

/// `1h 2m 3s`, dropping leading zero units. A negative uptime (clock stepped
/// backwards) is shown as zero rather than as a nonsense duration.
fn format_uptime(secs: i64) -> String {
    let secs = secs.max(0);
    let (days, hours, minutes, seconds) = (
        secs / 86_400,
        secs % 86_400 / 3_600,
        secs % 3_600 / 60,
        secs % 60,
    );
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{seconds}s"),
        (0, 0, _) => format!("{minutes}m {seconds}s"),
        (0, _, _) => format!("{hours}h {minutes}m {seconds}s"),
        _ => format!("{days}d {hours}h {minutes}m"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::app::DatastoreStatus;

    fn report(datastore: DatastoreStatus) -> StatusReport {
        StatusReport {
            version: "0.1.0".into(),
            uptime_secs: 3_725,
            palace_name: "default".into(),
            drawer_count: 12,
            jobs_queued: 1,
            jobs_running: 2,
            jobs_paused: 3,
            mode: MemoryMode::Full,
            pid: 4242,
            started_at: Utc::now(),
            bind_addr: "127.0.0.1:8420".into(),
            palace_path: "/data/palace".into(),
            datastore,
        }
    }

    fn healthy() -> DatastoreStatus {
        DatastoreStatus {
            ok: true,
            backend: "embedded".into(),
            location: "/data/palace/db".into(),
            error: None,
            migration_version: 2,
            latest_version: 2,
            pending: vec![],
        }
    }

    fn view(daemon: Option<StatusReport>, registry: RegistryView) -> StatusView {
        StatusView {
            running: daemon.is_some(),
            endpoint: "http://127.0.0.1:8420".into(),
            endpoint_source: EndpointSource::Config,
            mcp_url: "http://127.0.0.1:8420/mcp".into(),
            palace_path: "/data/palace".into(),
            registry,
            daemon,
        }
    }

    fn absent() -> RegistryView {
        RegistryView {
            state: "absent",
            pid: None,
            bind_addr: None,
        }
    }

    #[test]
    fn a_stopped_daemon_exits_with_the_distinct_not_running_code() {
        // Scripts rely on 3 meaning "stopped" and 1 meaning "broken".
        assert_eq!(view(None, absent()).exit_code(), EXIT_NOT_RUNNING);
    }

    #[test]
    fn a_healthy_daemon_exits_zero() {
        let view = view(Some(report(healthy())), absent());
        assert_eq!(view.exit_code(), EXIT_HEALTHY);
    }

    #[test]
    fn an_unreachable_datastore_makes_a_running_daemon_exit_degraded() {
        let datastore = DatastoreStatus {
            ok: false,
            error: Some("connection refused".into()),
            ..healthy()
        };
        assert_eq!(
            view(Some(report(datastore)), absent()).exit_code(),
            EXIT_DEGRADED
        );
    }

    #[test]
    fn pending_migrations_make_a_running_daemon_exit_degraded() {
        let datastore = DatastoreStatus {
            migration_version: 1,
            pending: vec!["canonical-timestamps".into()],
            ..healthy()
        };
        assert_eq!(
            view(Some(report(datastore)), absent()).exit_code(),
            EXIT_DEGRADED
        );
    }

    #[test]
    fn the_running_report_answers_where_what_and_how_to_connect() {
        let text = view(Some(report(healthy())), absent()).render_human();
        assert!(text.contains("MemCastle is running\n"), "{text}");
        assert!(text.contains("pid 4242, up 1h 2m 5s"), "{text}");
        assert!(
            text.contains("http://127.0.0.1:8420 (from the configuration)"),
            "{text}"
        );
        assert!(text.contains("http://127.0.0.1:8420/mcp"), "{text}");
        assert!(text.contains("default (/data/palace)"), "{text}");
        assert!(
            text.contains("embedded /data/palace/db, migrations 2/2"),
            "{text}"
        );
        assert!(text.contains("memcastle restart"), "{text}");
    }

    #[test]
    fn a_degraded_report_names_the_datastore_error_and_the_migrate_command() {
        let datastore = DatastoreStatus {
            ok: false,
            error: Some("connection refused".into()),
            pending: vec!["diary-provenance".into()],
            ..healthy()
        };
        let text = view(Some(report(datastore)), absent()).render_human();
        assert!(text.contains("DEGRADED"), "{text}");
        assert!(text.contains("UNAVAILABLE"), "{text}");
        assert!(text.contains("connection refused"), "{text}");
        assert!(text.contains("memcastle migrate"), "{text}");
    }

    #[test]
    fn the_stopped_report_says_how_to_start_and_where_it_looked() {
        let text = view(None, absent()).render_human();
        assert!(text.contains("not running"), "{text}");
        assert!(text.contains("http://127.0.0.1:8420"), "{text}");
        assert!(text.contains("/data/palace"), "{text}");
        assert!(text.contains("memcastle serve"), "{text}");
    }

    #[test]
    fn a_stale_registry_is_diagnosed_instead_of_looking_like_a_daemon_that_never_ran() {
        let registry = RegistryView {
            state: "stale",
            pid: Some(999),
            bind_addr: Some("127.0.0.1:9000".into()),
        };
        let text = view(None, registry).render_human();
        assert!(text.contains("stale"), "{text}");
        assert!(text.contains("pid 999"), "{text}");
    }

    #[test]
    fn uptime_drops_leading_zero_units_and_never_goes_negative() {
        assert_eq!(format_uptime(-5), "0s");
        assert_eq!(format_uptime(59), "59s");
        assert_eq!(format_uptime(61), "1m 1s");
        assert_eq!(format_uptime(3_725), "1h 2m 5s");
        assert_eq!(format_uptime(90_061), "1d 1h 1m");
    }

    #[test]
    fn the_json_view_has_the_documented_top_level_fields() {
        // Field names are the `--json` scripting contract.
        let value = serde_json::to_value(view(None, absent())).unwrap();
        for key in [
            "running",
            "endpoint",
            "endpoint_source",
            "mcp_url",
            "palace_path",
            "registry",
            "daemon",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["endpoint_source"], "config");
        assert_eq!(value["registry"]["state"], "absent");
    }
}
