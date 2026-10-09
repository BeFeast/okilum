//! Guarded insertion into an existing reminders note. Run on a worker.
use super::Plan;
use crate::file_editor::{FileEditor, Save};
use anyhow::{bail, ensure, Context, Result};
use std::path::{Component, Path, PathBuf};

/// Issued only after FileEditor has saved the complete insertion. Source history
/// remains in durable editor state; this in-process capability is not a journal.
#[derive(Clone, Debug)]
pub struct Receipt {
    root: PathBuf,
    relative: String,
    plan: Plan,
}

impl Receipt {
    pub fn path(&self) -> &str {
        &self.relative
    }

    pub fn undo(&self, root: &Path, drafts: &Path) -> Result<()> {
        ensure!(
            root.canonicalize()? == self.root,
            "Undo belongs to a different vault"
        );
        let path = bound_path(&self.root, &self.relative)?;
        let mut editor = open_clean(&path, drafts)?;
        let before = self
            .plan
            .undo_source(editor.text())
            .map_err(anyhow::Error::msg)?;
        editor.set_text(before.to_owned())?;
        save(&mut editor)
    }
}

/// Apply a previously captured plan, comparing its entire preimage under the
/// editor lock. `drafts` is durable editor state outside the disposable index.
/// Missing destinations refuse: exclusive creation is a separate operation.
pub fn apply(root: &Path, drafts: &Path, relative: &str, plan: &Plan) -> Result<Receipt> {
    let root = root.canonicalize()?;
    let path = bound_path(&root, relative)?;
    let mut editor = open_clean(&path, drafts)?;
    let after = plan
        .apply_source(editor.text())
        .map_err(anyhow::Error::msg)?;
    editor.set_text(after.to_owned())?;
    save(&mut editor)?;
    Ok(Receipt {
        root,
        relative: relative.into(),
        plan: plan.clone(),
    })
}

/// Add one formatted reminder (`reminder_task::format`) to the configured note:
/// append when it exists, create it when it does not. A note that appears
/// between the check and the creation is appended to, never overwritten.
pub fn add(root: &Path, drafts: &Path, relative: &str, reminder: &str) -> Result<Receipt> {
    let canonical = root.canonicalize()?;
    checked_relative(relative)?;
    let plan = |source: &str| Plan::new(source, reminder).map_err(anyhow::Error::msg);
    for _ in 0..2 {
        match std::fs::symlink_metadata(canonical.join(relative)) {
            Ok(_) => {
                let source = std::fs::read_to_string(bound_path(&canonical, relative)?)
                    .context("The reminders note must be readable UTF-8 text")?;
                return apply(&canonical, drafts, relative, &plan(&source)?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match create_and_apply(&canonical, relative, &plan("")?) {
                    Ok(receipt) => return Ok(receipt),
                    // Lost the creation race: loop once and append instead.
                    Err(_) if canonical.join(relative).symlink_metadata().is_ok() => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e).context("The reminders note is unavailable"),
        }
    }
    bail!("The reminders note keeps changing. Try again.")
}

/// Create the missing destination with its first reminder. Creation is
/// exclusive (NOREPLACE through descriptors in an existing real folder), so a
/// note that appeared meanwhile is never overwritten: the caller re-plans and
/// uses `apply`. The plan must describe an empty preimage.
///
/// Undo restores the empty note through FileEditor and keeps the file: a path
/// based delete cannot be made race-free against a concurrent writer.
pub fn create_and_apply(root: &Path, relative: &str, plan: &Plan) -> Result<Receipt> {
    let root = root.canonicalize()?;
    let rel = checked_relative(relative)?;
    let after = plan.apply_source("").map_err(anyhow::Error::msg)?;
    crate::note_files::create_with_source(&root, rel, after.as_bytes())?;
    Ok(Receipt {
        root,
        relative: relative.into(),
        plan: plan.clone(),
    })
}

fn checked_relative(relative: &str) -> Result<&Path> {
    let rel = Path::new(relative);
    ensure!(
        !relative.is_empty()
            && rel.components().all(|c| matches!(c, Component::Normal(_)))
            && rel
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md")),
        "Expected a vault-relative Markdown reminders path"
    );
    Ok(rel)
}

fn bound_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let rel = checked_relative(relative)?;
    let mut path = root.to_owned();
    for component in rel.components() {
        path.push(component);
        let metadata =
            std::fs::symlink_metadata(&path).context("The reminders note is unavailable")?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Reminders paths cannot traverse symlinks"
        );
    }
    ensure!(
        path.canonicalize()? == path,
        "The reminders location changed"
    );
    Ok(path)
}

fn open_clean(path: &Path, drafts: &Path) -> Result<FileEditor> {
    let editor = FileEditor::open(path, drafts)?;
    ensure!(
        editor.path() == path,
        "The reminders location changed while opening it"
    );
    ensure!(
        !editor.dirty(),
        "The reminders note has unsaved edits. Open it before adding a reminder."
    );
    Ok(editor)
}

fn save(editor: &mut FileEditor) -> Result<()> {
    match editor.save()? {
        Save::Saved => Ok(()),
        Save::Conflict => {
            bail!("The reminders note changed. The edit was not saved; its draft is retained.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TASK: &str = "- [ ] Review [[Source.md]] 📅 2026-11-01\n";

    fn fixture(source: &str) -> (tempfile::TempDir, PathBuf, PathBuf, Plan) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Reminders.md"), source).unwrap();
        let drafts = temp.path().join("state/editor-drafts");
        let plan = Plan::new(source, TASK).unwrap();
        (temp, root, drafts, plan)
    }

    #[test]
    fn append_and_undo_preserve_bytes_and_archive_preimage() {
        for source in ["", "# Reminders", "\u{feff}# תזכורות\r\n", TASK] {
            let (_temp, root, drafts, plan) = fixture(source);
            let receipt = apply(&root, &drafts, "Reminders.md", &plan).unwrap();
            assert_eq!(receipt.path(), "Reminders.md");
            let saved = std::fs::read_to_string(root.join(receipt.path())).unwrap();
            assert_eq!(saved, plan.apply_source(source).unwrap());
            assert!(drafts
                .join("source-history")
                .read_dir()
                .unwrap()
                .next()
                .is_some());
            receipt.undo(&root, &drafts).unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join(receipt.path())).unwrap(),
                source
            );
            assert!(receipt.undo(&root, &drafts).is_err());
        }
    }

    #[test]
    fn stale_apply_and_undo_preserve_external_content() {
        let (_temp, root, drafts, plan) = fixture("# Reminders\n");
        let path = root.join("Reminders.md");
        std::fs::write(&path, "External\n").unwrap();
        assert!(apply(&root, &drafts, "Reminders.md", &plan).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "External\n");
        let fresh = Plan::new("External\n", TASK).unwrap();
        let receipt = apply(&root, &drafts, "Reminders.md", &fresh).unwrap();
        assert!(receipt.undo(root.parent().unwrap(), &drafts).is_err());
        let later = format!("{}Later\n", std::fs::read_to_string(&path).unwrap());
        std::fs::write(&path, &later).unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), later);
    }

    #[test]
    fn active_editor_and_recovered_draft_block_apply_and_undo() {
        let (_temp, root, drafts, plan) = fixture("Original\n");
        let path = root.join("Reminders.md");
        let mut editor = FileEditor::open(&path, &drafts).unwrap();
        assert!(apply(&root, &drafts, "Reminders.md", &plan).is_err());
        editor.set_text("Unsaved\n".into()).unwrap();
        drop(editor);
        assert!(apply(&root, &drafts, "Reminders.md", &plan).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Original\n");
        let mut editor = FileEditor::open(&path, &drafts).unwrap();
        assert_eq!(editor.text(), "Unsaved\n");
        editor.reload().unwrap();
        drop(editor);
        let receipt = apply(&root, &drafts, "Reminders.md", &plan).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        let mut editor = FileEditor::open(&path, &drafts).unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        editor.set_text("New draft\n".into()).unwrap();
        drop(editor);
        assert!(receipt.undo(&root, &drafts).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        let mut editor = FileEditor::open(&path, &drafts).unwrap();
        assert_eq!(editor.text(), "New draft\n");
        editor.reload().unwrap();
        drop(editor);
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Original\n");
    }

    #[test]
    fn refuses_missing_and_non_markdown_destinations_and_traversal() {
        let (_temp, root, drafts, plan) = fixture("");
        for relative in ["missing.md", "../Reminders.md", "/Reminders.md", ""] {
            assert!(
                apply(&root, &drafts, relative, &plan).is_err(),
                "{relative}"
            );
        }
        std::fs::write(root.join("data.txt"), "").unwrap();
        assert!(apply(&root, &drafts, "data.txt", &plan).is_err());
        assert!(!root.join("missing.md").exists());
        assert_eq!(std::fs::read_to_string(root.join("data.txt")).unwrap(), "");
        // An existing empty Markdown file is distinct from a missing destination.
        apply(&root, &drafts, "Reminders.md", &plan).unwrap();
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".tessera-"))
            .collect()
    }

    #[test]
    fn create_publishes_the_first_reminder_and_undo_keeps_an_empty_note() {
        let (_temp, root, drafts, _) = fixture("");
        // Positive control: the leftover probe must see a temporary name.
        std::fs::write(root.join(".tessera-create-probe"), "").unwrap();
        assert_eq!(leftovers(&root).len(), 1);
        std::fs::remove_file(root.join(".tessera-create-probe")).unwrap();
        std::fs::create_dir(root.join("Inbox")).unwrap();
        let plan = Plan::new("", TASK).unwrap();
        let receipt = create_and_apply(&root, "Inbox/Later.md", &plan).unwrap();
        let path = root.join("Inbox/Later.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TASK);
        assert!(leftovers(&root.join("Inbox")).is_empty());
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        assert!(receipt.undo(&root, &drafts).is_err());
    }

    #[test]
    fn create_refuses_existing_destinations_and_unsuitable_plans() {
        let (_temp, root, _drafts, _) = fixture("Original\n");
        let plan = Plan::new("", TASK).unwrap();
        assert!(create_and_apply(&root, "Reminders.md", &plan).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("Reminders.md")).unwrap(),
            "Original\n"
        );
        // A plan with a non-empty preimage describes an existing note, not a new one.
        let appended = Plan::new("Original\n", TASK).unwrap();
        assert!(create_and_apply(&root, "Fresh.md", &appended).is_err());
        assert!(!root.join("Fresh.md").exists());
        for relative in ["Missing/Fresh.md", "../Fresh.md", "Fresh.txt", ""] {
            assert!(
                create_and_apply(&root, relative, &plan).is_err(),
                "{relative}"
            );
        }
        assert!(!root.join("Missing").exists() && !root.join("Fresh.txt").exists());
        assert!(!root.parent().unwrap().join("Fresh.md").exists());
        assert!(leftovers(&root).is_empty());
    }

    #[test]
    fn add_creates_then_appends_and_each_receipt_undoes_its_own_line() {
        let (_temp, root, drafts, _) = fixture("");
        std::fs::remove_file(root.join("Reminders.md")).unwrap();
        let first = add(&root, &drafts, "Reminders.md", TASK).unwrap();
        let second_line = "- [ ] Pay [[Bills.md]] 📅 2026-11-02\n";
        let second = add(&root, &drafts, "Reminders.md", second_line).unwrap();
        let path = root.join("Reminders.md");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{TASK}\n{second_line}")
        );
        // Undo is receipt-scoped: the older receipt must not erase newer content.
        assert!(first.undo(&root, &drafts).is_err());
        second.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TASK);
        first.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        // An emptied note is still an existing note and is appended to.
        add(&root, &drafts, "Reminders.md", TASK).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TASK);
    }

    #[test]
    fn add_refuses_invalid_lines_unreadable_notes_and_unsafe_paths() {
        let (_temp, root, drafts, _) = fixture("Original\n");
        let path = root.join("Reminders.md");
        assert!(add(&root, &drafts, "Reminders.md", "- [ ] No date\n").is_err());
        assert!(add(&root, &drafts, "../Reminders.md", TASK).is_err());
        assert!(add(&root, &drafts, "Reminders.txt", TASK).is_err());
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(add(&root, &drafts, "Reminders.md", TASK).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0xfe, 0x00]);
        assert!(leftovers(&root).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn create_refuses_symlinked_folders_and_dangling_destinations() {
        let (temp, root, _drafts, _) = fixture("");
        let plan = Plan::new("", TASK).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("alias")).unwrap();
        assert!(create_and_apply(&root, "alias/Fresh.md", &plan).is_err());
        std::os::unix::fs::symlink(outside.join("target.md"), root.join("Dangling.md")).unwrap();
        assert!(create_and_apply(&root, "Dangling.md", &plan).is_err());
        assert!(!outside.join("Fresh.md").exists() && !outside.join("target.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_destinations_parents_and_undo_replacement() {
        let (_temp, root, drafts, plan) = fixture("Original\n");
        let receipt = apply(&root, &drafts, "Reminders.md", &plan).unwrap();
        let path = root.join("Reminders.md");
        let other = root.join("Other.md");
        std::fs::rename(&path, &other).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        assert!(apply(&root, &drafts, "Reminders.md", &plan).is_err());
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        assert!(apply(&root, &drafts, "alias/Other.md", &plan).is_err());
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            plan.apply_source("Original\n").unwrap()
        );
    }
}
