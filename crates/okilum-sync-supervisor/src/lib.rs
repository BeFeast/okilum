//! The managed-sync supervisor (design: docs/sync-supervisor-shape.md and
//! docs/sync-supervisor-contract.md). It owns the Syncthing process tree for one
//! instance, writes the generation hint, serves Status and authorized Stop over the
//! platform transport, and ends with its tree. It reads the lifecycle journal and never
//! writes it. This crate carries no GUI code.
pub mod args;
pub mod startup;

#[cfg(unix)]
pub mod serve;
