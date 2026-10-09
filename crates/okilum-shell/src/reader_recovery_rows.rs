//! Text for the Recover notes list. Pure presentation: the row says which note,
//! what kind of entry and how old; the full path, exact time and the reason an
//! entry is protected stay in the row details, never dropped.
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// The note's name as the user knows it: no folder, no `.md`.
pub(super) fn title(rel: &Path) -> String {
    rel.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel.display().to_string())
}

/// What the entry is. Core labels append a reason or a detail after the
/// leading phrase ("Unsaved draft — protected", "Before link move: a → b");
/// both belong in the details, not in the row.
pub(super) fn kind(label: &str) -> &str {
    label.split([':', '—', '·']).next().unwrap_or(label).trim()
}

/// Titles shared by notes at different paths. Several versions of one note are
/// not ambiguous; they differ by kind and age.
pub(super) fn ambiguous_titles<'a>(paths: impl Iterator<Item = &'a Path>) -> HashSet<String> {
    let mut by_title: HashMap<String, HashSet<&Path>> = HashMap::new();
    for rel in paths {
        by_title.entry(title(rel)).or_default().insert(rel);
    }
    by_title
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(title, _)| title)
        .collect()
}

/// `Title · Kind · age`. A folder is added only when another note shares the
/// title, so same-named notes stay distinguishable without noise elsewhere.
pub(super) fn row_label(rel: &Path, label: &str, age: &str, ambiguous: bool) -> String {
    let folder = rel
        .parent()
        .filter(|folder| ambiguous && !folder.as_os_str().is_empty())
        .map(|folder| format!(" ({})", folder.display()))
        .unwrap_or_default();
    format!("{}{folder} · {} · {age}", title(rel), kind(label))
}

/// Everything the row leaves out: exact path, the core label verbatim, the
/// exact time and why a protected entry is not expired.
pub(super) fn row_details(rel: &Path, label: &str, saved: &str, protected: bool) -> String {
    let mut details = format!("{}\n{label}\n{saved}", rel.display());
    if protected {
        details.push_str(
            "\nProtected: kept until recovered; the usual version and age limits do not remove it.",
        );
    }
    details
}

/// A link-move endpoint as the user knows it. Only a `.md` suffix is dropped:
/// a folder named `Plan.v2` keeps its whole name.
fn move_name(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

/// `Old → New · 3 files · Interrupted` for the Recover link moves list. The
/// folder part of each endpoint stays, so a move between folders is readable.
pub(super) fn move_row_label(from: &str, to: &str, files: usize, complete: bool) -> String {
    let files = if files == 1 {
        "1 file".to_owned()
    } else {
        format!("{files} files")
    };
    let state = if complete { "Completed" } else { "Interrupted" };
    format!(
        "{} → {} · {files} · {state}",
        move_name(from),
        move_name(to)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str) -> &Path {
        Path::new(path)
    }

    #[test]
    fn move_row_uses_note_names_singular_counts_and_capitalised_state() {
        assert_eq!(
            move_row_label("Work/Plan.md", "Home/Plan.md", 1, false),
            "Work/Plan → Home/Plan · 1 file · Interrupted"
        );
        assert_eq!(
            move_row_label("Plan.v2", "Archive/Plan.v2", 0, true),
            "Plan.v2 → Archive/Plan.v2 · 0 files · Completed"
        );
        assert_eq!(
            move_row_label("A.md", "B.md", 12, true),
            "A → B · 12 files · Completed"
        );
    }

    #[test]
    fn title_drops_folder_and_extension() {
        assert_eq!(title(p("Projects/Meeting notes.md")), "Meeting notes");
        assert_eq!(title(p("a.b.md")), "a.b");
        assert_eq!(title(p("")), "");
    }

    #[test]
    fn kind_keeps_only_the_leading_phrase() {
        assert_eq!(kind("Unsaved draft — protected"), "Unsaved draft");
        assert_eq!(kind("Protected save recovery"), "Protected save recovery");
        assert_eq!(kind("Before save · vault-side archive"), "Before save");
        assert_eq!(kind("Before link move: a.md → b.md"), "Before link move");
        assert_eq!(
            kind("Unassigned displaced source — save a copy to inspect"),
            "Unassigned displaced source"
        );
    }

    #[test]
    fn same_title_at_different_paths_is_ambiguous_but_versions_of_one_note_are_not() {
        let paths = [
            p("Work/Plan.md"),
            p("Home/Plan.md"),
            p("Work/Plan.md"),
            p("Notes/Other.md"),
            p("Notes/Other.md"),
        ];
        let ambiguous = ambiguous_titles(paths.into_iter());
        assert!(ambiguous.contains("Plan"));
        assert!(!ambiguous.contains("Other"));
    }

    #[test]
    fn row_names_the_note_kind_and_age_without_internals() {
        let row = row_label(
            p("Projects/Meeting notes.md"),
            "Unsaved draft — protected",
            "2 h ago",
            false,
        );
        assert_eq!(row, "Meeting notes · Unsaved draft · 2 h ago");
        assert!(!row.contains(".md") && !row.contains("protected"));
    }

    #[test]
    fn ambiguous_row_carries_its_folder_and_root_notes_do_not() {
        let nested = row_label(p("Work/Plan.md"), "Before save", "3 d ago", true);
        assert_eq!(nested, "Plan (Work) · Before save · 3 d ago");
        let root = row_label(p("Plan.md"), "Before save", "3 d ago", true);
        assert_eq!(root, "Plan · Before save · 3 d ago");
    }

    #[test]
    fn details_keep_path_label_time_and_protection_reason() {
        let details = row_details(
            p("Work/Plan.md"),
            "Before link move: Work/Plan.md → Home/Plan.md",
            "2026-10-08 12:34 UTC",
            true,
        );
        assert!(details.contains("Work/Plan.md\n"));
        assert!(details.contains("Before link move: Work/Plan.md → Home/Plan.md"));
        assert!(details.contains("2026-10-08 12:34 UTC"));
        assert!(details.contains("Protected:"));
        let plain = row_details(p("Plan.md"), "Before save", "2026-10-08 12:34 UTC", false);
        assert!(!plain.contains("Protected:"));
    }
}
