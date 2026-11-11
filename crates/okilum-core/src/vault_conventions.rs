//! Additive vault naming conventions. These classify data, never service paths.
use std::path::Path;

pub fn hidden(path: &str) -> bool {
    path.split('/')
        .any(|part| part.starts_with(['_', '@', '.']))
}

pub fn index_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.eq_ignore_ascii_case("@index.md") || name.eq_ignore_ascii_case("_index.md")
}

/// Prefer the @ family; preserve ambiguity within the preferred family.
pub fn index_candidates<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut at = Vec::new();
    let mut legacy = Vec::new();
    for path in paths.filter(|path| index_name(path)) {
        if path.rsplit('/').next().unwrap_or(path).starts_with('@') {
            at.push(path);
        } else {
            legacy.push(path);
        }
    }
    if at.is_empty() {
        legacy
    } else {
        at
    }
}

/// Only the root @Assets tree has the Markdown-as-asset convention.
/// Ordinary @ notes and the legacy daily-note exception remain notes.
pub fn asset_content(path: &str) -> bool {
    path.starts_with("@Assets/")
}

pub fn uses_at(root: &Path) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.eq_ignore_ascii_case("@index.md")
            || matches!(name.as_ref(), "@Assets" | "@Schema") && entry.file_type()?.is_dir()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Markdown assets never become note-list results, even through persisted history.
pub fn markdown_asset(path: &str) -> bool {
    asset_content(path)
        && Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}
