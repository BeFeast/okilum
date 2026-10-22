//! Sprint-only opt-in app report delivery; the menu click is the Send action.
use super::model::Report;
use anyhow::{Context, Result};
use std::path::Path;

pub(crate) fn enabled() -> bool {
    cfg!(target_os = "macos") && env!("OKILUM_RELEASE_VERSION").starts_with("Sprint ")
}
pub(super) fn context() -> serde_json::Value {
    let path = std::env::var_os("OKILUM_SPRINT_CONTEXT")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .parent()?
                .parent()
                .map(|contents| contents.join("Resources/sprint-context.json"))
        });
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({"branch":"unknown"}))
}
pub(super) fn title() -> String {
    let context = context();
    let branch = context
        .get("branch")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");
    format!(
        "[Sprint report] {branch} {} manual",
        env!("OKILUM_SOURCE_COMMIT")
            .chars()
            .take(8)
            .collect::<String>()
    )
}
pub(super) fn enrich(mut report: Report, app_log: Option<&Path>) -> Result<Report> {
    if !enabled() || report.log.is_none() {
        return Ok(report);
    }
    let executable = std::env::current_exe()?;
    let resources = executable
        .parent()
        .and_then(Path::parent)
        .context("Sprint bundle resources unavailable; your draft is kept.")?
        .join("Resources");
    let mut command = std::process::Command::new("python3");
    command
        .arg(resources.join("sprint-report.py"))
        .arg("--context")
        .arg(resources.join("sprint-context.json"))
        .arg("--collect")
        .arg("--stage")
        .arg("manual");
    if let Some(path) = app_log {
        command.arg("--app-log").arg(path);
    }
    let output = command
        .output()
        .context("Couldn't collect Sprint diagnostics; your draft is kept.")?;
    anyhow::ensure!(
        output.status.success(),
        "Couldn't collect Sprint diagnostics; your draft is kept."
    );
    let collected: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    report.log = Some(
        collected["log"]
            .as_str()
            .context("Sprint report log unavailable")?
            .to_owned(),
    );
    Ok(report)
}
