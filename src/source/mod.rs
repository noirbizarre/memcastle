//! Source packages: what a third-party mining source is on disk, and the tooling to make one.
//!
//! A source package is a WebAssembly Component plus a `memcastle-source.toml` manifest (docs/adr/026). This module
//! is everything about packages that is not *running* one: reading and validating the manifest, packing and
//! unpacking the distributable archive, scaffolding a new source, building it, and the conformance cases it is
//! tested against. Running a component is `crate::mining::wasm`; deciding what is installed is `crate::app`.
//!
//! Nothing here touches the store or the jobs: `memcastle source init`, `build`, `test` and `package` work without
//! a daemon, which is the whole point of a development workflow (AGENTS.md, invariant 1).

pub mod build;
pub mod conformance;
pub mod manifest;
pub mod package;
pub mod publish;
pub mod scaffold;
pub mod signing;

/// The manifest's file name, at the top of a package directory and of a package archive.
pub const MANIFEST_FILE: &str = "memcastle-source.toml";

/// The component's file name inside an installed package and a package archive.
pub const COMPONENT_FILE: &str = "source.wasm";

/// The WIT source of the contract, as this MemCastle implements it. Embedded so a scaffolded source is built
/// against exactly the contract of the MemCastle that scaffolded it.
pub const CONTRACT_WIT: &str = include_str!("../../wit/memcastle-source.wit");
