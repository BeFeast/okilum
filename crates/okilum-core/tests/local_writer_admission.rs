//! Production entrypoints, including independently launched writer/holder
//! processes. Ready markers are positive controls AFTER admission, never timers.
#![cfg(unix)]
use okilum_core::{
    file_editor::{FileEditor, Save},
    note_files::{self, PreparedCopy},
    note_move::MovePlan,
};
use okilum_write_authority::{Authority, Phase};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    drafts: PathBuf,
    authority: Authority,
}
impl Fixture {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("vault");
        fs::create_dir(&root).unwrap();
        let drafts = t.path().join("drafts");
        fs::write(root.join("note.md"), b"original\r\n").unwrap();
        let authority = Authority::for_root(&root).unwrap();
        Self {
            _temp: t,
            root,
            drafts,
            authority,
        }
    }
    fn release(&self, h: okilum_write_authority::Hold) {
        let token = h.token().clone();
        h.release(&token).unwrap();
    }
    fn child(&self, mode: &str) -> OwnedChild {
        let ready = self._temp.path().join(format!("{mode}.ready"));
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "local_writer_admission_child",
                "--nocapture",
            ])
            .env("LOCAL_WRITER_FIXTURE_ROOT", &self.root)
            .env("LOCAL_WRITER_FIXTURE_DRAFTS", &self.drafts)
            .env("LOCAL_WRITER_FIXTURE_MODE", mode)
            .env("LOCAL_WRITER_FIXTURE_READY", &ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut owned = OwnedChild(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready.exists() {
            assert!(
                owned.0.try_wait().unwrap().is_none(),
                "production child exited before its positive ready control"
            );
            assert!(
                Instant::now() < deadline,
                "production child never admitted/reached its boundary"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        owned
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl OwnedChild {
    fn finish(&mut self) {
        self.0.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                let mut out = String::new();
                let mut err = String::new();
                self.0
                    .stdout
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut out)
                    .unwrap();
                self.0
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut err)
                    .unwrap();
                assert!(status.success(), "child failed: {out}\n{err}");
                assert!(out.contains("1 passed"), "child test never ran: {out}");
                return;
            }
            assert!(Instant::now() < deadline, "production child did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
#[test]
fn local_writer_admission_production_delayed_copy_drains_across_processes_and_retains_editor_draft()
{
    let f = Fixture::new();
    let mut child = f.child("prepared-copy");
    assert!(!f.root.join("copy.bin").exists());
    let mut h = f.authority.begin_drain("production-drain").unwrap();
    let before = h.token().clone();
    assert!(
        !h.try_hold(&before).unwrap(),
        "an admitted production PreparedCopy must pin drain"
    );
    assert!(note_files::create(&f.root, Path::new("blocked.md")).is_err());
    let mut editor = FileEditor::open(&f.root.join("note.md"), &f.drafts).unwrap();
    editor.set_text("retained draft\r\n".into()).unwrap();
    assert!(editor.save().is_err());
    assert_eq!(fs::read(f.root.join("note.md")).unwrap(), b"original\r\n");
    drop(editor);
    let mut recovered = FileEditor::open(&f.root.join("note.md"), &f.drafts).unwrap();
    assert_eq!(recovered.text(), "retained draft\r\n");
    assert!(recovered.dirty());
    // The admitted delayed publisher still completes while new admission is shut.
    child.finish();
    assert_eq!(
        fs::read(f.root.join("copy.bin")).unwrap(),
        b"\0exact\xffcopy\r\n"
    );
    assert!(h.try_hold(&before).unwrap());
    assert!(
        h.try_hold(&before).is_err(),
        "a consumed transition revision cannot replay"
    );
    let other = Fixture::new();
    note_files::create(&other.root, Path::new("unrelated.md")).unwrap();
    assert!(recovered.save().is_err());
    f.release(h);
    assert_eq!(recovered.save().unwrap(), Save::Saved);
    note_files::create(&f.root, Path::new("blocked.md")).unwrap();
    assert_eq!(
        fs::read(f.root.join("note.md")).unwrap(),
        b"retained draft\r\n"
    );
}
#[test]
fn local_writer_admission_holder_crash_and_restarted_writer_require_explicit_recovery() {
    let f = Fixture::new();
    let mut holder = f.child("hold");
    assert_eq!(f.authority.snapshot().unwrap().phase, Phase::Held);
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    let restarted = Authority::for_root(&f.root).unwrap();
    let observed = restarted.snapshot().unwrap();
    assert_eq!(observed.phase, Phase::RecoveryPending);
    let mut writer = f.child("refused-save");
    writer.finish();
    assert_eq!(fs::read(f.root.join("note.md")).unwrap(), b"original\r\n");
    let mut h = restarted.recover(&observed).unwrap();
    assert!(restarted.recover(&observed).is_err());
    let t = h.token().clone();
    assert!(h.try_hold(&t).unwrap());
    f.release(h);
    let mut editor = FileEditor::open(&f.root.join("note.md"), &f.drafts).unwrap();
    assert_eq!(editor.text(), "crash draft");
    assert_eq!(editor.save().unwrap(), Save::Saved);
    assert!(restarted.recover(&observed).is_err());
}
#[test]
fn local_writer_admission_core_publishers_and_recovery_copies_refuse_then_resume() {
    let f = Fixture::new();
    let move_plan =
        MovePlan::prepare(&f.root, Path::new("note.md"), Path::new("moved.md")).unwrap();
    let capture =
        okilum_core::capture::Capture::new(Path::new(""), "capture draft".into()).unwrap();
    let records = f._temp.path().join("capture-records");
    let h = f.authority.begin_drain("publisher-matrix").unwrap();
    assert!(move_plan.commit().is_err());
    assert!(PreparedCopy::prepare(&f.root.join("copy.bin"), &f.drafts, b"asset").is_err());
    assert!(note_files::create_folders(&f.root, Path::new("new-folder/nested"), false).is_err());
    assert!(!f.root.join("new-folder").exists());
    assert!(
        note_files::create_attachment_with_source(&f.root, Path::new("asset.bin"), b"asset")
            .is_err()
    );
    assert!(capture.publish(&f.root, &f.drafts, &records).is_err());
    assert_eq!(
        okilum_core::capture::recover(&f.root, &records)
            .unwrap()
            .len(),
        1
    );
    assert!(okilum_core::source_history::save_copy(
        &f.root,
        Path::new("history.md"),
        &f.drafts,
        "history"
    )
    .is_err());
    assert!(!f.root.join("moved.md").exists());
    assert!(!f.root.join(capture.path()).exists());
    f.release(h);
    capture.publish(&f.root, &f.drafts, &records).unwrap();
    okilum_core::source_history::save_copy(&f.root, Path::new("history.md"), &f.drafts, "history")
        .unwrap();
    MovePlan::prepare(&f.root, Path::new("note.md"), Path::new("moved.md"))
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(fs::read(f.root.join("moved.md")).unwrap(), b"original\r\n");
}
#[test]
fn local_writer_admission_task_and_reminder_keep_refused_proposals_as_drafts() {
    let f = Fixture::new();
    let task = "- [ ] Task\n";
    fs::write(f.root.join("task.md"), task).unwrap();
    fs::write(f.root.join("reminders.md"), "# Reminders\n").unwrap();
    let target = okilum_core::task_edit::Target::capture("task.md", task, 1).unwrap();
    let reminder = "- [ ] Review 📅 2026-11-04\n";
    let h = f.authority.begin_drain("typed-edits").unwrap();
    assert!(okilum_core::task_edit::write::apply(
        &f.root,
        &f.drafts,
        &target,
        okilum_core::task_edit::Change::Checked(true)
    )
    .is_err());
    assert!(
        okilum_core::reminder_append::write::add(&f.root, &f.drafts, "reminders.md", reminder)
            .is_err()
    );
    assert_eq!(fs::read_to_string(f.root.join("task.md")).unwrap(), task);
    assert_eq!(
        fs::read_to_string(f.root.join("reminders.md")).unwrap(),
        "# Reminders\n"
    );
    let mut task = FileEditor::open(&f.root.join("task.md"), &f.drafts).unwrap();
    let mut reminder = FileEditor::open(&f.root.join("reminders.md"), &f.drafts).unwrap();
    assert_eq!(task.text(), "- [x] Task\n");
    assert!(reminder.text().contains("📅 2026-11-04"));
    f.release(h);
    assert_eq!(task.save().unwrap(), Save::Saved);
    assert_eq!(reminder.save().unwrap(), Save::Saved);
}
#[test]
#[ignore = "subprocess fixture, explicitly selected by the production regressions"]
fn local_writer_admission_child() {
    let root = PathBuf::from(std::env::var_os("LOCAL_WRITER_FIXTURE_ROOT").unwrap());
    let drafts = PathBuf::from(std::env::var_os("LOCAL_WRITER_FIXTURE_DRAFTS").unwrap());
    let ready = PathBuf::from(std::env::var_os("LOCAL_WRITER_FIXTURE_READY").unwrap());
    let mode = std::env::var("LOCAL_WRITER_FIXTURE_MODE").unwrap();
    let mut copy = None;
    let mut hold = None;
    match mode.as_str() {
        "prepared-copy" => {
            copy = Some(
                PreparedCopy::prepare(&root.join("copy.bin"), &drafts, b"\0exact\xffcopy\r\n")
                    .unwrap(),
            );
        }
        "hold" => {
            let a = Authority::for_root(&root).unwrap();
            let mut h = a.begin_drain("crashed-process").unwrap();
            let token = h.token().clone();
            assert!(h.try_hold(&token).unwrap());
            hold = Some(h);
        }
        "refused-save" => {
            let mut editor = FileEditor::open(&root.join("note.md"), &drafts).unwrap();
            editor.set_text("crash draft".into()).unwrap();
            assert!(editor.save().is_err());
        }
        _ => panic!("unknown fixture mode"),
    }
    fs::write(ready, b"admitted production boundary reached").unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    assert_eq!(line, "go\n");
    if let Some(copy) = copy {
        copy.publish().unwrap();
    }
    if let Some(hold) = hold {
        let token = hold.token().clone();
        hold.release(&token).unwrap();
    }
}

#[cfg(feature = "brain")]
#[test]
fn local_writer_admission_source_store_retains_durable_request_before_refusing_publication() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use okilum_core::source::{SourceStore, SourceWrite, WriteBoundary, SCHEMA};
    let f = Fixture::new();
    let store = SourceStore::open(
        "01000000-0000-4000-8000-000000000001",
        &f.root,
        &f._temp.path().join("source-state"),
        WriteBoundary::Managed,
    )
    .unwrap();
    let req = SourceWrite {
        schema: SCHEMA.into(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        brain_id: "01000000-0000-4000-8000-000000000001".into(),
        path: "source.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode(b"proposed\0exact\xff"),
    };
    let h = f.authority.begin_drain("source-store").unwrap();
    assert!(store.write(req.clone()).is_err());
    assert!(!f.root.join("source.md").exists());
    let retained = store.recovery_record(&req.operation_id).unwrap();
    assert_eq!(retained.request, req);
    assert!(retained.receipt.is_none());
    f.release(h);
    store.write(req).unwrap();
    assert_eq!(
        fs::read(f.root.join("source.md")).unwrap(),
        b"proposed\0exact\xff"
    );
}
