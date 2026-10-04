//! The source adapters MemCastle ships.
//!
//! Each file here is one provider's discovery, reading and normalisation, and nothing else: no job logic, no
//! store, no drawers (those are `pipeline`'s). Adding a provider is adding a file here, an arm in `mining::run`
//! and `mining::providers`, and a section in `docs/mining-sources.md`.

pub mod directory;
pub mod pi_sessions;
mod watermark;
