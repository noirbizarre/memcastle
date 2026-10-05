//! Installing the agent integrations a MemCastle ships (docs/adr/034).
//!
//! An integration is lifecycle glue for one agent (AGENTS.md, invariant 8). This module is not that glue: it is the
//! tooling that finds an integration among the assets MemCastle ships, checks it against the MemCastle and the agent
//! it will run with, copies it somewhere the user owns, and tells the agent about it.
//!
//! Like `crate::source`, it works on files and talks to no daemon: installing an integration must work before a daemon
//! has ever run, and it never touches the store or the jobs. It opens no network either: everything it installs is
//! already on this machine, in the package or the checkout the assets root names.
//!
//! - [`manifest`] reads `memcastle-integration.toml`.
//! - [`catalog`] does discovery under the assets root, the same for a package and a checkout.
//! - [`agent`] holds what differs between agents.
//! - [`install`] installs, updates and removes, and works out the state `list` shows.
//! - [`render`] is what the CLI prints about them.

pub mod agent;
pub mod catalog;
pub mod install;
pub mod manifest;
pub mod render;

pub use agent::{Locations, SystemRunner};
pub use catalog::{Catalog, Shipped};
pub use install::{
    Action, Change, ChangeKind, Context, Outcome, State, Status, inspect, install, remove, update,
};
pub use manifest::{AgentKind, IntegrationManifest, MANIFEST_FILE};
