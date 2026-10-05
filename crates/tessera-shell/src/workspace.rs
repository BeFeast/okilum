//! Ordinary application workspace selection. Backend roots are identities, never
//! paths to open on this desktop. Only the separate read-only Reader uses local paths.
use super::{brain, Opts, Reader};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    v_flex, Disableable as _, IconName,
};
use serde_json::{json, Value};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Clone, Debug, PartialEq)]
struct Profile {
    label: String,
    endpoint: SocketAddr,
    identity: Value,
}
impl Profile {
    fn from_value(value: &Value) -> Result<Self, String> {
        if value["schema"] != "tessera-workspace/v1" {
            return Err("Unsupported saved workspace. Select a brain again.".into());
        }
        let label = value["label"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        let endpoint = parse_endpoint(value["endpoint"].as_str().unwrap_or_default())?;
        let identity = value["identity"].clone();
        validate_identity(&identity)?;
        if label.is_empty() {
            return Err("A project label is required.".into());
        }
        Ok(Self {
            label,
            endpoint,
            identity,
        })
    }
    fn value(&self) -> Value {
        json!({"schema":"tessera-workspace/v1", "label":self.label,
            "endpoint":self.endpoint.to_string(), "identity":self.identity})
    }
    fn verify(&self, capabilities: &Value) -> Result<(), String> {
        if capabilities["workspace_guard"] != true || capabilities["workspace"] != self.identity {
            return Err("Backend identity does not match the saved brain. Restore its endpoint and Retry, or explicitly select another brain. No work was sent.".into());
        }
        Ok(())
    }
}
fn parse_endpoint(text: &str) -> Result<SocketAddr, String> {
    text.trim()
        .parse::<SocketAddr>()
        .ok()
        .filter(|e| e.ip().is_loopback())
        .ok_or_else(|| "Use a loopback backend address, for example 127.0.0.1:24161.".into())
}
fn validate_identity(identity: &Value) -> Result<(), String> {
    let valid = identity["brain_id"]
        .as_str()
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        && identity["root"]
            .as_str()
            .is_some_and(|s| s.starts_with('/'))
        && identity["records_dir"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && identity["managed"] == true;
    if valid {
        Ok(())
    } else {
        Err("This backend does not identify a managed brain. Configure its managed root before opening it.".into())
    }
}
fn settings_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("tessera/workspace.json")
}
fn save_profile(path: &std::path::Path, profile: &Profile) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid workspace settings path.")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Cannot create workspace settings: {e}"))?;
    let temporary = parent.join(format!(".workspace-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&profile.value())?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    let _ = std::fs::remove_file(temporary);
    result.map_err(|e: std::io::Error| format!("Workspace was not saved: {e}"))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum EntryPage {
    #[default]
    Overview,
    Managed,
    Local,
}

/// Frozen at action time; editing a form never changes retry ownership or diagnostics.
#[derive(Clone, Debug)]
struct ConnectionAttempt {
    label: String,
    endpoint: String,
    expected: Option<Profile>,
}
impl ConnectionAttempt {
    fn new(
        replace: bool,
        saved: Option<&Profile>,
        label: &str,
        endpoint: &str,
    ) -> Result<Self, String> {
        if !replace {
            let saved = saved.ok_or("Select a brain first.")?.clone();
            return Ok(Self {
                label: saved.label.clone(),
                endpoint: saved.endpoint.to_string(),
                expected: Some(saved),
            });
        }
        Ok(Self {
            label: label.trim().into(),
            endpoint: endpoint.trim().into(),
            expected: None,
        })
    }
    fn validate(&self) -> Result<SocketAddr, String> {
        let endpoint = parse_endpoint(&self.endpoint)?;
        if self.label.is_empty() {
            return Err("Enter a project label.".into());
        }
        Ok(endpoint)
    }
    fn accept(&self, capabilities: &Value) -> Result<Profile, String> {
        let profile = match &self.expected {
            Some(saved) => saved.clone(),
            None => Profile {
                label: self.label.clone(),
                endpoint: self.validate()?,
                identity: capabilities["workspace"].clone(),
            },
        };
        validate_identity(&profile.identity)?;
        profile.verify(capabilities)?;
        Ok(profile)
    }
    fn description(&self) -> String {
        format!(
            "{}: {} · {}",
            if self.expected.is_some() {
                "Saved brain"
            } else {
                "Selected brain"
            },
            self.label,
            self.endpoint
        )
    }
}

pub struct Workspace {
    reader: Option<Entity<Reader>>,
    brain: Option<Entity<brain::BrainView>>,
    profile: Option<Profile>,
    connectors: Option<Entity<super::connectors::ConnectorsView>>,
    showing_connectors: bool,
    connector_subscription: Option<Subscription>,
    discussion_subscription: Option<Subscription>,
    label: Entity<InputState>,
    endpoint: Entity<InputState>,
    vault: Entity<InputState>,
    showing_brain: bool,
    settings: bool,
    busy: bool,
    entry_page: EntryPage,
    connection_details: bool,
    attempt: Option<ConnectionAttempt>,
    entry_return_focus: Option<FocusHandle>,
    entry_focus: FocusHandle,
    initial_entry_focus_pending: bool,
    retry_entry_focus: FocusHandle,
    managed_entry_focus: FocusHandle,
    local_entry_focus: FocusHandle,
    local_form_focus: FocusHandle,
    overview_feedback: Option<(Option<String>, Option<ConnectionAttempt>)>,
    error: Option<String>,
    export_status: Option<String>,
    exported_path: Option<PathBuf>,
    showing_export: bool,
}
impl Workspace {
    pub fn new(mut opts: Opts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        if super::reader_recovery::is_recovering(cx) {
            opts.defer_saved_connection = true;
        }
        let loaded = match std::fs::read(settings_path()) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|v| Profile::from_value(&v))
                .map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Cannot read saved workspace: {e}")),
        };
        Self::with_loaded_profile(opts, loaded, window, cx)
    }
    fn with_loaded_profile(
        opts: Opts,
        loaded: Result<Option<Profile>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        window.on_window_should_close(cx, |window, cx| {
            crate::reader_editor::save_window(window.window_handle(), cx)
        });
        let error = loaded.as_ref().err().cloned();
        let profile = loaded.ok().flatten();
        let label = cx.new(|cx| InputState::new(window, cx).placeholder("Project label"));
        let endpoint = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Backend address, e.g. 127.0.0.1:24161")
        });
        let vault = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Local vault directory (read-only notes)")
        });
        if let Some(profile) = &profile {
            label.update(cx, |s, cx| s.set_value(profile.label.clone(), window, cx));
            endpoint.update(cx, |s, cx| {
                s.set_value(profile.endpoint.to_string(), window, cx)
            });
        }
        let reader =
            opts.vault.as_ref().or(opts.open_path.as_ref()).map(|_| {
                cx.new(|cx| Reader::new(opts.clone(), window, cx).embedded_in_workspace())
            });
        if let Some(root) = &opts.vault {
            vault.update(cx, |s, cx| {
                s.set_value(root.display().to_string(), window, cx)
            });
        }
        let showing_brain = reader.is_none();
        let settings = reader.is_none();
        let this = Self {
            reader,
            brain: None,
            profile,
            connectors: None,
            showing_connectors: false,
            connector_subscription: None,
            discussion_subscription: None,
            label,
            endpoint,
            vault,
            showing_brain,
            settings,
            busy: false,
            entry_page: EntryPage::Overview,
            connection_details: false,
            attempt: None,
            entry_return_focus: None,
            entry_focus: cx.focus_handle(),
            initial_entry_focus_pending: true,
            retry_entry_focus: cx.focus_handle(),
            managed_entry_focus: cx.focus_handle(),
            local_entry_focus: cx.focus_handle(),
            local_form_focus: cx.focus_handle(),
            overview_feedback: None,
            error,
            export_status: None,
            exported_path: None,
            showing_export: false,
        };
        this.focus_initial_entry(window, cx);
        if this.profile.is_some() && this.reader.is_none() && !opts.defer_saved_connection {
            cx.spawn_in(window, async move |this, cx| {
                let _ = this.update_in(cx, |this, window, cx| this.connect(false, window, cx));
            })
            .detach();
        }
        this
    }
    fn focus_initial_entry(&self, window: &mut Window, cx: &mut App) {
        if self.reader.is_none() && self.brain.is_none() {
            // Root's Tab actions need a focused descendant before the first click.
            // This container is not a tab stop and never steals retained-view focus.
            self.entry_focus.focus(window, cx);
        }
    }

    fn choose_entry(&mut self, page: EntryPage, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.entry_page == EntryPage::Overview && page != EntryPage::Overview {
            self.overview_feedback = Some((self.error.take(), self.attempt.take()));
        }
        self.entry_page = page;
        self.connection_details = false;
        self.error = None;
        self.attempt = None;
        if page == EntryPage::Overview {
            if let Some((error, attempt)) = self.overview_feedback.take() {
                self.error = error;
                self.attempt = attempt;
            }
            if let Some(focus) = self.entry_return_focus.take() {
                // The anchor is not a tab stop; its next stop is the retained choice
                // button. Toolkit buttons intentionally suppress pointer focus.
                window.focus(&focus, cx);
                window.focus_next(cx);
            } else {
                window.blur(cx);
                window.focus_next(cx);
            }
        } else {
            self.entry_return_focus = Some(if page == EntryPage::Managed {
                self.managed_entry_focus.clone()
            } else {
                self.local_entry_focus.clone()
            });
            if page == EntryPage::Managed {
                window.focus(&self.label.read(cx).focus_handle(cx), cx);
            } else {
                window.focus(&self.local_form_focus, cx);
            }
        }
        cx.notify();
    }

    fn render_entry(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use super::brand::{control, palette};
        let colors = palette(cx);
        let mut content = v_flex()
            .w_full()
            .max_w(px(if self.entry_page == EntryPage::Overview {
                480.
            } else {
                640.
            }))
            .min_w_0()
            .whitespace_normal()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(super::brand::logo(24., cx))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Tessera")),
            )
            .child(
                div().text_2xl().font_weight(FontWeight::SEMIBOLD).child(
                    self.profile
                        .as_ref()
                        .map(|p| p.label.clone())
                        .unwrap_or_else(|| "Open your workspace".into()),
                ),
            );
        let status = if self.busy {
            "Connecting…"
        } else if self.entry_page == EntryPage::Overview
            && self.error.is_some()
            && self.attempt.is_some()
        {
            "Brain unavailable"
        } else if self.brain.is_some() {
            "Your workspace is open."
        } else if self.profile.is_some() {
            "Open your saved brain or choose another workspace."
        } else {
            "Connect to a project brain or read a local vault."
        };
        content = content.child(div().text_color(colors.text_muted).child(status));
        if self.entry_page == EntryPage::Overview && self.profile.is_some() {
            content = content.child(
                div()
                    .track_focus(&self.retry_entry_focus)
                    .tab_group()
                    .tab_stop(false)
                    .child(
                        control("retry-brain", cx)
                            .w_full()
                            .primary()
                            .label("Retry saved brain")
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.connect(false, window, cx)),
                            ),
                    ),
            );
        }
        // Keep choice buttons mounted so returning from either form restores its invoker.
        content = content
            .child(
                div()
                    .id("managed-entry-choice")
                    .track_focus(&self.managed_entry_focus)
                    .tab_group()
                    .tab_stop(false)
                    .child(
                        control("choose-managed-brain", cx)
                            .debug_selector(|| "choose-managed-brain".into())
                            .ghost()
                            .w_full()
                            .justify_start()
                            .accessibility_label(if self.profile.is_some() {
                                "Open another brain"
                            } else {
                                "Open a project brain"
                            })
                            .child(div().flex_1().text_left().child(if self.profile.is_some() {
                                "Open another brain"
                            } else {
                                "Open a project brain"
                            }))
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_entry(EntryPage::Managed, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("local-entry-choice")
                    .track_focus(&self.local_entry_focus)
                    .tab_group()
                    .tab_stop(false)
                    .child(
                        control("choose-local-vault", cx)
                            .debug_selector(|| "choose-local-vault".into())
                            .ghost()
                            .w_full()
                            .justify_start()
                            .accessibility_label("Open local vault (read-only)")
                            .child(
                                div()
                                    .flex_1()
                                    .text_left()
                                    .child("Open local vault (read-only)"),
                            )
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_entry(EntryPage::Local, window, cx)
                            })),
                    ),
            );
        match self.entry_page {
            EntryPage::Overview => {
                if self.profile.is_some() || self.error.is_some() {
                    content =
                        content.child(
                            control("connection-details", cx)
                                .ghost()
                                .justify_start()
                                .accessibility_label(if self.connection_details {
                                    "Hide connection details"
                                } else {
                                    "Connection details"
                                })
                                .child(div().flex_1().text_left().child(
                                    if self.connection_details {
                                        "Hide connection details"
                                    } else {
                                        "Connection details"
                                    },
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connection_details = !this.connection_details;
                                    cx.notify();
                                })),
                        );
                    if self.connection_details {
                        if let Some(attempt) = &self.attempt {
                            content = content.child(div().child(attempt.description()));
                        }
                        if let Some(profile) = &self.profile {
                            content = content.child(div().child(format!("Saved endpoint: {}", profile.endpoint)))
                                .child(div().child(format!("Backend root: {}", profile.identity["root"].as_str().unwrap_or_default())))
                                .child(div().text_sm().text_color(colors.text_muted).child("The backend root identifies the workspace on the server. It is not a folder on this computer."));
                        }
                        if let Some(error) = &self.error {
                            content =
                                content.child(div().text_color(colors.danger).child(error.clone()));
                        }
                    }
                }
            }
            EntryPage::Managed => {
                content = content.child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Open a project brain"))
                    .child("Selecting this backend changes the selected workspace to the brain it identifies. No service or tunnel is started.")
                    .child("Workspace name").child(Input::new(&self.label).disabled(self.busy))
                    .child("Backend address").child(Input::new(&self.endpoint).disabled(self.busy))
                    .child(control("select-brain", cx).primary().label("Select project brain").disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.connect(true, window, cx))));
            }
            EntryPage::Local => {
                content = content
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Read-only local vault"),
                    )
                    .child("Your files stay unchanged.")
                    .child(
                        div()
                            .id("local-open-actions")
                            .track_focus(&self.local_form_focus)
                            .child(super::reader_open::controls(super::reader_open::ControlsPresentation::Entry)),
                    )
                    .child(
                        control("local-advanced-path", cx)
                            .ghost()
                            .label("Enter a folder path…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.connection_details = !this.connection_details;
                                this.error = None;
                                if this.connection_details {
                                    this.vault.update(cx, |s, cx| s.focus(window, cx));
                                }
                                cx.notify();
                            })),
                    )
                    .when(self.connection_details, |content| {
                        content
                            .child("Paste a literal path without surrounding quotes or added backslashes before spaces.")
                        .child(Input::new(&self.vault).disabled(self.busy))
                            .child(
                                control("open-readonly", cx)
                                    .label("Open folder")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_reader(window, cx)
                                    })),
                            )
                    });
            }
        }
        if self.entry_page != EntryPage::Overview {
            if let Some(error) = &self.error {
                if let Some(attempt) = &self.attempt {
                    content = content.child(div().child(attempt.description()));
                }
                content = content.child(div().text_color(colors.danger).child(error.clone()));
            }
            content = content.child(
                control("entry-back", cx)
                    .ghost()
                    .justify_start()
                    .accessibility_label("Back")
                    .child(div().flex_1().text_left().child("Back"))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.choose_entry(EntryPage::Overview, window, cx)
                    })),
            );
        }
        v_flex()
            .id("workspace-entry")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .p_6()
            .items_center()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !this.busy {
                    if this.connection_details {
                        this.connection_details = false;
                    } else if this.entry_page != EntryPage::Overview {
                        this.choose_entry(EntryPage::Overview, window, cx);
                    }
                    cx.notify();
                }
            }))
            .child(content)
    }

    fn install_brain(&mut self, profile: &Profile, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| {
            let mut view = brain::BrainView::new_guarded(
                profile.endpoint,
                Some(profile.identity.clone()),
                window,
                cx,
            );
            view.set_discussion_workspace_label(profile.label.clone());
            view
        });
        self.discussion_subscription = Some(cx.subscribe_in(
            &view,
            window,
            |this, _, event: &brain::OpenDiscussionOwner, window, cx| {
                this.open_discussion_owner(event.clone(), window, cx);
            },
        ));
        self.brain = Some(view);
    }
    fn open_discussion_owner(
        &mut self,
        event: brain::OpenDiscussionOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.brain.as_ref().is_some_and(|b| b.read(cx).dirty(cx)) {
            return;
        }
        let profile = Profile {
            label: "Retained Discussion workspace".into(),
            endpoint: event.endpoint,
            identity: event.workspace,
        };
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                validate_identity(&profile.identity)?;
                let capabilities = brain::rpc(profile.endpoint, json!({"op":"capabilities"}))?;
                profile.verify(&capabilities)?;
                Ok::<_, String>(profile)
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if this.brain.as_ref().is_some_and(|b| b.read(cx).dirty(cx)) {
                    this.error = Some("The source draft changed while connecting. Keep or finish it before opening the original Discussion workspace.".into()); cx.notify(); return;
                }
                match result {
                    Ok(profile) => {
                        if let Err(e) = save_profile(&settings_path(), &profile) { this.error=Some(e); cx.notify(); return; }
                        this.install_brain(&profile, window, cx);
                        this.label.update(cx, |input,cx|input.set_value(profile.label.clone(),window,cx));
                        this.endpoint.update(cx, |input,cx|input.set_value(profile.endpoint.to_string(),window,cx));
                        this.profile=Some(profile); this.connectors=None; this.connector_subscription=None;
                        this.showing_connectors=false; this.showing_brain=true; this.settings=false; this.error=None;
                        if let Some(view)=&this.brain { view.update(cx,|view,_cx|view.queue_retained_discussion(event.operation_id)); }
                    }
                    Err(e)=>this.error=Some(format!("{e} The retained operation was not resent; the current workspace is unchanged.")),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
    fn connect(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.brain.as_ref().is_some_and(|b| b.read(cx).dirty(cx)) {
            self.error = Some(
                "Save or explicitly discard the open source draft before changing the backend."
                    .into(),
            );
            cx.notify();
            return;
        }
        let attempt = match ConnectionAttempt::new(
            replace,
            self.profile.as_ref(),
            self.label.read(cx).value().as_ref(),
            self.endpoint.read(cx).value().as_ref(),
        ) {
            Ok(attempt) => attempt,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.attempt = Some(attempt.clone());
        let endpoint = match attempt.validate() {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.settings = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let capabilities = brain::rpc(endpoint, json!({"op":"capabilities"}))?;
                    let profile = attempt.accept(&capabilities)?;
                    Ok::<_, String>(profile)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_connection(result, &settings_path(), window, cx);
            });
        })
        .detach();
    }
    fn finish_connection(
        &mut self,
        result: Result<Profile, String>,
        profile_path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match result {
            Ok(profile) => {
                if self.brain.as_ref().is_some_and(|b| b.read(cx).dirty(cx)) {
                    self.error = Some("The open draft changed while connecting. Save or discard it before selecting a backend.".into());
                    cx.notify();
                    return;
                }
                if let Err(error) = save_profile(profile_path, &profile) {
                    self.error = Some(error);
                    cx.notify();
                    return;
                }
                self.install_brain(&profile, window, cx);
                self.connector_subscription = None;
                self.connectors = None;
                self.showing_connectors = false;
                self.profile = Some(profile);
                self.showing_brain = true;
                self.settings = false;
                self.entry_page = EntryPage::Overview;
                self.connection_details = false;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
                self.settings = true;
            }
        }
        cx.notify();
    }
    fn export_brain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.brain.is_none() {
            return;
        }
        let Some(profile) = self.profile.clone() else {
            return;
        };
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let downloads = home.join("Downloads");
        let directory = if downloads.is_dir() { downloads } else { home };
        let receiver = cx.prompt_for_new_path(&directory, Some("brain-export.tar"));
        self.busy = true;
        self.export_status =
            Some("Choose where to save the brain archive on this computer…".into());
        self.exported_path = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(path)) => Ok(path),
                Ok(Err(e)) => Err(format!("Cannot open the save dialog: {e}")),
                Err(_) => Err("The save dialog closed unexpectedly.".into()),
            };
            let result = match selected {
                Ok(Some(path)) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.export_status = Some("Exporting saved Markdown and attachments…".into());
                        cx.notify();
                    });
                    cx.background_executor().spawn(async move {
                        let ready = super::export::download(profile.endpoint, &profile.identity, &path)?;
                        let count = |key: &str| ready["manifest"][key].as_array().map_or(0, Vec::len);
                        Ok::<_, String>(Some((path, count("files"), count("exclusions"), count("dependencies"))))
                    }).await
                }
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                match result {
                    Ok(Some((path, files, exclusions, dependencies))) => {
                        this.export_status = Some(format!("Saved {} — {files} files. {exclusions} excluded paths; {dependencies} link dependencies to inspect. Details are in manifest.json.", path.display()));
                        this.exported_path = Some(path);
                    }
                    Ok(None) => this.export_status = None,
                    Err(error) => this.export_status = Some(format!("Export failed: {error}")),
                }
                cx.notify();
            });
        }).detach();
    }

    fn show_surface(&mut self, brain: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.showing_connectors = false;
        self.showing_export = false;
        self.showing_brain = brain;
        self.settings = false;
        cx.notify();
    }
    fn open_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let value = self.vault.read(cx).value();
        if value.trim().is_empty() {
            return;
        }
        let root = PathBuf::from(value.as_ref());
        let opts = Opts {
            vault: Some(root),
            ..Opts::default()
        };
        if let Some(reader) = &self.reader {
            reader.update(cx, |reader, cx| reader.start_loading(opts, window, cx));
        } else {
            self.reader = Some(cx.new(|cx| Reader::new(opts, window, cx).embedded_in_workspace()));
        }
        self.showing_connectors = false;
        self.showing_brain = false;
        self.settings = false;
        self.error = None;
        cx.notify();
    }
}
impl Workspace {
    fn open_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(profile) = &self.profile {
            if self.connectors.is_none() {
                let view = cx.new(|cx| {
                    super::connectors::ConnectorsView::new(
                        profile.endpoint,
                        profile.identity.clone(),
                        window,
                        cx,
                    )
                });
                self.connector_subscription = Some(cx.subscribe_in(
                    &view,
                    window,
                    |this, _, _: &super::connectors::ConnectionsChanged, window, cx| {
                        if let Some(brain) = &this.brain {
                            brain.update(cx, |brain, cx| brain.refresh_connections(window, cx));
                        }
                    },
                ));
                self.connectors = Some(view);
            }
            self.showing_connectors = true;
            self.settings = false;
            self.showing_export = false;
            cx.notify();
        }
    }
}
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use super::brain::Collection;
        use super::brand::{control, palette};
        let colors = palette(cx);
        let label = self
            .profile
            .as_ref()
            .map(|p| p.label.clone())
            .unwrap_or_else(|| "Your workspace".into());
        let brain_visible = self.showing_brain
            && !self.settings
            && !self.showing_connectors
            && !self.showing_export;
        let entry_only = self.reader.is_none() && self.brain.is_none();
        let local_only = self.reader.is_some() && self.brain.is_none();
        let initial_focus = (entry_only
            && self.settings
            && !self.busy
            && self.initial_entry_focus_pending)
            .then(|| {
                let view = cx.entity().downgrade();
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        // Defer from paint so the complete entry focus tree is committed.
                        window.defer(cx, move |window, cx| {
                            let _ = view.update(cx, |this, cx| {
                                if this.busy || !this.settings || !this.initial_entry_focus_pending
                                {
                                    return;
                                }
                                this.initial_entry_focus_pending = false;
                                // Never steal a control the user has already focused.
                                if this.entry_focus.is_focused(window) {
                                    let anchor = if this.profile.is_some() {
                                        &this.retry_entry_focus
                                    } else {
                                        &this.managed_entry_focus
                                    };
                                    anchor.focus(window, cx);
                                    window.focus_next(cx);
                                }
                            });
                        });
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size(px(1.))
            });
        let mut nav = v_flex()
            .id("workspace-navigation")
            .w(px(if window.viewport_size().width < px(1100.) {
                174.
            } else {
                208.
            }))
            .flex_shrink_0()
            .h_full()
            .p_3()
            // Native traffic lights occupy the window corner, outside the detail TitleBar.
            .when(cfg!(target_os = "macos"), |nav| {
                nav.pt(gpui_component::TITLE_BAR_HEIGHT + px(12.))
            })
            .gap_2()
            .bg(colors.sidebar)
            .border_r_1()
            .border_color(colors.border_subtle)
            .child(
                h_flex()
                    .h(px(44.))
                    .gap_2()
                    .child(super::brand::logo(24., cx))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_lg()
                            .child("Tessera"),
                    ),
            )
            .child(
                control("workspace-settings", cx)
                    .ghost()
                    .h_auto()
                    .py_3()
                    .justify_start()
                    .accessibility_label(label.clone())
                    .tooltip(label.clone())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_left()
                            .text_ellipsis()
                            .child(label.clone()),
                    )
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings = true;
                        this.showing_connectors = false;
                        this.showing_export = false;
                        cx.notify();
                    })),
            );
        for (id, title, collection) in [
            ("workspace-inbox", "Inbox", Collection::Inbox),
            ("workspace-attention", "Attention", Collection::Attention),
            ("workspace-brain", "All goals", Collection::Goals),
            ("workspace-sources", "Project brain", Collection::Sources),
        ] {
            let selected = brain_visible
                && self
                    .brain
                    .as_ref()
                    .is_some_and(|b| b.read(cx).collection() == collection);
            nav = nav.child(
                control(id, cx)
                    .ghost()
                    .justify_start()
                    .w_full()
                    .accessibility_label(title)
                    .child(div().flex_1().text_left().child(title))
                    .icon(match collection {
                        Collection::Inbox | Collection::Attention => IconName::Inbox,
                        Collection::Goals => IconName::CircleCheck,
                        Collection::Sources => IconName::FileText,
                    })
                    .when(selected, |b| b.bg(colors.selected).text_color(colors.link))
                    .disabled(self.busy || self.brain.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(brain) = &this.brain {
                            brain.update(cx, |brain, cx| {
                                brain.select_collection(collection, window, cx);
                                brain.refresh_connections(window, cx);
                            });
                        }
                        this.show_surface(true, cx);
                    })),
            );
        }
        nav = nav
            .child(
                control("workspace-reader", cx)
                    .ghost()
                    .justify_start()
                    .accessibility_label("Read-only vault")
                    .child(div().flex_1().text_left().child("Read-only vault"))
                    .icon(IconName::BookOpen)
                    .disabled(self.busy || self.reader.is_none())
                    .when(
                        !self.showing_brain
                            && !self.settings
                            && !self.showing_connectors
                            && !self.showing_export,
                        |b| b.bg(colors.selected),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.show_surface(false, cx))),
            )
            .child(div().flex_1())
            .child(
                control("workspace-connectors", cx)
                    .ghost()
                    .justify_start()
                    .accessibility_label("Connections")
                    .child(div().flex_1().text_left().child("Connections"))
                    .icon(IconName::Settings)
                    .when(self.showing_connectors, |b| b.bg(colors.selected))
                    .disabled(self.busy || self.brain.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.open_connections(window, cx))),
            )
            .child(
                control("workspace-export", cx)
                    .ghost()
                    .justify_start()
                    .accessibility_label("Export brain")
                    .child(div().flex_1().text_left().child("Export brain"))
                    .when(self.showing_export, |b| b.bg(colors.selected))
                    .disabled(self.busy || self.brain.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.showing_export = true;
                        this.settings = false;
                        this.showing_connectors = false;
                        cx.notify();
                    })),
            )
            .child(
                control("workspace-theme", cx)
                    .ghost()
                    .justify_start()
                    .accessibility_label("Change appearance")
                    .child(
                        div()
                            .flex_1()
                            .text_left()
                            .child(format!("Appearance: {}", super::appearance_label(cx))),
                    )
                    .on_click(cx.listener(|_, _, window, cx| super::cycle_appearance(window, cx))),
            )
            .child(div().pt_3().text_xs().text_color(colors.text_muted).child(
                if self.brain.is_some() {
                    "Managed brain"
                } else {
                    "Local workspace"
                },
            ));
        let mut toolbar = h_flex().w_full().gap_3().px_5().h(px(56.));
        if brain_visible {
            if let Some(brain) = &self.brain {
                toolbar = toolbar.child(
                    div()
                        .flex_1()
                        .max_w(px(650.))
                        .child(Input::new(&brain.read(cx).search_input())),
                );
            }
        } else {
            toolbar = toolbar.child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(colors.text_muted)
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(if self.showing_connectors {
                        "Workspace / Connections"
                    } else if self.showing_export {
                        "Workspace / Export"
                    } else if self.settings {
                        if entry_only {
                            if self.profile.is_some() {
                                "Workspace recovery"
                            } else {
                                "Open workspace"
                            }
                        } else {
                            "Workspace settings"
                        }
                    } else {
                        "Local vault · read-only"
                    }),
            );
        }
        if brain_visible {
            toolbar = toolbar.child(
                control("workspace-refresh", cx)
                    .ghost()
                    .label("Refresh")
                    .disabled(self.busy || self.brain.as_ref().is_none_or(|b| b.read(cx).busy()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(brain) = &this.brain {
                            brain.update(cx, |brain, cx| brain.refresh_workspace(window, cx));
                        }
                    })),
            );
        }
        toolbar = toolbar.when(local_only, |toolbar| {
            toolbar
                .when(!self.settings, |toolbar| {
                    toolbar.child(
                        control("local-workspace-settings", cx)
                            .ghost()
                            .label("Workspace")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings = true;
                                this.showing_connectors = false;
                                this.showing_export = false;
                                cx.notify();
                            })),
                    )
                })
                .when(self.settings, |toolbar| {
                    toolbar.child(
                        control("local-return-reader", cx)
                            .ghost()
                            .label("Return to Reader")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_surface(false, cx);
                                if let Some(reader) = &this.reader {
                                    reader.update(cx, |reader, cx| {
                                        let focus = reader.content.read(cx).focus_handle().clone();
                                        focus.focus(window, cx);
                                    });
                                }
                            })),
                    )
                })
        });
        toolbar = toolbar.when(
            !entry_only || self.entry_page != EntryPage::Local,
            |toolbar| {
                toolbar.child(div().id("workspace-open-actions").child(
                    super::reader_open::controls(super::reader_open::ControlsPresentation::Toolbar),
                ))
            },
        );
        toolbar = toolbar.when(entry_only || local_only, |toolbar| {
            toolbar.child(
                control("entry-appearance", cx)
                    .ghost()
                    .label(if local_only {
                        "Appearance".to_owned()
                    } else {
                        format!("Appearance: {}", super::appearance_label(cx))
                    })
                    .tooltip(format!("Appearance: {}", super::appearance_label(cx)))
                    .on_click(cx.listener(|_, _, window, cx| super::cycle_appearance(window, cx))),
            )
        });
        toolbar = toolbar.when(!entry_only && !local_only, |toolbar| {
            toolbar.child(
                control("workspace-capture", cx)
                    .primary()
                    .label("New thought")
                    .icon(IconName::Plus)
                    .disabled(self.busy || self.brain.as_ref().is_none_or(|b| b.read(cx).busy()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_surface(true, cx);
                        if let Some(brain) = &this.brain {
                            brain.update(cx, |brain, cx| brain.begin_thought(window, cx));
                        }
                    })),
            )
        });
        let mut detail = v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(gpui_component::TitleBar::new().h(px(56.)).child(toolbar));
        if self.showing_export {
            let dirty = self.brain.as_ref().is_some_and(|b| b.read(cx).dirty(cx));
            let mut export = v_flex().id("workspace-export-detail").flex_1().min_h_0().overflow_y_scroll().p_6().gap_4()
                .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Export your brain"))
                .child("A portable archive of saved Markdown, attachments and records, with an exact file manifest.")
                .child(div().p_4().rounded(px(8.)).bg(colors.surface_raised).child("Execution journals and credentials are not included. Opening this archive never resumes work."));
            if dirty {
                export=export.child(div().p_4().border_1().border_color(colors.warning).rounded(px(8.)).child("Your source draft is unsaved. It stays in this window; the archive contains the saved file."))
                    .child(control("export-return-draft",cx).label("Return to draft").on_click(cx.listener(|this,_,_,cx|this.show_surface(true,cx))));
            }
            export = export.child(
                control("export-download", cx)
                    .primary()
                    .label(if dirty {
                        "Export saved files"
                    } else {
                        "Choose location & export"
                    })
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.export_brain(window, cx))),
            );
            if let Some(status) = &self.export_status {
                export = export.child(
                    div()
                        .p_4()
                        .bg(colors.surface_raised)
                        .rounded(px(8.))
                        .child(status.clone()),
                );
            }
            if let Some(path) = &self.exported_path {
                let path = path.clone();
                export = export.child(
                    control("show-export", cx)
                        .label("Show file")
                        .on_click(move |_, _, cx| cx.reveal_path(&path)),
                );
            }
            detail = detail.child(export);
        } else if self.showing_connectors {
            if let Some(connectors) = &self.connectors {
                detail = detail.child(div().flex_1().min_h_0().child(connectors.clone()));
            }
        } else if self.settings {
            detail = detail.child(self.render_entry(window, cx));
        } else if self.showing_brain {
            if let Some(brain) = &self.brain {
                detail = detail.child(div().flex_1().min_h_0().child(brain.clone()));
            }
        } else if let Some(reader) = &self.reader {
            detail = detail.child(div().flex_1().min_h_0().child(reader.clone()));
        }
        h_flex()
            .track_focus(&self.entry_focus)
            .tab_stop(false)
            .key_context("TesseraWorkspace")
            .on_action(cx.listener(|this, _: &brain::SearchBrain, window, cx| {
                this.show_surface(true, cx);
                if let Some(brain) = &this.brain {
                    brain.update(cx, |brain, cx| brain.focus_search(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &brain::CaptureThought, window, cx| {
                this.show_surface(true, cx);
                if let Some(brain) = &this.brain {
                    brain.update(cx, |brain, cx| brain.begin_thought(window, cx));
                }
            }))
            .size_full()
            .min_h_0()
            .bg(colors.canvas)
            .text_color(colors.text)
            .when(!entry_only && !local_only, |shell| shell.child(nav))
            .child(detail)
            .when_some(initial_focus, |shell, hook| shell.child(hook))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn profile() -> Profile {
        Profile {
            label: "Fixture".into(),
            endpoint: "127.0.0.1:24161".parse().unwrap(),
            identity: json!({"brain_id":"01000000-0000-4000-8000-000000000001","root":"/tmp/brain","records_dir":"records","managed":true}),
        }
    }
    fn fixture_workspace(window: &mut Window, cx: &mut Context<Workspace>) -> Workspace {
        let view = Workspace {
            reader: None,
            brain: None,
            profile: None,
            connectors: None,
            showing_connectors: false,
            connector_subscription: None,
            discussion_subscription: None,
            label: cx.new(|cx| InputState::new(window, cx)),
            endpoint: cx.new(|cx| InputState::new(window, cx)),
            vault: cx.new(|cx| InputState::new(window, cx)),
            showing_brain: true,
            settings: true,
            busy: false,
            entry_page: EntryPage::Overview,
            connection_details: false,
            attempt: None,
            entry_return_focus: None,
            entry_focus: cx.focus_handle(),
            initial_entry_focus_pending: true,
            retry_entry_focus: cx.focus_handle(),
            managed_entry_focus: cx.focus_handle(),
            local_entry_focus: cx.focus_handle(),
            local_form_focus: cx.focus_handle(),
            overview_feedback: None,
            error: None,
            export_status: None,
            exported_path: None,
            showing_export: false,
        };
        view.focus_initial_entry(window, cx);
        view
    }
    #[gpui::test]
    fn late_file_delivery_never_connects_saved_brain_until_explicit_action(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::super::reader_open::install(cx);
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut saved = profile();
        saved.endpoint = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = requests.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let probe = std::thread::spawn(move || {
            use std::io::Write as _;
            while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _ = stream.write_all(b"{}\n"); // deliberately invalid identity; no installation/write
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1))
                    }
                    Err(error) => panic!("synthetic endpoint failed: {error}"),
                }
            }
        });
        let expected = saved.clone();
        let (view, visual) = cx.add_window_view(|window, cx| {
            Workspace::with_loaded_profile(
                Opts {
                    defer_saved_connection: true,
                    ..Opts::default()
                },
                Ok(Some(saved)),
                window,
                cx,
            )
        });
        visual.run_until_parked();
        let fixture =
            std::env::temp_dir().join(format!("tessera-late327-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&fixture).unwrap();
        let file = fixture.join("exact.md");
        std::fs::write(&file, "# Exact\n").unwrap();
        view.update_in(visual, |view, _, cx| {
            assert!(!view.busy);
            assert!(view.brain.is_none());
            assert!(
                view.attempt.is_none(),
                "no capabilities/provider/import attempt before late event"
            );
            super::super::reader_open::dispatch_urls(
                vec![url::Url::from_file_path(&file).unwrap().into()],
                cx,
            );
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert!(!view.busy);
            assert!(view.brain.is_none());
            assert!(view.attempt.is_none());
            assert_eq!(view.profile, Some(expected.clone()));
            assert_eq!(
                requests.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "late file event must not contact the synthetic backend"
            );
            // Positive control: the existing explicit action still selects exactly
            // the saved identity, even if editable connection fields differ.
            view.endpoint
                .update(cx, |s, cx| s.set_value("127.0.0.1:9", window, cx));
            view.connect(false, window, cx);
            assert!(view.busy);
            assert_eq!(
                view.attempt.as_ref().unwrap().expected,
                Some(expected.clone())
            );
            assert_eq!(
                view.attempt.as_ref().unwrap().endpoint,
                expected.endpoint.to_string()
            );
        });
        visual.run_until_parked();
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "explicit-connect positive control must contact exactly the saved endpoint"
        );
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        probe.join().unwrap();
        std::fs::remove_dir_all(fixture).unwrap();
    }
    #[gpui::test]
    fn local_entry_native_pickers_open_exact_results_and_cancel_neutrally(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::super::reader_open::install(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-onboarding327-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("Заметка space.md");
        std::fs::write(&file, "# Exact\n").unwrap();
        let (view, visual) = cx.add_window_view(fixture_workspace);
        view.update_in(visual, |view, window, cx| {
            view.choose_entry(EntryPage::Local, window, cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(view.error.is_none());
            assert!(!view.connection_details);
        });
        let original_windows = visual.windows().len();
        let click = |visual: &mut VisualTestContext, id: &'static str| {
            let bounds = visual
                .debug_bounds(id)
                .expect("native picker action visible on local entry");
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            assert!(
                visual.did_prompt_for_paths(),
                "positive control: real path prompt requested"
            );
        };
        click(visual, "reader-open-file");
        visual.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            None
        });
        visual.run_until_parked();
        assert_eq!(visual.windows().len(), original_windows);
        view.read_with(visual, |view, _| assert!(view.error.is_none()));
        click(visual, "reader-open-file");
        visual.simulate_path_prompt_response(|_| Some(vec![file.clone()]));
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                super::super::reader_open::reader_locations(cx),
                vec![(root.clone(), "Заметка space.md".into())]
            )
        });
        click(visual, "reader-open-folder");
        visual.simulate_path_prompt_response(|options| {
            assert!(!options.files && options.directories);
            Some(vec![root.clone()])
        });
        visual.run_until_parked();
        visual.update(|_, cx| assert_eq!(super::super::reader_open::reader_locations(cx).len(), 2));
        view.read_with(visual, |view, _| {
            assert!(view.reader.is_none());
            assert!(view.brain.is_none());
            assert!(view.error.is_none());
            assert_eq!(view.entry_page, EntryPage::Local);
        });
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(std::fs::read_to_string(file).unwrap(), "# Exact\n");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn pending_connection_cannot_hide_settings_or_replace_retained_reader(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(fixture_workspace);
        view.update(cx, |view, cx| {
            view.show_surface(false, cx);
            assert!(!view.settings); // Positive control: ordinary navigation succeeds.
            view.busy = true;
            view.settings = true;
            view.show_surface(true, cx);
            assert!(view.settings);
            assert!(!view.showing_brain);
            assert!(view.profile.is_none());
            view.busy = false;
            view.show_surface(true, cx);
            assert!(!view.settings);
            assert!(view.showing_brain);
        });
    }
    #[test]
    fn retry_ignores_edited_fields_and_requires_saved_identity() {
        let saved = profile();
        let retry =
            ConnectionAttempt::new(false, Some(&saved), "", "invalid edited address").unwrap();
        assert_eq!(retry.validate().unwrap(), saved.endpoint);
        assert_eq!(retry.label, saved.label);
        assert_eq!(
            retry
                .accept(&json!({"workspace":saved.identity,"workspace_guard":true}))
                .unwrap(),
            saved
        );
        let mut other = saved.identity.clone();
        other["brain_id"] = json!("02000000-0000-4000-8000-000000000002");
        assert!(retry
            .accept(&json!({"workspace":other,"workspace_guard":true}))
            .is_err());
        assert!(retry.accept(&json!({"workspace":saved.identity})).is_err());
        assert!(ConnectionAttempt::new(false, None, "Edited", "127.0.0.1:99").is_err());
        let selection =
            ConnectionAttempt::new(true, Some(&saved), "Another", "127.0.0.1:99").unwrap();
        assert_eq!(selection.validate().unwrap().port(), 99);
        assert!(selection.expected.is_none());
        assert!(selection
            .description()
            .contains("Selected brain: Another · 127.0.0.1:99"));
        assert_eq!(
            selection
                .accept(&json!({"workspace":other,"workspace_guard":true}))
                .unwrap()
                .identity,
            other
        );
        assert!(
            ConnectionAttempt::new(true, Some(&saved), "Another", "invalid")
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[gpui::test]
    fn secondary_forms_restore_feedback_focus_and_respect_busy(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(fixture_workspace);
        view.update_in(cx, |view, window, cx| {
            assert_eq!(view.entry_page, EntryPage::Overview);
            assert!(view.profile.is_none());
            view.profile = Some(profile());
            view.attempt =
                Some(ConnectionAttempt::new(false, view.profile.as_ref(), "", "").unwrap());
            view.error = Some("Saved endpoint unavailable".into());
            let invoker = cx.focus_handle();
            window.focus(&invoker, cx);
            view.choose_entry(EntryPage::Managed, window, cx);
            assert!(view.label.read(cx).focus_handle(cx).is_focused(window));
            assert!(view.error.is_none());
            view.label
                .update(cx, |input, cx| input.set_value("Unsaved label", window, cx));
            view.choose_entry(EntryPage::Local, window, cx);
            assert!(view.local_form_focus.is_focused(window));
            view.busy = true;
            view.choose_entry(EntryPage::Overview, window, cx);
            assert_eq!(view.entry_page, EntryPage::Local);
            view.busy = false;
            view.choose_entry(EntryPage::Overview, window, cx);
            assert!(!invoker.is_focused(window));
            assert_eq!(view.error.as_deref(), Some("Saved endpoint unavailable"));
            assert_eq!(
                view.attempt.as_ref().unwrap().expected.as_ref(),
                view.profile.as_ref()
            );
            assert_eq!(view.label.read(cx).value().as_ref(), "Unsaved label");
        });
    }

    #[gpui::test]
    fn cold_entry_actions_work_before_any_pointer_input(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for saved in [false, true] {
            let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
            let capture = slot.clone();
            let (_, cx) = cx.add_window_view(move |window, cx| {
                let workspace = cx.new(|cx| {
                    let mut view = fixture_workspace(window, cx);
                    if saved {
                        let mut p = profile();
                        p.endpoint = "127.0.0.1:0".parse().unwrap();
                        view.profile = Some(p);
                    }
                    view
                });
                *capture.borrow_mut() = Some(workspace.clone());
                gpui_component::Root::new(workspace, window, cx)
            });
            let view = slot.borrow().clone().unwrap();
            cx.run_until_parked();
            let enter = |cx: &mut VisualTestContext| {
                let keystroke = Keystroke::parse("enter").unwrap();
                cx.simulate_event(KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                    prefer_character_input: false,
                });
                cx.simulate_event(KeyUpEvent { keystroke });
                cx.run_until_parked();
            };
            // Initial focus is an enabled action, not an inert container. Enter
            // proves first choice / saved retry can activate without a pointer.
            enter(cx);
            if saved {
                view.read_with(cx, |view, _| {
                    assert!(view.attempt.as_ref().unwrap().expected.is_some());
                    assert!(view.error.is_some()); // known-unreachable target, no writes
                });
                cx.simulate_keystrokes("tab");
                cx.run_until_parked();
                enter(cx);
                view.read_with(cx, |view, _| {
                    assert_eq!(view.entry_page, EntryPage::Managed)
                });
            } else {
                view.read_with(cx, |view, _| {
                    assert_eq!(view.entry_page, EntryPage::Managed)
                });
                cx.simulate_keystrokes("escape");
                cx.run_until_parked();
                cx.simulate_keystrokes("tab");
                cx.run_until_parked();
                enter(cx);
                view.read_with(cx, |view, _| assert_eq!(view.entry_page, EntryPage::Local));
            }
        }
    }

    #[gpui::test]
    fn mouse_choice_escape_returns_to_choice_instead_of_previous_appearance_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(fixture_workspace);
        cx.run_until_parked();
        // Appearance is the first tab stop. Pointer activation must not capture it
        // as the return destination merely because the toolkit leaves it focused.
        view.update_in(cx, |view, window, cx| {
            view.entry_focus.focus(window, cx);
            window.focus_next(cx);
        });
        let previous = cx.update(|window, cx| window.focused(cx).unwrap());
        for (id, page) in [
            ("choose-managed-brain", EntryPage::Managed),
            ("choose-local-vault", EntryPage::Local),
        ] {
            cx.update(|window, cx| window.focus(&previous, cx));
            let bounds = cx
                .debug_bounds(id)
                .expect("positive control: choice is visible");
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
            view.read_with(cx, |view, _| assert_eq!(view.entry_page, page));
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.entry_page, EntryPage::Overview)
            });
            cx.update(|window, _| assert!(!previous.is_focused(window)));
            // Enter proves the restored focus belongs to the exact visible invoker,
            // rather than a hidden field, an anchor, or another entry control.
            let keystroke = Keystroke::parse("enter").unwrap();
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
            cx.run_until_parked();
            view.read_with(cx, |view, _| assert_eq!(view.entry_page, page));
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn failed_connection_retains_entities_and_profile_then_same_identity_opens(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(fixture_workspace);
        let directory =
            std::env::temp_dir().join(format!("workspace-recovery-{}", uuid::Uuid::new_v4()));
        let path = directory.join("workspace.json");
        let mut saved = profile();
        // Port zero cannot address an existing backend during scheduled startup reads.
        saved.endpoint = "127.0.0.1:0".parse().unwrap();
        save_profile(&path, &saved).unwrap();
        let original = std::fs::read(&path).unwrap();
        view.update_in(cx, |view, window, cx| {
            view.profile = Some(saved.clone());
            view.install_brain(&saved, window, cx);
            let retained = view.brain.clone().unwrap();
            view.busy = true;
            view.finish_connection(Err("Transport failed".into()), &path, window, cx);
            assert!(!view.busy);
            assert!(view.settings);
            assert_eq!(view.brain.as_ref().unwrap(), &retained);
            assert_eq!(view.profile.as_ref(), Some(&saved));
            assert_eq!(std::fs::read(&path).unwrap(), original);
            view.show_surface(true, cx);
            assert!(!view.settings);
            assert_eq!(view.brain.as_ref().unwrap(), &retained);
            let accepted = ConnectionAttempt::new(false, Some(&saved), "", "")
                .unwrap()
                .accept(&json!({"workspace":saved.identity,"workspace_guard":true}));
            view.finish_connection(accepted, &path, window, cx);
            assert!(!view.settings);
            assert!(view.showing_brain);
            assert!(view.error.is_none());
            assert_eq!(view.profile.as_ref(), Some(&saved));
            assert_eq!(std::fs::read(&path).unwrap(), original);
        });
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn saved_profile_roundtrip_and_replace() {
        let path = std::env::temp_dir().join(format!(
            "tessera-profile-{}/workspace.json",
            uuid::Uuid::new_v4()
        ));
        let mut p = profile();
        save_profile(&path, &p).unwrap();
        assert_eq!(
            Profile::from_value(&serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap())
                .unwrap(),
            p
        );
        p.label = "Renamed".into();
        save_profile(&path, &p).unwrap();
        assert_eq!(
            Profile::from_value(&serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap())
                .unwrap(),
            p
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn changed_identity_or_legacy_backend_is_rejected() {
        let p = profile();
        assert!(p
            .verify(&json!({"workspace":p.identity,"workspace_guard":true}))
            .is_ok());
        for field in ["root", "brain_id", "records_dir", "managed"] {
            let mut identity = p.identity.clone();
            identity[field] = json!("different");
            assert!(p
                .verify(&json!({"workspace":identity,"workspace_guard":true}))
                .is_err());
        }
        assert!(p.verify(&json!({"workspace":p.identity})).is_err());
    }
    #[test]
    fn profile_boundary_is_explicit() {
        let mut value = profile().value();
        value["endpoint"] = json!("192.0.2.1:80");
        assert!(Profile::from_value(&value).is_err());
        value = profile().value();
        value["identity"]["managed"] = json!(false);
        assert!(Profile::from_value(&value).is_err());
        value = profile().value();
        value["identity"]["root"] = json!("../unrelated");
        assert!(Profile::from_value(&value).is_err());
    }
}
