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
                && (connected || vault.shared_with.is_empty())
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
#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
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
    let target = format!("gui/{}/com.befeast.okilum.sync", binding.owner);
    // TERM lets the new helper flush its owned tree. -k would SIGKILL it and
    // leave an orphan runtime, which the lifetime lease correctly refuses.
    let stopped = std::process::Command::new("/bin/launchctl")
        .args(["kill", "SIGTERM", &target])
        .output()?;
    if !stopped.status.success() {
        let started = std::process::Command::new("/bin/launchctl")
            .args(["kickstart", &target])
            .output()?;
        anyhow::ensure!(
            started.status.success(),
            "Sync helper restart failed: {}",
            String::from_utf8_lossy(&started.stderr)
        );
    }
    Ok(())
}
