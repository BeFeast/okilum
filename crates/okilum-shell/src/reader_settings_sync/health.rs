//! Sync failures belong in every main window, independently of Settings.
use super::*;
use std::collections::BTreeMap;
const STALE: Duration = Duration::from_secs(10 * 60);
const UNREACHABLE: Duration = Duration::from_secs(2 * 60);

/// Runtime observation is independent of pairing/listener/replica reconciliation.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Runtime {
    pub down: bool,
    pub unreachable: bool,
}

#[derive(Default)]
pub(super) struct Health {
    down: bool,
    unavailable_since: Option<Instant>,
    unavailable_vaults: BTreeMap<PathBuf, Instant>,
    disconnected: BTreeMap<PathBuf, Instant>,
    stale: bool,
}
impl Health {
    pub(super) fn failed(&mut self, now: Instant) {
        let since = self.unavailable_since.get_or_insert(now);
        self.down |= now.saturating_duration_since(*since) > UNREACHABLE;
    }
    pub(super) fn observe(&mut self, overview: &direct::Overview, runtime: Runtime, now: Instant) {
        self.down = false;
        if runtime.unreachable {
            self.failed(now);
        } else {
            self.unavailable_since = None;
        }
        self.down |= runtime.down;
        self.unavailable_vaults
            .retain(|path, _| overview.vaults.contains_key(path));
        self.disconnected
            .retain(|path, _| overview.vaults.contains_key(path));
        self.stale = false;
        for (path, vault) in &overview.vaults {
            if !vault.available {
                let since = self.unavailable_vaults.entry(path.clone()).or_insert(now);
                self.down |= now.saturating_duration_since(*since) > UNREACHABLE;
                self.disconnected.remove(path);
                continue;
            }
            self.unavailable_vaults.remove(path);
            self.down |= vault.errors;
            // Idle folders, sleeping peers, scans and active transfers are normal.
            // Count items as well as bytes: deletions/empty files need no bytes.
            if vault.connected || vault.needed_items == 0 || vault.shared_with.is_empty() {
                self.disconnected.remove(path);
            } else {
                let since = self.disconnected.entry(path.clone()).or_insert(now);
                self.stale |= now.saturating_duration_since(*since) > STALE;
            }
        }
    }
    pub(super) fn message(&self) -> Option<&'static str> {
        if self.down {
            Some("Sync needs attention; changes from your other computers may be missing.")
        } else if self.stale {
            Some("This vault has not synced with your other computers for more than 10 minutes.")
        } else {
            None
        }
    }
}
impl SyncSettings {
    pub(super) fn render_health(&self, cx: &mut Context<Self>) -> AnyElement {
        if !reader_features::enabled(reader_features::Feature::Sync, cx) {
            return div().into_any_element();
        }
        let Some(message) = self.health.message() else {
            return div().into_any_element();
        };
        h_flex()
            .id("sync-failure-banner")
            .debug_selector(|| "sync-failure-banner".into())
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .p_4()
            .gap_4()
            .bg(rgb(0xb91c1c))
            .text_color(rgb(0xffffff))
            .child(div().flex_1().min_w_0().whitespace_normal().child(message))
            .child(
                Button::new("sync-fix")
                    .label("Fix")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        #[cfg(target_os = "macos")]
                        this.run(Operation::Restart, cx);
                        #[cfg(not(target_os = "macos"))]
                        this.run(Operation::Read, cx);
                        reader_settings::show_sync(window, cx);
                    })),
            )
            .into_any_element()
    }
}
#[cfg(all(target_os = "macos", not(test)))]
pub(super) fn managed_enabled() -> bool {
    use okilum_sync_controller::sidecar::{
        macos::account_state_directory, store::UnixStore, Intent,
    };
    if reader_history::isolated_state_directory().is_some() {
        return false;
    }
    (|| -> anyhow::Result<bool> {
        let store = UnixStore::open_existing(&account_state_directory()?)?;
        let tx = store.begin(Instant::now() + Duration::from_secs(2))?;
        Ok(tx
            .current()?
            .is_some_and(|saved| saved.intent() == Intent::Enabled))
    })()
    .unwrap_or(false)
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct HelperDown;
#[cfg(any(target_os = "macos", test))]
impl std::fmt::Display for HelperDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sync helper is not running")
    }
}
#[cfg(any(target_os = "macos", test))]
impl std::error::Error for HelperDown {}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct RestUnavailable;
#[cfg(any(target_os = "macos", test))]
impl std::fmt::Display for RestUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Syncthing REST unavailable for enabled Sync")
    }
}
#[cfg(any(target_os = "macos", test))]
impl std::error::Error for RestUnavailable {}

#[cfg(any(target_os = "macos", test))]
pub(super) fn runtime(result: &anyhow::Result<()>) -> Runtime {
    match result {
        Err(error) if error.is::<HelperDown>() => Runtime {
            down: true,
            unreachable: false,
        },
        Err(error) if error.is::<RestUnavailable>() => Runtime {
            down: false,
            unreachable: true,
        },
        // Lifecycle/inspection failures remain Settings diagnostics. They do not
        // establish either a stopped helper or an unreachable Sync REST service.
        _ => Runtime::default(),
    }
}

#[cfg(all(target_os = "macos", not(test)))]
pub(super) fn check_managed() -> anyhow::Result<()> {
    if !managed_enabled() {
        return Ok(());
    }
    use okilum_sync_controller::{
        daemon,
        sidecar::{
            discovery,
            macos::{account_state_directory, health::account_helper_running},
        },
    };
    let state = account_state_directory()?;
    // Health observes the launchd job independently of lifecycle authentication.
    // Sparkle may unlink a live old helper image while installing the new bundle;
    // failed code-path/Status inspection does not mean that helper stopped.
    let helper = account_helper_running();
    if matches!(helper, Ok(false)) {
        return Err(HelperDown.into());
    }
    let rest_available = discovery::managed_configuration_path(&state)
        .ok()
        .flatten()
        .is_some_and(|config| !daemon::discover(&[config]).candidates.is_empty());
    if !rest_available {
        return Err(RestUnavailable.into());
    }
    // Unknown helper observation is actionable in Settings, but a healthy REST
    // service must not become a false red alarm after the outage grace period.
    helper.map(|_| ())
}

#[cfg(target_os = "macos")]
pub(super) fn repair_or_restart(controller: &Controller) -> anyhow::Result<()> {
    // Fix is explicit consent to refresh this computer's existing replica
    // witness. It never adds a new vault or changes peers/device identity.
    let snapshot = controller.snapshot()?;
    let configs = controller.configuration_paths()?;
    let inspected = snapshot
        .attached
        .iter()
        .map(|vault| direct::inspect_existing(&configs, &vault.path))
        .collect::<anyhow::Result<Vec<_>>>();
    if let Ok(inspected) = inspected {
        if !inspected.is_empty() {
            for vault in inspected {
                controller.attach_existing(&vault, true)?;
            }
            controller.repair_transfer_identities(true)?;
            #[cfg(not(test))]
            if check_managed().is_err() {
                return restart_managed();
            }
            return Ok(());
        }
    }
    restart_managed()
}

#[cfg(target_os = "macos")]
pub(super) fn restart_managed() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use okilum_sync_controller::sidecar::{
        macos::{
            account_state_directory,
            bundle::{BundleGuard, NativeSignature},
        },
        store::UnixStore,
        supervisor::ipc::code_requirement::CodeRequirement,
        Intent,
    };
    // Reader's explicit isolated profiles never mutate account-wide Sync.
    anyhow::ensure!(
        reader_history::isolated_state_directory().is_none(),
        "Restart the isolated profile's Sync connection, then Refresh"
    );
    let state = account_state_directory()?;
    let store = UnixStore::open_existing(&state)?;
    let binding = {
        let tx = store.begin(Instant::now() + Duration::from_secs(2))?;
        let saved = tx.current()?.context("Sync journal unavailable")?;
        anyhow::ensure!(
            saved.intent() == Intent::Enabled && saved.stop().is_none() && saved.update().is_none(),
            "finish the pending Sync lifecycle operation before restarting"
        );
        saved.binding().clone()
    };
    let own = std::env::current_exe()?.canonicalize()?;
    let app = own
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("app bundle unavailable")?;
    let team = option_env!("OKILUM_SIGNING_TEAM_ID").context("Sync signing policy unavailable")?;
    let identifier = option_env!("OKILUM_BUNDLE_ID").context("Sync signing policy unavailable")?;
    let bundle = BundleGuard::new(
        app,
        "okilum",
        binding.clone(),
        CodeRequirement::team_and_identifier(team, identifier)?,
        CodeRequirement::team_and_identifier(team, &format!("{identifier}.sync"))?,
        NativeSignature,
    )?;
    bundle.verify(&binding)?;
    // Older installed helpers have no SIGTERM cleanup. Stop only the
    // authenticated account runtime before replacing that helper, so its
    // surviving child cannot permanently block the new supervisor lease.
    if let Some(config) =
        okilum_sync_controller::sidecar::discovery::managed_configuration_path(&state)?
    {
        let discovered = okilum_sync_controller::daemon::discover(&[config]);
        for candidate in discovered.candidates {
            candidate
                .identity
                .connect()?
                .shutdown(&binding.device_identity)?;
        }
    }
    let target = format!("gui/{}/com.befeast.okilum.sync", binding.owner);
    // TERM lets the new helper flush its owned tree. -k would SIGKILL it and
    // leave an orphan runtime, which the lifetime lease correctly refuses.
    let stopped = bounded_request(
        std::process::Command::new("/bin/launchctl").args(["kill", "SIGTERM", &target]),
        Duration::from_secs(5),
    )?;
    if stopped == Some(false) {
        let started = bounded_request(
            std::process::Command::new("/bin/launchctl").args(["kickstart", &target]),
            Duration::from_secs(5),
        )?;
        anyhow::ensure!(started != Some(false), "Sync helper restart request failed");
    }
    Ok(())
}

/// launchctl can hang after the service has already started. Bound only our
/// CLI child; never signal the helper by a guessed PID or block Sync forever.
#[cfg(target_os = "macos")]
fn bounded_request(
    command: &mut std::process::Command,
    budget: Duration,
) -> anyhow::Result<Option<bool>> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.success()));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            // The service request may have succeeded. The regular authenticated
            // recheck, rather than the launchctl exit, establishes Sync health.
            return Ok(None);
        }
        std::thread::sleep(left.min(Duration::from_millis(25)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[cfg(target_os = "macos")]
    #[test]
    fn launchctl_style_request_has_success_failure_and_hung_child_controls() -> anyhow::Result<()> {
        use std::process::Command;
        assert_eq!(
            bounded_request(&mut Command::new("/usr/bin/true"), Duration::from_secs(3))?,
            Some(true)
        );
        assert_eq!(
            bounded_request(&mut Command::new("/usr/bin/false"), Duration::from_secs(3))?,
            Some(false)
        );
        assert_eq!(
            bounded_request(
                Command::new("/bin/sleep").arg("60"),
                Duration::from_millis(100)
            )?,
            None
        );
        Ok(())
    }

    fn healthy() -> direct::Overview {
        let mut overview = direct::Overview::default();
        overview.vaults.insert(
            PathBuf::from("/synthetic"),
            direct::VaultOverview {
                available: true,
                connected: true,
                state: "Up to date".into(),
                shared_with: vec!["Other computer".into()],
                ..Default::default()
            },
        );
        overview
    }

    #[test]
    fn lifecycle_inspection_failure_with_healthy_rest_never_starts_an_outage_timer() {
        let now = Instant::now();
        let mut health = Health::default();
        let overview = healthy();
        let unavailable_image = Err(anyhow::anyhow!(
            "Sync helper inspection unavailable: original image was unlinked by update"
        ));
        for elapsed in [Duration::ZERO, UNREACHABLE * 2, STALE * 2] {
            health.observe(&overview, runtime(&unavailable_image), now + elapsed);
            assert!(
                health.message().is_none(),
                "inspection failure is not a REST outage"
            );
        }
        health.observe(&overview, runtime(&Err(HelperDown.into())), now + STALE * 3);
        assert!(
            health.message().is_some(),
            "a confirmed stopped helper must warn"
        );
        health.observe(&overview, runtime(&unavailable_image), now + STALE * 4);
        assert!(
            health.message().is_none(),
            "live recovery clears even before lifecycle repair"
        );
    }

    #[test]
    fn confirmed_rest_outage_has_grace_and_recovers_independently_of_inspection() {
        let now = Instant::now();
        let mut health = Health::default();
        let overview = healthy();
        let unavailable = Err(RestUnavailable.into());
        health.observe(&overview, runtime(&unavailable), now);
        assert!(health.message().is_none());
        health.observe(
            &overview,
            runtime(&unavailable),
            now + UNREACHABLE + Duration::from_secs(1),
        );
        assert!(health.message().is_some());
        health.observe(
            &overview,
            runtime(&Ok(())),
            now + UNREACHABLE + Duration::from_secs(2),
        );
        assert!(health.message().is_none());
    }

    #[test]
    fn idle_up_to_date_is_healthy_even_with_old_contact_and_every_peer_asleep() {
        let now = Instant::now();
        let mut health = Health::default();
        let mut overview = healthy();
        overview.vaults.values_mut().next().unwrap().connected = false;
        overview.computers.insert(
            uuid::Uuid::nil(),
            direct::ComputerOverview {
                connected: Some(false),
                last_seen: Some("2000-01-01T00:00:00Z".into()),
                ..Default::default()
            },
        );
        health.observe(&overview, Runtime::default(), now);
        health.observe(&overview, Runtime::default(), now + STALE * 10);
        assert!(
            health.message().is_none(),
            "no pending items means no stalled sync"
        );
    }

    #[test]
    fn helper_down_warns_immediately_and_healthy_observation_clears_it() {
        let now = Instant::now();
        let mut health = Health::default();
        let overview = healthy();
        health.observe(
            &overview,
            Runtime {
                down: true,
                unreachable: false,
            },
            now,
        );
        assert!(health.message().unwrap().contains("needs attention"));
        health.observe(&overview, Runtime::default(), now + Duration::from_secs(1));
        assert!(health.message().is_none());
    }

    #[test]
    fn rest_unreachable_requires_two_continuous_minutes_and_recovery_resets_it() {
        let now = Instant::now();
        let mut health = Health::default();
        let mut overview = healthy();
        overview.vaults.values_mut().next().unwrap().available = false;
        health.observe(&overview, Runtime::default(), now);
        health.observe(&overview, Runtime::default(), now + UNREACHABLE);
        assert!(
            health.message().is_none(),
            "startup/update races have a grace period"
        );
        health.observe(
            &overview,
            Runtime::default(),
            now + UNREACHABLE + Duration::from_secs(1),
        );
        assert!(
            health.message().is_some(),
            "persistent REST outage must warn"
        );
        overview.vaults.values_mut().next().unwrap().available = true;
        health.observe(
            &overview,
            Runtime::default(),
            now + UNREACHABLE + Duration::from_secs(2),
        );
        assert!(health.message().is_none());
        overview.vaults.values_mut().next().unwrap().available = false;
        health.observe(&overview, Runtime::default(), now + UNREACHABLE * 2);
        assert!(health.message().is_none(), "new outage gets its own timer");
    }

    #[test]
    fn unavailable_runtime_without_attached_vaults_has_the_same_grace_period() {
        let now = Instant::now();
        let mut health = Health::default();
        let overview = direct::Overview::default();
        let unavailable = Runtime {
            down: false,
            unreachable: true,
        };
        health.observe(&overview, unavailable, now);
        assert!(health.message().is_none());
        health.observe(
            &overview,
            unavailable,
            now + UNREACHABLE + Duration::from_secs(1),
        );
        assert!(health.message().is_some());
        health.observe(
            &overview,
            Runtime::default(),
            now + UNREACHABLE + Duration::from_secs(2),
        );
        assert!(health.message().is_none());
    }

    #[test]
    fn only_disconnected_folders_with_pending_items_warn_after_ten_minutes() {
        let now = Instant::now();
        let mut health = Health::default();
        let mut overview = healthy();
        let vault = overview.vaults.values_mut().next().unwrap();
        vault.state = "Syncing".into();
        vault.needed_items = 1; // A deletion can need zero bytes.
        health.observe(&overview, Runtime::default(), now);
        health.observe(&overview, Runtime::default(), now + STALE * 2);
        assert!(
            health.message().is_none(),
            "connected transfer is not an outage"
        );
        overview.vaults.values_mut().next().unwrap().connected = false;
        health.observe(&overview, Runtime::default(), now + STALE * 2);
        health.observe(&overview, Runtime::default(), now + STALE * 3);
        assert!(health.message().is_none());
        health.observe(
            &overview,
            Runtime::default(),
            now + STALE * 3 + Duration::from_secs(1),
        );
        assert!(health.message().unwrap().contains("10 minutes"));
        overview.vaults.values_mut().next().unwrap().connected = true;
        health.observe(
            &overview,
            Runtime::default(),
            now + STALE * 3 + Duration::from_secs(2),
        );
        assert!(
            health.message().is_none(),
            "reconnection clears without waiting for transfer"
        );
        overview.vaults.values_mut().next().unwrap().connected = false;
        health.observe(&overview, Runtime::default(), now + STALE * 4);
        assert!(
            health.message().is_none(),
            "reconnection resets the disconnect timer"
        );
        overview.vaults.values_mut().next().unwrap().needed_items = 0;
        health.observe(&overview, Runtime::default(), now + STALE * 6);
        assert!(
            health.message().is_none(),
            "completed folder clears while peers sleep"
        );
    }

    #[test]
    fn folder_errors_warn_immediately_and_clear_automatically() {
        let now = Instant::now();
        let mut health = Health::default();
        let mut overview = healthy();
        overview.vaults.values_mut().next().unwrap().errors = true;
        health.observe(&overview, Runtime::default(), now);
        assert!(health.message().is_some());
        overview.vaults.values_mut().next().unwrap().errors = false;
        health.observe(&overview, Runtime::default(), now + Duration::from_secs(1));
        assert!(health.message().is_none());
    }
}
