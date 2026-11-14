use super::super::draft::Recovery;
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    dir: tempfile::TempDir,
    source: Source,
    attempt: Attempt,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "base\n").unwrap();
        fs::set_permissions(
            dir.path().join("note.md"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let source =
            Source::parse(&format!("ssh://offline{}/note.md", dir.path().display())).unwrap();
        Self {
            dir,
            source,
            attempt: Attempt::new(Revision::of("base\n"), "my draft\n".into()),
        }
    }
    fn run(&self, program: &str) -> io::Result<Outcome> {
        let mut command = Command::new("python3");
        command.args(["-I", "-c", program]);
        run(command, &self.source, &self.attempt, || false)
    }
    fn text(&self) -> String {
        fs::read_to_string(self.dir.path().join("note.md")).unwrap()
    }
    fn archives(&self) -> Vec<String> {
        fs::read_dir(self.dir.path().join(".okilum-remote-history"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .map(|p| fs::read_to_string(p).unwrap())
            .collect()
    }
    fn no_temporary(&self) {
        assert!(fs::read_dir(self.dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".okilum-remote-save-")));
        assert!(fs::read_dir(self.dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".okilum-save-probe-")));
    }
}
fn injected(marker: &str, code: &str) -> String {
    assert_eq!(PROGRAM.matches(marker).count(), 1);
    PROGRAM.replace(marker, code)
}
const OUTSIDE: &str = "fd = os.open(name, os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW, dir_fd=directory); os.write(fd, b'outside\\n'); os.fsync(fd); os.close(fd)";

#[test]
fn safe1610_success_preserves_mode_preimage_and_leaves_no_temporary() {
    let f = Fixture::new();
    assert!(matches!(
        f.run(PROGRAM).unwrap(),
        Outcome::Saved {
            already_present: false,
            ..
        }
    ));
    assert_eq!(f.text(), "my draft\n");
    assert_eq!(
        fs::metadata(f.dir.path().join("note.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert!(f.archives().iter().all(|s| s == "base\n"));
    assert!(!f.archives().is_empty());
    f.no_temporary();
}
#[test]
fn safe1610_remote_edit_before_save_never_overwrites() {
    let f = Fixture::new();
    fs::write(f.dir.path().join("note.md"), "outside\n").unwrap();
    assert!(matches!(
        f.run(PROGRAM).unwrap(),
        Outcome::Conflict {
            rollback_race: false,
            ..
        }
    ));
    assert_eq!(f.text(), "outside\n");
    f.no_temporary();
}
#[test]
fn safe1610_remote_edit_between_check_and_exchange_is_reversed() {
    let f = Fixture::new();
    let answer = f.run(&injected("# test:before_exchange", OUTSIDE)).unwrap();
    assert!(
        matches!(answer, Outcome::Conflict { rollback_race: false, remote: Some(Snapshot { text, .. }), .. } if text == "outside\n")
    );
    assert_eq!(f.text(), "outside\n");
    assert!(f.archives().contains(&"outside\n".into()));
    f.no_temporary();
}
#[test]
fn safe1610_rollback_race_preserves_both_remote_versions() {
    let f = Fixture::new();
    let program = injected("# test:before_exchange", OUTSIDE).replace(
        "# test:before_rollback",
        &OUTSIDE.replace("outside", "second"),
    );
    assert!(
        matches!(f.run(&program).unwrap(), Outcome::Conflict { rollback_race: true, raced: Some(Snapshot { text, .. }), .. } if text == "second\n")
    );
    assert_eq!(f.text(), "outside\n");
    let kept = f.archives();
    assert!(kept.contains(&"outside\n".into()) && kept.contains(&"second\n".into()));
    // A subsequent explicit save still works: recovery copies must not hard-link the source.
    let next = Attempt::new(Revision::of("outside\n"), "resolved\n".into());
    let mut command = Command::new("python3");
    command.args(["-I", "-c", PROGRAM]);
    assert!(matches!(
        run(command, &f.source, &next, || false).unwrap(),
        Outcome::Saved { .. }
    ));
}
#[test]
fn safe1610_drop_before_staging_retry_is_guarded() {
    let f = Fixture::new();
    assert!(f
        .run(&injected("# test:before_staging", "os._exit(91)"))
        .is_err());
    assert_eq!(f.text(), "base\n");
    assert!(matches!(
        f.run(PROGRAM).unwrap(),
        Outcome::Saved {
            already_present: false,
            ..
        }
    ));
    f.no_temporary();
}
#[test]
fn safe1610_drop_after_commit_retry_is_idempotent() {
    let f = Fixture::new();
    assert!(f
        .run(&injected("# test:after_commit", "os._exit(91)"))
        .is_err());
    assert_eq!(f.text(), "my draft\n");
    let kept = f.archives();
    assert!(matches!(
        f.run(PROGRAM).unwrap(),
        Outcome::Saved {
            already_present: true,
            ..
        }
    ));
    assert_eq!(f.archives(), kept);
    f.no_temporary();
}
#[test]
fn safe1610_drop_after_exchange_must_resume_preimage_verification() {
    let f = Fixture::new();
    let program = injected("# test:before_exchange", OUTSIDE)
        .replace("# test:after_exchange", "os._exit(91)");
    assert!(f.run(&program).is_err());
    assert_eq!(f.text(), "my draft\n");
    assert!(matches!(f.run(PROGRAM).unwrap(), Outcome::Conflict { .. }));
    assert_eq!(f.text(), "outside\n");
    assert!(f.archives().contains(&"outside\n".into()));
}
#[test]
fn safe1610_unsupported_exchange_keeps_readonly_without_fallback() {
    let f = Fixture::new();
    let program = PROGRAM.replace(
        "rename = getattr(libc, \"renameat2\", None)",
        "rename = None",
    );
    assert!(
        matches!(f.run(&program).unwrap(),Outcome::Unsupported{message} if message==UNSUPPORTED)
    );
    assert_eq!(f.text(), "base\n");
    f.no_temporary();
}
#[test]
fn safe1610_sftp_only_and_missing_python_send_no_draft() {
    let f = Fixture::new();
    assert!(matches!(
        f.run("import sys;print('This service allows sftp connections only.');sys.stdout.flush();sys.exit(1)")
            .unwrap(),
        Outcome::Unsupported { .. }
    ));
    let mut command = Command::new("python3");
    command.args([
        "-I",
        "-c",
        "import sys;sys.stderr.write('python3: not found');sys.exit(127)",
    ]);
    assert!(matches!(
        run(command, &f.source, &f.attempt, || false).unwrap(),
        Outcome::Unsupported { .. }
    ));
    assert_eq!(f.text(), "base\n");
}
#[test]
fn safe1610_fixed_command_never_interpolates_path_content_or_revision() {
    let source = Source::parse("ssh://offline/notes/%24%28touch%20owned%29%20%27.md").unwrap();
    let attempt = Attempt::new(Revision::of("base"), "$(touch owned) ' ; draft".into());
    let clock = std::sync::Arc::new(crate::ssh_askpass::Clock::default());
    let (command, _askpass) = source
        .target
        .guarded_markdown_save_command(COMMAND.as_str(), &clock);
    let args = command
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(args.last(), Some(&COMMAND.as_str()));
    assert!(COMMAND.starts_with("python3 -I -c '"));
    assert!(!COMMAND.contains(&attempt.text));
    let payload: serde_json::Value =
        serde_json::from_slice(&request(&source, &attempt).unwrap()).unwrap();
    assert_eq!(
        STANDARD
            .decode(payload["path_b64"].as_str().unwrap())
            .unwrap(),
        b"/notes/$(touch owned) '.md"
    );
}
#[test]
fn safe1610_durable_restart_pending_identity_and_stale_writer_refusal() {
    let f = Fixture::new();
    let local = tempfile::tempdir().unwrap();
    let (recovery, mut record) = Recovery::open(local.path(), &f.source, "base\n".into()).unwrap();
    record.text = "my draft\n".into();
    let attempt = record.attempt();
    recovery.persist(2, &record).unwrap();
    assert!(!recovery
        .persist(
            1,
            &super::super::draft::Record {
                text: "stale".into(),
                ..record.clone()
            }
        )
        .unwrap());
    assert!(Recovery::open(local.path(), &f.source, "base\n".into()).is_err());
    drop(recovery);
    let (_, reopened) = Recovery::recover(local.path(), &f.source).unwrap();
    assert_eq!(reopened.text, "my draft\n");
    assert_eq!(reopened.base, "base\n");
    assert_eq!(reopened.pending.unwrap().id, attempt.id);
}
