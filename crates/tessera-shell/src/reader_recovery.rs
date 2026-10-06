//! A durable launch marker, separate from draft journals and disposable indexes.
use anyhow::Result;
use gpui::{App, Global};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Signal callbacks do only an atomic store. Saving, marker cleanup and GPUI
/// shutdown always run on the normal application thread, never in a handler.
struct TerminationSignals {
    requested: Arc<std::sync::atomic::AtomicBool>,
    registrations: Vec<signal_hook_registry::SigId>,
}
impl Global for TerminationSignals {}
impl TerminationSignals {
    fn install() -> Result<Self> {
        let mut signals = Self {
            requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            registrations: vec![],
        };
        for signal in [rustix::process::Signal::TERM, rustix::process::Signal::INT] {
            let requested = signals.requested.clone();
            // SAFETY: this lock-free atomic store is the entire signal handler.
            let registration = unsafe {
                signal_hook_registry::register(signal.as_raw(), move || {
                    requested.store(true, std::sync::atomic::Ordering::SeqCst);
                })?
            };
            signals.registrations.push(registration);
        }
        Ok(signals)
    }
    #[cfg(test)]
    fn requested(&self) -> bool {
        self.requested.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl Drop for TerminationSignals {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            signal_hook_registry::unregister(registration);
        }
    }
}

fn install_termination(cx: &mut App) {
    let signals = match TerminationSignals::install() {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("Cannot install graceful termination handlers: {error}");
            return;
        }
    };
    let requested = signals.requested.clone();
    cx.set_global(signals);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            if requested.load(std::sync::atomic::Ordering::SeqCst) {
                // Use the same native quit path as the application/OS menu. Its
                // on_app_quit observers flush editors and window preferences.
                cx.update(|cx| cx.quit());
                break;
            }
        }
    })
    .detach();
}

struct Run {
    marker: PathBuf,
    stale: Vec<PathBuf>,
    _lock: File,
}
impl Drop for Run {
    fn drop(&mut self) {
        // Explicitly release the open-file-description lock before close. A
        // concurrent subprocess fork may briefly retain a descriptor until exec.
        // Keep the marker itself: only clean() acknowledges a safe shutdown.
        let _ = rustix::fs::flock(&self._lock, rustix::fs::FlockOperation::Unlock);
    }
}
impl Run {
    fn begin(directory: &Path) -> Result<Self> {
        let directory = directory.join("reader-runs");
        fs::create_dir_all(&directory)?;
        let mut stale = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "active") {
                continue;
            }
            let file = File::open(&path)?;
            // Another live Tessera process is not a crashed launch.
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => stale.push(path),
                Err(rustix::io::Errno::WOULDBLOCK) => (),
                Err(error) => return Err(error.into()),
            }
        }
        let marker = directory.join(format!("{}.active", uuid::Uuid::new_v4()));
        let pending = marker.with_extension("pending");
        let lock = File::create_new(&pending)?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
        lock.sync_all()?;
        fs::rename(pending, &marker)?;
        File::open(&directory)?.sync_all()?;
        Ok(Self {
            marker,
            stale,
            _lock: lock,
        })
    }

    // Explicit only: dropping on panic must leave evidence of the failed launch.
    fn clean(&self) -> Result<()> {
        for path in self.stale.iter().chain(std::iter::once(&self.marker)) {
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        File::open(self.marker.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}

pub(crate) struct RecoveryStartup(pub bool);
impl Global for RecoveryStartup {}

pub(crate) fn is_recovering(cx: &App) -> bool {
    cx.try_global::<RecoveryStartup>()
        .is_some_and(|state| state.0)
}

pub(crate) fn install(directory: &Path, config_directory: Option<&Path>, cx: &mut App) {
    // Tests install their own isolated signal handlers in subprocesses.
    if !cfg!(test) {
        install_termination(cx);
    }
    // Track both historical locations: moving either one aside must not hide a
    // crash recorded in the other. Neither journals nor preferences are moved.
    let mut directories = vec![directory.to_path_buf()];
    if let Some(config) = config_directory {
        if config != directory {
            directories.push(config.to_path_buf());
        }
    }
    let mut runs = Vec::new();
    let mut recovering = false;
    for directory in directories {
        match Run::begin(&directory) {
            Ok(run) => {
                recovering |= !run.stale.is_empty();
                runs.push(run);
            }
            Err(error) => {
                recovering = true;
                eprintln!("Cannot record Reader launch: {error}");
            }
        }
    }
    cx.set_global(RecoveryStartup(recovering));
    let runs = Arc::new(runs);
    cx.on_app_quit(move |cx| {
        let protected = super::reader_editor::protect_all_for_quit(cx);
        let runs = runs.clone();
        async move {
            // A canonical conflict with a durable draft is a clean quit. Failed
            // draft persistence (or an in-flight move) must retain the safety marker.
            if !protected {
                return;
            }
            for run in runs.iter() {
                if let Err(error) = run.clean() {
                    eprintln!("Cannot finish Reader launch marker: {error}");
                }
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn os_quit_cleans_both_launch_markers(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("bundle");
        let config = root.path().join("config");
        cx.update(|cx| {
            install(&state, Some(&config), cx);
            for directory in [&state, &config] {
                assert_eq!(
                    fs::read_dir(directory.join("reader-runs")).unwrap().count(),
                    1
                );
            }
            cx.shutdown();
        });
        for directory in [&state, &config] {
            let next = Run::begin(directory).unwrap();
            assert!(next.stale.is_empty());
            next.clean().unwrap();
        }
    }

    // The real signal is delivered only to this subprocess, never to the test runner.
    #[test]
    fn signal_child() {
        let Some(directory) = std::env::var_os("TESSERA_SIGNAL_TEST_DIRECTORY") else {
            return;
        };
        let directory = PathBuf::from(directory);
        let signals = TerminationSignals::install().unwrap();
        let run = Run::begin(&directory).unwrap();
        use std::io::Write;
        println!("SIGNALS_READY");
        std::io::stdout().flush().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !signals.requested() {
            assert!(
                std::time::Instant::now() < deadline,
                "termination was not delivered"
            );
            std::thread::park_timeout(std::time::Duration::from_millis(10));
        }
        run.clean().unwrap();
    }

    #[test]
    fn termination_signals_are_graceful_but_sigkill_retains_crash_evidence() {
        use rustix::process::{kill_process, Pid, Signal};
        use std::io::{BufRead, BufReader, Read};
        use std::process::{Command, Stdio};
        for signal in [Signal::TERM, Signal::INT, Signal::KILL] {
            let directory = tempfile::tempdir().unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reader_recovery::tests::signal_child",
                    "--nocapture",
                ])
                .env("TESSERA_SIGNAL_TEST_DIRECTORY", directory.path())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut output = BufReader::new(child.stdout.take().unwrap());
            let ready = output
                .by_ref()
                .lines()
                .any(|line| line.unwrap().contains("SIGNALS_READY"));
            assert!(ready, "child exited before installing signal handlers");
            kill_process(Pid::from_raw(child.id() as i32).unwrap(), signal).unwrap();
            // Keep stdout open and drain the test harness footer after readiness.
            // Dropping the pipe at the handshake would manufacture a BrokenPipe exit.
            std::io::copy(&mut output, &mut std::io::sink()).unwrap();
            let status = child.wait().unwrap();
            assert_eq!(status.success(), signal != Signal::KILL);
            let next = Run::begin(directory.path()).unwrap();
            assert_eq!(!next.stale.is_empty(), signal == Signal::KILL);
            next.clean().unwrap();
        }
    }

    #[gpui::test]
    fn either_state_location_can_trigger_safe_startup(cx: &mut gpui::TestAppContext) {
        for stale_config in [false, true] {
            let root =
                std::env::temp_dir().join(format!("tessera-two-roots-{}", uuid::Uuid::new_v4()));
            let state = root.join("uk.oklabs.tessera");
            let config = root.join("tessera");
            drop(Run::begin(if stale_config { &config } else { &state }).unwrap());
            cx.update(|cx| {
                install(&state, Some(&config), cx);
                assert!(is_recovering(cx));
            });
            // Both locations now carry the live launch, while all other files remain untouched.
            assert!(state.join("reader-runs").is_dir());
            assert!(config.join("reader-runs").is_dir());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn crash_marker_survives_drop_and_live_launches_are_not_crashes() {
        let directory =
            std::env::temp_dir().join(format!("tessera-run-test-{}", uuid::Uuid::new_v4()));
        let first = Run::begin(&directory).unwrap();
        assert!(first.stale.is_empty());
        let concurrent = Run::begin(&directory).unwrap();
        assert!(concurrent.stale.is_empty());
        concurrent.clean().unwrap();
        let inherited = first._lock.try_clone().unwrap();
        drop(first); // Release synchronously even while a duplicated descriptor exists.
        let recovered = Run::begin(&directory).unwrap();
        assert_eq!(recovered.stale.len(), 1);
        drop(inherited);
        recovered.clean().unwrap();
        let clean = Run::begin(&directory).unwrap();
        assert!(clean.stale.is_empty());
        clean.clean().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}
