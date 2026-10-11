//! Plugin authoring contracts and archive validation, independent of the daemon.

pub mod manifest;
pub mod package;
pub mod signing;

/// Manifest at a plugin repository's root and in its release archive.
pub const MANIFEST_FILE: &str = "plugin.toml";
