//! Local task mutations through the same lock, draft, history and atomic-save
//! implementation as the Reader editor. Call on a worker; no protocol activation.
use super::{Change, Plan, Target};
use crate::file_editor::{FileEditor, Save};
use crate::tasks::{Index, Task};
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
        let _admission = okilum_write_authority::operation(&self.root)?;
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
    okilum_write_authority::Authority::for_root(&root)?;
    let path = bound_path(&root, target.path())?;
    let mut editor = open_clean(&path, drafts)?;
    let plan = target
        .plan(editor.text(), change)
        .map_err(anyhow::Error::msg)?;
    commit_plan(root, &mut editor, plan)
}

/// Apply an occurrence from the immutable index that produced the displayed row.
/// Run on a worker. Source validation happens while FileEditor owns its lock;
/// callers must not substitute the latest index for the displayed snapshot.
pub fn apply_indexed(
    root: &Path,
    drafts: &Path,
    displayed: &Index,
    task: &Task,
    change: Change,
) -> Result<Option<Receipt>> {
    let root = root.canonicalize()?;
    okilum_write_authority::Authority::for_root(&root)?;
    let path = bound_path(&root, &task.path)?;
    let mut editor = open_clean(&path, drafts)?;
    let target = displayed
        .edit_target(task, editor.text())
        .map_err(anyhow::Error::msg)?;
    let plan = target
        .plan(editor.text(), change)
        .map_err(anyhow::Error::msg)?;
    commit_plan(root, &mut editor, plan)
}

fn commit_plan(root: PathBuf, editor: &mut FileEditor, plan: Plan) -> Result<Option<Receipt>> {
    if plan.before == plan.after {
        return Ok(None);
    }
    let _admission = okilum_write_authority::operation(&root)?;
    editor.set_text(plan.after.clone())?;
    save(editor)?;
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
    okilum_write_authority::Authority::for_root(root)?;
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
    fn indexed(text: &str) -> (Index, Task) {
        let mut index = Index::default();
        index.replace("a.md", text);
        let task = index
            .query(&crate::tasks::Query::parse(
                "",
                time::macros::date!(2026 - 10 - 07),
            ))
            .remove(0);
        (index, task)
    }

    #[test]
    fn windows_local_writer_review_task_refusal_does_not_block_retry_or_undo() {
        let (_temp, root, drafts, text) = fixture();
        let (index, task) = indexed(&text);
        let a = okilum_write_authority::Authority::for_root(&root).unwrap();
        let hold = a.begin_drain("task-refusal").unwrap();
        assert!(apply_indexed(&root, &drafts, &index, &task, Change::Checked(true)).is_err());
        assert!(!FileEditor::has_unsaved_draft(&root.join("a.md"), &drafts).unwrap());
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        let receipt = apply_indexed(&root, &drafts, &index, &task, Change::Checked(true))
            .unwrap()
            .unwrap();
        let hold = a.begin_drain("undo-refusal").unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        assert!(!FileEditor::has_unsaved_draft(&root.join("a.md"), &drafts).unwrap());
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), text);
    }

    // `windows_` runs it on the native Windows CI lane too (#1127).
    #[test]
    fn windows_indexed_write_and_undo_preserve_occurrence_and_exact_source() {
        let (_temp, root, drafts, text) = fixture();
        // Identical labels still denote separate occurrences, never a bulk edit.
        let task_line = text.lines().last().unwrap();
        let source = format!("{text}{task_line}\r\n");
        std::fs::write(root.join("a.md"), &source).unwrap();
        let (index, task) = indexed(&source);
        assert!(
            apply_indexed(&root, &drafts, &index, &task, Change::Checked(false))
                .unwrap()
                .is_none()
        );
        let receipt = apply_indexed(&root, &drafts, &index, &task, Change::Checked(true))
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            source.replacen("[ ]", "[x]", 1)
        );
        // The old display cannot write again after this successful source change.
        assert!(apply_indexed(&root, &drafts, &index, &task, Change::Checked(false)).is_err());
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), source);
        let receipt = apply_indexed(
            &root,
            &drafts,
            &index,
            &task,
            Change::Scheduled(time::macros::date!(2026 - 10 - 10)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            source.replacen("⏳ 2026-10-07", "⏳ 2026-10-10", 1)
        );
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), source);
    }

    #[test]
    fn indexed_write_refuses_stale_snapshot_and_fabricated_occurrence() {
        let (_temp, root, drafts, text) = fixture();
        let (index, task) = indexed(&text);
        let mut fabricated = task.clone();
        fabricated.line += 1;
        assert!(apply_indexed(&root, &drafts, &index, &fabricated, Change::Checked(true)).is_err());
        assert!(apply_indexed(
            &root,
            &drafts,
            &Index::default(),
            &task,
            Change::Checked(true)
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), text);
        let external = text + "\r\nExternal prose\r\n";
        std::fs::write(root.join("a.md"), &external).unwrap();
        assert!(apply_indexed(&root, &drafts, &index, &task, Change::Checked(true)).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            external
        );
        let (fresh, fresh_task) = indexed(&external);
        assert_eq!(
            task.text, fresh_task.text,
            "prose changed, the task text did not"
        );
        assert_eq!(task.line, fresh_task.line);
        let receipt = apply_indexed(&root, &drafts, &fresh, &fresh_task, Change::Checked(true))
            .unwrap()
            .unwrap();
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            external
        );
    }

    #[test]
    fn indexed_write_respects_editor_lock_and_dirty_recovery() {
        let (_temp, root, drafts, text) = fixture();
        let (index, task) = indexed(&text);
        let mut editor = FileEditor::open(&root.join("a.md"), &drafts).unwrap();
        assert!(apply_indexed(&root, &drafts, &index, &task, Change::Checked(true)).is_err());
        let draft = text.clone() + "Unsaved draft";
        editor.set_text(draft.clone()).unwrap();
        drop(editor);
        assert!(apply_indexed(&root, &drafts, &index, &task, Change::Checked(true)).is_err());
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), text);
        let recovered = FileEditor::open(&root.join("a.md"), &drafts).unwrap();
        assert_eq!(recovered.text(), draft);
    }

    #[test]
    fn indexed_undo_preserves_active_edits_and_recovery_until_explicit_reload() {
        let (_temp, root, drafts, text) = fixture();
        let (index, task) = indexed(&text);
        let receipt = apply_indexed(&root, &drafts, &index, &task, Change::Checked(true))
            .unwrap()
            .unwrap();
        let saved = text.replacen("[ ]", "[x]", 1);
        let path = root.join("a.md");
        let mut editor = FileEditor::open(&path, &drafts).unwrap();
        let unsaved = format!("{saved}Unsaved work after checking the task\r\n");
        editor.set_text(unsaved.clone()).unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        assert_eq!(editor.text(), unsaved);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        drop(editor);

        assert!(receipt.undo(&root, &drafts).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        let mut recovered = FileEditor::open(&path, &drafts).unwrap();
        assert_eq!(recovered.text(), unsaved);
        assert!(recovered.dirty());
        // Only an explicit editor reload discards the retained recovery text.
        recovered.reload().unwrap();
        drop(recovered);
        // Positive control: the same receipt remains usable when the guard clears.
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
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
    #[cfg(unix)]
    #[test]
    fn editor_binding_refuses_redirected_parents_and_hard_links() {
        let (_temp, root, drafts, text) = fixture();
        let target = Target::capture("a.md", &text, 4).unwrap();
        std::fs::hard_link(root.join("a.md"), root.join("alias.md")).unwrap();
        assert!(apply(&root, &drafts, &target, Change::Checked(true)).is_err());
        std::fs::remove_file(root.join("alias.md")).unwrap();
        // Simulate a parent replacement between bound_path and open_clean.
        let expected = bound_path(&root.canonicalize().unwrap(), "a.md").unwrap();
        let moved = root.with_file_name("moved-vault");
        std::fs::rename(&root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &root).unwrap();
        assert!(open_clean(&expected, &drafts).is_err());
        assert_eq!(std::fs::read_to_string(moved.join("a.md")).unwrap(), text);
    }
    #[cfg(unix)]
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

    #[test]
    fn view_check_with_done_preserves_bytes_refuses_stale_and_undoes_exact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let drafts = temp.path().join("state/editor-drafts");
        std::fs::create_dir(&root).unwrap();
        let original = "\u{feff}---\r\ntype: questions\r\n---\r\n- [ ] Approve `@views/out/` [owner:: oleg] 📅 2026-11-26  \r\n- [ ] Sibling line  \r\n";
        let file = root.join("questions.md");
        std::fs::write(&file, original).unwrap();
        let mut index = Index::default();
        index.replace("questions.md", original);
        let task = index.note_tasks("questions.md").remove(0);
        let done = time::macros::date!(2026 - 11 - 24);

        let receipt = apply_indexed(
            &root,
            &drafts,
            &index,
            &task,
            Change::CheckWithDone {
                checked: true,
                done,
            },
        )
        .unwrap()
        .unwrap();
        let expected = "\u{feff}---\r\ntype: questions\r\n---\r\n- [x] Approve `@views/out/` [owner:: oleg] 📅 2026-11-26 [done:: 2026-11-24]  \r\n- [ ] Sibling line  \r\n";
        assert_eq!(std::fs::read(&file).unwrap(), expected.as_bytes());

        // Stale index is refused with no write.
        let err = apply_indexed(
            &root,
            &drafts,
            &index,
            &task,
            Change::CheckWithDone {
                checked: false,
                done,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("The note changed, refresh"),
            "{err}"
        );
        assert_eq!(std::fs::read(&file).unwrap(), expected.as_bytes());

        // Undo restores the exact previous bytes.
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), original.as_bytes());
    }

    #[test]
    fn view_check_with_done_handles_lone_cr_and_refuses_multiline_spans_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let drafts = temp.path().join("state/editor-drafts");
        std::fs::create_dir(&root).unwrap();
        let done = time::macros::date!(2026 - 11 - 24);

        // Lone CR must not leak edits into the sibling task; Undo restores exact bytes.
        let cr_file = root.join("cr.md");
        let cr_original = "- [ ] First\r- [x] Second [done:: 2026-01-01]\n";
        std::fs::write(&cr_file, cr_original).unwrap();
        let mut index = Index::default();
        index.replace("cr.md", cr_original);
        let task = index.note_tasks("cr.md").remove(0);
        let receipt = apply_indexed(
            &root,
            &drafts,
            &index,
            &task,
            Change::CheckWithDone {
                checked: true,
                done,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read(&cr_file).unwrap(),
            b"- [x] First [done:: 2026-11-24]\r- [x] Second [done:: 2026-01-01]\n"
        );
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read(&cr_file).unwrap(), cr_original.as_bytes());

        // Multi-line code span, link, and read-only legacy emoji completion are refused with zero disk writes.
        for (name, source) in [
            ("multiline-code.md", "- [ ] Example `code\n  continued`\n"),
            (
                "multiline-link.md",
                "- [ ] [Example\n  label](https://example.org)\n",
            ),
            ("legacy-date.md", "- [ ] Task ✅ 2026-01-01\n"),
            ("legacy-bad.md", "- [ ] Task ✅ nope\n"),
            (
                "legacy-with-inline.md",
                "- [ ] Task ✅ 2026-01-01 [done:: 2026-01-02]\n",
            ),
            (
                "multiline-obsidian-comment.md",
                "- [ ] Actual %% hidden\n  still hidden %%\n",
            ),
        ] {
            let file = root.join(name);
            std::fs::write(&file, source).unwrap();
            let target = Target::capture(name, source, 1).unwrap();
            assert!(apply(
                &root,
                &drafts,
                &target,
                Change::CheckWithDone {
                    checked: true,
                    done,
                },
            )
            .is_err());
            assert_eq!(std::fs::read(&file).unwrap(), source.as_bytes());
        }

        // Displayed row "Third" after a lone CR targets line 3 via Index, leaving First & Second untouched.
        let cr3_file = root.join("cr3.md");
        let cr3_original = "- [ ] First\r- [ ] Second\n- [ ] Third\n";
        std::fs::write(&cr3_file, cr3_original).unwrap();
        let mut cr3_index = Index::default();
        cr3_index.replace("cr3.md", cr3_original);
        let third = cr3_index
            .note_tasks("cr3.md")
            .into_iter()
            .find(|t| t.text == "Third")
            .unwrap();
        let cr3_receipt = apply_indexed(
            &root,
            &drafts,
            &cr3_index,
            &third,
            Change::CheckWithDone {
                checked: true,
                done,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read(&cr3_file).unwrap(),
            b"- [ ] First\r- [ ] Second\n- [x] Third [done:: 2026-11-24]\n"
        );
        cr3_receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read(&cr3_file).unwrap(), cr3_original.as_bytes());
    }
}
