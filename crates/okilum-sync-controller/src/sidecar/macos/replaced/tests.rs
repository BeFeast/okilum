use super::*;
use std::collections::VecDeque;

const UID: u32 = 501;
const CURRENT: FileId = FileId {
    device: 1,
    inode: 100,
};
const OLD: FileId = FileId {
    device: 1,
    inode: 42,
};

fn mapping(file: FileId, unlinked: bool, name: &str) -> Mapping {
    Mapping {
        file,
        unlinked,
        name: name.into(),
    }
}

#[test]
fn only_a_complete_map_without_the_current_file_and_with_an_old_helper_is_replaced() {
    let system = mapping(
        FileId {
            device: 1,
            inode: 7,
        },
        false,
        "dyld",
    );
    let current = mapping(CURRENT, false, HELPER_NAME);
    assert_eq!(
        classify(&[system.clone(), current.clone()], true, CURRENT),
        Image::Current
    );
    assert_eq!(
        classify(&[current, mapping(OLD, true, "")], false, CURRENT),
        Image::Current,
        "mapping the current file is positive evidence even mid-walk"
    );
    // Sparkle deletes the old bundle: the old image is unlinked and unnamed.
    let deleted = mapping(OLD, true, "");
    assert_eq!(
        classify(&[system.clone(), deleted.clone()], true, CURRENT),
        Image::Replaced
    );
    // A bundle moved aside keeps its name but is not the current file.
    let moved = mapping(OLD, false, HELPER_NAME);
    assert_eq!(
        classify(&[system.clone(), moved], true, CURRENT),
        Image::Replaced
    );
    assert!(
        matches!(
            classify(&[system.clone(), deleted], false, CURRENT),
            Image::Unknown(_)
        ),
        "an incomplete walk never proves replacement"
    );
    assert!(
        matches!(classify(&[system], true, CURRENT), Image::Unknown(_)),
        "an unrelated image is not a replaced helper"
    );
    assert!(matches!(classify(&[], true, CURRENT), Image::Unknown(_)));
}

/// A scripted launchd and process table. Every effect is recorded.
struct Fake {
    launchd: VecDeque<Option<u32>>,
    /// The process each PID resolves to, per observation.
    processes: VecDeque<Option<Process>>,
    /// Image answers, per observation.
    images: VecDeque<Image>,
    /// After SIGTERM the process answers with this (None: it exited).
    after_term: Option<Option<Process>>,
    events: Vec<String>,
    kick: Kick,
}
impl Fake {
    fn new() -> Self {
        Self {
            launchd: VecDeque::new(),
            processes: VecDeque::new(),
            images: VecDeque::new(),
            after_term: None,
            events: vec![],
            kick: Kick::Started,
        }
    }
    fn signals(&self) -> Vec<&String> {
        self.events
            .iter()
            .filter(|event| event.starts_with("TERM"))
            .collect()
    }
}
impl HelperOs for Fake {
    fn launchd_pid(&mut self) -> Result<Option<u32>> {
        let answer = if self.launchd.len() > 1 {
            self.launchd.pop_front().unwrap()
        } else {
            *self.launchd.front().expect("launchd script")
        };
        Ok(answer)
    }
    fn process(&mut self, pid: u32) -> Result<Option<Process>> {
        if let Some(after) = self.after_term.filter(|_| !self.signals().is_empty()) {
            return Ok(after);
        }
        let answer = if self.processes.len() > 1 {
            self.processes.pop_front().unwrap()
        } else {
            *self.processes.front().expect("process script")
        };
        Ok(answer.filter(|process| process.pid == pid))
    }
    fn image(&mut self, _: u32) -> Result<Image> {
        Ok(if self.images.len() > 1 {
            self.images.pop_front().unwrap()
        } else {
            self.images.front().expect("image script").clone()
        })
    }
    fn terminate(&mut self, pid: u32) -> Result<()> {
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

#[test]
fn replaced_helper_gets_one_sigterm_after_prepare_then_a_kickstart() -> Result<()> {
    let old = helper(43464, 1_700_000_000);
    let mut os = Fake::new();
    os.launchd = [Some(old.pid)].into();
    os.processes = [Some(old)].into();
    os.images = [Image::Replaced].into();
    os.after_term = Some(None);
    let mut prepared = 0;
    let outcome = restart_replaced(&mut os, UID, EXIT_WAIT, || {
        prepared += 1;
        Ok(())
    })?;
    assert_eq!(prepared, 1);
    assert_eq!(
        outcome,
        Outcome::Restarted {
            old,
            signalled: true,
            kick: Kick::Started
        }
    );
    assert_eq!(os.events, ["TERM 43464", "kickstart"]);
    Ok(())
}

#[test]
fn a_helper_on_the_current_image_or_an_unproven_one_is_never_touched() -> Result<()> {
    for image in [Image::Current, Image::Unknown("walk incomplete".into())] {
        let old = helper(10, 5);
        let mut os = Fake::new();
        os.launchd = [Some(old.pid)].into();
        os.processes = [Some(old)].into();
        os.images = [image.clone()].into();
        let outcome = restart_replaced(&mut os, UID, EXIT_WAIT, || {
            anyhow::bail!("prepare must not run for {image:?}")
        })?;
        assert!(matches!(outcome, Outcome::Current | Outcome::Unknown(_)));
        assert!(os.events.is_empty(), "{image:?}: {:?}", os.events);
    }
    let mut os = Fake::new();
    os.launchd = [None].into();
    assert_eq!(
        restart_replaced(&mut os, UID, EXIT_WAIT, || Ok(()))?,
        Outcome::NotRunning
    );
    assert!(os.events.is_empty());
    Ok(())
}

#[test]
fn another_users_process_is_refused_before_any_effect() {
    let mut foreign = helper(10, 5);
    foreign.uid = 0;
    let mut os = Fake::new();
    os.launchd = [Some(10)].into();
    os.processes = [Some(foreign)].into();
    os.images = [Image::Replaced].into();
    assert!(restart_replaced(&mut os, UID, EXIT_WAIT, || Ok(())).is_err());
    assert!(os.events.is_empty());
}

#[test]
fn prepare_failure_sends_no_signal() {
    let old = helper(10, 5);
    let mut os = Fake::new();
    os.launchd = [Some(old.pid)].into();
    os.processes = [Some(old)].into();
    os.images = [Image::Replaced].into();
    let error = restart_replaced(&mut os, UID, EXIT_WAIT, || {
        anyhow::bail!("bundle verification failed")
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("bundle verification failed"));
    assert!(os.events.is_empty());
}

#[test]
fn a_newer_helper_or_a_reused_pid_is_never_signalled() -> Result<()> {
    let old = helper(10, 5);
    // The old helper exited during prepare (its runtime was stopped) and
    // launchd already runs a new one: only the kickstart remains.
    let mut os = Fake::new();
    os.launchd = [Some(old.pid), Some(11)].into();
    os.processes = [Some(old), Some(old), None].into();
    os.images = [Image::Replaced].into();
    assert_eq!(
        restart_replaced(&mut os, UID, EXIT_WAIT, || Ok(()))?,
        Outcome::Restarted {
            old,
            signalled: false,
            kick: Kick::Started
        }
    );
    assert!(os.signals().is_empty());
    // The PID now names an unrelated process with another start time.
    let reused = helper(10, 6);
    let mut os = Fake::new();
    os.launchd = [Some(old.pid)].into();
    os.processes = [Some(old), Some(old), Some(reused)].into();
    os.images = [Image::Replaced].into();
    let outcome = restart_replaced(&mut os, UID, EXIT_WAIT, || Ok(()))?;
    assert!(matches!(
        outcome,
        Outcome::Restarted {
            signalled: false,
            ..
        }
    ));
    assert!(os.signals().is_empty());
    // Still alive, but launchd moved on to another PID: not signalled.
    let mut os = Fake::new();
    os.launchd = [Some(old.pid), Some(11)].into();
    os.processes = [Some(old)].into();
    os.images = [Image::Replaced].into();
    assert!(matches!(
        restart_replaced(&mut os, UID, EXIT_WAIT, || Ok(()))?,
        Outcome::Unknown(_)
    ));
    assert!(os.events.is_empty());
    // Same PID, but the second image read no longer proves replacement.
    let mut os = Fake::new();
    os.launchd = [Some(old.pid)].into();
    os.processes = [Some(old)].into();
    os.images = [Image::Replaced, Image::Current].into();
    assert!(matches!(
        restart_replaced(&mut os, UID, EXIT_WAIT, || Ok(()))?,
        Outcome::Unknown(_)
    ));
    assert!(os.events.is_empty());
    // The identity changed between the first image read and prepare.
    let mut os = Fake::new();
    os.launchd = [Some(old.pid)].into();
    os.processes = [Some(old), Some(reused)].into();
    os.images = [Image::Replaced].into();
    assert!(matches!(
        restart_replaced(&mut os, UID, EXIT_WAIT, || {
            anyhow::bail!("prepare must not run")
        })?,
        Outcome::Unknown(_)
    ));
    assert!(os.events.is_empty());
    Ok(())
}

#[test]
fn a_helper_that_outlives_sigterm_is_left_running_without_sigkill() -> Result<()> {
    let old = helper(10, 5);
    let mut os = Fake::new();
    os.launchd = [Some(old.pid)].into();
    os.processes = [Some(old)].into();
    os.images = [Image::Replaced].into();
    os.after_term = Some(Some(old));
    assert_eq!(
        restart_replaced(&mut os, UID, Duration::from_millis(1), || Ok(()))?,
        Outcome::LeftRunning { old }
    );
    assert_eq!(os.events, ["TERM 10"], "one SIGTERM, no kill, no kickstart");
    Ok(())
}

#[test]
fn kickstart_failure_is_reported_not_fatal() -> Result<()> {
    let old = helper(10, 5);
    let mut os = Fake::new();
    os.launchd = [Some(old.pid)].into();
    os.processes = [Some(old)].into();
    os.images = [Image::Replaced].into();
    os.after_term = Some(None);
    os.kick = Kick::Failed("exit status: 113: Could not find service".into());
    assert_eq!(
        restart_replaced(&mut os, UID, EXIT_WAIT, || Ok(()))?,
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
    };

    /// A copy of /bin/sleep named like the helper, run from a fake bundle.
    struct Helper {
        _root: tempfile::TempDir,
        path: PathBuf,
        child: Option<Child>,
    }
    impl Helper {
        fn start() -> Result<Self> {
            let root = tempfile::tempdir()?;
            let path = root.path().join(HELPER_NAME);
            fs::copy("/bin/sleep", &path)?;
            let child = Command::new(&path).arg("60").spawn()?;
            Ok(Self {
                _root: root,
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
    }
    impl Drop for Helper {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// The native adapter with launchd replaced by the test's own child.
    struct TestChild {
        pid: u32,
        current: PathBuf,
    }
    impl HelperOs for TestChild {
        fn launchd_pid(&mut self) -> Result<Option<u32>> {
            Ok(Some(self.pid))
        }
        fn process(&mut self, pid: u32) -> Result<Option<Process>> {
            native::process(pid)
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
        // Deleted old bundle (Sparkle): the old image is unlinked.
        let helper = Helper::start()?;
        assert_eq!(native::image(helper.pid(), &helper.path)?, Image::Current);
        helper.replace(None)?;
        assert_eq!(native::image(helper.pid(), &helper.path)?, Image::Replaced);
        // Old bundle moved aside: same name, another file.
        let helper = Helper::start()?;
        let aside = helper._root.path().join("previous");
        fs::create_dir(&aside)?;
        helper.replace(Some(&aside.join(HELPER_NAME)))?;
        assert_eq!(native::image(helper.pid(), &helper.path)?, Image::Replaced);
        // Control: an unrelated program is never a replaced helper.
        let mut other = Command::new("/bin/sleep").arg("60").spawn()?;
        let image = native::image(other.id(), &helper.path);
        let _ = other.kill();
        let _ = other.wait();
        assert!(matches!(image?, Image::Unknown(_)), "unrelated image");
        Ok(())
    }

    #[test]
    fn process_identity_has_owner_and_start_time_and_ends_with_exit() -> Result<()> {
        let mut helper = Helper::start()?;
        let process = native::process(helper.pid())?.context("helper not observed")?;
        assert_eq!(process.uid, unsafe { libc::geteuid() });
        assert!(process.started.0 > 0);
        assert_eq!(
            native::process(helper.pid())?,
            Some(process),
            "stable identity"
        );
        let mut child = helper.child.take().unwrap();
        child.kill()?;
        // Exited but unreaped: a zombie is no longer the helper.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(native::process(child.id())?, None);
        child.wait()?;
        assert_eq!(native::process(child.id())?, None);
        Ok(())
    }

    #[test]
    fn only_the_replaced_helper_is_terminated_with_sigterm() -> Result<()> {
        // Control: a helper on the current image keeps running.
        let mut current = Helper::start()?;
        let mut os = TestChild {
            pid: current.pid(),
            current: current.path.clone(),
        };
        assert_eq!(
            restart_replaced(&mut os, unsafe { libc::geteuid() }, EXIT_WAIT, || Ok(()))?,
            Outcome::Current
        );
        assert!(current.child.as_mut().unwrap().try_wait()?.is_none());

        let mut replaced = Helper::start()?;
        replaced.replace(None)?;
        let mut os = TestChild {
            pid: replaced.pid(),
            current: replaced.path.clone(),
        };
        let mut prepared = false;
        let outcome = restart_replaced(&mut os, unsafe { libc::geteuid() }, EXIT_WAIT, || {
            prepared = true;
            Ok(())
        })?;
        assert!(prepared);
        assert!(
            matches!(
                outcome,
                Outcome::Restarted {
                    signalled: true,
                    kick: Kick::Started,
                    ..
                }
            ),
            "{outcome:?}"
        );
        let status = replaced.child.take().unwrap().wait()?;
        assert_eq!(
            status.signal(),
            Some(libc::SIGTERM),
            "SIGTERM, never SIGKILL"
        );
        assert!(current.child.as_mut().unwrap().try_wait()?.is_none());
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
