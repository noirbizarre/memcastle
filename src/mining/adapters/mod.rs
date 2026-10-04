//! The source adapters compiled into MemCastle.
//!
//! Only `directory` is: every other source (Pi history included, `sources/pi`) is an installed WebAssembly component
//! (docs/adr/026, docs/adr/028).
//!
//! Each file here is one provider's discovery, reading and normalisation, and nothing else: no job logic, no
//! store, no drawers (those are `pipeline`'s). Adding a provider is adding a file here, an arm in `mining::run`
//! and `mining::providers`, and a section in `docs/mining-sources.md`.

pub mod directory;
mod project_file;
mod watermark;
