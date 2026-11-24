//! Read-only execution of the vault's declarative view_api: 0 contract.
mod bind;
pub mod builtin;
mod compute;
mod parse;
mod render;
mod run;
pub(crate) mod snapshot;

pub use parse::{parse, validate, Show, ViewDoc};
pub use render::{render_json, render_markdown};
pub use run::{
    coverage, run, Coverage, Group, Index, NearProvider, Row, RunContext, Section, Status,
    TreeNode, ViewIdentity, ViewPart, ViewResult,
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Finding {
    pub path: String,
    /// One-based source line and character column.
    pub line: usize,
    pub col: usize,
    pub severity: Severity,
    pub message: String,
}

impl Finding {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}
