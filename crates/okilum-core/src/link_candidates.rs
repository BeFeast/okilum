//! Portable source-snapshot link candidates shared by reconcile and move preview.
#[path = "link_rewrite/index.rs"]
mod index;
#[path = "link_rewrite/syntax.rs"]
pub mod syntax;
#[cfg(any(unix, windows))]
use crate::Vault;
#[cfg(any(unix, windows))]
use anyhow::{ensure, Result};
pub use index::CandidateIndex;
use std::collections::BTreeMap;
use std::path::Path;
