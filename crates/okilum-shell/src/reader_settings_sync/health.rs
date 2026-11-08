//! Sync failures belong in every main window, independently of Settings.
use super::*;
use std::collections::BTreeMap;
const STALE: Duration = Duration::from_secs(10 * 60);
#[derive(Default)]
pub(super) struct Health {
    down: bool,
    stalled: BTreeMap<PathBuf, (Instant, u64)>,
    stale: bool,
}
impl Health {
    pub(super) fn failed(&mut self) {
        self.down = true;
    }
    pub(super) fn observe(&mut self, overview: &direct::Overview, now: Instant) {
        self.down = overview.vaults.values().any(|vault| !vault.available);
        let connected = overview
            .computers
            .values()
            .any(|computer| computer.connected == Some(true));
        self.stalled
            .retain(|path, _| overview.vaults.contains_key(path));
        self.stale = false;
        let old_contact = !connected
            && overview
                .computers
                .values()
                .filter_map(|computer| computer.last_seen.as_deref())
                .filter_map(|seen| chrono::DateTime::parse_from_rfc3339(seen).ok())
                .any(|seen| {
                    chrono::Utc::now().signed_duration_since(seen).num_seconds()
                        > STALE.as_secs() as i64
                });
        for (path, vault) in &overview.vaults {
            if vault.available
                && vault.state == "Up to date"
                && (vault.connected || vault.shared_with.is_empty())
            {
                self.stalled.remove(path);
                continue;
            }
            let entry = self
                .stalled
                .entry(path.clone())
                .or_insert((now, vault.needed_bytes));
            if vault.needed_bytes < entry.1 {
                entry.0 = now;
            }
            entry.1 = vault.needed_bytes;
            self.stale |= now.saturating_duration_since(entry.0) > STALE
                || old_contact && !vault.shared_with.is_empty();
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

#[cfg(all(target_os = "macos", not(test)))]
pub(super) fn check_managed() -> anyhow::Result<()> {
    if !managed_enabled() {
        return Ok(());
    }
    inspect_managed()
}

#[cfg(all(target_os = "macos", not(test)))]
pub(super) fn check_managed_with_recovery(
    check: impl Fn() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Sparkle can unlink the previous bundle while its signed helper remains
    // alive. Reuse the existing guarded restart once per app process; never
    // rebind replica witnesses or treat a restart request as healthy Sync.
    static RECOVERY_REQUESTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    recover_managed_once(check_managed(), &RECOVERY_REQUESTED, &check, || {
        restart_managed_guarded(&check)
    })
}

#[cfg(any(test, target_os = "macos"))]
fn recover_managed_once(
    observed: anyhow::Result<()>,
    requested: &std::sync::atomic::AtomicBool,
    check_request: impl FnOnce() -> anyhow::Result<()>,
    restart: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let Err(error) = observed else {
        return Ok(());
    };
    check_request()?;
    if !requested.swap(true, std::sync::atomic::Ordering::SeqCst) {
        use anyhow::Context as _;
        restart().context("Could not restart the managed Sync helper")?;
    }
    // The helper may take time to renew its generation. Keep the original
    // failure visible until the next authenticated helper + REST observation.
    Err(error)
}

#[cfg(all(target_os = "macos", not(test)))]
fn inspect_managed() -> anyhow::Result<()> {
    use okilum_sync_controller::{
        daemon,
        sidecar::{discovery, macos::account_state_directory},
    };
    // REST alone can be an orphan runtime: also authenticate the helper.
    use anyhow::Context as _;
    use okilum_sync_controller::sidecar::{
        macos::connection::NativeSupervisor, store::UnixStore,
        supervisor::ipc::code_requirement::CodeRequirement,
    };
    let state = account_state_directory()?;
    let binding = {
        let store = UnixStore::open_existing(&state)?;
        let tx = store.begin(Instant::now() + Duration::from_secs(2))?;
        tx.current()?
            .context("Sync journal unavailable")?
            .binding()
            .clone()
    };
    let own = std::env::current_exe()?.canonicalize()?;
    let app = own
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("app bundle unavailable")?;
    let team = option_env!("OKILUM_SIGNING_TEAM_ID").context("Sync signing policy unavailable")?;
    let identifier = option_env!("OKILUM_BUNDLE_ID").context("Sync signing policy unavailable")?;
    let supervisor = NativeSupervisor::open(
        app,
        "okilum",
        binding,
        CodeRequirement::team_and_identifier(team, identifier)?,
        CodeRequirement::team_and_identifier(team, &format!("{identifier}.sync"))?,
        Duration::from_secs(2),
    )?;
    anyhow::ensure!(
        matches!(supervisor.inspect()?, discovery::Discovered::Live(_)),
        "Sync helper is not responding"
    );
    let config = discovery::managed_configuration_path(&state)?
        .ok_or_else(|| anyhow::anyhow!("enabled Sync has no managed configuration"))?;
    let discovered = daemon::discover(&[config]);
    anyhow::ensure!(
        !discovered.candidates.is_empty(),
        "Syncthing REST unavailable for enabled Sync"
    );
    Ok(())
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
    restart_managed_guarded(|| Ok(()))
}

#[cfg(target_os = "macos")]
fn restart_managed_guarded(check: impl Fn() -> anyhow::Result<()>) -> anyhow::Result<()> {
    check()?;
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
    let revalidate = || -> anyhow::Result<()> {
        check()?;
        let tx = store.begin(Instant::now() + Duration::from_secs(2))?;
        let saved = tx.current()?.context("Sync journal unavailable")?;
        anyhow::ensure!(
            saved.binding() == &binding
                && saved.intent() == Intent::Enabled
                && saved.stop().is_none()
                && saved.update().is_none(),
            "Sync ownership or lifecycle changed before helper recovery"
        );
        Ok(())
    };
    // Older installed helpers have no SIGTERM cleanup. Stop only the
    // authenticated account runtime before replacing that helper, so its
    // surviving child cannot permanently block the new supervisor lease.
    if let Some(config) =
        okilum_sync_controller::sidecar::discovery::managed_configuration_path(&state)?
    {
        let discovered = okilum_sync_controller::daemon::discover(&[config]);
        for candidate in discovered.candidates {
            let client = candidate.identity.connect()?;
            revalidate()?;
            client.shutdown(&binding.device_identity)?;
        }
    }
    let target = format!("gui/{}/com.befeast.okilum.sync", binding.owner);
    // TERM lets the new helper flush its owned tree. -k would SIGKILL it and
    // leave an orphan runtime, which the lifetime lease correctly refuses.
    revalidate()?;
    let stopped = bounded_request(
        std::process::Command::new("/bin/launchctl").args(["kill", "SIGTERM", &target]),
        Duration::from_secs(5),
    )?;
    if stopped == Some(false) {
        revalidate()?;
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
    #[test]
    fn managed_recovery_requests_once_and_waits_for_authenticated_health() {
        use std::{cell::Cell, sync::atomic::AtomicBool};
        let requested = AtomicBool::new(false);
        let requests = Cell::new(0);
        for _ in 0..3 {
            let result = recover_managed_once(
                Err(anyhow::anyhow!("Sync helper is not responding")),
                &requested,
                || Ok(()),
                || {
                    requests.set(requests.get() + 1);
                    Ok(())
                },
            );
            assert_eq!(
                result.unwrap_err().to_string(),
                "Sync helper is not responding"
            );
        }
        assert_eq!(
            requests.get(),
            1,
            "a stale helper needs one bounded recovery request"
        );
        recover_managed_once(
            Ok(()),
            &requested,
            || Ok(()),
            || panic!("authenticated recovery must not restart the healthy helper"),
        )
        .unwrap();
        assert_eq!(requests.get(), 1);
    }

    #[test]
    fn healthy_managed_sync_does_not_consume_recovery_or_restart() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let requested = AtomicBool::new(false);
        recover_managed_once(
            Ok(()),
            &requested,
            || Ok(()),
            || panic!("healthy Sync must stay running"),
        )
        .unwrap();
        assert!(!requested.load(Ordering::SeqCst));
    }

    #[test]
    fn revoked_managed_recovery_ticket_has_no_effect_and_preserves_retry() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let epoch = Epoch::default();
        let ticket = epoch.ticket();
        epoch.invalidate();
        let requested = AtomicBool::new(false);
        let result = recover_managed_once(
            Err(anyhow::anyhow!("helper unavailable")),
            &requested,
            || {
                ticket.check()?;
                Ok(())
            },
            || panic!("a revoked worker must not restart Sync"),
        );
        assert!(result.unwrap_err().to_string().contains("superseded"));
        assert!(!requested.load(Ordering::SeqCst));
    }

    #[test]
    fn refused_managed_recovery_stays_failed_without_repeated_requests() {
        use std::sync::atomic::AtomicBool;
        let requested = AtomicBool::new(false);
        let result = recover_managed_once(
            Err(anyhow::anyhow!("helper unavailable")),
            &requested,
            || Ok(()),
            || anyhow::bail!("bundle ownership could not be verified"),
        );
        let text = format!("{:#}", result.unwrap_err());
        assert!(text.contains("bundle ownership could not be verified"));
        assert!(recover_managed_once(
            Err(anyhow::anyhow!("helper unavailable")),
            &requested,
            || Ok(()),
            || panic!("refused ownership cannot cause a restart loop")
        )
        .is_err());
    }
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

    #[test]
    fn missing_runtime_is_immediate_and_connected_success_clears_the_alert() {
        let now = Instant::now();
        let mut health = Health::default();
        let mut overview = direct::Overview::default();
        overview
            .vaults
            .insert(PathBuf::from("/synthetic"), Default::default());
        health.observe(&overview, now);
        assert!(health.message().unwrap().contains("needs attention"));
        let vault = overview.vaults.values_mut().next().unwrap();
        vault.available = true;
        vault.state = "Up to date".into();
        health.observe(&overview, now);
        assert!(health.message().is_none());
        health.failed();
        assert!(health.message().is_some());
    }
    #[test]
    fn introduced_mesh_peer_keeps_the_vault_healthy_when_the_named_computer_sleeps() {
        let now = Instant::now();
        let mut health = Health::default();
        let mut overview = direct::Overview::default();
        overview.computers.insert(
            uuid::Uuid::nil(),
            direct::ComputerOverview {
                connected: Some(false),
                last_seen: Some("2000-01-01T00:00:00Z".into()),
                ..Default::default()
            },
        );
        overview.vaults.insert(
            PathBuf::from("/synthetic"),
            direct::VaultOverview {
                available: true,
                connected: true,
                state: "Up to date".into(),
                shared_with: vec!["Introduced computer".into()],
                ..Default::default()
            },
        );
        health.observe(&overview, now);
        health.observe(&overview, now + STALE + Duration::from_secs(1));
        assert!(
            health.message().is_none(),
            "the named inviter is not the only Sync transport"
        );
        overview.vaults.values_mut().next().unwrap().connected = false;
        health.observe(&overview, now + STALE + Duration::from_secs(2));
        assert!(
            health.message().is_some(),
            "negative control: losing every carrier is stale"
        );
    }

    #[test]
    fn offline_or_stalled_vault_warns_after_ten_minutes_and_progress_resets_timer() {
        let now = Instant::now();
        let mut health = Health::default();
        let mut overview = direct::Overview::default();
        overview.vaults.insert(
            PathBuf::from("/synthetic"),
            direct::VaultOverview {
                available: true,
                state: "Syncing".into(),
                needed_bytes: 100,
                ..Default::default()
            },
        );
        health.observe(&overview, now);
        assert!(health.message().is_none());
        health.observe(&overview, now + STALE);
        assert!(health.message().is_none());
        health.observe(&overview, now + STALE + Duration::from_secs(1));
        assert!(health.message().unwrap().contains("10 minutes"));
        overview.vaults.values_mut().next().unwrap().needed_bytes = 50;
        health.observe(&overview, now + STALE + Duration::from_secs(2));
        assert!(health.message().is_none());
    }
}
