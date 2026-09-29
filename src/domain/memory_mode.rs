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
//! | mode        | read (`search`/`recall`/`wake_up`/`diary_read`, job `list`/`show`) | write (`checkpoint`/`emergency_checkpoint`/`diary_write`, `mine`, applied `repair`) |
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
//! not a dry run). Gating only the obvious methods would let a `Disabled`
//! session read palace content through the job list, or a `ReadOnly` one
//! mutate the palace by submitting a mine.
//!
//! What stays ungated is genuinely administrative: `status` (counts and
//! version, no content), `pause_job`/`resume_job`/`cancel_job`/`retry_job`
//! (they need a job id, which a session that cannot list jobs never
//! learns), `submit_demo` (touches no palace content), `submit_audit` and
//! a dry-run `submit_repair` (they only report). This resolves the
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
    /// Whether this mode permits `search`/`recall`/`wake_up`/`diary_read`.
    #[must_use]
    pub fn allows_read(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Whether this mode permits `checkpoint`/`emergency_checkpoint`/
    /// `diary_write`.
    #[must_use]
    pub fn allows_write(self) -> bool {
        matches!(self, Self::Full)
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
}
