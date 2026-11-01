// Shared desktop Sync settings. All controller IO runs off the GPUI thread.
use super::*;
use gpui_component::input::{Input, InputState};
use okilum_sync_controller::direct::{self, Controller, ExistingVault, Snapshot};

struct BackgroundSync(Entity<SyncSettings>);
impl Global for BackgroundSync {}
pub(super) fn shared(window: &mut Window, cx: &mut App) -> Entity<SyncSettings> {
    if let Some(background) = cx.try_global::<BackgroundSync>() {
        let view = background.0.clone();
        if view.read(cx).snapshot.is_none() && !view.read(cx).busy {
            view.update(cx, |view, cx| view.run(Operation::Read, cx));
        }
        return view;
    }
    let view = cx.new(|cx| SyncSettings::new(window, cx));
    cx.set_global(BackgroundSync(view.clone()));
    view
}
pub(super) fn suspend(cx: &mut App) {
    if let Some(view) = cx.try_global::<BackgroundSync>().map(|s| s.0.clone()) {
        view.update(cx, |view, cx| {
            view.epoch += 1;
            if let Some(controller) = view.controller.take() {
                controller.shutdown();
            }
            view.snapshot = None;
            view.inspected = None;
            view.busy = false;
            view.error = None;
            cx.notify();
        });
    }
}

enum Operation {
    Read,
    Begin(String),
    Redeem(String, String),
    Approve(uuid::Uuid),
    Reject(uuid::Uuid),
    Cancel,
    Catalog(uuid::Uuid),
    Inspect(PathBuf),
    Consent(ExistingVault),
    Attach(uuid::Uuid, String, PathBuf),
}
struct Output {
    controller: Controller,
    snapshot: Snapshot,
    inspected: Option<ExistingVault>,
}
pub(super) struct SyncSettings {
    controller: Option<Controller>,
    snapshot: Option<Snapshot>,
    inspected: Option<ExistingVault>,
    name: Entity<InputState>,
    invitation: Entity<InputState>,
    selected_peer: Option<uuid::Uuid>,
    busy: bool,
    epoch: u64,
    error: Option<String>,
}
impl SyncSettings {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            controller: None,
            snapshot: None,
            inspected: None,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("This computer’s name")),
            invitation: cx.new(|cx| InputState::new(window, cx).placeholder("Paste pairing URL")),
            selected_peer: None,
            busy: false,
            epoch: 0,
            error: None,
        };
        this.run(Operation::Read, cx);
        this
    }
    fn run(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if self.busy || !reader_features::enabled(reader_features::Feature::Sync, cx) {
            return;
        }
        self.epoch += 1;
        let epoch = self.epoch;
        self.busy = true;
        self.error = None;
        if matches!(operation, Operation::Inspect(_) | Operation::Cancel) {
            self.inspected = None;
        }
        let controller = self.controller.clone();
        let access = reader_features::access(reader_features::Feature::Sync, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            okilum_t3::scope_network(access, async move {
                let result = cx
                    .background_executor()
                    .spawn(okilum_t3::scope_current_network(async move {
                        perform(controller, operation)
                    }))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.epoch != epoch
                        || !reader_features::enabled(reader_features::Feature::Sync, cx)
                    {
                        return;
                    }
                    this.busy = false;
                    match result {
                        Ok(output) => {
                            this.controller = Some(output.controller);
                            if this.selected_peer.is_none()
                                && !output.snapshot.catalog.is_empty()
                                && output.snapshot.peers.len() == 1
                            {
                                this.selected_peer = Some(output.snapshot.peers[0].id);
                            }
                            this.snapshot = Some(output.snapshot);
                            if let Some(inspected) = output.inspected {
                                this.inspected = Some(inspected);
                            }
                        }
                        Err(error) => {
                            // Do not echo invitation URLs, keys or remote errors into logs.
                            let message = error.to_string().to_lowercase();
                            if message.contains("expired")
                                || message.contains("reject")
                                || message.contains("cancel")
                            {
                                if let Some(snapshot) = this.snapshot.as_mut() {
                                    snapshot.pending = None;
                                    snapshot.offer_url = None;
                                    snapshot.status = "Pairing did not complete".into();
                                }
                            }
                            this.error = Some(user_error(&error));
                        }
                    }
                    cx.notify();
                    this.schedule_refresh(cx);
                });
            })
            .await
        })
        .detach();
    }
    fn schedule_refresh(&self, cx: &mut Context<Self>) {
        if !self.snapshot.as_ref().is_some_and(sync_refresh_needed) || self.error.is_some() {
            return;
        }
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.epoch == epoch && !this.busy {
                    this.run(Operation::Read, cx);
                }
            });
        })
        .detach();
    }
    fn start_pairing(&mut self, redeem: bool, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        if direct::validate_name(&name).is_err() {
            self.error = Some(
                "Give this computer a name of up to 100 bytes without control characters.".into(),
            );
            cx.notify();
            return;
        }
        if redeem {
            let invitation = self.invitation.read(cx).value().trim().to_string();
            if invitation.is_empty() {
                self.error = Some("Paste the pairing URL from your other computer.".into());
                cx.notify();
                return;
            }
            if let Err(error) = direct::validate_pairing_input(&invitation, &name) {
                self.error = Some(user_error(&error));
                cx.notify();
                return;
            }
            self.run(Operation::Redeem(invitation, name), cx);
        } else {
            self.run(Operation::Begin(name), cx);
        }
    }
    fn choose_folder(&mut self, remote: Option<(uuid::Uuid, String)>, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                if remote.is_some() {
                    "Choose a local folder for this vault"
                } else {
                    "Choose an existing synced vault"
                }
                .into(),
            ),
        });
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picker.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| {
                        if this.epoch != epoch {
                            return;
                        }
                        this.run(
                            match remote {
                                Some((peer, vault)) => Operation::Attach(peer, vault, path),
                                None => Operation::Inspect(path),
                            },
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
    }
}
fn sync_refresh_needed(snapshot: &Snapshot) -> bool {
    snapshot.offer_url.is_some() || snapshot.pending.is_some() || snapshot.preparing
}
fn locations() -> anyhow::Result<(PathBuf, Vec<PathBuf>)> {
    use anyhow::Context as _;
    #[cfg(target_os = "linux")]
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .context("home unavailable")?;
    let configs = direct::configuration_paths()?;
    #[cfg(target_os = "linux")]
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"))
        .join("okilum/sync/direct");
    #[cfg(target_os = "linux")]
    for parent in [state.parent().and_then(Path::parent), state.parent()] {
        ensure_private_sync_directory(parent.context("Sync state parent unavailable")?)?;
    }
    #[cfg(target_os = "macos")]
    let state =
        mac_direct_state(&okilum_sync_controller::sidecar::macos::account_state_directory()?)?;
    Ok((state, configs))
}
fn ensure_private_sync_directory(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o077 == 0,
        "private Sync state parent required"
    );
    Ok(())
}
#[cfg(any(target_os = "macos", test))]
fn mac_direct_state(managed: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context as _;
    let parent = managed.parent().context("Sync state parent unavailable")?;
    ensure_private_sync_directory(parent)?;
    Ok(parent.join("SyncDirect"))
}
fn perform(controller: Option<Controller>, operation: Operation) -> anyhow::Result<Output> {
    anyhow::ensure!(
        okilum_t3::NetworkAccess::current().allowed(),
        "Sync is disabled"
    );
    // Validate before filesystem discovery and macOS runtime bootstrap.
    match &operation {
        Operation::Begin(name) => direct::validate_name(name)?,
        Operation::Redeem(url, name) => direct::validate_pairing_input(url, name)?,
        _ => {}
    }
    let (state, configs) = locations()?;
    let controller = match controller {
        Some(c) => c,
        None => Controller::open(state, configs.clone())?,
    };
    #[cfg(target_os = "macos")]
    if matches!(&operation, Operation::Begin(_) | Operation::Redeem(_, _))
        && controller.configuration_paths()?.is_empty()
    {
        connect_managed_runtime(&controller)?;
    }
    let mut inspected = None;
    let snapshot = match operation {
        Operation::Read => controller.refresh()?,
        Operation::Begin(name) => controller.begin_pairing(&name)?,
        Operation::Redeem(url, name) => controller.redeem(&url, &name)?,
        Operation::Approve(id) => controller.approve(id)?,
        Operation::Reject(id) => controller.reject(id)?,
        Operation::Cancel => controller.cancel()?,
        Operation::Catalog(peer) => {
            let catalog = controller.catalog(peer)?;
            let mut snapshot = controller.snapshot()?;
            snapshot.catalog = catalog;
            snapshot
        }
        Operation::Inspect(path) => {
            inspected = Some(direct::inspect_existing(
                &controller.configuration_paths()?,
                &path,
            )?);
            controller.snapshot()?
        }
        Operation::Consent(vault) => {
            controller.attach_existing(&vault, true)?;
            controller.snapshot()?
        }
        Operation::Attach(peer, vault, path) => controller.attach(peer, &vault, &path)?,
    };
    Ok(Output {
        controller,
        snapshot,
        inspected,
    })
}
#[cfg(target_os = "macos")]
fn connect_managed_runtime(controller: &Controller) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use okilum_sync_controller::sidecar::{
        macos::bootstrap::{connect_current_app, ConnectOutcome, SigningPolicy},
        supervisor::ipc::code_requirement::CodeRequirement,
    };
    let team = option_env!("OKILUM_SIGNING_TEAM_ID").context("Sync signing policy unavailable")?;
    let identifier = option_env!("OKILUM_BUNDLE_ID").context("Sync bundle policy unavailable")?;
    let policy = SigningPolicy {
        app: CodeRequirement::team_and_identifier(team, identifier)?,
        helper: CodeRequirement::team_and_identifier(team, &format!("{identifier}.sync"))?,
        runtime: CodeRequirement::team_and_identifier(team, &format!("{identifier}.syncthing"))?,
    };
    match connect_current_app(true, policy, Duration::from_secs(45))? {
        ConnectOutcome::Ready {
            config_path,
            device_id,
        } => {
            // Only authenticated REST-ready identity becomes a usable direct target.
            anyhow::ensure!(!device_id.is_empty(), "Sync device identity unavailable");
            controller.set_configurations(vec![config_path])?;
            Ok(())
        }
        ConnectOutcome::RequiresApproval { .. } => {
            anyhow::bail!("macOS Sync approval required; Connect again after approval")
        }
    }
}
fn user_error(error: &anyhow::Error) -> String {
    let text = error.to_string().to_lowercase();
    if text.contains("approval") {
        "Allow Okilum Sync in macOS System Settings → General → Login Items, then choose Connect again.".into()
    } else if text.contains("signing") || text.contains("bundle") || text.contains("runtime") {
        "A verified Sync runtime is not available in this build. Install a signed beta that includes Sync and try again.".into()
    } else if text.contains("nonempty") || text.contains("non-empty") {
        "This folder already contains files. Lossless merge preview is required; choose an empty folder to continue.".into()
    } else if text.contains("expired") {
        "This pairing URL expired. Create a new one on the other computer.".into()
    } else if text.contains("daemon")
        || text.contains("configuration")
        || text.contains("syncthing")
    {
        "No compatible Syncthing connection is available. Select the existing instance explicitly, or check its connection and refresh.".into()
    } else if text.contains("reject") {
        "The other computer declined pairing. Create a new pairing URL to try again.".into()
    } else {
        "Sync could not finish this step. Check that both computers are reachable and try again."
            .into()
    }
}

#[cfg(test)]
#[path = "reader_settings_sync/tests.rs"]
mod tests;
#[path = "reader_settings_sync/view.rs"]
mod view;
