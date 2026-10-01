//! Local-first, always-on memory server for AI coding agents over MCP/HTTP
//!
//! # Architecture
//!
//! ```text
//! cli / mcp / api          <- interfaces (thin: parse, dispatch, serialize)
//!        |
//!       app                <- application services (the only layer the above may call)
//!        |
//!   domain + jobs + search  <- pure model + scheduling + retrieval logic
//!        |
//!      store                <- SurrealDB, embedded or remote
//! ```
//!
//! `dbadmin` is a side listener, started on request by `app` (never by `serve`): SurrealDB's WebSocket
//! protocol over a clone of the daemon's own database handle, for SurrealDB Studio.
//!
//! `main.rs`/`cli.rs` are thin: every subcommand either runs `server::run`
//! (the `serve`/`daemon` command) or goes through `client::DaemonClient`
//! (everything else), with two narrow exceptions: `migrate` connects to
//! storage itself, and `restart` also manages the daemon process (registry
//! file plus respawn) — see `docs/architecture.md` for the full rationale.

#![allow(clippy::result_large_err)]
#![warn(missing_docs)]

pub mod api;
pub mod app;
pub mod assets;
pub mod audit;
pub mod checkpoint;
pub mod client;
pub mod config;
pub mod dbadmin;
pub mod domain;
pub mod error;
pub mod jobs;
pub mod mcp;
pub mod migrate;
pub mod mining;
pub mod repair;
pub mod search;
pub mod server;
pub mod store;

pub use error::{Error, Result};
