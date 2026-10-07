//! Linux Sync settings. Blocking controller work stays off the GPUI thread.
use super::*;
use gpui_component::{
    button::ButtonGroup,
    input::{Input, InputState},
    switch::Switch,
};
use tessera_sync_controller::{
    daemon::{self, DaemonIdentity},
    desktop::{Desktop, Setup, Snapshot},
    folder::LocalStatus,
    pairing::{Approval, Service},
    presentation::FolderState,
    runtime::Selection,
};

// Keep an explicitly opened controller alive when Settings closes, so pairing,
// first-receive and pending revocation can finish while the application is open.
struct BackgroundSync(Entity<SyncSettings>);
impl Global for BackgroundSync {}
pub(super) fn shared(window: &mut Window, cx: &mut App) -> Entity<SyncSettings> {
    if let Some(background) = cx.try_global::<BackgroundSync>() {
        return background.0.clone();
    }
    let view = cx.new(|cx| SyncSettings::new(window, cx));
    cx.set_global(BackgroundSync(view.clone()));
    view
}

struct Candidate {
    identity: DaemonIdentity,
    label: String,
    supported: bool,
}
enum Operation {
    Read,
    Enable(Option<Setup>),
    Refresh,
    Pause(bool),
    Disable,
    Remove,
}
struct Output {
    package_available: bool,
    snapshot: Snapshot,
    candidates: Option<Vec<Candidate>>,
    unavailable: bool,
    folder: Option<LocalStatus>,
    approval: Option<Approval>,
    removal: Option<tessera_sync_controller::removal::Outcome>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ErrorField {
    General,
    Address,
    Name,
    Folder,
}
pub(super) struct SyncSettings {
    desktop: Option<Desktop>,
    address: Entity<InputState>,
    name: Entity<InputState>,
    destination: Option<PathBuf>,
    candidates: Vec<Candidate>,
    selected: Option<usize>,
    unavailable: bool,
    loaded: bool,
    busy: bool,
    refresh_epoch: u64,
    error: Option<String>,
    error_field: ErrorField,
    output: Option<Output>,
    conflicts_busy: bool,
    conflicts: Option<(
        PathBuf,
        Result<tessera_sync_controller::conflicts::Inventory, String>,
    )>,
}
impl SyncSettings {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        let desktop = home.map(|home| {
            let state_home = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(".local/state"));
            let config_home = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(".config"));
            Desktop {
                state: state_home.join("tessera/sync"),
                home,
                state_home,
                config_home,
            }
        });
        let address =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://your-sync-service"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Computer name"));
        let mut this = Self {
            desktop,
            address,
            name,
            destination: None,
            candidates: vec![],
            selected: None,
            unavailable: false,
            loaded: false,
            busy: false,
            refresh_epoch: 0,
            error: None,
            error_field: ErrorField::General,
            output: None,
            conflicts_busy: false,
            conflicts: None,
        };
        this.run(Operation::Read, cx);
        this
    }
    fn run(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(desktop) = self.desktop.clone() else {
            self.error = Some("Your home folder is unavailable.".into());
            return;
        };
        self.refresh_epoch += 1;
        self.busy = true;
        self.error = None;
        self.error_field = ErrorField::General;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, recovered) = cx
                .background_executor()
                .spawn(async move {
                    let result = perform(&desktop, operation);
                    let recovered = if result.is_err() {
                        desktop.read().ok()
                    } else {
                        None
                    };
                    (result, recovered)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(mut output) => {
                        if let Some(candidates) = output.candidates.take() {
                            this.candidates = candidates;
                            this.unavailable = output.unavailable;
                            this.selected = None;
                        }
                        this.loaded = true;
                        this.output = Some(output);
                    }
                    Err(error) => {
                        if let Some(snapshot) = recovered {
                            this.loaded = true;
                            if let Some(output) = this.output.as_mut() {
                                output.snapshot = snapshot;
                                output.folder = None;
                                output.approval = None;
                            } else {
                                this.output = Some(Output {
                                    package_available: std::path::Path::new("/usr/bin/syncthing")
                                        .is_file(),
                                    snapshot,
                                    candidates: None,
                                    unavailable: false,
                                    folder: None,
                                    approval: None,
                                    removal: None,
                                });
                            }
                        }
                        // Credentials stay in controller journals, never in the view model.
                        eprintln!("Sync operation failed: {error}");
                        this.error = Some(user_error(&error).into());
                    }
                }
                cx.notify();
                this.schedule_refresh(cx);
            });
        })
        .detach();
    }
    fn inspect_conflicts(&mut self, cx: &mut Context<Self>) {
        if self.conflicts_busy {
            return;
        }
        let Some(destination) = self
            .output
            .as_ref()
            .and_then(|o| o.snapshot.setup.as_ref())
            .map(|s| s.destination.clone())
        else {
            return;
        };
        self.conflicts_busy = true;
        self.conflicts = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let root = destination.clone();
            let result = cx.background_executor().spawn(async move {
                tessera_sync_controller::conflicts::inspect(&root)
                    .map_err(|_| "Could not inspect this folder. Check its location and access permissions.".to_string())
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.conflicts_busy = false;
                // A finished scan belongs to the original folder, never a new setup.
                if this.output.as_ref().and_then(|o| o.snapshot.setup.as_ref())
                    .is_some_and(|s| s.destination == destination) {
                    this.conflicts = Some((destination, result));
                }
                cx.notify();
            });
        }).detach();
    }
    fn schedule_refresh(&self, cx: &mut Context<Self>) {
        let Some(output) = &self.output else {
            return;
        };
        let Some(runtime) = &output.snapshot.runtime else {
            return;
        };
        if !output.package_available
            && matches!(runtime.selection, Selection::Managed(_))
            && !runtime.removed
            && output.removal.is_none()
        {
            return;
        }
        if !runtime.desired_enabled && !runtime.removed && output.removal.is_none() {
            return;
        }
        if output.removal.as_ref().is_some_and(|r| r.complete()) {
            return;
        }
        let removing = runtime.removed || output.removal.is_some();
        let epoch = self.refresh_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(5))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh_epoch == epoch && !this.busy {
                    this.run(
                        if removing {
                            Operation::Remove
                        } else {
                            Operation::Refresh
                        },
                        cx,
                    );
                }
            });
        })
        .detach();
    }
    fn enable(&mut self, cx: &mut Context<Self>) {
        if self
            .output
            .as_ref()
            .and_then(|o| o.snapshot.setup.as_ref())
            .is_some()
        {
            self.run(Operation::Enable(None), cx);
            return;
        }
        let Some(destination) = self.destination.clone() else {
            self.error_field = ErrorField::Folder;
            self.error = Some("Choose an empty folder or an existing copy of this vault.".into());
            cx.notify();
            return;
        };
        let name = self.name.read(cx).value().trim().to_owned();
        if name.trim().is_empty() {
            self.error_field = ErrorField::Name;
            self.error = Some("Give this computer a name.".into());
            cx.notify();
            return;
        }
        let origin = self
            .address
            .read(cx)
            .value()
            .trim()
            .trim_end_matches('/')
            .to_owned();
        let Ok(url) = url::Url::parse(&origin) else {
            self.error_field = ErrorField::Address;
            self.error = Some("Enter your sync service’s HTTPS address.".into());
            cx.notify();
            return;
        };
        if url.scheme() != "https" || url.host_str().is_none() {
            self.error_field = ErrorField::Address;
            self.error = Some("Enter your sync service’s HTTPS address.".into());
            cx.notify();
            return;
        }
        let Some(desktop) = self.desktop.as_ref() else {
            return;
        };
        let selection = if let Some(index) = self.selected {
            Selection::Reuse(self.candidates[index].identity.clone())
        } else {
            match desktop.managed_selection() {
                Ok(selection) => selection,
                Err(_) => {
                    self.error = Some("No local connection is available. Try again.".into());
                    cx.notify();
                    return;
                }
            }
        };
        self.run(
            Operation::Enable(Some(Setup {
                origin,
                name,
                destination,
                selection,
            })),
            cx,
        );
    }
    fn field_error(&self, field: ErrorField) -> Option<AnyElement> {
        if self.error_field != field {
            return None;
        }
        self.error
            .as_ref()
            .map(|error| div().text_sm().child(error.clone()).into_any_element())
    }
    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose an empty folder or a known vault copy".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picker.await {
                let _ = this.update(cx, |this, cx| {
                    this.destination = paths.into_iter().next();
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
fn user_error(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("unsupported") && message.contains("Syncthing version") {
        "Sync needs Syncthing 2.1.6. Check the installed package."
    } else if message.contains("unknown nonempty destination") {
        "Choose an empty folder, or select Syncthing’s existing copy of this vault."
    } else if message.contains("overlaps another folder") {
        "This location overlaps another synced folder. Choose a separate folder."
    } else if message.contains("ignore policy") {
        "This folder uses different sync exclusions and needs review."
    } else if message.contains("computer name") {
        "Use a computer name of up to 100 characters."
    } else {
        "Sync could not finish this step. Check the connection and try again."
    }
}

fn service(origin: &str) -> anyhow::Result<Service> {
    // Explicit test trust is confined to the non-publishing Linux QA harness.
    #[cfg(feature = "settings-ui-harness")]
    let certificate = std::env::var_os("TESSERA_SYNC_TEST_CA")
        .map(std::fs::read)
        .transpose()?;
    #[cfg(not(feature = "settings-ui-harness"))]
    let certificate: Option<Vec<u8>> = None;
    Service::new(origin, certificate.as_deref())
}
fn perform(desktop: &Desktop, operation: Operation) -> anyhow::Result<Output> {
    let mut snapshot = desktop.read()?;
    let mut output = Output {
        package_available: std::path::Path::new("/usr/bin/syncthing").is_file(),
        snapshot: snapshot.clone(),
        candidates: None,
        unavailable: false,
        folder: None,
        approval: None,
        removal: None,
    };
    match operation {
        Operation::Read => {
            // Resume an already requested Disable after an interrupted stop.
            // No runtime journal exists before the first explicit Enable.
            if snapshot
                .runtime
                .as_ref()
                .is_some_and(|r| !r.desired_enabled && !r.removed)
            {
                output.snapshot = desktop.disable()?;
            }
            let discovery = daemon::discover(&desktop.configs()?);
            output.unavailable = !discovery.unavailable.is_empty();
            output.candidates = Some(
                discovery
                    .candidates
                    .into_iter()
                    .map(|c| Candidate {
                        label: c
                            .folders
                            .first()
                            .and_then(|(_, p)| p.file_name())
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Existing Syncthing".into()),
                        supported: c.version == daemon::CLIENT_VERSION,
                        identity: c.identity,
                    })
                    .collect(),
            );
        }
        Operation::Enable(setup) => {
            let setup = setup
                .or_else(|| snapshot.setup.take())
                .ok_or_else(|| anyhow::anyhow!("setup missing"))?;
            if matches!(setup.selection, Selection::Managed(_)) && !output.package_available {
                return Ok(output);
            }
            let service = service(&setup.origin)?;
            desktop.enable(setup, &service)?;
            let progress = desktop.refresh(&service)?;
            output.snapshot = progress.snapshot;
            output.approval = progress.approval;
            output.folder = progress.folder;
        }
        Operation::Refresh => {
            if let Some(setup) = snapshot.setup {
                if matches!(setup.selection, Selection::Managed(_)) && !output.package_available {
                    return Ok(output);
                }
                let progress = desktop.refresh(&service(&setup.origin)?)?;
                output.snapshot = progress.snapshot;
                output.approval = progress.approval;
                output.folder = progress.folder;
            }
        }
        Operation::Pause(paused) => output.folder = Some(desktop.pause(paused)?),
        Operation::Disable => output.snapshot = desktop.disable()?,
        Operation::Remove => {
            let setup = snapshot
                .setup
                .ok_or_else(|| anyhow::anyhow!("setup missing"))?;
            output.removal = Some(desktop.remove(&service(&setup.origin)?)?);
            output.snapshot = desktop.read()?;
        }
    }
    Ok(output)
}
impl Render for SyncSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        let saved = self.output.as_ref().and_then(|o| o.snapshot.setup.as_ref());
        let runtime = self
            .output
            .as_ref()
            .and_then(|o| o.snapshot.runtime.as_ref());
        let enabled = runtime.is_some_and(|r| r.desired_enabled && !r.removed);
        let removed = runtime.is_some_and(|r| r.removed)
            || self
                .output
                .as_ref()
                .is_some_and(|o| o.removal.is_some() && o.snapshot.setup.is_some());
        let local = self.output.as_ref().and_then(|o| o.folder.as_ref());
        let reused = runtime.is_some_and(|r| matches!(r.selection, Selection::Reuse(_)));
        let paused = local.is_some_and(|s| s.folder["paused"] == true);
        let state = local.map(FolderState::from_local);
        let removal = self.output.as_ref().and_then(|o| o.removal.as_ref());
        let needs_package = self.output.as_ref().is_some_and(|o| !o.package_available)
            && saved
                .map(|s| matches!(s.selection, Selection::Managed(_)))
                .unwrap_or(self.selected.is_none());
        let label = if needs_package && !removed {
            "Install Syncthing to enable Sync"
        } else if self.error.is_some() {
            "Needs attention"
        } else if self.busy {
            "Working…"
        } else if removal.is_some_and(|r| r.complete()) {
            // Completion retires the setup/runtime before this snapshot is read.
            // Confirm removal while allowing a fresh explicit connection below.
            "Removed"
        } else if removed {
            "Removal pending"
        } else if !enabled {
            "Off"
        } else if self.output.as_ref().is_some_and(|o| o.approval.is_some()) {
            "Approve this computer in your browser"
        } else {
            state.map(FolderState::label).unwrap_or("Preparing")
        };
        let mut content = v_flex()
            .w_full()
            .gap_4()
            .child(super::reader_settings::setting_row(
                "Sync",
                "Keep this vault on your computers.",
                Switch::new("sync-enabled")
                    .checked(enabled)
                    .disabled(
                        self.busy
                            || !self.loaded
                            || removed
                            || (!enabled && needs_package)
                            // Missing inventory means we cannot exclude folder overlap,
                            // even when the requested daemon would be a new managed one.
                            || (saved.is_none() && self.unavailable),
                    )
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if *checked {
                            this.enable(cx);
                        } else {
                            this.run(Operation::Disable, cx);
                        }
                    })),
                cx,
            ))
            .child(
                h_flex().justify_between().gap_2().child(label).child(
                    Button::new("sync-refresh")
                        .icon(IconName::RotateCw)
                        .ghost()
                        .disabled(self.busy)
                        .accessibility_label("Refresh Sync status")
                        .tooltip("Refresh Sync status")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.output.as_ref().is_some_and(|o| {
                                o.snapshot.runtime.as_ref().is_some_and(|r| r.removed)
                                    || (o.removal.is_some() && o.snapshot.setup.is_some())
                            }) {
                                this.run(Operation::Remove, cx);
                            } else {
                                this.run(
                                    if this
                                        .output
                                        .as_ref()
                                        .is_some_and(|o| o.snapshot.setup.is_some())
                                    {
                                        Operation::Refresh
                                    } else {
                                        Operation::Read
                                    },
                                    cx,
                                );
                            }
                        })),
                ),
            );
        if saved.is_none() {
            let destination = self
                .destination
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Choose a folder".into());
            content = content
                .child(v_flex().gap_2().child("Sync service").child(Input::new(&self.address).disabled(self.busy)).children(self.field_error(ErrorField::Address)))
                .child(v_flex().gap_2().child("This computer").child(Input::new(&self.name).disabled(self.busy)).children(self.field_error(ErrorField::Name)))
                .child(h_flex().justify_between().child(destination).child(Button::new("sync-folder")
                    .icon(IconName::Folder).ghost().disabled(self.busy).accessibility_label("Choose vault folder").tooltip("Choose an empty folder or a known copy")
                    .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx)))))
                .children(self.field_error(ErrorField::Folder))
                .child(div().text_sm().text_color(p.text_muted).child("Start with an empty folder, or reuse a known copy. Your files stay on this computer when Sync is removed."));
            if !self.candidates.is_empty() {
                let mut choices = vec![Button::new("sync-managed")
                    .ghost()
                    .label("Tessera")
                    .selected(self.selected.is_none())
                    .disabled(self.busy)
                    .tooltip("Use Tessera’s background service")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        cx.notify();
                    }))];
                for (index, candidate) in self.candidates.iter().enumerate() {
                    choices.push(
                        Button::new(("sync-reuse", index))
                            .ghost()
                            .label(candidate.label.clone())
                            .selected(self.selected == Some(index))
                            .disabled(self.busy || !candidate.supported)
                            .tooltip(if candidate.supported {
                                format!("Reuse {}", candidate.identity.config_file.display())
                            } else {
                                "This instance needs Syncthing 2.1.6.".into()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(index);
                                cx.notify();
                            })),
                    );
                }
                content = content
                    .child(
                        div()
                            .text_sm()
                            .text_color(p.text_muted)
                            .child("Choose Tessera or an existing Syncthing instance."),
                    )
                    .child(
                        ButtonGroup::new("sync-runtime")
                            .flex_wrap()
                            .children(choices),
                    );
            }
            if self.unavailable {
                content = content.child(div().text_sm().text_color(p.text_muted)
                .child("An existing Syncthing instance is unavailable. Start it and refresh before enabling Sync."));
            }
        } else if let Some(setup) = saved {
            let folder = setup
                .destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let identity = runtime
                .and_then(|r| r.identity.as_ref())
                .map(|i| i.device_id.as_str())
                .unwrap_or("Pending");
            let details = format!(
                "{}\nSyncthing device: {}",
                setup.destination.display(),
                identity
            );
            content = content.child(v_flex().gap_1().child(h_flex().justify_between().child(setup.name.clone())
                .child(Button::new("sync-details").icon(IconName::Info).ghost()
                    .accessibility_label("Connection details").tooltip(details))).child(folder)
                .child(div().text_sm().text_color(p.text_muted).child(if reused {
                    "Using existing Syncthing. It may keep syncing when Tessera is disabled or removed."
                } else if enabled { "Runs in the background after you close Tessera." }
                else { "Managed by Tessera." })));
        }
        if needs_package && !removed {
            content = content.child(v_flex().gap_1()
                .child(div().text_sm().text_color(p.text_muted)
                    .child("On Arch / Omarchy: sudo pacman -S syncthing"))
                .child(div().text_sm().text_color(p.text_muted)
                    .child("On other Linux distributions, install Syncthing with your package manager. Then refresh here. Reader works without it.")));
        }
        if let Some(approval) = self.output.as_ref().and_then(|o| o.approval.as_ref()) {
            let code = approval
                .request
                .code
                .chars()
                .collect::<Vec<_>>()
                .chunks(4)
                .map(|c| c.iter().collect::<String>())
                .collect::<Vec<_>>()
                .join("  ");
            let approval_url = approval.approval_url.clone();
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(code),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child("Match this code in your browser.")
                            .child(
                                Button::new("sync-approval")
                                    .icon(IconName::ExternalLink)
                                    .ghost()
                                    .accessibility_label("Open approval in browser")
                                    .tooltip("Approve this computer")
                                    .on_click(move |_, _, cx| cx.open_url(&approval_url)),
                            ),
                    ),
            );
        }
        if saved.is_some() && !removed {
            content = content.child(
                h_flex()
                    .gap_2()
                    .when(enabled && local.is_some(), |row| {
                        row.child(
                            Button::new("sync-pause")
                                .icon(if paused {
                                    IconName::Play
                                } else {
                                    IconName::Pause
                                })
                                .ghost()
                                .disabled(self.busy)
                                .accessibility_label(if paused {
                                    "Resume Sync"
                                } else {
                                    "Pause Sync"
                                })
                                .tooltip(if paused {
                                    "Resume this folder"
                                } else {
                                    "Pause this folder"
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run(Operation::Pause(!paused), cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("sync-remove")
                            .icon(IconName::CircleX)
                            .ghost()
                            .disabled(self.busy)
                            .accessibility_label("Remove this computer from Sync")
                            .tooltip("Remove this computer; keep local files")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.run(Operation::Remove, cx)),
                            ),
                    ),
            );
        }
        if let Some(setup) = saved.filter(|_| !removed) {
            content = content.child(
                h_flex().justify_between().child("Conflict copies").child(
                    Button::new("sync-check-conflicts")
                        .icon(IconName::Search)
                        .ghost()
                        .disabled(self.conflicts_busy)
                        .accessibility_label("Check for conflict copies")
                        .tooltip("Check this folder for possible conflict copies")
                        .on_click(cx.listener(|this, _, _, cx| this.inspect_conflicts(cx))),
                ),
            );
            if self.conflicts_busy {
                content = content.child(
                    div()
                        .text_sm()
                        .text_color(p.text_muted)
                        .child("Checking local filenames…"),
                );
            } else if let Some((_, result)) = self
                .conflicts
                .as_ref()
                .filter(|(root, _)| *root == setup.destination)
            {
                match result {
                    Ok(found) => {
                        if found.complete && found.copies.is_empty() {
                            content = content.child(
                                div()
                                    .text_sm()
                                    .text_color(p.text_muted)
                                    .child("No conflict copies found in this check."),
                            );
                        }
                        if !found.copies.is_empty() {
                            content = content.child(div().text_sm().text_color(p.text_muted)
                                .child("Possible conflict copies from this check. Both versions are kept; review them in File Manager."));
                            for (index, relative) in found.copies.iter().enumerate() {
                                let path = setup.destination.join(relative);
                                let filename =
                                    relative.file_name().unwrap_or_default().to_string_lossy();
                                let label = filename
                                    .split(".sync-conflict-")
                                    .next()
                                    .unwrap_or(&filename)
                                    .to_string();
                                let tooltip =
                                    format!("Show in File Manager: {}", relative.display());
                                content = content.child(
                                    h_flex().justify_between().gap_2().child(label).child(
                                        Button::new(("sync-conflict-copy", index))
                                            .icon(IconName::Folder)
                                            .ghost()
                                            .accessibility_label(
                                                "Show conflict copy in File Manager",
                                            )
                                            .tooltip(tooltip)
                                            .on_click(move |_, window, cx| {
                                                super::reader_files::reveal(&path, window, cx)
                                            }),
                                    ),
                                );
                            }
                        }
                        if !found.complete {
                            content = content.child(div().text_sm().text_color(p.text_muted)
                                .child("This check is incomplete: some locations were unavailable or the scan limit was reached. More copies may exist."));
                        }
                    }
                    Err(message) => {
                        content = content.child(
                            div()
                                .text_sm()
                                .text_color(p.text_muted)
                                .child(message.clone()),
                        )
                    }
                }
            }
        }
        if enabled && !removed {
            if let Some(local) = local {
                for message in tessera_sync_controller::presentation::attention_messages(local) {
                    content =
                        content.child(div().text_sm().text_color(p.text_muted).child(message));
                }
            }
        }
        if let Some(last) = self
            .output
            .as_ref()
            .and_then(|o| o.snapshot.last_connected_at)
        {
            let seconds = (chrono::Utc::now().timestamp().max(0) as u64).saturating_sub(last);
            let age = if seconds < 60 {
                "Last connected just now".into()
            } else if seconds < 3600 {
                format!("Last connected {} min ago", seconds / 60)
            } else if seconds < 86400 {
                format!("Last connected {} h ago", seconds / 3600)
            } else {
                format!("Last connected {} days ago", seconds / 86400)
            };
            content = content.child(div().text_sm().text_color(p.text_muted).child(age));
        }
        if let Some(error) = self
            .error
            .as_ref()
            .filter(|_| self.error_field == ErrorField::General || saved.is_some())
        {
            content = content.child(div().text_sm().child(error.clone()));
        }
        content
    }
}
