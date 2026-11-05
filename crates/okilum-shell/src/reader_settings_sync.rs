// Shared desktop Sync settings. All controller IO runs off the GPUI thread.
use super::*;
use epoch::{Epoch, Ticket};
use gpui_component::input::{Input, InputState};
use okilum_sync_controller::direct::{self, Controller, ExistingVault, Snapshot};

#[cfg(target_os = "linux")]
pub(crate) use linux_runtime::managed_service_directory;

#[cfg(target_os = "linux")]
type DaemonIdentityForNetwork = okilum_sync_controller::daemon::DaemonIdentity;

struct BackgroundSync(Entity<SyncSettings>);
impl Global for BackgroundSync {}
pub(super) fn shared(window: &mut Window, cx: &mut App) -> Entity<SyncSettings> {
    if let Some(background) = cx.try_global::<BackgroundSync>() {
        let view = background.0.clone();
        if view.read(cx).snapshot.is_none() && !view.read(cx).busy {
            view.update(cx, |view, cx| view.run(Operation::EnterPairing, cx));
        }
        return view;
    }
    let view = cx.new(|cx| SyncSettings::new(window, cx));
    cx.set_global(BackgroundSync(view.clone()));
    view
}
/// Reuse an already running controller; a conflict never starts Sync or
/// discovers credentials on the UI thread merely to decorate a notice.
pub(super) fn active_controller(cx: &App) -> Option<Controller> {
    cx.try_global::<BackgroundSync>()?
        .0
        .read(cx)
        .controller
        .clone()
}
pub(super) fn suspend(cx: &mut App) {
    if let Some(view) = cx.try_global::<BackgroundSync>().map(|s| s.0.clone()) {
        view.update(cx, |view, cx| {
            view.epoch.invalidate();
            view.lifetime.invalidate();
            if let Some(controller) = view.controller.take() {
                controller.shutdown();
            }
            view.snapshot = None;
            view.inspected = None;
            view.busy = false;
            view.error = None;
            view.awaiting_open = None;
            view.pending_destination = None;
            view.destination_error = None;
            view.presentation = Default::default();
            cx.notify();
        });
    }
}

#[derive(Clone)]
enum Operation {
    Read,
    EnterPairing,
    #[cfg(target_os = "linux")]
    SelectConnection(PathBuf),
    #[cfg(target_os = "linux")]
    Network(
        DaemonIdentityForNetwork,
        okilum_sync_controller::network_policy::Policy,
    ),
    #[cfg(target_os = "linux")]
    Share(PathBuf),
    Begin(String),
    Redeem(String, String),
    Approve(uuid::Uuid),
    Reject(uuid::Uuid),
    Cancel,
    Catalog(uuid::Uuid),
    Inspect(PathBuf),
    Consent(ExistingVault),
    Attach(uuid::Uuid, String, PathBuf),
    AttachNew(uuid::Uuid, String, PathBuf),
}
#[cfg(test)]
#[derive(Clone)]
struct TestWorker(std::sync::Arc<dyn Fn(Operation) -> anyhow::Result<Output> + Send + Sync>);
#[cfg(test)]
impl Global for TestWorker {}

struct Output {
    overview: direct::Overview,
    connection_error: Option<String>,
    controller: Controller,
    snapshot: Snapshot,
    inspected: Option<ExistingVault>,
    selected_destination: Option<PathBuf>,
    #[cfg(target_os = "linux")]
    runtimes: Vec<linux_runtime::Choice>,
    #[cfg(target_os = "linux")]
    vaults: Vec<PathBuf>,
}
pub(super) struct SyncSettings {
    presentation: presentation::Presentation,
    controller: Option<Controller>,
    snapshot: Option<Snapshot>,
    inspected: Option<ExistingVault>,
    #[cfg(target_os = "linux")]
    runtimes: Vec<linux_runtime::Choice>,
    #[cfg(target_os = "linux")]
    vaults: Vec<PathBuf>,
    name: Entity<InputState>,
    invitation: Entity<InputState>,
    selected_peer: Option<uuid::Uuid>,
    busy: bool,
    epoch: Epoch,
    lifetime: Epoch,
    error: Option<String>,
    awaiting_open: Option<PathBuf>,
    pending_destination: Option<Operation>,
    destination_error: Option<(String, String)>,
    receiving: bool,
    scroll: ScrollHandle,
    #[cfg(target_os = "linux")]
    conflicts:
        std::collections::BTreeMap<PathBuf, Entity<crate::reader_sync_conflicts::InventoryView>>,
}
impl SyncSettings {
    pub(super) fn receive(&mut self, cx: &mut Context<Self>) {
        self.receiving = true;
        self.presentation.wizard = Some(presentation::Step::Paste);
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            presentation: Default::default(),
            #[cfg(target_os = "linux")]
            runtimes: vec![],
            #[cfg(target_os = "linux")]
            vaults: vec![],
            controller: None,
            snapshot: None,
            inspected: None,
            name: cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder("This computer’s name");
                input.set_value(default_computer_name(), window, cx);
                input
            }),
            invitation: cx.new(|cx| InputState::new(window, cx).placeholder("Paste pairing URL")),
            selected_peer: None,
            busy: false,
            epoch: Epoch::default(),
            lifetime: Epoch::default(),
            error: None,
            awaiting_open: None,
            pending_destination: None,
            destination_error: None,
            receiving: false,
            scroll: ScrollHandle::new(),
            #[cfg(target_os = "linux")]
            conflicts: Default::default(),
        };
        this.run(Operation::EnterPairing, cx);
        this
    }
    fn run(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if self.busy || !reader_features::enabled(reader_features::Feature::Sync, cx) {
            return;
        }
        let destination_vault = match &operation {
            Operation::Attach(_, vault, _) | Operation::AttachNew(_, vault, _) => {
                self.destination_error = None;
                Some(vault.clone())
            }
            _ => None,
        };
        let refresh = matches!(operation, Operation::Read);
        if !refresh {
            self.presentation.retry = Some(operation.clone());
            self.presentation.details = false;
            self.presentation.error_details = None;
        }
        if matches!(operation, Operation::Redeem(..)) {
            self.presentation.wizard = Some(presentation::Step::Waiting);
        }
        if matches!(operation, Operation::Attach(..) | Operation::AttachNew(..)) {
            self.presentation.wizard = Some(presentation::Step::Progress);
        }
        let action = operation.action_name();
        let trace = reader_diagnostics::trace(cx);
        if !refresh {
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        let epoch = self.epoch.begin();
        self.busy = true;
        self.error = None;
        if matches!(operation, Operation::Inspect(_) | Operation::Cancel) {
            self.inspected = None;
        }
        let controller = self.controller.clone();
        // Pairing observes the authenticated channel/journal, not a repeated
        // system inventory or executable hash. Vault operations retain full checks.
        let pairing_action = match &operation {
            Operation::Read | Operation::Approve(_) | Operation::Reject(_) | Operation::Cancel => {
                self.snapshot.as_ref().is_some_and(sync_pairing_in_progress)
            }
            Operation::Catalog(_) => true,
            _ => false,
        };
        let pairing_poll = if pairing_action {
            self.controller
                .as_ref()
                .zip(self.snapshot.as_ref())
                .map(|(controller, snapshot)| Output {
                    overview: self.presentation.overview.clone(),
                    connection_error: None,
                    controller: controller.clone(),
                    snapshot: snapshot.clone(),
                    inspected: None,
                    selected_destination: None,
                    #[cfg(target_os = "linux")]
                    runtimes: self.runtimes.clone(),
                    #[cfg(target_os = "linux")]
                    vaults: self.vaults.clone(),
                })
        } else {
            None
        };

        #[cfg(test)]
        let worker = cx.try_global::<TestWorker>().cloned();
        let access = reader_features::access(reader_features::Feature::Sync, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let worker_epoch = epoch.clone();
            okilum_t3::scope_network(access, async move {
                let result = cx
                    .background_executor()
                    .spawn(okilum_t3::scope_current_network(async move {
                        #[cfg(test)]
                        if let Some(worker) = worker {
                            return worker_epoch.run(|| (worker.0)(operation))?;
                        }
                        if let Some(mut output) = pairing_poll {
                            let guard = ControllerGuard {
                                ticket: worker_epoch,
                                controller: output.controller.clone(),
                            };
                            guard.check()?;
                            output.snapshot = match operation {
                                Operation::Approve(id) => output.controller.approve(id)?,
                                Operation::Reject(id) => output.controller.reject(id)?,
                                Operation::Cancel => output.controller.cancel()?,
                                Operation::Read => output.controller.refresh_pairing()?,
                                Operation::Catalog(peer) => {
                                    let catalog = output.controller.catalog(peer)?;
                                    let mut snapshot = output.controller.snapshot()?;
                                    snapshot.catalog = catalog;
                                    snapshot
                                }
                                _ => unreachable!(
                                    "only pairing journal actions use cached runtime metadata"
                                ),
                            };
                            guard.check()?;
                            return Ok(output);
                        }
                        perform_scoped(controller, operation, &worker_epoch)
                    }))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if !epoch.is_current()
                        || !reader_features::enabled(reader_features::Feature::Sync, cx)
                    {
                        if let Ok(output) = result {
                            output.controller.shutdown();
                        }
                        return;
                    }
                    this.busy = false;
                    let succeeded = result.is_ok();
                    match result {
                        Ok(output) => {
                            this.publish(output, cx);
                        }
                        Err(error) => {
                            this.presentation.transfers.clear();
                            this.presentation.error_details = failure_details(action, &error)
                                ["error"]
                                .as_str()
                                .map(str::to_owned);
                            if let Some(trace) = &trace {
                                trace.event("sync_action_failed", failure_details(action, &error));
                            }
                            if !refresh {
                                this.awaiting_open = None;
                            }
                            // Do not echo invitation URLs, keys or remote errors into logs.
                            let message = error.to_string().to_lowercase();
                            if message.contains("expired")
                                || message.contains("reject")
                                || message.contains("cancel")
                            {
                                if message.contains("expired") {
                                    this.presentation.wizard = Some(presentation::Step::Expired);
                                } else {
                                    this.presentation.wizard = Some(presentation::Step::Paste);
                                }
                                if let Some(snapshot) = this.snapshot.as_mut() {
                                    snapshot.pending = None;
                                    snapshot.offer_url = None;
                                    snapshot.status = "Pairing did not complete".into();
                                }
                            }
                            let message = user_error(&error);
                            if let Some(vault) = destination_vault {
                                this.presentation.wizard = Some(presentation::Step::Vaults);
                                this.destination_error = Some((vault, message.clone()));
                            }
                            this.error = Some(message);
                        }
                    }
                    cx.notify();
                    if let Some(operation) = this.pending_destination.take() {
                        this.run(operation, cx);
                    } else if succeeded && !this.presentation.transfers.is_empty() {
                        let operation = this.presentation.transfers.pop_front().unwrap();
                        this.run(operation, cx);
                    } else {
                        this.schedule_refresh(cx);
                    }
                });
            })
            .await
        })
        .detach();
    }
    fn publish(&mut self, output: Output, cx: &mut Context<Self>) {
        self.presentation.overview = output.overview;
        if let Some(pending) = output.snapshot.pending.as_ref().filter(|p| p.incoming) {
            if self.presentation.notified != Some(pending.request_id) {
                self.presentation.notified = Some(pending.request_id);
                let name = pending.name.clone();
                cx.background_executor()
                    .spawn(async move {
                        presentation::notify_approval(&name);
                    })
                    .detach();
            }
        }
        if self.presentation.wizard.is_some() {
            if output.snapshot.offer_url.is_some() && output.snapshot.pending.is_none() {
                self.presentation.wizard = Some(presentation::Step::Link);
            } else if output
                .snapshot
                .pending
                .as_ref()
                .is_some_and(|p| !p.incoming)
            {
                self.presentation.wizard = Some(presentation::Step::Waiting);
            } else if !output.snapshot.catalog.is_empty() {
                if self.presentation.wizard != Some(presentation::Step::Progress) {
                    self.presentation.wizard = Some(presentation::Step::Vaults);
                }
            } else if self.presentation.wizard == Some(presentation::Step::Link)
                && self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.peers.len() < output.snapshot.peers.len())
            {
                self.presentation.wizard = None;
            } else if self.presentation.wizard == Some(presentation::Step::Link)
                && output.snapshot.pending.is_none()
                && self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.offer_url.is_some())
                && self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.peers.len() == output.snapshot.peers.len())
            {
                self.presentation.wizard = Some(presentation::Step::Expired);
            }
        }
        self.error = output.connection_error;
        // Refreshes must not pull someone away while they read an unchanged step.
        let changed = self.snapshot.as_ref().is_none_or(|old| {
            old.status != output.snapshot.status
                || old.pending.as_ref().map(|p| (p.request_id, p.incoming))
                    != output
                        .snapshot
                        .pending
                        .as_ref()
                        .map(|p| (p.request_id, p.incoming))
                || old.offer_url != output.snapshot.offer_url
                || old.peers.iter().map(|p| p.id).collect::<Vec<_>>()
                    != output
                        .snapshot
                        .peers
                        .iter()
                        .map(|p| p.id)
                        .collect::<Vec<_>>()
                || old.catalog != output.snapshot.catalog
        });
        if changed {
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        #[cfg(target_os = "linux")]
        {
            self.runtimes = output.runtimes;
            self.vaults = output.vaults;
        }
        super::vault_name::remember_sync(
            output
                .snapshot
                .attached
                .iter()
                .map(|v| (v.path.clone(), v.label.clone()))
                .collect(),
            cx,
        );
        self.controller = Some(output.controller);
        if let Some(path) = output.selected_destination {
            self.awaiting_open = Some(path);
        }
        if self.selected_peer.is_none()
            && !output.snapshot.catalog.is_empty()
            && output.snapshot.peers.len() == 1
        {
            self.selected_peer = Some(output.snapshot.peers[0].id);
        }
        if self.awaiting_open.as_ref().is_some_and(|path| {
            !output.snapshot.preparing && output.snapshot.attached.iter().any(|v| &v.path == path)
        }) {
            if let Ok(directory) = reader_history::state_directory() {
                let path = self.awaiting_open.take().unwrap();
                reader_open::open_vault_in_new_window(path, directory, cx);
            }
        }
        self.snapshot = Some(output.snapshot);
        // Approval banners can be visible in other Reader/Settings windows.
        cx.refresh_windows();
        if let Some(inspected) = output.inspected {
            self.inspected = Some(inspected);
        }
    }
    fn schedule_refresh(&self, cx: &mut Context<Self>) {
        let Some(interval) = self.snapshot.as_ref().and_then(sync_refresh_interval) else {
            return;
        };
        let epoch = self.epoch.ticket();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(interval).await;
            let _ = this.update(cx, |this, cx| {
                if epoch.is_current() && !this.busy {
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
    fn choose_new_folder(
        &mut self,
        peer: uuid::Uuid,
        vault: String,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            self.presentation.picking_vault = false;
            self.error =
                Some("Your home folder is unavailable. Choose an existing empty folder.".into());
            cx.notify();
            return;
        };
        let picker = cx.prompt_for_new_path(&home, Some(&destination_name(label)));
        let epoch = self.lifetime.ticket();
        cx.spawn(async move |this, cx| {
            let result = picker.await;
            let _ = this.update(cx, |this, cx| {
                if !epoch.is_current()
                    || !reader_features::enabled(reader_features::Feature::Sync, cx)
                {
                    return;
                }
                match result {
                    Ok(Ok(Some(path))) => {
                        this.accept_destination(Operation::AttachNew(peer, vault, path), cx);
                    }
                    Ok(Ok(None)) => { this.presentation.picking_vault=false; }
                    _ => {
                        this.presentation.picking_vault=false;
                        let message = "The folder dialog could not return a local folder. Choose the folder again, or use an existing empty folder.".to_string();
                        this.destination_error = Some((vault, message.clone()));
                        this.error = Some(message);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    fn accept_destination(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.presentation.picking_vault) {
            if let Operation::Attach(_, ref vault, _) | Operation::AttachNew(_, ref vault, _) =
                operation
            {
                self.presentation
                    .destinations
                    .insert(vault.clone(), operation);
                cx.notify();
                return;
            }
        }
        // A periodic refresh may still own the worker when the native picker
        // answers. Keep explicit user intent until that worker publishes.
        if self.busy {
            self.pending_destination = Some(operation);
        } else {
            self.run(operation, cx);
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
        let epoch = self.lifetime.ticket();
        cx.spawn(async move |this, cx| {
            let path = picker
                .await
                .ok()
                .and_then(|result| result.ok())
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            let _ = this.update(cx, |this, cx| {
                if !epoch.is_current() {
                    return;
                }
                if let Some(path) = path {
                    this.accept_destination(
                        match remote {
                            Some((peer, vault)) => Operation::Attach(peer, vault, path),
                            None => Operation::Inspect(path),
                        },
                        cx,
                    );
                } else {
                    this.presentation.picking_vault = false;
                }
            });
        })
        .detach();
    }
}
fn destination_name(label: &str) -> String {
    let name: String = label
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .take(100)
        .collect();
    let name = name.trim().trim_matches('.').trim();
    if name.is_empty() {
        "Notes".into()
    } else {
        name.into()
    }
}
fn default_computer_name() -> String {
    sysinfo::System::host_name()
        .filter(|name| direct::validate_name(name).is_ok())
        .unwrap_or_else(|| "This computer".into())
}
fn sync_pairing_in_progress(snapshot: &Snapshot) -> bool {
    snapshot.offer_url.is_some() || snapshot.pending.is_some()
}
fn sync_refresh_interval(snapshot: &Snapshot) -> Option<Duration> {
    if sync_pairing_in_progress(snapshot) {
        Some(Duration::from_secs(1))
    } else if snapshot.preparing {
        Some(Duration::from_secs(2))
    } else if !snapshot.peers.is_empty() {
        Some(Duration::from_secs(30))
    } else {
        None
    }
}
fn locations() -> anyhow::Result<(PathBuf, Vec<PathBuf>)> {
    let configs = direct::configuration_paths()?;
    #[cfg(target_os = "macos")]
    let configs = {
        // Offline inventory only. A saved managed config is still authenticated
        // by the controller's ordinary inspection; rediscovery never bootstraps.
        let mut configs = configs;
        if let Some(managed) =
            okilum_sync_controller::sidecar::discovery::managed_configuration_path(
                &okilum_sync_controller::sidecar::macos::account_state_directory()?,
            )?
        {
            if !configs.contains(&managed) {
                configs.push(managed);
            }
        }
        configs
    };
    #[cfg(target_os = "linux")]
    let state = linux_runtime::state_path(&reader_history::state_directory()?)?;
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
    let mut metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == rustix::process::geteuid().as_raw(),
        "private Sync state parent required"
    );
    // The app's own state folder is normally created 0755 by the platform; an
    // owner-held real directory is tightened instead of blocking Sync (#2589).
    if metadata.mode() & 0o077 != 0 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        metadata = std::fs::symlink_metadata(path)?;
    }
    anyhow::ensure!(
        metadata.mode() & 0o077 == 0,
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
// A worker can open a controller after suspend has taken the view's old handle.
// Revoke every clone of that late controller when its worker exits stale.
struct ControllerGuard {
    ticket: Ticket,
    controller: Controller,
}
impl ControllerGuard {
    fn check(&self) -> anyhow::Result<()> {
        self.ticket.check()?;
        Ok(())
    }
}
impl Drop for ControllerGuard {
    fn drop(&mut self) {
        if !self.ticket.is_current() {
            self.controller.shutdown();
        }
    }
}
#[cfg(test)]
fn perform(controller: Option<Controller>, operation: Operation) -> anyhow::Result<Output> {
    perform_scoped(controller, operation, &Epoch::default().ticket())
}
fn perform_scoped(
    controller: Option<Controller>,
    operation: Operation,
    ticket: &Ticket,
) -> anyhow::Result<Output> {
    perform_with_locations(controller, operation, ticket, locations)
}
fn perform_with_locations(
    controller: Option<Controller>,
    operation: Operation,
    ticket: &Ticket,
    resolve: impl FnOnce() -> anyhow::Result<(PathBuf, Vec<PathBuf>)>,
) -> anyhow::Result<Output> {
    use anyhow::Context as _;
    ticket.run(|| {
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
        let (state, configs) = resolve().context("resolve local Sync state and connections")?;
        // The old desktop stored service enrollment alongside `direct`, not
        // inside it. Do not bootstrap a second daemon over that saved setup.
        #[cfg(target_os = "linux")]
        legacy::check(&state)?;
        #[cfg(target_os = "linux")]
        let complete_configs = configs.clone();
        #[cfg(target_os = "linux")]
        let mut runtime = linux_runtime::inspect(&state, &configs)?;
        #[cfg(target_os = "linux")]
        let operation = linux_runtime::route_share(operation, &runtime)?;
        #[cfg(target_os = "linux")]
        let configs = runtime.configs.clone();
        ticket.check()?;
        let controller = match controller {
            Some(c) => c,
            None => Controller::open(state.clone(), configs.clone())?,
        };
        let guard = ControllerGuard {
            ticket: ticket.clone(),
            controller: controller.clone(),
        };
        guard.check()?;
        #[cfg(target_os = "macos")]
        prepare_mac_runtime(&controller, &operation, connect_managed_runtime)?;
        guard.check()?;
        #[cfg(target_os = "linux")]
        {
            if matches!(
                &operation,
                Operation::EnterPairing
                    | Operation::Begin(_)
                    | Operation::Redeem(_, _)
                    | Operation::Share(_)
            ) {
                // Pairing is explicit Sync consent. Reuse the bootstrap's complete
                // inventory and packaged-runtime guards, including its final rescan.
                runtime = linux_runtime::prepare_for_pairing(runtime, || {
                    guard.check()?;
                    linux_runtime::prepare_managed(&state, &complete_configs, || guard.check())?;
                    guard.check()?;
                    linux_runtime::inspect(&state, &complete_configs)
                })?;
            }
            if let Operation::SelectConnection(path) = &operation {
                linux_runtime::select(&state, &mut runtime, path, &controller.snapshot()?)?;
            }
            if matches!(
                &operation,
                Operation::Begin(_)
                    | Operation::Redeem(_, _)
                    | Operation::Attach(_, _, _)
                    | Operation::AttachNew(_, _, _)
                    | Operation::Share(_)
            ) {
                runtime.inventory.folders()?;
                anyhow::ensure!(
                    runtime.choices.iter().filter(|c| c.selected).count() == 1,
                    "Choose a Sync connection before pairing"
                );
            }
            if let Operation::Share(path) = &operation {
                linux_runtime::check_destination(&runtime, path)?;
            }
            if let Operation::Attach(_, _, path) | Operation::AttachNew(_, _, path) = &operation {
                // Check other runtimes before creating even an empty directory.
                linux_runtime::check_destination(&runtime, path)?;
            }
            if let Operation::AttachNew(_, _, path) = &operation {
                guard.check()?;
                destination::prepare(path)?;
            }
            if let Some(selected) = runtime.choices.iter().find(|c| c.selected) {
                let requested = if let Operation::Network(expected, policy) = &operation {
                    anyhow::ensure!(
                        selected.managed && selected.identity == *expected,
                        "network policy selected managed identity changed"
                    );
                    Some(*policy)
                } else {
                    None
                };
                if selected.managed {
                    guard.check()?;
                    okilum_sync_controller::network_policy::reconcile(
                        &state.parent().unwrap().join("managed/prepared"),
                        &selected.identity,
                        requested,
                    )?;
                } else {
                    anyhow::ensure!(
                        requested.is_none(),
                        "network policy external runtime is read-only"
                    );
                }
            } else {
                anyhow::ensure!(
                    !matches!(&operation, Operation::Network(..)),
                    "network policy choose a Sync connection first"
                );
            }
            controller.set_configurations(runtime.configs.clone())?;
        }
        #[cfg(target_os = "macos")]
        if let Operation::AttachNew(_, _, path) = &operation {
            guard.check()?;
            destination::prepare(path)?;
        }
        let selected_destination = match &operation {
            Operation::Attach(_, _, path) | Operation::AttachNew(_, _, path) => {
                Some(path.canonicalize()?)
            }
            _ => None,
        };
        #[cfg(target_os = "linux")]
        let refresh_inventory_after_action = !matches!(
            &operation,
            Operation::EnterPairing
                | Operation::Begin(_)
                | Operation::Redeem(_, _)
                | Operation::Approve(_)
                | Operation::Reject(_)
                | Operation::Cancel
                | Operation::Catalog(_)
        );
        let mut inspected = None;
        let snapshot = match operation {
            Operation::Read | Operation::EnterPairing => controller.refresh()?,
            #[cfg(target_os = "linux")]
            Operation::SelectConnection(_) | Operation::Network(..) => controller.refresh()?,
            #[cfg(target_os = "linux")]
            Operation::Share(path) => {
                let prepared = state
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("Sync state parent unavailable"))?
                    .join("managed/prepared");
                controller.share_vault(&prepared, &path).with_context(|| {
                    format!(
                        "share vault {} using {}",
                        path.display(),
                        prepared.display()
                    )
                })?
            }
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
            Operation::Attach(peer, vault, path) | Operation::AttachNew(peer, vault, path) => {
                controller.attach(peer, &vault, &path)?
            }
        };
        guard.check()?;
        // Publish folder counts from the same completed action as the snapshot.
        #[cfg(target_os = "linux")]
        let runtime = if refresh_inventory_after_action {
            linux_runtime::inspect(&state, &complete_configs)?
        } else {
            runtime
        };
        Ok(Output {
            overview: controller.overview().unwrap_or_default(),
            #[cfg(target_os = "linux")]
            connection_error: linux_runtime::connection_error(&runtime),
            #[cfg(target_os = "macos")]
            connection_error: None,
            controller,
            snapshot,
            inspected,
            selected_destination,
            #[cfg(target_os = "linux")]
            runtimes: runtime.choices,
            #[cfg(target_os = "linux")]
            vaults: reader_history::ReadingHistory::known_vault_roots(
                state
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| anyhow::anyhow!("Sync profile unavailable"))?,
            )?,
        })
    })?
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
    match connect_current_app(true, policy, Duration::from_secs(45))
        .context("macOS Sync connection setup failed")?
    {
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
    #[cfg(target_os = "linux")]
    if error.is::<legacy::PreviousConnection>() {
        return legacy::MESSAGE.into();
    }
    let text = format!("{error:#}").to_lowercase();
    if text.contains("sync is disabled") {
        "Sync is disabled. Enable Sync in Settings → Features, then try Add computer again.".into()
    } else if text.contains("local sync folder") {
        "The local folder could not be created or checked. Choose a location you can write to with an existing parent folder, or use an existing empty folder.".into()
    } else if text.contains("network policy") {
        "The network setting could not be verified. Refresh and try again. Custom connection addresses may need review in Syncthing.".into()
    } else if text.contains("sync setup could not verify") {
        "Sync setup could not verify the bundled connection. Refresh to retry.".into()
    } else if text.starts_with("not connected to ") {
        "The other computer could not be reached. Keep Okilum open there and check that both computers have a network connection.".into()
    } else if cfg!(target_os = "linux") && text.contains("ignore policy") {
        "This vault’s .stignore needs review before sharing. Required exclusions: .okilum-index, .okilum-save-*, /.claude/worktrees and /worktrees. Includes and negated rules need a separately reviewed policy.".into()
    } else if cfg!(target_os = "linux") && text.contains("existing synced vault") {
        "This vault already belongs to Syncthing. Use Choose existing synced vault below.".into()
    } else if cfg!(target_os = "linux") && text.contains("packaged") {
        "This build does not include the verified Sync runtime. Install a Linux beta with Sync and try again.".into()
    } else if cfg!(target_os = "linux") && text.contains("managed") {
        "Sharing an ordinary vault requires the bundled Sync connection. Keep existing Syncthing vaults on Choose existing synced vault.".into()
    } else if text.contains("choose a sync connection") {
        "Choose which existing Sync connection to use below, then try again.".into()
    } else if text.contains("approval") {
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
        "Sync could not finish this step. Check the connection on both computers, then try again."
            .into()
    }
}

#[cfg(any(target_os = "macos", test))]
fn prepare_mac_runtime(
    controller: &Controller,
    operation: &Operation,
    connect: impl FnOnce(&Controller) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Creating an invitation uses the app's pairing listener and identity only.
    // Opening Settings and Add computer must not depend on SMAppService approval,
    // global process inventory, signing checks or the 45-second runtime startup.
    if matches!(
        operation,
        Operation::Redeem(..) | Operation::Attach(..) | Operation::AttachNew(..)
    ) && controller.configuration_paths()?.is_empty()
    {
        connect(controller)?;
    }
    Ok(())
}

#[path = "reader_settings_sync/failure.rs"]
mod failure;
use failure::failure_details;

#[path = "reader_settings_sync/presentation.rs"]
pub(super) mod presentation;

#[path = "reader_settings_sync/cli.rs"]
pub(crate) mod cli;
#[path = "reader_settings_sync/epoch.rs"]
mod epoch;
#[cfg(test)]
#[path = "reader_settings_sync/tests.rs"]
mod tests;
#[path = "reader_settings_sync/view.rs"]
mod view;

#[cfg(target_os = "linux")]
#[path = "reader_settings_sync/linux_runtime.rs"]
mod linux_runtime;

#[cfg(target_os = "linux")]
#[path = "reader_settings_sync/legacy.rs"]
mod legacy;

#[path = "reader_settings_sync/destination.rs"]
mod destination;

#[path = "reader_settings_sync/advanced.rs"]
mod advanced;
