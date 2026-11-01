//! Reader integration boundary: caller supplies current editor and policy snapshots.
//! No source writes, background requests or implicit vault traversal.
use okilum_core::agent_language::{self, Report, Status};

pub const POLICY_PATH: &str = "_Schema/language.yaml";

/// The parent reader owns loading the bounded policy snapshot and presenting
/// `status_text` plus range/hover decorations. Call on current source or policy
/// changes, and discard a result if the editor revision changed before display.
pub fn diagnostics(enabled: bool, path: &str, source: &str, policy: Result<&str, &str>) -> Report {
    if !enabled || !okilum_core::quick_open::is_agent_file(path) {
        return Report {
            status: Status::NotApplicable,
            diagnostics: Vec::new(),
        };
    }
    match policy {
        Ok(policy) => agent_language::diagnose(path, source, Some(policy)),
        Err(reason) => Report::unknown(format!("Language policy unavailable: {reason}")),
    }
}

pub fn status_text(report: &Report) -> String {
    match &report.status {
        Status::NotApplicable => String::new(),
        Status::Unknown(reason) => format!("Language check unknown: {reason}"),
        Status::Checked if report.diagnostics.is_empty() => "Language check: no matches".into(),
        Status::Checked => format!("Language check: {} matches", report.diagnostics.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_policy_is_visible_only_for_enabled_agent_files() {
        assert_eq!(
            diagnostics(false, "AGENTS.md", "", Err("missing")).status,
            Status::NotApplicable
        );
        assert_eq!(
            diagnostics(true, "note.md", "", Err("missing")).status,
            Status::NotApplicable
        );
        let report = diagnostics(true, "AGENTS.md", "", Err("missing"));
        assert!(status_text(&report).contains("unknown"));
        assert!(status_text(&report).contains("missing"));
    }
}
