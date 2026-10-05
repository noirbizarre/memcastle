//! Every test that starts a daemon inside the test process, as one binary.
//!
//! These used to be sixteen binaries of their own. Each one linked the whole server (SurrealDB, the MCP stack and the
//! WebAssembly runtime, about 600 MB with debug info), which cost several seconds apiece to link on every build and
//! was most of what compiling the test suite took. One binary links it once. nextest still runs each test in a process
//! of its own, so nothing is shared at run time that was not before.
//!
//! To add a test of this kind, create `tests/in_process/<name>.rs` and list it below. A test that needs a real process
//! boundary (a daemon that is killed, a CLI against nothing) stays a binary of its own under `tests/`, and so does
//! anything that builds a component, which is named `wasm_*` (docs/development.md).

// `tests/common` is shared with the `wasm_*` binaries, so it lives beside them rather than in here.
#[path = "../common/mod.rs"]
mod common;

mod audit;
mod auth;
mod cli_daemon;
mod concurrency;
mod db_endpoint;
mod dedup;
mod extraction;
mod integration_contract;
mod mcp_memory_mode;
mod mcp_temporal;
mod memory_mode;
mod notes;
mod palace;
mod repair;
mod retrieval;
mod server;
mod skills;
mod sources;
mod web;
