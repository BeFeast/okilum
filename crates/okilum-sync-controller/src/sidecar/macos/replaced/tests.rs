use super::*;
use std::{cell::Cell, collections::VecDeque, rc::Rc};

const UID: u32 = 501;
const CURRENT: FileId = FileId {
    device: 1,
    inode: 100,
};

fn mapping(inode: u64) -> Mapping {
    Mapping {
        file: FileId { device: 1, inode },
    }
}

#[test]
fn only_a_complete_map_without_the_bundle_file_is_another_image() {
    let system = mapping(7);
    assert_eq!(
        classify(&[system.clone(), mapping(CURRENT.inode)], true, CURRENT),
        Image::Current
    );
    assert_eq!(
        classify(&[mapping(CURRENT.inode)], false, CURRENT),
        Image::Current,
        "mapping the bundle file is positive evidence even mid-walk"
    );
    assert_eq!(
        classify(&[system.clone(), mapping(42)], true, CURRENT),
        Image::Other
    );
    assert!(
        matches!(
            classify(&[system, mapping(42)], false, CURRENT),
            Image::Unknown(_)
        ),
        "an incomplete walk never proves another image"
    );
}

#[test]
fn snapshot_round_trips_and_rejects_unknown_fields() -> Result<()> {
    let snapshot = Snapshot {
        executable: "/Applications/Okilum.app/Contents/MacOS/okilum-sync-supervisor".into(),
        helper: helper(43464, 1_700_000_000),
    };
    let text = serde_json::to_string(&snapshot)?;
    assert_eq!(serde_json::from_str::<Snapshot>(&text)?, snapshot);
    let extra = text.replacen('{', r#"{"pid_only":1,"#, 1);
    assert!(serde_json::from_str::<Snapshot>(&extra).is_err());
    Ok(())
}

/// A scripted launchd and process table. Every effect is recorded.
struct Fake {
    launchd: VecDeque<Option<u32>>,
    /// The helper PID's process, per observation (until SIGTERM).
    helper: VecDeque<Option<Process>>,
    /// After SIGTERM the helper answers with this (None: it exited).
    after_term: Option<Process>,
    images: VecDeque<Image>,
    /// The helper's runtime child and how many polls it stays alive after the
    /// graceful stop request (None: it never finishes within the test).
    runtime: Vec<Process>,
    runtime_polls: Option<usize>,
    /// A child that appears once the stop was requested.
    late_child: Option<Process>,
    /// Set by `stop_runtime`, shared with the closure the test passes.
    stop_requested: Rc<Cell<bool>>,
    events: Vec<String>,
    kick: Kick,
}
impl Fake {
    fn new(old: Process) -> Self {
        Self {
            launchd: [Some(old.pid)].into(),
            helper: [Some(old)].into(),
            after_term: None,
            images: [Image::Other].into(),
            runtime: vec![],
            runtime_polls: Some(0),
            late_child: None,
            stop_requested: Rc::new(Cell::new(false)),
            events: vec![],
            kick: Kick::Started,
        }
    }
    fn signalled(&self) -> bool {
        self.events.iter().any(|event| event.starts_with("TERM"))
    }
    fn next<T: Clone>(queue: &mut VecDeque<T>) -> T {
        if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            queue.front().expect("script").clone()
        }
    }
    fn runtime_alive(&self) -> bool {
        !self.runtime.is_empty()
            && (!self.stop_requested.get() || self.runtime_polls.is_none_or(|polls| polls > 0))
    }
}
impl HelperOs for Fake {
    fn launchd_pid(&mut self) -> Result<Option<u32>> {
        Ok(Self::next(&mut self.launchd))
    }
    fn process(&mut self, pid: u32) -> Result<Option<Process>> {
        if let Some(child) = self.runtime.iter().find(|child| child.pid == pid).copied() {
            let alive = self.runtime_alive();
            if self.stop_requested.get() {
                if let Some(polls) = self.runtime_polls.as_mut() {
                    *polls = polls.saturating_sub(1);
                }
            }
            return Ok(alive.then_some(child));
        }
        if self.signalled() {
            return Ok(self.after_term.filter(|process| process.pid == pid));
        }
        Ok(Self::next(&mut self.helper).filter(|process| process.pid == pid))
    }
    fn children(&mut self, _: u32) -> Result<Vec<Process>> {
        let mut children = if self.runtime_alive() {
            self.runtime.clone()
        } else {
            vec![]
        };
        if self.stop_requested.get() {
            children.extend(self.late_child);
        }
        Ok(children)
    }
    fn image(&mut self, _: u32) -> Result<Image> {
        Ok(Self::next(&mut self.images))
    }
    fn terminate(&mut self, pid: u32) -> Result<()> {
        assert!(
            !self.runtime_alive(),
            "SIGTERM while the runtime still runs would let the helper SIGKILL it"
        );
        self.events.push(format!("TERM {pid}"));
        Ok(())
    }
    fn kickstart(&mut self) -> Kick {
        self.events.push("kickstart".into());
        self.kick.clone()
    }
    fn sleep(&mut self, _: Duration) {}
}

fn helper(pid: u32, started: u64) -> Process {
    Process {
        pid,
        uid: UID,
        started: (started, 0),
    }
}

/// `restart_captured` whose `stop_runtime` makes the fake runtime start its
/// graceful shutdown, recorded in the event list.
fn restart(os: &mut Fake, old: Process, waits: Waits) -> Result<Outcome> {
    let stop = os.stop_requested.clone();
    let requested = Rc::new(Cell::new(false));
    let seen = requested.clone();
    let outcome = restart_captured(
        os,
        UID,
        old,
        waits,
        || Ok(()),
        move |_| {
            stop.set(true);
            seen.set(true);
            Ok(())
        },
    );
    if requested.get() {
        os.events.insert(0, "stop runtime".into());
    }
    outcome
}

#[test]
fn capture_takes_only_launchds_helper_running_this_bundles_file() -> Result<()> {
    let old = helper(43464, 1_700_000_000);
    let mut os = Fake::new(old);
    os.images = [Image::Current].into();
    assert_eq!(capture(&mut os, UID)?, Some(old));
    // Another bundle's intact helper, or an unproven image: never captured.
    for image in [Image::Other, Image::Unknown("walk incomplete".into())] {
        let mut os = Fake::new(old);
        os.images = [image].into();
        assert_eq!(capture(&mut os, UID)?, None);
    }
    // Identity or launchd job changed while the image was read.
    let mut os = Fake::new(old);
    os.images = [Image::Current].into();
    os.helper = [Some(old), Some(helper(43464, 1_700_000_001))].into();
    assert_eq!(capture(&mut os, UID)?, None);
    let mut os = Fake::new(old);
    os.images = [Image::Current].into();
    os.launchd = [Some(old.pid), Some(1)].into();
    assert_eq!(capture(&mut os, UID)?, None);
    let mut os = Fake::new(old);
    os.launchd = [None].into();
    assert_eq!(capture(&mut os, UID)?, None);
    let mut foreign = old;
    foreign.uid = 0;
    let mut os = Fake::new(foreign);
    os.images = [Image::Current].into();
    assert!(capture(&mut os, UID).is_err());
    assert!(os.events.is_empty());
    Ok(())
}

#[test]
fn captured_helper_gets_one_sigterm_after_its_runtime_exited_then_a_kickstart() -> Result<()> {
    let old = helper(43464, 1_700_000_000);
    let mut os = Fake::new(old);
    os.runtime = vec![helper(43470, 1_700_000_001)];
    os.runtime_polls = Some(3);
    let outcome = restart(&mut os, old, Waits::default())?;
    assert_eq!(
        outcome,
        Outcome::Restarted {
            old,
            signalled: true,
            kick: Kick::Started
        }
    );
    assert_eq!(os.events, ["stop runtime", "TERM 43464", "kickstart"]);
    assert!(outcome.settled());
    // Without a runtime child there is nothing to stop first.
    let mut os = Fake::new(old);
    restart(&mut os, old, Waits::default())?;
    assert_eq!(os.events, ["TERM 43464", "kickstart"]);
    Ok(())
}

#[test]
fn prepare_runs_before_the_runtime_stop_and_feeds_it() -> Result<()> {
    let old = helper(10, 5);
    let mut os = Fake::new(old);
    os.runtime = vec![helper(11, 6)];
    let stop = os.stop_requested.clone();
    let order = std::cell::RefCell::new(vec![]);
    restart_captured(
        &mut os,
        UID,
        old,
        Waits::default(),
        || {
            order.borrow_mut().push("prepare");
            Ok(7)
        },
        |prepared: &i32| {
            assert_eq!(*prepared, 7);
            order.borrow_mut().push("stop runtime");
            stop.set(true);
            Ok(())
        },
    )?;
    assert_eq!(*order.borrow(), ["prepare", "stop runtime"]);
    Ok(())
}

#[test]
fn a_runtime_still_shutting_down_is_never_interrupted() -> Result<()> {
    let old = helper(10, 5);
    let mut os = Fake::new(old);
    os.runtime = vec![helper(11, 6)];
    os.runtime_polls = None;
    let outcome = restart(
        &mut os,
        old,
        Waits {
            runtime: Duration::from_millis(1),
            exit: EXIT_WAIT,
        },
    )?;
    assert_eq!(outcome, Outcome::RuntimeRunning { old });
    assert!(!outcome.settled(), "the next launch retries");
    assert_eq!(os.events, ["stop runtime"], "no signal, no kickstart");
    Ok(())
}

#[test]
fn stop_or_prepare_failure_sends_no_signal() {
    let old = helper(10, 5);
    let mut os = Fake::new(old);
    os.runtime = vec![helper(11, 6)];
    let error = restart_captured(
        &mut os,
        UID,
        old,
        Waits::default(),
        || Ok(()),
        |_| anyhow::bail!("REST shutdown refused"),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("REST shutdown refused"));
    assert!(os.events.is_empty());
    let mut os = Fake::new(old);
    let error = restart_captured(
        &mut os,
        UID,
        old,
        Waits::default(),
        || -> Result<()> { anyhow::bail!("bundle verification failed") },
        |_| unreachable!(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("bundle verification failed"));
    assert!(os.events.is_empty());
}

#[test]
fn an_unreplaced_gone_or_unproven_helper_is_never_touched() -> Result<()> {
    let old = helper(10, 5);
    for (image, expected) in [
        (Image::Current, Outcome::Current),
        (
            Image::Unknown("walk incomplete".into()),
            Outcome::Unknown("walk incomplete".into()),
        ),
    ] {
        let mut os = Fake::new(old);
        os.images = [image].into();
        let outcome = restart_captured(
            &mut os,
            UID,
            old,
            Waits::default(),
            || -> Result<()> { anyhow::bail!("prepare must not run") },
            |_| unreachable!(),
        )?;
        assert_eq!(outcome, expected);
        assert!(os.events.is_empty());
    }
    // The captured PID now names another process (new start time): gone.
    let mut os = Fake::new(old);
    os.helper = [Some(helper(10, 6))].into();
    assert_eq!(restart(&mut os, old, Waits::default())?, Outcome::Gone);
    assert!(os.events.is_empty());
    let mut os = Fake::new(old);
    os.helper = [None].into();
    assert_eq!(restart(&mut os, old, Waits::default())?, Outcome::Gone);
    let mut foreign = old;
    foreign.uid = 0;
    assert!(restart(&mut Fake::new(foreign), foreign, Waits::default()).is_err());
    Ok(())
}

#[test]
fn a_newer_helper_a_reused_pid_or_a_new_child_is_never_signalled() -> Result<()> {
    let old = helper(10, 5);
    // Exited after its runtime stopped; launchd already runs a new helper.
    let mut os = Fake::new(old);
    os.helper = [Some(old), Some(old), None].into();
    os.launchd = [Some(11)].into();
    assert_eq!(
        restart(&mut os, old, Waits::default())?,
        Outcome::Restarted {
            old,
            signalled: false,
            kick: Kick::Started
        }
    );
    assert!(!os.signalled());
    // The PID was reused by an unrelated process just before the signal.
    let mut os = Fake::new(old);
    os.helper = [Some(old), Some(old), Some(helper(10, 6))].into();
    assert!(matches!(
        restart(&mut os, old, Waits::default())?,
        Outcome::Restarted {
            signalled: false,
            ..
        }
    ));
    assert!(!os.signalled());
    // Still alive, but launchd's job moved to another PID.
    let mut os = Fake::new(old);
    os.launchd = [Some(11)].into();
    assert!(matches!(
        restart(&mut os, old, Waits::default())?,
        Outcome::Unknown(_)
    ));
    assert!(os.events.is_empty());
    // The second image read maps the bundle file again.
    let mut os = Fake::new(old);
    os.images = [Image::Other, Image::Current].into();
    assert!(matches!(
        restart(&mut os, old, Waits::default())?,
        Outcome::Unknown(_)
    ));
    assert!(os.events.is_empty());
    // A new child appeared after the runtime exited: not signalled.
    let mut os = Fake::new(old);
    os.runtime = vec![helper(11, 6)];
    os.late_child = Some(helper(12, 7));
    assert!(matches!(
        restart(&mut os, old, Waits::default())?,
        Outcome::Unknown(_)
    ));
    assert_eq!(os.events, ["stop runtime"]);
    Ok(())
}

#[test]
fn a_helper_that_outlives_sigterm_is_left_running_without_sigkill() -> Result<()> {
    let old = helper(10, 5);
    let mut os = Fake::new(old);
    os.after_term = Some(old);
    let outcome = restart(
        &mut os,
        old,
        Waits {
            runtime: RUNTIME_WAIT,
            exit: Duration::from_millis(1),
        },
    )?;
    assert_eq!(outcome, Outcome::LeftRunning { old });
    assert!(!outcome.settled());
    assert_eq!(os.events, ["TERM 10"], "one SIGTERM, no kill, no kickstart");
    Ok(())
}

#[test]
fn kickstart_failure_is_reported_not_fatal() -> Result<()> {
    let old = helper(10, 5);
    let mut os = Fake::new(old);
    os.kick = Kick::Failed("exit status: 113: Could not find service".into());
    assert_eq!(
        restart(&mut os, old, Waits::default())?,
        Outcome::Restarted {
            old,
            signalled: true,
            kick: Kick::Failed("exit status: 113: Could not find service".into())
        }
    );
    Ok(())
}

#[cfg(target_os = "macos")]
mod native_tests {
    use super::super::native;
    use super::*;
    use anyhow::Context as _;
    use std::{
        fs,
        os::unix::process::ExitStatusExt,
        path::{Path, PathBuf},
        process::{Child, Command},
        time::Instant,
    };

    fn uid() -> u32 {
        unsafe { libc::geteuid() }
    }

    /// A program copied into a fake bundle under the helper's name.
    struct Helper {
        root: tempfile::TempDir,
        path: PathBuf,
        child: Option<Child>,
    }
    impl Helper {
        fn start() -> Result<Self> {
            Self::spawn("/bin/sleep", &["60"])
        }
        fn spawn(program: &str, args: &[&str]) -> Result<Self> {
            let root = tempfile::tempdir()?;
            let path = root.path().join(HELPER_NAME);
            fs::copy(program, &path)?;
            let child = Command::new(&path).args(args).spawn()?;
            Ok(Self {
                root,
                path,
                child: Some(child),
            })
        }
        fn pid(&self) -> u32 {
            self.child.as_ref().unwrap().id()
        }
        /// What an app update does to the helper: a new file at the same path.
        fn replace(&self, keep_old_as: Option<&Path>) -> Result<()> {
            match keep_old_as {
                Some(aside) => fs::rename(&self.path, aside)?,
                None => fs::remove_file(&self.path)?,
            }
            fs::copy("/bin/sleep", &self.path)?;
            Ok(())
        }
        fn running(&mut self) -> Result<bool> {
            Ok(self.child.as_mut().unwrap().try_wait()?.is_none())
        }
    }
    impl Drop for Helper {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// The native adapter with launchd replaced by one of the test's children.
    struct TestLaunchd {
        pid: u32,
        current: PathBuf,
    }
    impl HelperOs for TestLaunchd {
        fn launchd_pid(&mut self) -> Result<Option<u32>> {
            Ok(Some(self.pid))
        }
        fn process(&mut self, pid: u32) -> Result<Option<Process>> {
            native::process(pid)
        }
        fn children(&mut self, pid: u32) -> Result<Vec<Process>> {
            native::children(pid)
        }
        fn image(&mut self, pid: u32) -> Result<Image> {
            native::image(pid, &self.current)
        }
        fn terminate(&mut self, pid: u32) -> Result<()> {
            native::terminate(pid)
        }
        fn kickstart(&mut self) -> Kick {
            Kick::Started
        }
        fn sleep(&mut self, duration: Duration) {
            std::thread::sleep(duration)
        }
    }

    #[test]
    fn a_running_helper_maps_its_file_until_an_update_replaces_it() -> Result<()> {
        // Deleted old bundle (Sparkle): the running image is no longer the file.
        let helper = Helper::start()?;
        assert_eq!(native::image(helper.pid(), &helper.path)?, Image::Current);
        helper.replace(None)?;
        assert_eq!(native::image(helper.pid(), &helper.path)?, Image::Other);
        // Old bundle moved aside.
        let helper = Helper::start()?;
        let aside = helper.root.path().join("previous");
        fs::create_dir(&aside)?;
        helper.replace(Some(&aside.join(HELPER_NAME)))?;
        assert_eq!(native::image(helper.pid(), &helper.path)?, Image::Other);
        Ok(())
    }

    #[test]
    fn another_bundles_helper_is_never_captured_or_signalled() -> Result<()> {
        // launchd runs bundle B's intact helper; this app is bundle A.
        let mut ours = Helper::start()?;
        let mut theirs = Helper::start()?;
        let mut os = TestLaunchd {
            pid: theirs.pid(),
            current: ours.path.clone(),
        };
        assert_eq!(capture(&mut os, uid())?, None, "B is not A's helper");
        // Positive control: A's own helper is captured.
        let mut os = TestLaunchd {
            pid: ours.pid(),
            current: ours.path.clone(),
        };
        let captured = capture(&mut os, uid())?.context("own helper not captured")?;
        assert_eq!(captured.pid, ours.pid());
        // After A is replaced, only A's captured identity is restarted.
        ours.replace(None)?;
        let outcome = restart_captured(
            &mut os,
            uid(),
            captured,
            Waits::default(),
            || Ok(()),
            |_| Ok(()),
        )?;
        assert!(
            matches!(
                outcome,
                Outcome::Restarted {
                    signalled: true,
                    ..
                }
            ),
            "{outcome:?}"
        );
        let status = ours.child.take().unwrap().wait()?;
        assert_eq!(
            status.signal(),
            Some(libc::SIGTERM),
            "SIGTERM, never SIGKILL"
        );
        assert!(theirs.running()?, "the other bundle's helper keeps running");
        Ok(())
    }

    #[test]
    fn process_identity_has_owner_and_start_time_and_ends_with_exit() -> Result<()> {
        let mut helper = Helper::start()?;
        let process = native::process(helper.pid())?.context("helper not observed")?;
        assert_eq!(process.uid, uid());
        assert!(process.started.0 > 0);
        assert_eq!(
            native::process(helper.pid())?,
            Some(process),
            "stable identity"
        );
        let mut child = helper.child.take().unwrap();
        let killed = child.kill();
        // Exited but unreaped: a zombie is no longer the helper.
        std::thread::sleep(Duration::from_millis(200));
        let zombie = native::process(child.id());
        child.wait()?;
        killed?;
        assert_eq!(zombie?, None);
        assert_eq!(native::process(child.id())?, None);
        Ok(())
    }

    /// A helper (a copy of bash) whose runtime child needs `delay` seconds to
    /// shut down gracefully after the stop request (SIGUSR1) and then records
    /// that it finished. The helper then idles without children until SIGTERM.
    struct SlowRuntime {
        helper: Helper,
        marker: PathBuf,
        _scratch: tempfile::TempDir,
    }
    impl SlowRuntime {
        fn start(delay: u32) -> Result<Self> {
            let scratch = tempfile::tempdir()?;
            let marker = scratch.path().join("graceful");
            let fifo = scratch.path().join("idle");
            ensure!(
                Command::new("/usr/bin/mkfifo")
                    .arg(&fifo)
                    .status()?
                    .success(),
                "mkfifo failed"
            );
            let runtime = format!(
                "trap 'sleep {delay}; echo graceful > \"{}\"; exit 0' USR1; while :; do sleep 0.1; done",
                marker.display()
            );
            let script = format!(
                "/bin/bash -c '{}' & wait; read -r _ < \"{}\"",
                runtime.replace('\'', r"'\''"),
                fifo.display()
            );
            let helper = Helper::spawn("/bin/bash", &["-c", &script])?;
            Ok(Self {
                helper,
                marker,
                _scratch: scratch,
            })
        }
        fn runtime(&self) -> Result<Process> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let [runtime] = native::children(self.helper.pid())?.as_slice() {
                    return Ok(*runtime);
                }
                ensure!(Instant::now() < deadline, "runtime child did not start");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        fn request_stop(runtime: Process) -> Result<()> {
            ensure!(
                unsafe { libc::kill(runtime.pid as i32, libc::SIGUSR1) } == 0,
                "stop request failed"
            );
            Ok(())
        }
    }

    #[test]
    fn a_slow_graceful_runtime_shutdown_completes_before_the_helper_gets_sigterm() -> Result<()> {
        // Longer than the helper's own 5 s cleanup grace before SIGKILL.
        let mut slow = SlowRuntime::start(7)?;
        let runtime = slow.runtime()?;
        let mut os = TestLaunchd {
            pid: slow.helper.pid(),
            current: slow.helper.path.clone(),
        };
        let captured = capture(&mut os, uid())?.context("helper not captured")?;
        slow.helper.replace(None)?;
        let started = Instant::now();
        let outcome = restart_captured(
            &mut os,
            uid(),
            captured,
            Waits {
                runtime: Duration::from_secs(30),
                exit: EXIT_WAIT,
            },
            || Ok(()),
            |_| SlowRuntime::request_stop(runtime),
        )?;
        assert!(
            started.elapsed() >= Duration::from_secs(7),
            "waited for the runtime"
        );
        assert!(
            matches!(
                outcome,
                Outcome::Restarted {
                    signalled: true,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert_eq!(
            fs::read_to_string(&slow.marker)?.trim(),
            "graceful",
            "the runtime finished its own shutdown"
        );
        let status = slow.helper.child.take().unwrap().wait()?;
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        Ok(())
    }

    #[test]
    fn a_runtime_still_shutting_down_after_the_wait_postpones_the_restart() -> Result<()> {
        let mut slow = SlowRuntime::start(7)?;
        let runtime = slow.runtime()?;
        let mut os = TestLaunchd {
            pid: slow.helper.pid(),
            current: slow.helper.path.clone(),
        };
        let captured = capture(&mut os, uid())?.context("helper not captured")?;
        slow.helper.replace(None)?;
        let outcome = restart_captured(
            &mut os,
            uid(),
            captured,
            Waits {
                runtime: Duration::from_secs(2),
                exit: EXIT_WAIT,
            },
            || Ok(()),
            |_| SlowRuntime::request_stop(runtime),
        )?;
        assert_eq!(outcome, Outcome::RuntimeRunning { old: captured });
        assert!(slow.helper.running()?, "helper not signalled");
        // The runtime still completes its graceful shutdown by itself.
        let deadline = Instant::now() + Duration::from_secs(15);
        while !slow.marker.exists() {
            ensure!(
                Instant::now() < deadline,
                "runtime shutdown was interrupted"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(slow.helper.running()?, "helper still not signalled");
        Ok(())
    }

    #[test]
    fn launchctl_style_request_reports_success_failure_and_hang() {
        assert_eq!(
            native::run_bounded(&mut Command::new("/usr/bin/true"), Duration::from_secs(3)),
            Kick::Started
        );
        let failed = native::run_bounded(
            Command::new("/bin/sh").args(["-c", "echo no such service >&2; exit 113"]),
            Duration::from_secs(3),
        );
        assert!(
            matches!(&failed, Kick::Failed(message) if message.contains("113") && message.contains("no such service")),
            "{failed:?}"
        );
        assert_eq!(
            native::run_bounded(
                Command::new("/bin/sleep").arg("60"),
                Duration::from_millis(100)
            ),
            Kick::TimedOut
        );
    }
}
