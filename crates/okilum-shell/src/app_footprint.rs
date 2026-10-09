//! Everything Okilum creates on this machine outside the user's vaults, and its
//! removal on uninstall (#974). The inventory is docs/uninstall.md; the source
//! gate test below fails when code computes a new base directory that the
//! inventory and `roots()` do not know about.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Prefix of the per-run full-text search sessions in the system temp dir.
pub(crate) const SEARCH_SESSION_PREFIX: &str = "okilum-search-session-";
/// Unsaved drafts are exported here before the app's state is removed.
const DRAFT_EXPORT_FOLDER: &str = "Okilum unsaved drafts";

/// Every directory the app owns, from the same functions that decide where it
/// writes. A root that holds a recorded vault is never removed.
pub(crate) fn roots() -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    if let Ok(state) = crate::reader_history::state_directory() {
        roots.insert(state);
    }
    if let Some(config) = crate::reader_layout::config_base() {
        roots.insert(config.join("okilum"));
    }
    if let Ok(cache) = crate::reader_open::cache_base() {
        roots.insert(cache.join("okilum"));
    }
    // Brain profiles and outboxes always use XDG config, also on macOS.
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        roots.insert(config.join("okilum"));
    }
    roots.into_iter().filter(|p| p.is_absolute()).collect()
}

/// What an uninstall removed, kept or could not remove; printed for support.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Report {
    pub removed: Vec<PathBuf>,
    pub exported_drafts: Vec<PathBuf>,
    pub kept_holding_vault: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Vault roots recorded in the state dir. Windows stores verbatim prefixes.
fn recorded_vaults(state: &Path) -> Vec<PathBuf> {
    let mut vaults = Vec::new();
    let mut add = |value: &serde_json::Value| {
        if let Some(path) = value.as_str() {
            let plain = path
                .strip_prefix(r"\\?\UNC\")
                .map(|rest| format!(r"\\{rest}"))
                .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_owned());
            vaults.push(PathBuf::from(plain));
        }
    };
    if let Some(session) = read_json(&state.join("update-session.json")) {
        for key in ["recent_roots", "quick_roots"] {
            for root in session[key].as_array().into_iter().flatten() {
                add(root);
            }
        }
        for root in session["last_documents"].as_object().into_iter().flatten() {
            add(&serde_json::Value::String(root.0.clone()));
        }
    }
    if let Some(ui) = read_json(&state.join("reader-ui.json")) {
        for key in ["vaults", "vault_colors"] {
            for root in ui[key].as_object().into_iter().flatten() {
                let root = root.0.strip_prefix("reader:").unwrap_or(root.0);
                add(&serde_json::Value::String(root.to_owned()));
            }
        }
    }
    vaults
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Export drafts whose text differs from the file on disk (or whose file is
/// gone) so removing the state never loses unsaved work.
fn export_unsaved_drafts(state: &Path, export: &Path, report: &mut Report) {
    let Ok(entries) = std::fs::read_dir(state.join("editor-drafts")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Some(draft) = read_json(&path) else {
            continue;
        };
        let (Some(note), Some(text)) = (draft["path"].as_str(), draft["text"].as_str()) else {
            continue;
        };
        if std::fs::read(note).is_ok_and(|disk| disk == text.as_bytes()) {
            continue;
        }
        let name = Path::new(note)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "draft.md".into());
        let target = unique(export, &name);
        let written = std::fs::create_dir_all(export).and_then(|()| std::fs::write(&target, text));
        match written {
            Ok(()) => report.exported_drafts.push(target),
            Err(error) => report.failed.push((path, format!("export draft: {error}"))),
        }
    }
}

fn unique(folder: &Path, name: &str) -> PathBuf {
    let candidate = folder.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, "md"));
    (2..)
        .map(|n| folder.join(format!("{stem} ({n}).{extension}")))
        .find(|path| !path.exists())
        .expect("unbounded")
}

/// Remove `roots` and stale search sessions in `temp`, keeping vaults.
pub(crate) fn purge(
    roots: &[PathBuf],
    state: Option<&Path>,
    temp: &Path,
    documents: &Path,
) -> Report {
    let mut report = Report::default();
    let vaults = state.map(recorded_vaults).unwrap_or_default();
    if let Some(state) = state {
        export_unsaved_drafts(state, &documents.join(DRAFT_EXPORT_FOLDER), &mut report);
    }
    for root in roots {
        if !root.exists() {
            continue;
        }
        if vaults.iter().any(|vault| vault.starts_with(root)) {
            report.kept_holding_vault.push(root.clone());
            continue;
        }
        match std::fs::remove_dir_all(root) {
            Ok(()) => report.removed.push(root.clone()),
            Err(error) => report.failed.push((root.clone(), error.to_string())),
        }
    }
    for session in search_sessions(temp) {
        match std::fs::remove_dir_all(&session) {
            Ok(()) => report.removed.push(session),
            Err(error) => report.failed.push((session, error.to_string())),
        }
    }
    report
}

fn search_sessions(temp: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(SEARCH_SESSION_PREFIX)
        })
        .map(|e| e.path())
        .collect()
}

/// A crashed or killed run leaves its search session behind (~100 MB each).
/// Sweep the ones older than a day at startup; a live session is recent.
pub(crate) fn sweep_stale_search_sessions() {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 60 * 60);
    for session in search_sessions(&std::env::temp_dir()) {
        let stale = std::fs::metadata(&session)
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified < cutoff);
        if stale {
            let _ = std::fs::remove_dir_all(&session);
        }
    }
}

/// Velopack's before-uninstall hook: everything Okilum created for this user.
#[cfg(windows)]
pub(crate) fn uninstall() {
    crate::markdown_handler::uninstall();
    windows::remove_registrations();
    let state = crate::reader_history::state_directory().ok();
    let documents = dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir);
    let report = purge(
        &roots(),
        state.as_deref(),
        &std::env::temp_dir(),
        &documents,
    );
    windows::remove_crash_dumps();
    eprintln!("Okilum uninstall: {report:?}");
}

#[cfg(windows)]
mod windows {
    use std::process::Command;

    /// Notifications register this AUMID (gpui system_notifications).
    const AUMID_KEY: &str = r"HKCU\Software\Classes\AppUserModelId\com.befeast.okilum";

    pub(super) fn remove_registrations() {
        let _ = Command::new("reg.exe")
            .args(["delete", AUMID_KEY, "/f"])
            .status();
    }

    /// Windows Error Reporting keeps local dumps named after the executable.
    pub(super) fn remove_crash_dumps() {
        let Some(folder) = dirs::data_local_dir().map(|d| d.join("CrashDumps")) else {
            return;
        };
        for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("okilum.exe.")
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn purge_removes_owned_dirs_and_sessions_but_never_a_vault_or_unsaved_text() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let state = base.join("state/okilum");
        let config = base.join("config/okilum");
        let temp = base.join("tmp");
        let documents = base.join("Documents");
        let vault = base.join("Notes");
        write(&vault.join("saved.md"), "same text");
        write(&vault.join("edited.md"), "old text");
        // A vault that lives inside a root keeps that whole root.
        let inner_vault = base.join("cache/okilum/inner-vault");
        write(&inner_vault.join("note.md"), "inner");
        write(
            &state.join("update-session.json"),
            &serde_json::json!({
                "recent_roots": [vault, inner_vault],
                "last_documents": {},
            })
            .to_string(),
        );
        write(&state.join("reader-ui.json"), "{}");
        for (key, note, text) in [
            ("a", vault.join("saved.md"), "same text"),
            ("b", vault.join("edited.md"), "unsaved שלום text"),
            ("c", vault.join("deleted.md"), "orphan draft"),
        ] {
            write(
                &state.join(format!("editor-drafts/{key}.json")),
                &serde_json::json!({"path": note, "base": "", "text": text}).to_string(),
            );
        }
        write(&config.join("reader-layout.json"), "{}");
        write(
            &temp.join(format!("{SEARCH_SESSION_PREFIX}x/meta.json")),
            "{}",
        );
        write(&temp.join("someone-else/keep.txt"), "not ours");
        let roots = vec![state.clone(), config.clone(), base.join("cache/okilum")];
        let report = purge(&roots, Some(&state), &temp, &documents);

        assert!(!state.exists() && !config.exists(), "{report:?}");
        assert_eq!(report.kept_holding_vault, vec![base.join("cache/okilum")]);
        assert!(
            inner_vault.join("note.md").exists(),
            "a root holding a vault stays"
        );
        assert_eq!(
            std::fs::read_to_string(vault.join("edited.md")).unwrap(),
            "old text"
        );
        assert!(
            vault.join("saved.md").exists(),
            "vault files are never touched"
        );
        assert!(!temp.join(format!("{SEARCH_SESSION_PREFIX}x")).exists());
        assert!(
            temp.join("someone-else/keep.txt").exists(),
            "positive control"
        );
        let exported: BTreeSet<String> = report
            .exported_drafts
            .iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        assert_eq!(
            exported,
            BTreeSet::from(["unsaved שלום text".to_owned(), "orphan draft".to_owned()]),
            "only drafts that differ from disk are exported"
        );
        assert!(report.failed.is_empty(), "{report:?}");
        // A second run finds nothing left and changes nothing.
        let again = purge(&roots, Some(&state), &temp, &documents);
        assert!(
            again.removed.is_empty() && again.exported_drafts.is_empty(),
            "{again:?}"
        );
    }

    #[test]
    fn windows_verbatim_vault_roots_are_recognised() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path();
        write(
            &state.join("update-session.json"),
            r#"{"recent_roots":["\\\\?\\C:\\Users\\u\\Notes","\\\\?\\UNC\\nas\\share\\vault"]}"#,
        );
        write(
            &state.join("reader-ui.json"),
            r#"{"vaults":{"reader:/home/u/notes":{}}}"#,
        );
        assert_eq!(
            recorded_vaults(state),
            vec![
                PathBuf::from(r"C:\Users\u\Notes"),
                PathBuf::from(r"\\nas\share\vault"),
                PathBuf::from("/home/u/notes"),
            ]
        );
    }

    /// Production code that computes a base directory, per source file. A new
    /// site means the app may write somewhere the uninstaller does not know:
    /// add it to docs/uninstall.md and `roots()` (or the OS-specific removal),
    /// then update this list.
    const BASE_DIRECTORY_SITES: &[(&str, usize)] = &[
        ("crates/okilum-shell/src/app_footprint.rs", 5),
        // Suggests ~/Downloads in the export dialog: the user picks the target.
        ("crates/okilum-shell/src/brain/context_ui.rs", 1),
        // Brain outboxes and profile: all under the `~/.config/okilum` root.
        ("crates/okilum-shell/src/brain/discussion_send_outbox.rs", 2),
        ("crates/okilum-shell/src/brain/editor_recovery.rs", 2),
        ("crates/okilum-shell/src/brain/native_outbox.rs", 2),
        ("crates/okilum-shell/src/brain/t3_route_outbox.rs", 2),
        ("crates/okilum-shell/src/brain.rs", 2),
        ("crates/okilum-shell/src/reader_history.rs", 4),
        ("crates/okilum-shell/src/reader_layout.rs", 3),
        ("crates/okilum-shell/src/reader_open.rs", 4),
        // Sync lives under the state root; its systemd unit is removed by
        // the sync controller (Linux part of #974).
        ("crates/okilum-shell/src/reader_settings_sync.rs", 3),
        // Moves notes to the system trash; not an app-owned location.
        ("crates/okilum-shell/src/reader_trash_fs.rs", 2),
        ("crates/okilum-shell/src/workspace.rs", 3),
    ];

    /// Production code only: drop each `#[cfg(test)] mod … { … }` block, which
    /// may sit anywhere in a file. Braces inside string and char literals are
    /// skipped so `format!("{x}")` does not unbalance the match.
    fn strip_test_modules(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::new();
        let mut start = 0;
        while let Some(found) = text[start..].find("#[cfg(test)]\nmod ") {
            let module = start + found;
            out.push_str(&text[start..module]);
            let Some(open) = text[module..].find('{').map(|i| module + i) else {
                return out;
            };
            let (mut depth, mut at) = (0usize, open);
            while at < bytes.len() {
                match bytes[at] {
                    b'"' => {
                        at += 1;
                        while at < bytes.len() && bytes[at] != b'"' {
                            at += if bytes[at] == b'\\' { 2 } else { 1 };
                        }
                    }
                    b'\'' if bytes.get(at + 2) == Some(&b'\'') => at += 2,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                at += 1;
            }
            start = at + 1;
        }
        out.push_str(&text[start.min(text.len())..]);
        out
    }

    #[test]
    fn strip_test_modules_keeps_code_after_a_mid_file_test_module() {
        let text = "a dirs::x\n#[cfg(test)]\nmod tests {\n fn f() { let _ = format!(\"{}\", '{'); dirs::y }\n}\nb dirs::z\n";
        let production = strip_test_modules(text);
        assert_eq!(production.matches("dirs::").count(), 2, "{production}");
        assert!(
            production.contains("b dirs::z"),
            "positive control: code after the module stays"
        );
    }

    #[test]
    fn every_base_directory_in_production_code_is_in_the_uninstall_inventory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut found = Vec::new();
        for crate_dir in ["crates/okilum-shell/src", "crates/okilum-core/src"] {
            let mut stack = vec![root.join(crate_dir)];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let name = path.to_string_lossy();
                    if !name.ends_with(".rs")
                        || name.contains("/tests")
                        || name.ends_with("tests.rs")
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(&path).unwrap();
                    let production = strip_test_modules(&text);
                    let count = [
                        "dirs::",
                        "\"XDG_STATE_HOME\"",
                        "\"XDG_CONFIG_HOME\"",
                        "\"XDG_CACHE_HOME\"",
                        "\"XDG_DATA_HOME\"",
                        "var_os(\"HOME\")",
                    ]
                    .iter()
                    .map(|needle| production.matches(needle).count())
                    .sum::<usize>();
                    if count > 0 {
                        let relative = path
                            .strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        found.push((relative, count));
                    }
                }
            }
        }
        found.sort();
        let expected: Vec<(String, usize)> = BASE_DIRECTORY_SITES
            .iter()
            .map(|(path, count)| ((*path).to_owned(), *count))
            .collect();
        assert_eq!(
            found, expected,
            "update docs/uninstall.md, roots() and this list"
        );
    }
}
