//! Unix owned tree for macOS (and Linux tests): the runtime is the leader of a new
//! process group, and the group plus the reaped root are what this owns (design:
//! docs/sync-sidecar-discovery.md). There is no kill-on-close guarantee: a process
//! that leaves the group is not owned. Stopped is therefore claimed only after the
//! group is empty AND no live same-user process runs the pinned runtime executable
//! with a start time not earlier than the root's. Anything else is Stopping.
use super::{ipc::Status, runtime::OwnedTree, Launch};
use anyhow::{ensure, Context, Result};
use rustix::{
    io::Errno,
    process::{
        kill_process_group, test_kill_process_group, waitid, Pid, Signal, WaitId, WaitIdOptions,
    },
};
use std::{
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) mod scan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GroupLeader {
    pid: Pid,
    group: Pid,
    started: u64,
}

/// A process inventory is evidence, not permission to adopt a group. Only the
/// exact, still-waitable leader captured at launch can authorize a signal.
fn signal_owned_group(
    owner: Option<GroupLeader>,
    inspect: impl FnOnce(Pid) -> Result<GroupLeader>,
    signal: impl FnOnce(Pid) -> Result<()>,
) -> Result<()> {
    let owner = owner.context("no captured unreaped process group leader")?;
    ensure!(
        owner.pid == owner.group,
        "owned process is not the group leader"
    );
    ensure!(
        inspect(owner.pid)? == owner,
        "process group leader identity changed; refusing to signal"
    );
    signal(owner.group)
}

fn inspect_leader(pid: Pid) -> Result<GroupLeader> {
    #[cfg(target_os = "macos")]
    let (started, group) = {
        let (started, group) = scan::leader(pid.as_raw_pid() as u32)?;
        (
            started,
            Pid::from_raw(group).context("invalid process group")?,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let (started, group) = (
        scan::started(pid.as_raw_pid() as u32)?,
        rustix::process::getpgid(Some(pid))?,
    );
    // Holding a Child value alone does not prove the PID is reserved: an
    // external waiter or SIGCHLD auto-reaping may already have released it.
    // NOWAIT preserves our reservation, including an exited zombie leader.
    waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .context("process group leader is no longer waitable")?;
    Ok(GroupLeader {
        pid,
        group,
        started,
    })
}

/// Start time of a live process in the platform's native unit, for the supervisor's
/// own generation hint. Comparable only with values from the same platform.
pub fn process_start_time(pid: u32) -> Result<u64> {
    scan::started(pid)
}

impl Launch {
    /// The fixed Syncthing argv (without argv[0]): isolated config/data, no browser,
    /// no self-restart, no self-upgrade. Absolute plain paths only.
    pub fn unix_arguments(&self) -> Result<Vec<String>> {
        for value in [&self.executable, &self.config, &self.data] {
            ensure!(
                value.starts_with('/')
                    && !value.chars().any(char::is_control)
                    && value.split('/').all(|part| part != "." && part != ".."),
                "absolute local path required"
            );
        }
        ensure!(
            self.config != self.data,
            "separate config and data directories required"
        );
        Ok([
            "serve",
            "--no-browser",
            "--no-restart",
            "--no-upgrade",
            "--config",
            &self.config,
            "--data",
            &self.data,
        ]
        .map(String::from)
        .to_vec())
    }
}

pub struct ProcessGroupTree {
    /// Some until reaped here. Revalidate before signaling: an external reaper can
    /// invalidate a Child without updating this value. An unreaped (even exited)
    /// leader keeps the group id reserved.
    root: Option<Child>,
    group: Pid,
    image: PathBuf,
    uid: u32,
    started: Option<u64>,
    stopped: bool,
    budget: Duration,
}
impl ProcessGroupTree {
    /// The caller has verified the staged payload and private config/data.
    /// `budget` bounds one Stop and should sit inside the exchange deadline.
    /// Child reaping must remain exclusive to this tree; inventory checks cannot
    /// make a concurrent external waiter atomic with a process-group signal.
    pub fn spawn(launch: &Launch, budget: Duration) -> Result<Self> {
        let arguments = launch.unix_arguments()?;
        let image = std::fs::canonicalize(&launch.executable)?;
        let child = Command::new(&image)
            .args(&arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        let pid = child.id();
        let group = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .context("invalid process id")?;
        let mut tree = Self {
            root: Some(child),
            group,
            image,
            uid: rustix::process::geteuid().as_raw(),
            started: None,
            stopped: false,
            budget,
        };
        match scan::started(pid) {
            Ok(started) => tree.started = Some(started),
            Err(error) => {
                // Without the root's start time Stopped could never be certified.
                // Nor can a numeric PID authorize cleanup of an unknown generation.
                return Err(error.context(
                    "cannot read the runtime start time; cleanup incomplete, runtime may still be running",
                ));
            }
        }
        Ok(tree)
    }
    fn kill_now(&mut self) -> Result<()> {
        self.signal_group(Signal::KILL)?;
        if let Some(child) = self.root.as_mut() {
            child.wait()?;
        }
        self.root = None;
        Ok(())
    }
    /// True once the leader has exited (not yet reaped). The supervisor uses it to
    /// notice a crashed runtime; `status` stays Running until `stop` certifies.
    pub fn root_exited(&self) -> Result<bool> {
        let Some(child) = &self.root else {
            return Ok(true);
        };
        let pid = Pid::from_raw(child.id() as i32).context("invalid process id")?;
        Ok(waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )?
        .is_some())
    }
    /// Signal the whole group. Nothing signalable left is not an error: ESRCH when it
    /// is already gone, and EPERM, which macOS returns when the only members left are
    /// zombies (present but not signalable). What remains is decided by `group_empty`
    /// and the deadline, so a member we truly cannot signal still ends as Stopping.
    fn signal_group(&self, signal: Signal) -> Result<()> {
        let owner = self
            .root
            .as_ref()
            .zip(self.started)
            .map(|(child, started)| -> Result<GroupLeader> {
                Ok(GroupLeader {
                    pid: i32::try_from(child.id())
                        .ok()
                        .and_then(Pid::from_raw)
                        .context("invalid process id")?,
                    group: self.group,
                    started,
                })
            })
            .transpose()?;
        signal_owned_group(owner, inspect_leader, |group| {
            match kill_process_group(group, signal) {
                Ok(()) | Err(Errno::SRCH) | Err(Errno::PERM) => Ok(()),
                Err(error) => Err(error.into()),
            }
        })
    }
    /// The group has no member, or only members we cannot see (EPERM counts as
    /// present). Never signals.
    fn group_empty(&self) -> Result<bool> {
        match test_kill_process_group(self.group) {
            Err(Errno::SRCH) => Ok(true),
            Ok(()) | Err(Errno::PERM) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
    fn escaped(&self) -> Result<bool> {
        let started = self.started.context("runtime start time is unknown")?;
        Ok(!scan::live_copies(&self.image, self.uid, started)?.is_empty())
    }
}
impl OwnedTree for ProcessGroupTree {
    fn status(&mut self) -> Result<Status> {
        if self.stopped {
            return Ok(Status::Stopped);
        }
        // An unreaped leader is never reported as done: its group may still have
        // members that only `stop` can signal safely.
        if self.root.is_some() || !self.group_empty()? {
            return Ok(Status::Running);
        }
        Ok(if self.escaped()? {
            Status::Running
        } else {
            self.stopped = true;
            Status::Stopped
        })
    }
    fn stop(&mut self) -> Result<Status> {
        if self.stopped {
            return Ok(Status::Stopped);
        }
        let now = Instant::now();
        let deadline = now
            .checked_add(self.budget)
            .context("invalid owned process stop budget")?;
        // An expired budget refuses to terminate anything.
        if self.budget.is_zero() {
            return Ok(Status::Stopping);
        }
        if self.root.is_some() {
            // SIGTERM to the whole group, a bounded wait for the leader, then
            // SIGKILL for stragglers; the leader stays unreaped throughout, so the
            // group id cannot have been recycled while signals are sent.
            self.signal_group(Signal::TERM)?;
            let grace = now + self.budget / 2;
            while Instant::now() < grace && !self.root_exited()? {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.signal_group(Signal::KILL)?;
            let mut child = self.root.take().context("root vanished")?;
            loop {
                if child.try_wait()?.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    self.root = Some(child);
                    return Ok(Status::Stopping);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // The leader is reaped: no more signals, only observation.
        while !self.group_empty()? {
            if Instant::now() >= deadline {
                return Ok(Status::Stopping);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Defence in depth: a copy of the runtime that left the group.
        loop {
            if !self.escaped()? {
                self.stopped = true;
                return Ok(Status::Stopped);
            }
            if Instant::now() >= deadline {
                return Ok(Status::Stopping);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for ProcessGroupTree {
    fn drop(&mut self) {
        // Best-effort cleanup only while the captured leader can still be
        // verified. A stale Child never authorizes signaling a reused group.
        if self.root.is_some() {
            let _ = self.kill_now();
        }
    }
}

#[cfg(test)]
mod tests;
