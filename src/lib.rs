//! Local-first, always-on memory server for AI coding agents over MCP/HTTP
//!
//! # Architecture
//!
//! ```text
//! cli / mcp / api          <- interfaces (thin: parse, dispatch, serialize)
//!        |
//!       app                <- application services (the only layer the above may call)
//!        |
//!   domain + jobs + search + embed + extract  <- pure model + scheduling + retrieval logic + providers
//!        |
//!      store                <- SurrealDB, embedded or remote
//! ```
//!
//! `dbadmin` is a side listener, started on request by `app` (never by `serve`): SurrealDB's WebSocket
//! protocol over a clone of the daemon's own database handle, for SurrealDB Studio.
//!
//! `events` is the daemon's change notifications: `app`, `jobs` and the handlers publish identifiers (never content)
//! after a write is saved, and `GET /api/events` relays them so a client re-reads instead of polling.
//!
//! `main.rs`/`cli.rs` are thin: `serve` runs `server::run`, daemon-dependent commands
//! go through `client::DaemonClient`, and `doctor` combines local read-only checks with
//! daemon reads when available. `migrate` alone connects to storage from the CLI;
//! `daemon start`/`daemon restart` also manage the daemon
//! process (registry file plus spawning `serve`) — see `docs/architecture.md`
//! for the full rationale. The local tooling (`source init|build|test|package|index|keygen` through [`source`], and
//! `integration` through [`integration`]) works on files with no daemon and touches neither `store` nor `jobs`.

#![allow(clippy::result_large_err)]
#![warn(missing_docs)]

pub mod api;
pub mod app;
pub mod assets;
pub mod audit;
pub mod checkpoint;
pub mod client;
pub mod config;
pub mod credential;
pub mod dbadmin;
pub mod dedup;
pub mod distribution;
pub mod doctor;
pub mod domain;
pub mod embed;
pub mod error;
pub mod events;
pub mod extract;
pub mod integration;
pub mod jobs;
pub mod mcp;
pub mod migrate;
pub mod mining;
pub mod plugin;
pub mod project;
pub mod repair;
pub mod search;
pub mod server;
pub mod source;
pub mod store;
pub mod term;
pub mod trigger;
pub mod tui;

pub use error::{Error, Result};
