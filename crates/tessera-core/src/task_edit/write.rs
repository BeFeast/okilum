//! Local task mutations through the same lock, draft, history and atomic-save
//! implementation as the Reader editor. Call on a worker; no protocol activation.
use super::{Change, Plan, Target};
use crate::file_editor::{FileEditor, Save};
use anyhow::{bail, ensure, Context, Result};
use std::path::{Component, Path, PathBuf};

/// An in-process Undo capability issued only after a successful save. The
/// editor also archives durable preimages outside the disposable index.
#[derive(Clone, Debug)]
pub struct Receipt {
    root: PathBuf,
    plan: Plan,
}
impl Receipt {
    pub fn path(&self) -> &str {
        &self.plan.path
    }
    pub fn saved_revision(&self) -> &str {
        &self.plan.after_revision
    }

    pub fn undo(&self, root: &Path, drafts: &Path) -> Result<()> {
        ensure!(
            root.canonicalize()? == self.root,
            "Undo belongs to a different vault"
        );
        let path = bound_path(&self.root, &self.plan.path)?;
        let mut editor = open_clean(&path, drafts)?;
        let before = self
            .plan
            .undo_source(editor.text())
            .map_err(anyhow::Error::msg)?;
        editor.set_text(before.to_owned())?;
        save(&mut editor)
    }
}

/// `drafts` is the Reader's durable editor-drafts directory, never its index.
/// A no-op returns None, avoiding an empty history entry or misleading Undo toast.
pub fn apply(
    root: &Path,
    drafts: &Path,
    target: &Target,
    change: Change,
) -> Result<Option<Receipt>> {
    let root = root.canonicalize()?;
    let path = bound_path(&root, target.path())?;
    let mut editor = open_clean(&path, drafts)?;
    let plan = target
        .plan(editor.text(), change)
        .map_err(anyhow::Error::msg)?;
    if plan.before == plan.after {
        return Ok(None);
    }
    editor.set_text(plan.after.clone())?;
    save(&mut editor)?;
    Ok(Some(Receipt { root, plan }))
}
fn open_clean(path: &Path, drafts: &Path) -> Result<FileEditor> {
    // Opening owns the editor lock before inspecting any retained draft. Never
    // adopt another editor's dirty recovery text as the dashboard's source.
    let editor = FileEditor::open(path, drafts)?;
    ensure!(
        editor.path() == path,
        "The task source location changed while opening it"
    );
    ensure!(
        !editor.dirty(),
        "This note has unsaved edits. Open its source before changing tasks."
    );
    Ok(editor)
}
fn save(editor: &mut FileEditor) -> Result<()> {
    match editor.save()? {
        Save::Saved => Ok(()),
        Save::Conflict => {
            bail!("This note changed. The task change was not saved; its draft is retained.")
        }
    }
}
fn bound_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let rel = Path::new(relative);
    ensure!(
        !relative.is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_))),
        "Invalid task source path"
    );
    let mut path = root.to_owned();
    for component in rel.components() {
        path.push(component);
        let metadata =
            std::fs::symlink_metadata(&path).context("The task source is unavailable")?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Task sources cannot traverse symlinks"
        );
    }
    ensure!(
        path.canonicalize()? == path,
        "The task source location changed"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let drafts = temp.path().join("state/editor-drafts");
        std::fs::create_dir(&root).unwrap();
        let text =
            "\u{feff}---\r\ntype: Note\r\n---\r\n- [ ] Привет 📅 2026-10-08 ⏳ 2026-10-07 ^id\r\n"
                .to_owned();
        std::fs::write(root.join("a.md"), &text).unwrap();
        (temp, root, drafts, text)
    }
    #[test]
    fn save_and_undo_preserve_exact_bytes_and_archive_history() {
        let (_temp, root, drafts, text) = fixture();
        let target = Target::capture("a.md", &text, 4).unwrap();
        let receipt = apply(
            &root,
            &drafts,
            &target,
            Change::Scheduled(time::macros::date!(2026 - 10 - 09)),
        )
        .unwrap()
        .unwrap();
        let changed = std::fs::read_to_string(root.join("a.md")).unwrap();
        assert_eq!(changed, text.replace("⏳ 2026-10-07", "⏳ 2026-10-09"));
        assert!(drafts
            .join("source-history")
            .read_dir()
            .unwrap()
            .next()
            .is_some());
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), text);
        assert!(receipt.undo(&root, &drafts).is_err());
        assert!(apply(&root, &drafts, &target, Change::Checked(false))
            .unwrap()
            .is_none());
    }
    #[test]
    fn active_editor_dirty_recovery_and_external_revision_refuse_without_overwrite() {
        let (_temp, root, drafts, text) = fixture();
        let target = Target::capture("a.md", &text, 4).unwrap();
        let mut editor = FileEditor::open(&root.join("a.md"), &drafts).unwrap();
        assert!(apply(&root, &drafts, &target, Change::Checked(true)).is_err());
        editor.set_text(text.clone() + "Draft").unwrap();
        drop(editor);
        assert!(apply(&root, &drafts, &target, Change::Checked(true)).is_err());
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), text);
        let editor = FileEditor::open(&root.join("a.md"), &drafts).unwrap();
        assert!(editor.text().ends_with("Draft"));
        drop(editor);
        // An independent state directory exercises stale-source refusal, without
        // deleting or rewriting the preserved recovery journal above.
        let clean = drafts.parent().unwrap().join("other-drafts");
        std::fs::write(root.join("a.md"), text.clone() + "External").unwrap();
        assert!(apply(&root, &clean, &target, Change::Checked(true)).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            text + "External"
        );
    }
    #[test]
    fn undo_refuses_later_edits_other_roots_and_symlink_replacements() {
        let (_temp, root, drafts, text) = fixture();
        let target = Target::capture("a.md", &text, 4).unwrap();
        let receipt = apply(&root, &drafts, &target, Change::Checked(true))
            .unwrap()
            .unwrap();
        std::fs::write(root.join("a.md"), "New external version").unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            "New external version"
        );
        assert!(receipt.undo(root.parent().unwrap(), &drafts).is_err());
        std::fs::rename(root.join("a.md"), root.join("other.md")).unwrap();
        std::os::unix::fs::symlink(root.join("other.md"), root.join("a.md")).unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        assert!(apply(&root, &drafts, &target, Change::Checked(true)).is_err());
    }
}
