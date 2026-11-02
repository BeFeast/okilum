use super::*;
use ::core::prelude::v1::test;
use okilum_core::log::virtual_log::Definition;
use std::fs;

struct Fixture {
    _dir: tempfile::TempDir,
    vault: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir_all(vault.join("logs")).unwrap();
        let path = vault.join("view.okilog");
        fs::write(
            &path,
            r#"{"version":1,"name":"Follow","glob":"logs/*.log","vault_root":true}"#,
        )
        .unwrap();
        fs::write(vault.join("logs/a.log"), "first\n").unwrap();
        Self {
            _dir: dir,
            vault,
            path,
        }
    }
    fn plan(&self) -> SelectionPlan {
        Definition::parse(&fs::read_to_string(&self.path).unwrap())
            .unwrap()
            .plan(&self.vault, &self.path, 0)
            .unwrap()
    }
    fn change(&self, name: &str) -> Vec<Change> {
        vec![Change::Changed(vec![self.vault.join(name)])]
    }
}
#[test]
fn follow_adds_new_members_appends_loaded_handles_and_never_reopens_known_identities() {
    let f = Fixture::new();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let old = initial.files[0].snapshot();
    let mut session = Session::new(&initial).unwrap();
    let cancel = AtomicBool::new(false);
    fs::write(f.vault.join("logs/b.log"), "second\n").unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/b.log"), &cancel)
        .unwrap();
    assert_eq!(update.sources.len(), 1);
    assert_eq!(update.sources[0].slot, 1);
    assert_eq!(update.sources[0].snapshot.raw(0).unwrap(), b"second");
    // A second path for an already retained inode must not duplicate its rows.
    fs::hard_link(f.vault.join("logs/a.log"), f.vault.join("logs/alias.log")).unwrap();
    assert!(session
        .refresh(&plan, &f.change("logs/alias.log"), &cancel)
        .unwrap()
        .sources
        .is_empty());
    let a = f.vault.join("logs/a.log");
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&a)
        .unwrap()
        .write_all(b"third\n")
        .unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log"), &cancel)
        .unwrap();
    assert_eq!(update.sources.len(), 1);
    assert_eq!(update.sources[0].slot, 0);
    assert_eq!(update.sources[0].snapshot.raw(0).unwrap(), b"first");
    assert_eq!(update.sources[0].snapshot.raw(1).unwrap(), b"third");
    assert_eq!(old.index().len(), 1);
    assert_eq!(old.raw(0).unwrap(), b"first");
    assert_eq!(session.known.len(), 2);
    let excluded = plan.select_excluding(&session.known, |_, _| {
        panic!("retained identities must not reopen content")
    });
    assert_eq!(excluded.checked, 0);
    assert!(excluded.files.is_empty());
}
#[test]
fn follow_removal_replacement_and_truncation_pause_without_publishing_changed_identity() {
    for action in ["remove", "replace", "truncate"] {
        let f = Fixture::new();
        let plan = f.plan();
        let initial = plan.select(|_, _| {});
        let mut session = Session::new(&initial).unwrap();
        let a = f.vault.join("logs/a.log");
        match action {
            "remove" => fs::remove_file(&a).unwrap(),
            "replace" => {
                fs::rename(&a, f.vault.join("logs/a.log.1")).unwrap();
                fs::write(&a, "replacement\n").unwrap();
            }
            _ => fs::write(&a, "x\n").unwrap(),
        }
        let error = session
            .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("retained"), "{action}: {error}");
        assert_eq!(session.published[&0].snapshot.raw(0).unwrap(), b"first");
        assert_eq!(session.published.len(), 1);
    }
}
#[test]
fn follow_lost_coverage_cancel_and_empty_events_keep_last_successful_snapshot() {
    for changes in [
        vec![Change::Incomplete("overflow positive control".into())],
        vec![Change::Changed(vec![])],
    ] {
        let f = Fixture::new();
        let plan = f.plan();
        let initial = plan.select(|_, _| {});
        let mut session = Session::new(&initial).unwrap();
        assert!(session
            .refresh(&plan, &changes, &AtomicBool::new(false))
            .is_err());
        assert_eq!(session.published[&0].snapshot.raw(0).unwrap(), b"first");
    }
    let f = Fixture::new();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    fs::write(f.vault.join("logs/b.log"), "not published\n").unwrap();
    assert!(session
        .refresh(&plan, &f.change("logs/b.log"), &AtomicBool::new(true))
        .is_err());
    assert_eq!(session.published.len(), 1);
}
#[test]
fn follow_empty_initial_selection_can_admit_a_later_member_without_source_zero_hole() {
    let f = Fixture::new();
    fs::remove_file(f.vault.join("logs/a.log")).unwrap();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    assert!(initial.files.is_empty());
    let mut session = Session::new(&initial).unwrap();
    fs::write(f.vault.join("logs/new.log"), "new\n").unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/new.log"), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(update.sources.len(), 1);
    assert_eq!(update.sources[0].slot, 0);
}

#[test]
fn follow_drains_bounded_append_backlog_without_requiring_another_event_or_membership_scan() {
    let f = Fixture::new();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let mut bytes = b"first\n".to_vec();
    let line = format!("{}\n", "x".repeat(4095));
    for _ in 0..1400 {
        bytes.extend_from_slice(line.as_bytes());
    }
    fs::write(f.vault.join("logs/a.log"), &bytes).unwrap();
    let cancel = AtomicBool::new(false);
    let first = session
        .refresh(&plan, &f.change("logs/a.log"), &cancel)
        .unwrap();
    assert!(
        first.more,
        "positive control: one poll cannot consume a >4MiB append"
    );
    assert_eq!(first.sources.len(), 1);
    let second = session.refresh(&plan, &[], &cancel).unwrap();
    assert!(!second.more);
    assert!(
        second.candidates.is_none(),
        "catch-up must not repeat membership discovery"
    );
    assert_eq!(second.sources[0].snapshot.bytes(), bytes);
}
