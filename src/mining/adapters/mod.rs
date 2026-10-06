//! The source adapters compiled into MemCastle.
//!
//! Only `directory` is: every other source (Pi history included, `sources/pi`) is an installed WebAssembly component
//! (docs/adr/026, docs/adr/028).
//!
//! Each file here is one source's discovery, reading and normalisation, and nothing else: no job logic, no
//! store, no drawers (those are `pipeline`'s). Adding a built-in source is adding a file here, an entry in
//! `mining::registry` and a section in `docs/mining-sources.md`; most sources are packages instead.

pub mod directory;
mod watermark;
