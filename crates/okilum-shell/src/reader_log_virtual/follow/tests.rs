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
fn follow_missing_retains_open_identity_then_continues_replacement_as_a_new_segment() {
    let f = Fixture::new();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let a = f.vault.join("logs/a.log");
    fs::remove_file(&a).unwrap();
    let missing = session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    assert!(missing.notice.unwrap().contains("missing"));
    assert!(missing.sources.is_empty());
    assert_eq!(session.published[&0].snapshot.raw(0).unwrap(), b"first");
    fs::write(&a, "replacement\n").unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    assert!(update.notice.unwrap().contains("Rotated"));
    assert_eq!(session.published.len(), 2);
    assert_eq!(session.published[&0].snapshot.raw(0).unwrap(), b"first");
    assert_eq!(
        session.published[&1].snapshot.raw(0).unwrap(),
        b"replacement"
    );
}
#[test]
fn follow_rotation_keeps_late_old_descriptor_writes_and_does_not_duplicate_the_moved_inode() {
    use std::io::Write;
    let f = Fixture::new();
    fs::write(
        &f.path,
        r#"{"version":1,"name":"Follow","glob":"logs/**","vault_root":true}"#,
    )
    .unwrap();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let a = f.vault.join("logs/a.log");
    let mut old_writer = fs::OpenOptions::new().append(true).open(&a).unwrap();
    fs::rename(&a, f.vault.join("logs/a.log.1")).unwrap();
    old_writer.write_all(b"late before observation\n").unwrap();
    fs::write(&a, "replacement\n").unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    assert!(update.notice.unwrap().contains("unobserved gaps"));
    assert_eq!(session.published.len(), 2);
    assert_eq!(session.known.len(), 2);
    assert_eq!(session.published[&0].label, Some("before rotation"));
    assert_eq!(
        session.published[&0].snapshot.raw(1).unwrap(),
        b"late before observation"
    );
    old_writer.write_all(b"late after observation\n").unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log.1"), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(update.sources.len(), 1);
    assert_eq!(update.sources[0].slot, 0);
    assert_eq!(
        update.sources[0].snapshot.raw(2).unwrap(),
        b"late after observation"
    );
    assert_eq!(
        session.published.len(),
        2,
        "renamed old inode must not become another source"
    );
}
#[test]
fn follow_copytruncate_preserves_earlier_logical_epoch_and_coalesces_exact_archive_bytes() {
    let f = Fixture::new();
    fs::write(
        &f.path,
        r#"{"version":1,"name":"Follow","glob":"logs/**","vault_root":true}"#,
    )
    .unwrap();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let a = f.vault.join("logs/a.log");
    fs::copy(&a, f.vault.join("logs/a.log.1")).unwrap();
    fs::write(&a, "x\n").unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    assert!(update.notice.unwrap().contains("coalesced"));
    assert_eq!(session.published.len(), 2);
    assert_eq!(session.aliases.len(), 1);
    assert!(
        session.retired[0].tail.is_none(),
        "truncated live inode must not also be read as its earlier epoch"
    );
    assert_eq!(session.published[&0].label, Some("before truncation"));
    assert_eq!(session.published[&0].snapshot.raw(0).unwrap(), b"first");
    assert_eq!(session.published[&1].snapshot.raw(0).unwrap(), b"x");
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
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(f.vault.join("logs/a.log"))
        .unwrap()
        .write_all(&bytes[b"first\n".len()..])
        .unwrap();
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

#[test]
fn follow_replacement_waits_for_real_query_and_time_membership_before_admission() {
    use std::io::Write;
    let f = Fixture::new();
    fs::write(&f.path, r#"{"version":1,"name":"Follow","glob":"logs/**","vault_root":true,"query":"message=keep","time":{"absolute":{"start":0,"end":2000000000}}}"#).unwrap();
    let a = f.vault.join("logs/a.log");
    fs::write(
        &a,
        "{\"time\":\"1970-01-01T00:00:01Z\",\"message\":\"keep\"}\n",
    )
    .unwrap();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    assert_eq!(
        initial.files.len(),
        1,
        "positive control: original meets both frozen criteria"
    );
    let mut session = Session::new(&initial).unwrap();
    fs::rename(&a, f.vault.join("logs/a.log.1")).unwrap();
    fs::write(
        &a,
        "{\"time\":\"1970-01-01T00:00:03Z\",\"message\":\"skip\"}\n",
    )
    .unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    assert!(update.notice.unwrap().contains("query/time"));
    assert_eq!(session.published.len(), 1);
    assert!(session.active[0].slot.is_none());
    fs::OpenOptions::new()
        .append(true)
        .open(&a)
        .unwrap()
        .write_all(b"{\"time\":\"1970-01-01T00:00:02Z\",\"message\":\"keep\"}\n")
        .unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(update.sources.len(), 1);
    assert_eq!(update.sources[0].slot, 1);
    assert_eq!(
        update.sources[0].snapshot.index().len(),
        2,
        "file-level membership admits the whole observed file"
    );
    assert!(session.active[0].slot.is_some());
}

#[test]
fn follow_changed_observed_prefix_refuses_publication_and_retains_history() {
    let f = Fixture::new();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let altered = f.vault.join("logs/altered.log");
    fs::write(&altered, "unrelated\n").unwrap();
    let snapshot = LogFile::open(&altered).unwrap().snapshot();
    let mut updates = Vec::new();
    assert!(session
        .stage(
            Some(0),
            &f.vault.join("logs/a.log"),
            snapshot,
            Some("before rotation"),
            &plan,
            &mut updates
        )
        .is_err());
    assert!(updates.is_empty());
    assert_eq!(session.published[&0].snapshot.raw(0).unwrap(), b"first");
}

#[test]
fn follow_history_bounds_are_terminal_without_dropping_accepted_epochs() {
    let f = Fixture::new();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let a = f.vault.join("logs/a.log");
    // This is the receiver's last successful receipt, not unpublished worker
    // state. The shared lease bound is allowed to stop before the 128-slot cap.
    let mut receipt = session.published.clone();
    let mut accepted = 0;
    let mut terminal = false;
    for epoch in 1..=MAX_FILES {
        fs::write(&a, format!("epoch {epoch:04}\n")).unwrap();
        match session.refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false)) {
            Ok(update) => {
                for source in update.sources {
                    receipt.insert(source.slot, source);
                }
                accepted += 1;
                assert_eq!(
                    receipt.len(),
                    accepted + 1,
                    "every accepted epoch remains represented"
                );
                assert!(receipt.len() <= MAX_FILES);
                assert_eq!(receipt[&0].snapshot.raw(0).unwrap(), b"first");
                for earlier in 1..=accepted {
                    assert_eq!(
                        receipt[&earlier].snapshot.raw(0).unwrap(),
                        format!("epoch {earlier:04}").as_bytes()
                    );
                }
            }
            Err(error) => {
                let message = error.to_string();
                let budget = error.is::<okilum_core::log::window_budget::BudgetBusy>();
                assert!(
                    budget
                        || message.contains("Follow memory bound")
                        || message.contains("retained segment limit")
                        || message.contains("rotation lifetime bound"),
                    "unexpected failure: {message}"
                );
                terminal = true;
                break;
            }
        }
    }
    assert!(
        accepted > 0,
        "positive control: real epochs must be admitted before testing retention at the bound"
    );
    assert!(
        terminal,
        "a finite shared-memory/segment bound must stop this run"
    );
    assert!(receipt.len() <= MAX_FILES);
    assert_eq!(receipt[&0].snapshot.raw(0).unwrap(), b"first");
}

#[test]
fn follow_byte_equivalent_compressed_rotation_is_an_explicit_archive_alias() {
    // The decoder reserves from the process-wide Log budget (#1428). Other
    // parallel tests deliberately exhaust it; exercise both outcomes in a
    // fresh process rather than interpreting their pressure as a bad archive.
    const CHILD: &str = "OKILUM_COMPRESSED_ALIAS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "reader_log::virtual_sources::follow::tests::follow_byte_equivalent_compressed_rotation_is_an_explicit_archive_alias",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "isolated alias test failed: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("compressed alias and pressure controls passed"));
        return;
    }
    let f = Fixture::new();
    fs::write(
        &f.path,
        r#"{"version":1,"name":"Follow","glob":"logs/**","vault_root":true}"#,
    )
    .unwrap();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let a = f.vault.join("logs/a.log");
    fs::rename(&a, f.vault.join("logs/a.log.1")).unwrap();
    fs::write(&a, "replacement\n").unwrap();
    session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    let archive = f.vault.join("logs/a.log.1.gz");
    fs::write(
        &archive,
        [
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 75, 203, 44, 42, 46, 225, 2, 0, 42, 179, 74, 199, 6,
            0, 0, 0,
        ],
    )
    .unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/a.log.1.gz"), &AtomicBool::new(false))
        .unwrap();
    assert!(update.notice.unwrap().contains("coalesced"));
    assert!(update.sources.is_empty());
    assert_eq!(session.published.len(), 2);
    assert_eq!(session.aliases.len(), 1);
    assert!(update
        .candidates
        .unwrap()
        .iter()
        .any(|c| c.path == archive && c.reason == Reason::DuplicateSource));
    // A byte-distinct archive cannot establish continuity by its name alone.
    let different = f.vault.join("logs/a.log.2.gz");
    // Valid gzip containing "second\n", with a different decoded prefix.
    fs::write(
        &different,
        [
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 43, 78, 77, 206, 207, 75, 225, 2, 0, 126, 192, 15,
            6, 7, 0, 0, 0,
        ],
    )
    .unwrap();
    assert_eq!(
        LogFile::open(&different).unwrap().raw(0).unwrap(),
        b"second"
    );
    // Consume the global pool with real retained snapshots. Membership must
    // fail closed under pressure, leaving the last accepted rows/aliases intact.
    let mut pressure = Vec::new();
    loop {
        match LogFile::open(&a) {
            Ok(file) => pressure.push(file),
            Err(error) => {
                assert!(
                    error.is::<okilum_core::log::window_budget::BudgetBusy>(),
                    "{error:#}"
                );
                break;
            }
        }
    }
    assert!(
        !pressure.is_empty(),
        "positive control: real reservations were held"
    );
    let error = session
        .refresh(&plan, &f.change("logs/a.log.2.gz"), &AtomicBool::new(false))
        .err()
        .expect("membership cannot decode under exhausted budget");
    assert!(error.to_string().contains("Membership refresh incomplete"));
    assert_eq!(session.published.len(), 2);
    assert_eq!(session.aliases.len(), 1);
    assert_eq!(session.published[&0].snapshot.raw(0).unwrap(), b"first");
    drop(pressure);
    let error = session
        .refresh(&plan, &f.change("logs/a.log.2.gz"), &AtomicBool::new(false))
        .err()
        .expect("byte-distinct compressed archive has unproven continuity");
    assert!(error
        .to_string()
        .contains("New compressed archive continuity is unproven"));
    assert_eq!(session.published.len(), 2);
    assert_eq!(session.aliases.len(), 1);
    // Successful ordinary membership remains the positive control after both
    // refusals; the ambiguous archive requires an explicit Rescan.
    fs::remove_file(&different).unwrap();
    fs::write(f.vault.join("logs/b.log"), "second\n").unwrap();
    let update = session
        .refresh(&plan, &f.change("logs/b.log"), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(update.sources.len(), 1);
    assert_eq!(update.sources[0].snapshot.raw(0).unwrap(), b"second");
    assert_eq!(session.published.len(), 3);
    assert_eq!(session.aliases.len(), 1);
    diagnostic_println!("compressed alias and pressure controls passed");
}

#[test]
fn follow_discovery_race_defers_replacement_to_its_existing_session_even_with_replayed_bytes() {
    let f = Fixture::new();
    fs::write(
        &f.path,
        r#"{"version":1,"name":"Follow","glob":"logs/**","vault_root":true}"#,
    )
    .unwrap();
    let plan = f.plan();
    let initial = plan.select(|_, _| {});
    let mut session = Session::new(&initial).unwrap();
    let a = f.vault.join("logs/a.log");
    fs::rename(&a, f.vault.join("logs/a.log.1")).unwrap();
    fs::write(&a, "second epoch\n").unwrap();
    session
        .refresh(&plan, &f.change("logs/a.log"), &AtomicBool::new(false))
        .unwrap();
    // Model creation between the tail-read phase and discovery, without a
    // timing-dependent thread or real watcher.
    fs::rename(&a, f.vault.join("logs/a.log.2")).unwrap();
    fs::write(&a, "first\n").unwrap();
    let mut discovered = plan.select_excluding(&session.known, |_, _| {});
    assert_eq!(discovered.files.len(), 1);
    let mut sources = Vec::new();
    let mut notices = Vec::new();
    assert!(session
        .discover(
            &discovered.files,
            &mut discovered.candidates,
            &mut sources,
            &mut notices
        )
        .unwrap());
    assert!(sources.is_empty() && session.aliases.is_empty());
    assert_eq!(session.active.len(), 1);
    let update = session
        .refresh(&plan, &[], &AtomicBool::new(false))
        .unwrap();
    assert_eq!(session.published.len(), 3);
    assert_eq!(session.active.len(), 1);
    assert_eq!(session.published[&2].snapshot.raw(0).unwrap(), b"first");
    assert!(update.notice.unwrap().contains("Rotated"));
    assert!(
        session.aliases.is_empty(),
        "a live replayed epoch is not an archive alias"
    );
}
