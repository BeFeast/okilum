//! Settings → Uninstall Okilum… (#588): stop the account Sync helper and
//! unregister its login agent before the app removes anything. The decision
//! is portable; every native effect goes through `Port`. Identity, Syncthing
//! configuration and the journal are never deleted, and the hub is not told.
use super::Status;
use crate::{
    private,
    sidecar::{
        authority::{launch::UninstallStop, Envelope},
        store::UnixStore,
        Binding,
    },
};
use anyhow::{ensure, Context, Result};
use std::{path::Path, time::Instant};

/// Native effects of an uninstall stop. Production: `bootstrap::NativeUninstall`.
pub(crate) trait Port {
    /// ServiceManagement status of the bundled agent.
    fn status(&mut self) -> Result<Status>;
    /// launchd reports a live process for the account helper job.
    fn helper_running(&mut self) -> Result<bool>;
    /// The signed installed app is the one this journal was enrolled with.
    fn verify_bundle(&mut self, binding: &Binding) -> Result<()>;
    /// Authenticated owned-tree Stop, captured supervisor exit and the durable
    /// verified retirement of `saved`'s claimed generation.
    fn retire(&mut self, store: &UnixStore, saved: &Envelope, deadline: Instant) -> Result<()>;
    /// Unregister once the journal records a verified retirement.
    fn unregister_retired(&mut self, binding: &Binding, deadline: Instant) -> Result<()>;
    /// Unregister an agent that no journal describes.
    fn unregister_unowned(&mut self) -> Result<()>;
}

fn unregistered(status: Status) -> bool {
    matches!(status, Status::NotRegistered | Status::NotFound)
}

/// Ok means no Okilum helper runs and no agent stays registered. Any error
/// means the caller must remove nothing.
pub(crate) fn stop(state: &Path, port: &mut impl Port, deadline: Instant) -> Result<()> {
    match std::fs::symlink_metadata(state) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return stop_without_journal(port)
        }
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    let store = UnixStore::open_existing(state)?;
    // Serialises with Connect's retirement and re-arm of a native generation.
    let _upgrade_lock = {
        let path = state.join("native-upgrade");
        private::directory(&path)?;
        private::lock(&path)?
    };
    let saved = current(&store, deadline)?;
    let binding = saved.binding().clone();
    ensure!(
        Path::new(&binding.state_directory) == state,
        "recovery pending: the Sync state folder changed"
    );
    port.verify_bundle(&binding)?;
    match saved.uninstall_stop()? {
        UninstallStop::NotLaunched => ensure!(
            unregistered(port.status()?),
            "recovery pending: the Sync agent is registered without a confirmed helper"
        ),
        UninstallStop::Retire => {
            port.retire(&store, &saved, deadline)?;
            let retired = current(&store, deadline)?;
            ensure!(
                retired.binding() == &binding
                    && retired.uninstall_stop()? == UninstallStop::Unregister,
                "the Sync helper stop was not recorded as a verified retirement"
            );
            port.verify_bundle(&binding)?;
            port.unregister_retired(&binding, deadline)?;
        }
        UninstallStop::Unregister => port.unregister_retired(&binding, deadline)?,
    }
    Ok(())
}

/// No journal means no Stop authority. An agent left registered (state folder
/// deleted by hand, interrupted first preparation) is unregistered only while
/// launchd runs no helper, so nothing can be orphaned by it.
fn stop_without_journal(port: &mut impl Port) -> Result<()> {
    if unregistered(port.status()?) {
        return Ok(());
    }
    ensure!(
        !port.helper_running()?,
        "recovery pending: a Sync helper is running without its journal"
    );
    port.unregister_unowned()?;
    ensure!(
        unregistered(port.status()?),
        "Sync agent unregistration is still pending"
    );
    ensure!(
        !port.helper_running()?,
        "a Sync helper started while its agent was unregistered"
    );
    Ok(())
}

fn current(store: &UnixStore, deadline: Instant) -> Result<Envelope> {
    let tx = store.begin(deadline)?;
    Ok(tx
        .current()?
        .context("recovery pending: the Sync journal is missing")?
        .clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{
        authority::launch::{UninstallStop, VerifiedExit},
        supervisor::ipc::Scope,
    };
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
    use uuid::Uuid;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Retire {
        /// Authenticated Stop and exit succeed and retirement is committed.
        Stops,
        /// The helper does not answer (absent/not Live): nothing is committed.
        Absent,
        /// Claims success without recording the retirement.
        Unrecorded,
    }
    struct Fake {
        status: Status,
        running: bool,
        retire: Retire,
        calls: Vec<&'static str>,
    }
    impl Fake {
        fn new(status: Status, running: bool, retire: Retire) -> Self {
            Self {
                status,
                running,
                retire,
                calls: Vec::new(),
            }
        }
    }
    impl Port for Fake {
        fn status(&mut self) -> Result<Status> {
            self.calls.push("status");
            Ok(self.status)
        }
        fn helper_running(&mut self) -> Result<bool> {
            self.calls.push("running");
            Ok(self.running)
        }
        fn verify_bundle(&mut self, _: &Binding) -> Result<()> {
            self.calls.push("verify");
            Ok(())
        }
        fn retire(&mut self, store: &UnixStore, saved: &Envelope, deadline: Instant) -> Result<()> {
            self.calls.push("retire");
            match self.retire {
                Retire::Absent => anyhow::bail!("helper needs verified recovery"),
                Retire::Unrecorded => Ok(()),
                Retire::Stops => {
                    let scope = Scope {
                        installation: saved.binding().installation,
                        instance: saved.binding().instance,
                        generation: saved.launch_registration().unwrap().generation(),
                    };
                    let (stopping, token) = saved.disable(scope)?;
                    store.begin(deadline)?.commit(stopping.clone())?;
                    self.running = false;
                    let retired = stopping.complete_native_retirement(VerifiedExit::for_test(
                        saved.binding().clone(),
                        token,
                    ))?;
                    store.begin(deadline)?.commit(retired)
                }
            }
        }
        fn unregister_retired(&mut self, _: &Binding, _: Instant) -> Result<()> {
            self.calls.push("unregister_retired");
            self.status = Status::NotRegistered;
            Ok(())
        }
        fn unregister_unowned(&mut self) -> Result<()> {
            self.calls.push("unregister_unowned");
            self.status = Status::NotRegistered;
            Ok(())
        }
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }
    fn enrolled(claimed: bool) -> (tempfile::TempDir, PathBuf, UnixStore, Binding) {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().canonicalize().unwrap().join("Sync");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let store = UnixStore::open_existing(&state).unwrap();
        let binding = Binding {
            installation: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            owner: "501".into(),
            supervisor: "/Applications/Okilum.app/Contents/MacOS/okilum-sync-supervisor".into(),
            state_directory: state.to_str().unwrap().into(),
            device_identity: "retained-device".into(),
        };
        store
            .begin(soon())
            .unwrap()
            .commit(Envelope::first_with_launch(binding.clone()))
            .unwrap();
        if claimed {
            store
                .dispatch_launch(&binding, soon())
                .unwrap()
                .execute(|_| Ok(()))
                .unwrap();
            store.claim_launch(&binding, soon()).unwrap();
        }
        (dir, state, store, binding)
    }
    fn saved(store: &UnixStore) -> Envelope {
        current(store, soon()).unwrap()
    }

    #[test]
    fn a_live_helper_is_stopped_and_retired_before_its_agent_is_unregistered() {
        let (_dir, state, store, binding) = enrolled(true);
        let mut port = Fake::new(Status::Enabled, true, Retire::Stops);
        stop(&state, &mut port, soon()).unwrap();
        assert_eq!(
            port.calls,
            ["verify", "retire", "verify", "unregister_retired"]
        );
        assert!(!port.running);
        let after = saved(&store);
        // Identity and journal stay; only a later explicit Connect re-arms.
        assert_eq!(after.binding(), &binding);
        assert_eq!(after.uninstall_stop().unwrap(), UninstallStop::Unregister);
        assert!(state.is_dir(), "state is kept");
        // A repeated uninstall after an interruption only unregisters again.
        let mut again = Fake::new(Status::Enabled, false, Retire::Absent);
        stop(&state, &mut again, soon()).unwrap();
        assert_eq!(again.calls, ["verify", "unregister_retired"]);
    }

    #[test]
    fn an_unanswering_helper_or_unrecorded_stop_refuses_without_unregistering() {
        for retire in [Retire::Absent, Retire::Unrecorded] {
            let (_dir, state, store, _) = enrolled(true);
            let before = saved(&store);
            let mut port = Fake::new(Status::Enabled, true, retire);
            assert!(stop(&state, &mut port, soon()).is_err(), "{retire:?}");
            assert_eq!(port.calls, ["verify", "retire"], "{retire:?}");
            assert_eq!(saved(&store), before, "{retire:?}: journal unchanged");
        }
    }

    #[test]
    fn a_never_registered_generation_needs_no_stop_but_a_registered_one_refuses() {
        let (_dir, state, _store, _) = enrolled(false);
        let mut idle = Fake::new(Status::NotRegistered, false, Retire::Absent);
        stop(&state, &mut idle, soon()).unwrap();
        assert_eq!(idle.calls, ["verify", "status"]);
        let mut registered = Fake::new(Status::Enabled, false, Retire::Absent);
        assert!(stop(&state, &mut registered, soon()).is_err());
        assert!(!registered.calls.iter().any(|c| c.starts_with("unregister")));
    }

    #[test]
    fn a_missing_state_folder_still_checks_and_clears_the_registered_agent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("Sync");
        // Nothing registered: nothing to do.
        let mut absent = Fake::new(Status::NotFound, false, Retire::Absent);
        stop(&missing, &mut absent, soon()).unwrap();
        assert_eq!(absent.calls, ["status"]);
        // Registered, no helper running: unregistered, never left behind.
        let mut stale = Fake::new(Status::Enabled, false, Retire::Absent);
        stop(&missing, &mut stale, soon()).unwrap();
        assert_eq!(
            stale.calls,
            [
                "status",
                "running",
                "unregister_unowned",
                "status",
                "running"
            ]
        );
        assert_eq!(stale.status, Status::NotRegistered);
        // Registered and a helper running without its journal: no Stop
        // authority exists, so the uninstall refuses and changes nothing.
        let mut live = Fake::new(Status::RequiresApproval, true, Retire::Absent);
        assert!(stop(&missing, &mut live, soon()).is_err());
        assert_eq!(live.calls, ["status", "running"]);
        assert!(!missing.exists(), "no state is created");
    }
}
