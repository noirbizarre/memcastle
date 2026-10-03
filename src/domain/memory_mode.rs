//! Per-session/per-request memory mode — the enforcement primitive behind
//! "memory must be explicitly disableable per session, never via a
//! daemon-global switch" (task brief §22, PLAN.md principle 8).
//!
//! This is deliberately a closed three-value enum, not a per-capability set
//! of flags: finer-grained modes (task brief §21) are out of scope for V1.
//! `api`/`mcp` only ever *extract* a value of this type (from an HTTP header
//! or an MCP session lookup) and pass it down — `AppServices` is the single
//! place that decides what each mode permits, via [`Self::allows_read`] and
//! [`Self::allows_write`]. Neither `api` nor `mcp` independently branches on
//! `MemoryMode` to decide what's allowed; see `app::AppServices`'s
//! `require_read`/`require_write` helpers.
//!
//! ## The allow/deny matrix
//!
//! | mode        | read (`search`/`recall`/`wake_up`/`diary_read`, job `list`/`show`, wing/room/drawer `list`/`show`) | write (`checkpoint`/`emergency_checkpoint`/`diary_write`, `mine`, applied `repair`, wing/room/drawer `create`/`delete`) |
//! |-------------|--------------------------------------------------|-------------------------------------------------------------|
//! | `Full`      | ok                                               | ok                                                            |
//! | `ReadOnly`  | ok                                               | rejected (`Error::ModeForbidden`)                             |
//! | `Disabled`  | rejected (`Error::ModeForbidden`)                | rejected (`Error::ModeForbidden`)                             |
//!
//! `Disabled` is deliberately symmetric: both reads and writes are rejected
//! with the same typed error, never a silent `Ok(empty)` for reads. An
//! empty result is indistinguishable from "genuinely found nothing", which
//! is exactly the ambiguity the context-isolation guarantee (no MemCastle-
//! derived content of any kind may reach a disabled session) must not
//! create — an error is unambiguous evidence that nothing was read.
//!
//! ## Explicit daemon operations vs automatic memory operations
//!
//! The gate follows *what an operation reads or writes*, not which method
//! name it has. A job record carries its whole input — for a checkpoint job,
//! the memory being written — so reading jobs is a memory read
//! (`list_jobs`/`get_job`), and a job whose purpose is to file or delete
//! drawers is a memory write (`submit_mine`, and `submit_repair` when it is
//! not a dry run). The hierarchy follows the same rule: the names and counts
//! of wings, rooms and drawers are palace content, so listing and showing them
//! is a read, and creating or deleting one is a write. Gating only the obvious methods would let a `Disabled`
//! session read palace content through the job list, or a `ReadOnly` one
//! mutate the palace by submitting a mine.
//!
//! What stays ungated is genuinely administrative: `status` (counts and
//! version, no content), `pause_job`/`resume_job`/`cancel_job`/`retry_job`
//! (they need a job id, which a session that cannot list jobs never
//! learns), `submit_demo` (touches no palace content), `submit_audit` and
//! a dry-run `submit_repair` (they only report). Over MCP that is
//! `memcastle_status`, `memcastle_audit`, a dry-run `memcastle_repair` and the
//! four `memcastle_job_*` control tools; `memcastle_job_get` and
//! `memcastle_job_list` are reads, and an applied `memcastle_repair` is a
//! write. This resolves the
//! "explicit daemon operations vs automatic memory operations" boundary the
//! task brief calls out (§22-26).

use serde::{Deserialize, Serialize};

/// A session's (HTTP request's / MCP session's) permission to touch
/// MemCastle's memory content — never a daemon-global switch. See the
/// module doc for the exact allow/deny matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    /// Reads and writes both proceed normally — today's behavior,
    /// unchanged.
    Full,
    /// Reads proceed; writes are rejected with an actionable error. A
    /// session in this mode must never mutate the palace.
    ReadOnly,
    /// Neither reads nor writes proceed — the palace is never touched, and
    /// the caller must behave as if MemCastle does not exist.
    Disabled,
}

impl MemoryMode {
    /// The HTTP header a client sets to ask for a non-default mode. One
    /// constant, shared by the daemon that reads it and the client that
    /// sends it, so the two cannot drift.
    pub const HEADER: &'static str = "x-memcastle-mode";

    /// The name this mode goes by everywhere it is spelled out: the
    /// `X-MemCastle-Mode` header, `memcastle_set_mode`'s argument, the CLI's
    /// `--mode`, and every message that reports it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ReadOnly => "read_only",
            Self::Disabled => "disabled",
        }
    }

    /// Whether this mode permits `search`/`recall`/`wake_up`/`diary_read`
    /// and the job reads (`job list`/`show`), which can expose memory
    /// content — see ADR-007.
    #[must_use]
    pub fn allows_read(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Whether this mode permits `checkpoint`/`emergency_checkpoint`/
    /// `diary_write`, and submitting `mine` or an applied (non-dry-run)
    /// `repair`, which also write memory content — see ADR-007.
    #[must_use]
    pub fn allows_write(self) -> bool {
        matches!(self, Self::Full)
    }
}

impl std::fmt::Display for MemoryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MemoryMode {
    type Err = String;

    /// Parse the names [`MemoryMode::as_str`] produces; anything else says
    /// what was expected, so no caller has to re-word it.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        [Self::Full, Self::ReadOnly, Self::Disabled]
            .into_iter()
            .find(|mode| mode.as_str() == raw)
            .ok_or_else(|| {
                format!("unknown memory mode `{raw}` (expected full, read_only or disabled)")
            })
    }
}

impl Default for MemoryMode {
    /// The mode assumed when an HTTP request sends no `X-MemCastle-Mode`
    /// header, or an MCP session never called `memcastle_set_mode` —
    /// existing clients that don't know about modes at all must see
    /// exactly today's unrestricted behavior.
    fn default() -> Self {
        Self::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_mode_allows_both_reads_and_writes() {
        assert!(MemoryMode::Full.allows_read());
        assert!(MemoryMode::Full.allows_write());
    }

    #[test]
    fn read_only_mode_allows_reads_but_not_writes() {
        assert!(MemoryMode::ReadOnly.allows_read());
        assert!(!MemoryMode::ReadOnly.allows_write());
    }

    #[test]
    fn disabled_mode_allows_neither_reads_nor_writes() {
        assert!(!MemoryMode::Disabled.allows_read());
        assert!(!MemoryMode::Disabled.allows_write());
    }

    #[test]
    fn the_default_mode_is_full() {
        assert_eq!(MemoryMode::default(), MemoryMode::Full);
    }

    #[test]
    fn mode_serializes_as_snake_case_strings() {
        assert_eq!(
            serde_json::to_value(MemoryMode::Full).unwrap(),
            serde_json::json!("full")
        );
        assert_eq!(
            serde_json::to_value(MemoryMode::ReadOnly).unwrap(),
            serde_json::json!("read_only")
        );
        assert_eq!(
            serde_json::to_value(MemoryMode::Disabled).unwrap(),
            serde_json::json!("disabled")
        );
    }

    #[test]
    fn every_mode_round_trips_through_its_name() {
        for mode in [MemoryMode::Full, MemoryMode::ReadOnly, MemoryMode::Disabled] {
            assert_eq!(mode.as_str().parse::<MemoryMode>(), Ok(mode));
            assert_eq!(mode.to_string(), mode.as_str());
            // The serde name is the same word, so the header, the tool
            // argument and the API JSON cannot disagree.
            assert_eq!(serde_json::to_value(mode).unwrap(), mode.as_str());
        }
    }

    #[test]
    fn an_unknown_mode_name_lists_what_is_accepted() {
        let error = "readonly".parse::<MemoryMode>().unwrap_err();
        assert!(error.contains("read_only"), "{error}");
    }
}
