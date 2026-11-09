//! The area's remote manager in the existing Reader window (#1418).
//! Network work stays off the UI thread; questionnaires remain in T3.
use super::*;
#[cfg(native_manager)]
use anyhow::Context as _;
use gpui_component::input::{Textarea, TextareaState};
use okilum_t3::{
    chat::{Conversation, MessageJournal, MessageRecord},
    manifest::AreaBinding,
    Environment, GatewayStatus, ManagerGateway, ServerIdentity,
};
use std::time::Duration;

mod agent_rules;
mod agent_skills;
#[cfg(native_manager)]
mod attachments;
pub(crate) mod board;
mod chat_ui;
#[cfg(native_manager)]
mod chrome;
#[cfg(native_manager)]
mod composer;
mod connections;
pub(crate) mod credentials;
mod design;
mod desktop;
#[cfg(native_manager)]
mod discuss_handoff;
#[cfg(native_manager)]
mod drafts;
mod executors;
#[cfg(native_manager)]
mod feed;
mod launch;
#[cfg(native_manager)]
mod links;
#[cfg(native_manager)]
mod message_types;
mod models;
#[cfg(native_manager)]
mod native;
#[cfg(all(test, native_manager))]
mod panel_tests;
#[cfg(all(target_os = "linux", any(test, feature = "project-ui-harness")))]
pub(crate) mod performance;
pub(crate) mod records;
#[cfg(native_manager)]
mod rotation;
mod rules_reread;
mod setup;
#[cfg(native_manager)]
mod threads;
#[cfg(native_manager)]
mod ttfm;

pub(super) fn save_browser_environment(
    state: &Path,
    environment: &Environment,
) -> anyhow::Result<()> {
    // This is the shared app default, not a project/environment override.
    // Named registry entries belong to explicit project connections.
    connections::save_last_origin(state, environment)
}

pub(super) fn browser_connections(
    state: &Path,
    projects: &[okilum_t3::projects::Project],
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    connections::for_projects(state, projects)
}

pub(super) fn prefill_app_browser_origin(state: Option<&Path>) -> anyhow::Result<Option<String>> {
    connections::last_origin(state)
}

pub(super) fn prefill_browser_origin(
    area: &Path,
    alias: &str,
    state: Option<&Path>,
) -> anyhow::Result<Option<String>> {
    connections::prefill_origin(area, alias, state)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ProjectSection {
    #[default]
    Manager,
    Rules,
    Skills,
}

/// An explicit Skills open keeps the portable vault-relative source identity.
struct ProjectSkillOpen(PathBuf);
impl EventEmitter<ProjectSkillOpen> for ProjectView {}

pub(crate) struct ProjectView {
    binding: Option<AreaBinding>,
    area: Option<PathBuf>,
    context_path: String,
    section: ProjectSection,
    resource_vault: Option<PathBuf>,
    _resource_observer: Option<Subscription>,
    _resource_open: Option<Subscription>,
    rules: Option<Entity<agent_rules::RulesView>>,
    skills: okilum_core::agent_skills::Inventory<agent_skills::Publication>,
    skills_owner: Option<agent_skills::Owner>,
    skills_notice: Option<String>,
    #[cfg(native_manager)]
    rotation: Option<Entity<rotation::coordinator::Coordinator>>,
    #[cfg(native_manager)]
    rotation_epoch: u64,
    #[cfg(native_manager)]
    rotation_connection_epoch: Option<u64>,
    #[cfg(native_manager)]
    rotation_validity: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(native_manager)]
    rotation_resolver: Option<rotation::coordinator::OwnerResolver>,
    #[cfg(native_manager)]
    rotation_expected_owner: Option<okilum_t3::rotation::Owner>,
    #[cfg(native_manager)]
    rotation_notice: Option<String>,
    #[cfg(native_manager)]
    rotation_policy: rotation::policy_owner::State,
    credentials: credentials::Store,
    state: Option<PathBuf>,
    environment: Option<Environment>,
    project_override: bool,
    connection_details: bool,
    gateway: Option<Arc<dyn ManagerGateway>>,
    identity: Option<ServerIdentity>,
    chat: Option<Conversation>,
    records: Vec<MessageRecord>,
    executors_open: bool,
    executors: Option<okilum_t3::executors::Snapshot>,
    executors_error: Option<String>,
    executors_refreshing: Option<u64>,
    model_target: Option<okilum_t3::models::Target>,
    model_ui: models::PickerUi,
    models: Option<okilum_t3::models::Snapshot>,
    model_candidate: Option<okilum_t3::ModelSelection>,
    model_error: Option<String>,
    model_busy: bool,
    model_open: bool,
    opening_t3: bool,
    service_generation: u64,
    launch: launch::Form,
    local_setup: Option<setup::Form>,
    origin: Entity<InputState>,
    compose: Entity<TextareaState>,
    reason: Option<String>,
    error: Option<String>,
    busy: bool,
    active: bool,
    polling: bool,
    generation: u64,
    _input: Subscription,
    _connection: Subscription,
    #[cfg(native_manager)]
    _attachment_input: Subscription,
    #[cfg(native_manager)]
    attachments: attachments::Runtime,
    last_poll_status: Option<GatewayStatus>,
    #[cfg(native_manager)]
    feed: Entity<feed::Feed>,
    #[cfg(native_manager)]
    chrome: Entity<chrome::Chrome>,
    #[cfg(native_manager)]
    composer: Entity<composer::Composer>,
    #[cfg(native_manager)]
    typed: message_types::State,
    #[cfg(native_manager)]
    discuss: discuss_handoff::DiscussHandoff,
    #[cfg(native_manager)]
    native: native::Native,
    #[cfg(native_manager)]
    threads: threads::Directory,
    #[cfg(native_manager)]
    ttfm: ttfm::Ttfm,
    #[cfg(native_manager)]
    ttfm_surface_visible: Option<bool>,
}

pub(super) struct ManagerStarted;
impl EventEmitter<ManagerStarted> for ProjectView {}

#[cfg(native_manager)]
pub(super) struct NativeConnected;
#[cfg(native_manager)]
impl EventEmitter<NativeConnected> for ProjectView {}

impl ProjectView {
    pub(crate) fn new(
        binding: Option<AreaBinding>,
        state: Option<PathBuf>,
        area: PathBuf,
        opened: std::time::Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let access = reader_features::access(reader_features::Feature::Agents, cx);
        let mut view = Self::empty(state.clone(), window, cx);
        #[cfg(native_manager)]
        {
            view.threads.enabled = true;
            view.ttfm_open(opened, cx);
        }
        let service_generation = view.service_generation;
        let area_copy = area.clone();
        view.area = Some(area);
        let state_copy = state.clone();
        let app_origin = cx
            .try_global::<reader_t3_settings::Connection>()
            .and_then(|c| c.origin.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (env, project_override) = match (&binding, &state_copy) {
                        (Some(binding), state) => {
                            let explicit = connections::project_origin(
                                &area_copy,
                                &binding.environment,
                                state.as_deref(),
                            )?;
                            let project_override = explicit.is_some();
                            let origin = explicit
                                .or(app_origin)
                                .or(connections::last_origin(state.as_deref())?);
                            (
                                origin
                                    .map(|origin| {
                                        Environment::from_input(&binding.environment, &origin)
                                    })
                                    .transpose()?,
                                project_override,
                            )
                        }
                        _ => (None, false),
                    };
                    Ok::<_, anyhow::Error>((binding, env, project_override))
                })
                .await;
            let env = this
                .update_in(cx, |this, window, cx| {
                    if this.service_generation != service_generation {
                        return None;
                    }
                    match result {
                        Ok((binding, env, project_override)) => {
                            this.project_override = project_override;
                            this.reason = match &binding {
                                None => Some("This area is not connected to T3.".into()),
                                Some(binding) if binding.manager.thread_id.is_none() => {
                                    Some("This area has no linked manager yet.".into())
                                }
                                _ => None,
                            };
                            this.binding = binding;
                            if let Some(env) = &env {
                                this.origin.update(cx, |input, cx| {
                                    input.set_value(env.origin(), window, cx)
                                });
                            }
                            cx.notify();
                            env
                        }
                        Err(error) => {
                            this.reason = None;
                            this.error = Some(format!("{error:#}"));
                            cx.notify();
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            let Some(env) = env else {
                return;
            };
            let task = match this.update_in(cx, |this, _, cx| {
                let mcp = reader_t3_settings::gateway(&env, cx);
                this.credentials.gateway(env.clone(), mcp, access, cx)
            }) {
                Ok(task) => task,
                Err(_) => return,
            };
            let result = task.await.map(|gateway| (env, gateway));
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_service_connection(service_generation, result, window, cx)
            });
        })
        .detach();
        view
    }

    fn empty(state: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let origin =
            cx.new(|cx| InputState::new(window, cx).placeholder("T3 server address (https://…)"));
        let compose = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(cfg!(native_manager))
                .placeholder("Message your manager…")
        });
        #[cfg(not(native_manager))]
        let input = cx.subscribe(&compose, |_, _, _: &InputEvent, cx| cx.notify());
        #[cfg(native_manager)]
        let parent = cx.entity().downgrade();
        #[cfg(native_manager)]
        let ttfm = ttfm::Ttfm::open(std::time::Instant::now(), false, cx);
        #[cfg(native_manager)]
        let feed = cx.new(|cx| {
            let mut feed = feed::Feed::new(parent.clone(), cx);
            feed.ttfm = Some(ttfm.clone());
            feed
        });
        #[cfg(native_manager)]
        let chrome = cx.new(|cx| chrome::Chrome::new(parent.clone(), cx));
        #[cfg(native_manager)]
        let composer = cx.new(|cx| {
            let mut composer = composer::Composer::new(compose.clone(), cx);
            composer.bind_history(feed.clone());
            composer.bind_parent(parent.clone(), cx);
            composer
        });
        #[cfg(native_manager)]
        let native = native::Native::new(window, cx);
        #[cfg(native_manager)]
        feed.update(cx, |feed, _| feed.actions = Some(native.actions.clone()));
        #[cfg(native_manager)]
        let input = cx.subscribe_in(
            &composer,
            window,
            |this, _, _: &composer::SendRequested, window, cx| {
                this.send(None, window, cx);
            },
        );
        #[cfg(native_manager)]
        let attachment_input = cx.subscribe_in(
            &composer,
            window,
            |this, _, request: &composer::AttachmentRequested, window, cx| {
                this.attachment_request(request, window, cx)
            },
        );
        #[cfg(native_manager)]
        {
            cx.subscribe(&composer, |_, _, _: &composer::TypedPanelChanged, cx| {
                cx.notify()
            })
            .detach();
            cx.subscribe(
                &composer,
                |this, _, request: &composer::RetypeRequested, cx| this.typed_retype(request, cx),
            )
            .detach();
            cx.subscribe(
                &composer,
                |this, _, opened: &composer::TypedMessageOpened, cx| {
                    this.typed_open_message(&opened.0, &opened.1, cx)
                },
            )
            .detach();
            cx.subscribe(
                &composer,
                |this, _, request: &composer::TypedReloadRequested, cx| {
                    if this.typed_scope().as_ref() == Some(&request.0)
                        && !this.typed_annotation_writing()
                    {
                        this.typed_reload_annotations(cx);
                    }
                },
            )
            .detach();
            cx.observe(&feed, |this, _, cx| this.typed_sync(cx))
                .detach();
        }
        #[cfg(native_manager)]
        cx.subscribe_in(
            &composer,
            window,
            |this, _, _: &composer::HistoryRecalled, window, cx| {
                this.draft_changed(window, cx);
            },
        )
        .detach();
        let weak = cx.weak_entity();
        let connection =
            window.observe_global::<reader_t3_settings::Connection>(cx, move |window, cx| {
                let origin = cx
                    .try_global::<reader_t3_settings::Connection>()
                    .and_then(|c| c.origin.clone());
                let _ = weak.update(cx, |this, cx| {
                    if this.local_setup.is_some() {
                        this.error = None;
                        cx.notify();
                        return;
                    }
                    if let Some(origin) = origin {
                        if reader_t3_settings::signed_out(&origin, cx) {
                            if this
                                .environment
                                .as_ref()
                                .is_some_and(|env| env.origin() == origin)
                            {
                                this.clear_connection(cx);
                            }
                            return;
                        }
                        // A project's explicit different server must not inherit another token.
                        if this.project_override
                            && this
                                .environment
                                .as_ref()
                                .is_some_and(|env| env.origin() != origin)
                        {
                            return;
                        }
                        this.origin
                            .update(cx, |input, cx| input.set_value(origin, window, cx));
                        this.connect_mode(false, window, cx);
                    }
                });
            });
        #[cfg(native_manager)]
        {
            let weak = cx.weak_entity();
            window
                .observe_global::<reader_t3_settings::manager::Connections>(
                    cx,
                    move |window, cx| {
                        let _ =
                            weak.update(cx, |this, cx| this.manager_connection_changed(window, cx));
                    },
                )
                .detach();
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    reader_t3_settings::manager::activated(window, cx);
                }
            })
            .detach();
        }
        Self {
            binding: None,
            area: None,
            context_path: String::new(),
            section: ProjectSection::Manager,
            resource_vault: None,
            _resource_observer: None,
            _resource_open: None,
            rules: None,
            skills: Default::default(),
            skills_owner: None,
            skills_notice: None,
            #[cfg(native_manager)]
            rotation: None,
            #[cfg(native_manager)]
            rotation_epoch: 0,
            #[cfg(native_manager)]
            rotation_connection_epoch: None,
            #[cfg(native_manager)]
            rotation_validity: None,
            #[cfg(native_manager)]
            rotation_resolver: None,
            #[cfg(native_manager)]
            rotation_expected_owner: None,
            #[cfg(native_manager)]
            rotation_notice: None,
            #[cfg(native_manager)]
            rotation_policy: Default::default(),
            credentials: credentials::Store::default(),
            state,
            environment: None,
            project_override: true,
            connection_details: false,
            gateway: None,
            identity: None,
            chat: None,
            records: Vec::new(),
            executors_open: false,
            executors: None,
            executors_error: None,
            executors_refreshing: None,
            model_target: None,
            model_ui: Default::default(),
            models: None,
            model_candidate: None,
            model_error: None,
            model_busy: false,
            model_open: false,
            opening_t3: false,
            service_generation: 0,
            launch: launch::Form::new(window, cx),
            local_setup: None,
            origin,
            compose,
            reason: Some("Loading project…".into()),
            error: None,
            busy: false,
            active: true,
            polling: false,
            generation: 0,
            _input: input,
            _connection: connection,
            #[cfg(native_manager)]
            _attachment_input: attachment_input,
            #[cfg(native_manager)]
            attachments: attachments::Runtime::default(),
            last_poll_status: None,
            #[cfg(native_manager)]
            feed,
            #[cfg(native_manager)]
            chrome,
            #[cfg(native_manager)]
            composer,
            #[cfg(native_manager)]
            typed: Default::default(),
            #[cfg(native_manager)]
            discuss: Default::default(),
            #[cfg(native_manager)]
            native,
            #[cfg(native_manager)]
            threads: threads::Directory::default(),
            #[cfg(native_manager)]
            ttfm,
            #[cfg(native_manager)]
            ttfm_surface_visible: None,
        }
    }

    #[cfg(native_manager)]
    pub(super) fn invalidate_rotation(&mut self, cx: &mut Context<Self>) {
        // Revoke before dropping/hiding the retained surface, including while a
        // draft barrier or store read is outstanding. Durable claims stay intact.
        if let Some(coordinator) = &self.rotation {
            coordinator.update(cx, |coordinator, cx| coordinator.invalidate(cx));
        }
        self.revoke_rotation();
    }

    /// Also callable from the existing synchronous handoff lifecycle hook,
    /// before the next paint or an async draft/store completion can run.
    #[cfg(native_manager)]
    pub(super) fn revoke_rotation(&mut self) {
        self.rotation_policy.revoke_requests();
        let active = self.rotation_resolver.is_some() || self.rotation.is_some();
        if let Some(valid) = self.rotation_validity.take() {
            valid.store(false, std::sync::atomic::Ordering::Release);
        }
        self.rotation = None;
        if active {
            self.rotation_epoch = self
                .rotation_epoch
                .checked_add(1)
                .expect("Rotation mount generation exhausted");
        }
        self.rotation_connection_epoch = None;
        self.rotation_resolver = None;
        self.rotation_expected_owner = None;
        self.rotation_notice = None;
    }

    /// The configured policy owner supplies its exact lifecycle revision; never
    /// derive one from a loaded checkpoint or substitute the connection counter.
    #[cfg(native_manager)]
    fn rotation_owner(view: &ProjectView, cx: &App) -> Option<okilum_t3::rotation::Owner> {
        if !view.active
            || view.section != ProjectSection::Manager
            || view.executors_open
            || !view.draft_handoff_surface_is_visible()
            || !view.draft_handoff_owner_ready(cx)
            || view.typed_scope().is_none()
            || view.native.deleted()
            || view.rotation_connection_epoch != Some(view.native.connection_epoch)
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return None;
        }
        let client = view.native.model_client()?;
        if !client.access_allowed()
            || view
                .threads
                .handoff_revoked
                .as_ref()
                .is_some_and(|revoked| Arc::ptr_eq(revoked, &client))
        {
            return None;
        }
        let environment = view.environment.as_ref()?;
        if reader_t3_settings::signed_out(&environment.origin(), cx) {
            return None;
        }
        let owner = (view.rotation_resolver?)(view, cx)?;
        let binding = view.binding.as_ref()?;
        if owner.origin != environment.origin()
            || binding.manager.project_id.as_ref() != Some(&owner.project)
            || binding.manager.thread_id.as_ref() != Some(&owner.manager)
            || view.chat.as_ref()?.thread_id != owner.manager
        {
            return None;
        }
        Some(owner)
    }

    /// Policy owner handoff: the caller supplies its canonical PolicyStore and
    /// resolver, plus only an exact reviewed source (None keeps Resume disabled).
    /// Loading is off-thread; Coordinator::mount captures the fresh typed draft.
    #[cfg(native_manager)]
    pub(super) fn mount_pending_rotation(
        &mut self,
        store: Arc<okilum_t3::rotation::PolicyStore>,
        exact_reviewed_source: Option<String>,
        owner: fn(&ProjectView, &App) -> Option<okilum_t3::rotation::Owner>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_rotation(cx);
        self.rotation_resolver = Some(owner);
        self.rotation_connection_epoch = Some(self.native.connection_epoch);
        let Some(expected_owner) = Self::rotation_owner(self, cx) else {
            self.revoke_rotation();
            return;
        };
        self.rotation_expected_owner = Some(expected_owner.clone());
        let epoch = self.rotation_epoch;
        cx.spawn_in(window, async move |this, cx| {
            let read_store = store.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move { read_store.load() })
                .await;
            // Mount outside ProjectView::update: the coordinator re-enters the
            // owner to capture ComposerSnapshot and must not borrow it recursively.
            let _ = cx.update(|window, cx| {
                let Some(project) = this.upgrade() else {
                    return;
                };
                let current = project.update(cx, |view, cx| {
                    view.rotation_epoch == epoch
                        && Self::rotation_owner(view, cx).as_ref() == Some(&expected_owner)
                });
                if !current {
                    return;
                }
                let mounted = match loaded {
                    Ok(Some(checkpoint)) if checkpoint.pending.is_some() => {
                        rotation::coordinator::Coordinator::mount(
                            &project,
                            store,
                            checkpoint,
                            exact_reviewed_source,
                            Self::rotation_owner,
                            window,
                            cx,
                        )
                        .map(Some)
                    }
                    Ok(_) => Ok(None),
                    Err(error) => Err(error),
                };
                project.update(cx, |view, cx| {
                    if view.rotation_epoch != epoch
                        || Self::rotation_owner(view, cx).as_ref() != Some(&expected_owner)
                    {
                        return;
                    }
                    match mounted {
                        Ok(coordinator) => {
                            view.rotation_validity = coordinator
                                .as_ref()
                                .map(|coordinator| coordinator.read(cx).revocation_token());
                            view.rotation = coordinator;
                        }
                        Err(error) => view.rotation_notice = Some(error),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// Observe the Reader's reconciled vault publication. This covers the
    /// existing local watcher and shared-session path without a second watcher.
    fn bind_resources(
        &mut self,
        reader: &Entity<Reader>,
        root: PathBuf,
        mut published: Arc<Vault>,
        mut publication_epoch: u64,
        cx: &mut Context<Self>,
    ) {
        let scope_changed = self.resource_vault.as_ref() != Some(&root);
        #[cfg(native_manager)]
        if scope_changed {
            self.invalidate_rotation(cx);
        }
        agent_skills::bind(self, reader.downgrade(), root.clone());
        self.resource_vault = Some(root.clone());
        self._resource_observer = Some(cx.observe(reader, move |this, reader, cx| {
            let reader = reader.read(cx);
            if reader.vault_root != root
                || (Arc::ptr_eq(&published, &reader.vault)
                    && publication_epoch == reader.source_publication_epoch)
            {
                return;
            }
            published = reader.vault.clone();
            publication_epoch = reader.source_publication_epoch;
            agent_skills::published(this, cx);
            // A publication represents a reconciled source batch; rebuild rather
            // than infer which imported/renamed source influenced the stack.
            this.resources_changed(
                &okilum_core::Changes {
                    rescan: true,
                    ..Default::default()
                },
                cx,
            );
        }));
        if scope_changed {
            self.sync_resource_lifecycle(cx);
        }
    }

    fn select_section(&mut self, section: ProjectSection, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        #[cfg(native_manager)]
        self.invalidate_rotation(cx);
        #[cfg(native_manager)]
        self.draft_handoff_connection_changing();
        self.section = section;
        #[cfg(native_manager)]
        self.draft_handoff_connection_ready(cx);
        self.sync_resource_lifecycle(cx);
        cx.notify();
    }

    fn sync_resource_lifecycle(&mut self, cx: &mut Context<Self>) {
        let enabled = self.active && reader_features::enabled(reader_features::Feature::Agents, cx);
        let scope = self.resource_vault.clone().zip(self.area.clone());
        if enabled && self.section == ProjectSection::Rules {
            if let Some((vault, area)) = scope {
                if let Some(rules) = &self.rules {
                    rules.update(cx, |view, cx| view.bind(vault, area, cx));
                } else {
                    self.rules =
                        Some(cx.new(|cx| agent_rules::RulesView::for_project(vault, area, cx)));
                }
            }
        } else if let Some(rules) = &self.rules {
            rules.update(cx, |view, cx| view.deactivate(cx));
        }
        if enabled && self.section == ProjectSection::Skills {
            self.refresh_skills(cx);
        } else {
            self.skills.clear();
            self.skills_notice = None;
        }
    }

    /// Also usable by the owner's immediate watcher callback before reconciliation.
    pub(super) fn service_metadata_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) = self.area.clone() else {
            return;
        };
        self.service_generation = self.service_generation.wrapping_add(1);
        let generation = self.service_generation;
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let layout = okilum_t3::service_layout::ServiceLayout::resolve(&area)?;
                    let project = okilum_t3::projects::Project::read(&area)?;
                    let binding = project.native_binding()?;
                    let origin = project
                        .environment
                        .as_deref()
                        .map(|alias| prefill_browser_origin(&area, alias, state.as_deref()))
                        .transpose()?
                        .flatten();
                    layout.check_current()?;
                    Ok::<_, anyhow::Error>((binding, origin))
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.service_generation != generation {
                    return;
                }
                match result {
                    Ok((binding, origin))
                        if binding == this.binding
                            && origin.as_deref()
                                == this.environment.as_ref().map(|env| env.origin()).as_deref() => {
                    }
                    Ok(_) => {
                        this.clear_connection(cx);
                        this.error = Some(
                            "Project service binding changed; reconnect to the existing Manager"
                                .into(),
                        );
                    }
                    Err(error) => {
                        this.clear_connection(cx);
                        this.error = Some(format!("Project service metadata: {error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn resources_changed(&mut self, changes: &okilum_core::Changes, cx: &mut Context<Self>) {
        if !self.active
            || changes.is_empty()
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return;
        }
        if self.section == ProjectSection::Rules {
            if let Some(rules) = &self.rules {
                rules.update(cx, |view, cx| view.external_changed(changes, cx));
            }
        }
        // Invalidate even while another request runs: its old generation must
        // never restore availability after a rename/delete or project switch.
        if self.section == ProjectSection::Skills {
            self.skills.invalidate("Local project sources changed");
            self.refresh_skills(cx);
        }
    }

    fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        agent_skills::reload(self, cx);
    }

    fn render_resource_section(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.section {
            ProjectSection::Rules => self.rules.as_ref().map_or_else(
                || {
                    div()
                        .child("Select a local project to inspect its rules.")
                        .into_any_element()
                },
                |rules| {
                    v_flex()
                        .size_full()
                        .flex_1()
                        .min_h_0()
                        .child(rules.clone())
                        .into_any_element()
                },
            ),
            ProjectSection::Skills => {
                let refresh = cx.entity().downgrade();
                let open = refresh.clone();
                let displayed_scope = self.skills.scope().cloned();
                let displayed_revision = self.skills.inventory_revision().cloned();
                v_flex()
                    .id("project-skills")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_2()
                    .when_some(self.skills_notice.clone(), |view, notice| {
                        view.child(div().text_color(cx.theme().danger).child(notice))
                    })
                    .child(agent_skills::render(
                        &self.skills,
                        move |_, cx| {
                            let _ = refresh.update(cx, |this, cx| this.refresh_skills(cx));
                        },
                        move |result, _, cx| {
                            let _ = open.update(cx, |this, cx| {
                                if !agent_skills::display_current(
                                    this,
                                    displayed_scope.as_ref(),
                                    displayed_revision.as_ref(),
                                    cx,
                                ) {
                                    return;
                                }
                                match result {
                                    Ok(path)
                                        if this.active
                                            && this.section == ProjectSection::Skills
                                            && this.skills.scope() == displayed_scope.as_ref()
                                            && reader_features::enabled(
                                                reader_features::Feature::Agents,
                                                cx,
                                            ) =>
                                    {
                                        cx.emit(ProjectSkillOpen(path))
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        this.skills.invalidate(error.clone());
                                        this.skills_notice = Some(error);
                                        cx.notify();
                                    }
                                }
                            });
                        },
                    ))
                    .into_any_element()
            }
            ProjectSection::Manager => div().into_any_element(),
        }
    }

    pub(super) fn suspend_module(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active(false, window, cx);
        self.clear_connection(cx);
    }

    /// Append only through the exact supported visible composer. This never
    /// selects a thread, prepares a send intent or falls back to a hidden view.
    pub(super) fn append_board_context(
        &mut self,
        area: &Path,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), okilum_t3::draft_handoff::Refusal> {
        #[cfg(not(native_manager))]
        {
            let _ = (area, text, window, cx);
            Err(okilum_t3::draft_handoff::Refusal::Unavailable)
        }
        #[cfg(native_manager)]
        {
            use okilum_t3::draft_handoff::{ComposerPort, Refusal};
            if self.area.as_deref() != Some(area) {
                return Err(Refusal::WrongTarget);
            }
            if text.trim().is_empty() {
                return Err(Refusal::Empty);
            }
            let target = self.draft_handoff_target(cx).ok_or(Refusal::Unavailable)?;
            let mut discuss = std::mem::take(&mut self.discuss);
            // One persistent sender owns IDs across clicks and A → B → A.
            // Restore it on refusal too: the receiver outlives each action.
            let result = (|| {
                discuss.set_visible(Some(target.clone()));
                let request = discuss.begin(&target.project, format!("---\n{text}"))?;
                match self.draft_handoff_port(window, cx).append(&request) {
                    Ok(ack) => {
                        discuss.set_visible(self.draft_handoff_target(cx));
                        discuss.acknowledge(ack)
                    }
                    Err(error) => {
                        discuss.cancel(&request);
                        Err(error)
                    }
                }
            })();
            self.discuss = discuss;
            result
        }
    }

    pub(crate) fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active_since(active, std::time::Instant::now(), window, cx);
    }

    fn set_active_since(
        &mut self,
        active: bool,
        opened: std::time::Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(native_manager)]
        if active && !self.active {
            self.ttfm_open(opened, cx);
        }
        #[cfg(native_manager)]
        if !active {
            self.ttfm.cancel();
            self.invalidate_rotation(cx);
        }
        #[cfg(native_manager)]
        if !active {
            self.draft_suspend_typed(cx);
            self.typed_invalidate(false, cx);
        }
        self.active = active;
        self.sync_resource_lifecycle(cx);
        if !active {
            self.opening_t3 = false;
        }
        #[cfg(native_manager)]
        self.set_threads_active(active, window, cx);
        #[cfg(native_manager)]
        if active && (self.threads.enabled || self.native.active()) {
            self.draft_restore(window, cx);
            self.native_start(window, cx);
            if self.threads.enabled && !self.native.active() && self.environment.is_some() {
                self.native_restore(false, window, cx);
            }
        } else {
            self.native.pause();
        }
        if active {
            self.start_polling(window, cx);
        }
    }

    fn finish_service_connection(
        &mut self,
        generation: u64,
        result: anyhow::Result<(Environment, Arc<dyn ManagerGateway>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.service_generation != generation {
            return;
        }
        self.finish_connection(result, window, cx);
    }

    fn finish_connection(
        &mut self,
        result: anyhow::Result<(Environment, Arc<dyn ManagerGateway>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        self.busy = false;
        if result
            .as_ref()
            .is_ok_and(|(env, _)| reader_t3_settings::signed_out(&env.origin(), cx))
        {
            self.clear_connection(cx);
            return;
        }
        match result {
            Ok((env, gateway)) => {
                #[cfg(native_manager)]
                self.invalidate_rotation(cx);
                #[cfg(native_manager)]
                self.attachments.clear();
                #[cfg(native_manager)]
                self.draft_flush(cx);
                #[cfg(native_manager)]
                {
                    self.draft_suspend_typed(cx);
                    self.typed_invalidate(true, cx);
                    self.typed_clear_pending(window, cx);
                }
                #[cfg(native_manager)]
                {
                    self.set_threads_active(false, window, cx);
                    self.threads.reset(cx);
                    self.native.clear();
                }
                self.environment = Some(env);
                self.connection_details = false;
                self.opening_t3 = false;
                self.gateway = Some(gateway);
                self.chat = self
                    .binding
                    .as_ref()
                    .and_then(|b| b.manager.thread_id.as_ref())
                    .map(Conversation::new);
                #[cfg(native_manager)]
                self.feed.update(cx, |feed, cx| feed.reset(cx));
                self.last_poll_status = None;
                self.identity = None;
                #[cfg(native_manager)]
                self.draft_prepare_identity(window, cx);
                self.model_target = None;
                self.model_ui = Default::default();
                self.model_open = false;
                self.model_busy = false;
                self.models = None;
                self.model_candidate = None;
                self.model_error = None;
                self.error = None;
                self.generation += 1;
                self.start_polling(window, cx);
                #[cfg(native_manager)]
                if self.threads.enabled {
                    self.native_restore(false, window, cx);
                }
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn clear_connection(&mut self, cx: &mut Context<Self>) {
        #[cfg(native_manager)]
        self.invalidate_rotation(cx);
        #[cfg(native_manager)]
        self.attachments.clear();
        #[cfg(native_manager)]
        self.draft_flush(cx);
        #[cfg(native_manager)]
        {
            self.draft_suspend_typed(cx);
            self.typed_invalidate(true, cx);
        }
        self.generation = self.generation.wrapping_add(1);
        self.polling = false;
        self.busy = false;
        self.gateway = None;
        self.identity = None;
        self.connection_details = false;
        self.opening_t3 = false;
        self.last_poll_status = None;
        self.model_target = None;
        self.model_ui = Default::default();
        self.model_open = false;
        self.model_busy = false;
        self.models = None;
        self.model_candidate = None;
        self.model_error = None;
        self.error = None;
        #[cfg(native_manager)]
        {
            self.threads.reset(cx);
            self.native.clear();
        }
        // History, retained send intents, and the unsent draft stay intact.
        cx.notify();
    }

    fn connection_is_compact(&self, _status: &GatewayStatus) -> bool {
        cfg!(native_manager) || (!self.connection_details && self.environment.is_some())
    }

    fn connection_line(&self, status: &GatewayStatus, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let Some(env) = self.environment.clone() else {
            return div().into_any_element();
        };
        let origin = env.origin();
        let host = url::Url::parse(&origin)
            .ok()
            .map(|url| url[url::Position::BeforeHost..url::Position::AfterPort].to_owned())
            .unwrap_or(origin);
        let _ = host;
        #[cfg(not(native_manager))]
        let label = match status {
            GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. } => "Connected",
            _ => "Offline",
        };
        #[cfg(native_manager)]
        let label = match self.manager_stage(cx) {
            reader_t3_settings::manager::Stage::Connected => self.native.status_label(cx),
            stage => stage.label(),
        };
        let owner = cx.entity().downgrade();
        let disabled = self.busy || self.model_busy || !reader_t3_settings::can_sign_out(&env, cx);
        let renew = matches!(status, GatewayStatus::ExpiresSoon { .. });
        let terminal_area = self.area.clone();
        h_flex()
            .items_center()
            .gap_2()
            .debug_selector(|| "project-connected-line".into())
            .child(div().size(px(6.)).rounded_full().bg(match label {
                "Connected" | "Ready" => brand::palette(cx).success,
                "Working" => brand::palette(cx).accent,
                "Connecting" | "Reconnecting" | "Reconnect needed" => brand::palette(cx).warning,
                "Error" => cx.theme().danger,
                _ => brand::palette(cx).text_muted,
            }))
            .child(
                div()
                    .min_w_0()
                    .text_size(px(design::LABEL))
                    .debug_selector(|| "project-chat-status".into())
                    .text_color(brand::palette(cx).text_muted)
                    .child(label),
            )
            .child(
                Button::new("project-connection-menu")
                    .debug_selector(|| "project-connection-menu".into())
                    .ghost()
                    .small()
                    .icon(IconName::Ellipsis)
                    .tooltip("Manager options")
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                        let change = owner.clone();
                        let diagnostics = owner.clone();
                        let executors = owner.clone();
                        let launch = owner.clone();
                        let sign_out = env.clone();
                        let terminal_area = terminal_area.clone();
                        #[cfg(native_manager)]
                        let menu = menu.item(rotation::policy_owner::menu_item(owner.clone(), cx));
                        menu.item(PopupMenuItem::new("Details").on_click(move |_, _, cx| {
                            let _ = diagnostics.update(cx, |this, cx| {
                                this.connection_details = true;
                                cx.notify();
                            });
                        }))
                        .item(
                            PopupMenuItem::new("Open terminal")
                                .disabled(terminal_area.is_none())
                                .on_click(move |_, window, cx| {
                                    if let Some(area) = &terminal_area {
                                        project_terminal::open_project(area.clone(), window, cx);
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Executors").on_click(move |_, window, cx| {
                                let _ = executors.update(cx, |this, cx| {
                                    #[cfg(native_manager)]
                                    this.invalidate_rotation(cx);
                                    this.executors_open = !this.executors_open;
                                    this.start_polling(window, cx);
                                    cx.notify();
                                });
                            }),
                        )
                        .item(
                            PopupMenuItem::new("Request executor")
                                .disabled(disabled)
                                .on_click(move |_, window, cx| {
                                    let _ =
                                        launch.update(cx, |this, cx| this.open_launch(window, cx));
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(if renew {
                                "Renew connection…"
                            } else {
                                "Change connection…"
                            })
                            .on_click(move |_, _, cx| {
                                let _ = change.update(cx, |this, cx| {
                                    this.connection_details = true;
                                    cx.notify();
                                });
                            }),
                        )
                        .item(
                            PopupMenuItem::new("Sign out").disabled(disabled).on_click(
                                move |_, _, cx| {
                                    reader_t3_settings::sign_out(sign_out.clone(), cx);
                                },
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    fn connection_environment(&self, cx: &App) -> anyhow::Result<Environment> {
        let binding = self
            .binding
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("This area is not connected to T3."))?;
        Environment::from_input(&binding.environment, &self.origin.read(cx).value())
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_override = true;
        self.connect_mode(false, window, cx);
    }

    fn connect_mode(&mut self, sign_in_again: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        let access = reader_features::access(reader_features::Feature::Agents, cx);
        if sign_in_again {
            reader_settings::show_t3(cx);
            return;
        }
        if self.busy || self.model_busy {
            return;
        }
        let env = match self.connection_environment(cx) {
            Ok(env) => env,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let service_generation = self.service_generation;
        #[cfg(native_manager)]
        if self.threads.enabled {
            self.invalidate_rotation(cx);
            self.set_threads_active(false, window, cx);
            self.threads.reset(cx);
            self.native.clear();
            self.generation = self.generation.wrapping_add(1);
            let generation = self.generation;
            let state = self.state.clone().filter(|_| self.project_override);
            self.busy = true;
            cx.spawn_in(window, async move |this, cx| {
                let saved_env = env.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        state
                            .as_deref()
                            .map_or(Ok(()), |state| connections::save(state, &saved_env))
                    })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.generation != generation
                        || this.service_generation != service_generation
                    {
                        return;
                    }
                    this.busy = false;
                    match result {
                        Ok(()) => {
                            this.environment = Some(env);
                            this.native_restore(true, window, cx);
                        }
                        Err(error) => {
                            this.error = Some(error.to_string());
                            cx.notify();
                        }
                    }
                });
            })
            .detach();
            return;
        }
        self.busy = true;
        let state = self.state.clone().filter(|_| self.project_override);
        cx.spawn_in(window, async move |this, cx| {
            let task = match this.update_in(cx, |this, _, cx| {
                let mcp = reader_t3_settings::gateway(&env, cx);
                this.credentials.gateway(env.clone(), mcp, access, cx)
            }) {
                Ok(task) => task,
                Err(_) => return,
            };
            let result = match task.await {
                Ok(gateway) => {
                    cx.background_executor()
                        .spawn(async move {
                            if let Some(state) = state {
                                connections::save(&state, &env)?;
                            }
                            Ok((env, gateway))
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_service_connection(service_generation, result, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn start_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (Some(area), Some(state), Some(gateway), Some(environment), Some(project_id)) = (
            self.area.clone(),
            self.state.clone(),
            self.gateway.clone(),
            self.environment.clone(),
            self.binding
                .as_ref()
                .and_then(|b| b.manager.project_id.clone()),
        ) else {
            self.error =
                Some("Connect to the configured T3 project before starting a manager.".into());
            cx.notify();
            return;
        };
        let context = self.context_path.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::reader_manager_start::start(
                        &area,
                        &context,
                        &state,
                        gateway.as_ref(),
                        &environment,
                        &project_id,
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(thread) => {
                        if let Some(binding) = &mut this.binding {
                            binding.manager.thread_id = Some(thread.clone());
                        }
                        this.chat = Some(Conversation::new(thread));
                        #[cfg(native_manager)]
                        this.feed.update(cx, |feed, cx| feed.reset(cx));
                        this.reason = None;
                        this.start_polling(window, cx);
                        cx.emit(ManagerStarted);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn can_send(&self) -> bool {
        #[cfg(native_manager)]
        if self.native.active() {
            return self.draft_ready()
                && !self.native.deleted()
                && self.native.client_can_operate()
                && self.prepared_attachments().is_ok();
        }
        #[cfg(native_manager)]
        if !self.draft_is_sendable() {
            return false;
        }

        self.identity.is_some()
            && self.gateway.as_ref().is_some_and(|gateway| {
                matches!(
                    gateway.status(),
                    GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. }
                )
            })
    }

    fn journal(&self) -> Option<MessageJournal> {
        Some(MessageJournal::new(
            self.state.as_ref()?,
            self.environment.as_ref()?,
            &self.chat.as_ref()?.thread_id,
        ))
    }

    fn start_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(native_manager)]
        if self.threads.enabled && !self.executors_open {
            return;
        }
        if self.polling || !self.active || self.gateway.is_none() || self.chat.is_none() {
            return;
        }
        self.polling = true;
        cx.spawn_in(window, async move |this, cx| loop {
            let job = this
                .update_in(cx, |this, window, cx| {
                    if !this.active {
                        this.polling = false;
                        return None;
                    }
                    #[cfg(native_manager)]
                    if (this.native.active() || this.threads.enabled) && !this.executors_open {
                        this.polling = false;
                        return None;
                    }
                    let gateway = this.gateway.clone()?;
                    if matches!(
                        gateway.status(),
                        GatewayStatus::NotConnected
                            | GatewayStatus::Expired
                            | GatewayStatus::Revoked
                    ) {
                        this.polling = false;
                        cx.notify();
                        return None;
                    }
                    Some((
                        gateway,
                        this.chat.as_ref()?.poll_request(),
                        this.journal(),
                        this.generation,
                        window.is_window_active(),
                        this.binding.as_ref()?.manager.project_id.clone()?,
                        (
                            this.models.is_some() && {
                                #[cfg(native_manager)]
                                {
                                    !this.native.active() && !this.threads.enabled
                                }
                                #[cfg(not(native_manager))]
                                {
                                    true
                                }
                            },
                            this.chat.as_ref()?.thread_id.clone(),
                        ),
                        this.executors_open
                            .then(|| this.area.clone().zip(this.binding.clone()))
                            .flatten(),
                    ))
                })
                .ok()
                .flatten();
            let Some((
                gateway,
                chat,
                journal,
                generation,
                focused,
                expected_project,
                (read_model_configuration, model_thread),
                executor_binding,
            )) = job
            else {
                break;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let identity = gateway.server_identity()?;
                    let chat = chat.fetch(gateway.as_ref())?;
                    anyhow::ensure!(
                        chat.project_id == expected_project,
                        "The linked manager belongs to another T3 project. Check the area binding."
                    );
                    let records = journal
                        .map(|journal| journal.records())
                        .transpose()?
                        .unwrap_or_default();
                    let executors = executor_binding.map(|(area, binding)| {
                        okilum_t3::executors::Snapshot::load(gateway.as_ref(), &area, &binding)
                    });
                    let configuration = read_model_configuration
                        .then(|| gateway.thread_configuration(&model_thread));
                    Ok::<_, anyhow::Error>((chat, identity, records, executors, configuration))
                })
                .await;
            let next = this
                .update_in(cx, |this, window, cx| {
                    if generation != this.generation {
                        return 0;
                    }
                    let mut delay = if focused { 2_000 } else { 15_000 };
                    let status = this.gateway.as_ref().map(|gateway| gateway.status());
                    let mut changed = this.last_poll_status != status;
                    this.last_poll_status = status;
                    match result {
                        Ok((chat, identity, records, executors, configuration)) => {
                            if chat.has_more {
                                delay = 100;
                            }
                            let Some(current) = this.chat.as_mut() else {
                                return 0;
                            };
                            let changes = match current.apply_poll(chat) {
                                Ok(changes) => changes,
                                Err(error) => {
                                    let error = Some(error.to_string());
                                    if this.error != error {
                                        this.error = error;
                                        cx.notify();
                                    }
                                    return 15_000;
                                }
                            };
                            let records_changed = this.records != records;
                            changed |= changes.changed()
                                || this.identity.as_ref() != Some(&identity)
                                || records_changed
                                || this.error.is_some();
                            this.identity = Some(identity);
                            if records_changed {
                                this.records = records;
                            }
                            this.error = None;
                            #[cfg(native_manager)]
                            this.feed.update(cx, |feed, cx| {
                                feed.apply(
                                    changes
                                        .positions
                                        .iter()
                                        .filter_map(|position| current.items.get(position)),
                                    records_changed.then_some(this.records.as_slice()),
                                    cx,
                                )
                            });
                            if !this.model_busy {
                                if let Some(Ok(configuration)) = configuration {
                                    if let Some(snapshot) = &mut this.models {
                                        if configuration.thread_id == snapshot.target.thread_id
                                            && snapshot.configuration != configuration
                                        {
                                            snapshot.configuration = configuration;
                                            changed = true;
                                        }
                                    }
                                }
                            }
                            if let Some(result) = executors {
                                match result {
                                    Ok(snapshot) => {
                                        changed |= this.executors.as_ref() != Some(&snapshot)
                                            || this.executors_error.is_some();
                                        this.executors = Some(snapshot);
                                        this.executors_error = None;
                                    }
                                    Err(error) => {
                                        let error = Some(format!("{error:#}"));
                                        changed |= this.executors_error != error;
                                        this.executors_error = error;
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let error = Some(format!("{error:#}"));
                            changed |= this.identity.is_some() || this.error != error;
                            this.identity = None;
                            this.error = error;
                            delay = 15_000;
                        }
                    }
                    // MCP fallback warms after authenticated polling, never on
                    // combobox open. Native connections use the shared stream.
                    if this.identity.is_some() && this.model_target.is_none() && !this.model_busy {
                        #[cfg(native_manager)]
                        let native = this.native.active() || this.threads.enabled;
                        #[cfg(not(native_manager))]
                        let native = false;
                        if !native {
                            this.model_target = this.manager_model_target();
                            this.warm_gateway_models(window, cx);
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                    delay
                })
                .unwrap_or(0);
            if next == 0 {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.polling = false;
                    this.start_polling(window, cx);
                });
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(next))
                .await;
        })
        .detach();
    }

    fn send(&mut self, retry: Option<MessageRecord>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        #[cfg(native_manager)]
        if self.threads.enabled && !self.native.active() {
            self.error = Some("Pair the T3 WebSocket connection before sending.".into());
            cx.notify();
            return;
        }
        #[cfg(native_manager)]
        if retry.is_none() && self.native.active() {
            self.native_send(window, cx);
            return;
        }
        if self.busy || !self.can_send() {
            return;
        }
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        let Some(journal) = self.journal() else {
            return;
        };
        let Some(thread_id) = self.chat.as_ref().map(|chat| chat.thread_id.clone()) else {
            return;
        };
        #[cfg(native_manager)]
        let draft = self.composer.read(cx).typed_source_text(cx);
        #[cfg(not(native_manager))]
        let draft = self.compose.read(cx).value().to_string();
        if retry.is_none() && draft.trim().is_empty() {
            return;
        }
        let is_new = retry.is_none();
        #[cfg(native_manager)]
        self.typed_sync(cx);
        #[cfg(native_manager)]
        let sent_snapshot = is_new.then(|| self.draft_sent(cx));
        #[cfg(native_manager)]
        let typed_capture = if is_new {
            match self.typed_send_type(&draft, cx) {
                Ok(capture) => capture,
                Err(error) => {
                    self.error = Some(error);
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        self.busy = true;
        self.error = None;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            #[cfg(not(native_manager))]
            let sent_text = draft.clone();
            let (result, records, retained_id) = cx
                .background_executor()
                .spawn(async move {
                    let mut retained_id = None;
                    let result = (|| {
                        let record = match retry {
                            Some(record) => record,
                            None => {
                                let message = okilum_t3::OutgoingMessage::queue(thread_id, draft);
                                #[cfg(native_manager)]
                                let record = match &typed_capture {
                                    Some(capture) => capture.prepare_mcp(&journal, message)?,
                                    None => journal.prepare_message(message)?,
                                };
                                #[cfg(not(native_manager))]
                                let record = journal.prepare_message(message)?;
                                record
                            }
                        };
                        retained_id = Some(record.message.client_request_id.clone());
                        journal.send(gateway.as_ref(), &record)
                    })();
                    (result, journal.records(), retained_id)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if generation != this.generation {
                    cx.notify();
                    return;
                }
                let retained = records.as_ref().is_ok_and(|records| {
                    records.iter().any(|record| {
                        Some(&record.message.client_request_id) == retained_id.as_ref()
                    })
                });
                #[cfg(native_manager)]
                if is_new && retained {
                    if let Some(sent) = &sent_snapshot {
                        this.draft_clear_after_send(sent, window, cx);
                    }
                    this.typed_reload_annotations(cx);
                }
                #[cfg(not(native_manager))]
                if is_new && retained && this.compose.read(cx).value() == sent_text {
                    this.compose
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
                match records {
                    Ok(records) => {
                        if this.records != records {
                            this.records = records;
                            #[cfg(native_manager)]
                            this.feed.update(cx, |feed, cx| {
                                feed.apply(std::iter::empty(), Some(&this.records), cx)
                            });
                        }
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                if let Err(error) = result {
                    this.error = Some(format!("{error:#}"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

pub(super) fn expiry_note(expires_at: u64) -> String {
    i64::try_from(expires_at)
        .ok()
        .and_then(|seconds| chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0))
        .map(|date| {
            format!(
                "Connection valid until {}",
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M %Z")
            )
        })
        .unwrap_or_else(|| "Connection expiry unavailable".into())
}

fn status_note(status: &GatewayStatus) -> Option<&'static str> {
    match status {
        GatewayStatus::Connected { .. } => None,
        GatewayStatus::NotConnected => {
            Some("Sign in once in Settings › T3 to use every project in this app.")
        }
        GatewayStatus::ExpiresSoon { .. } => {
            Some("Your T3 connection expires within three days. Choose Renew to sign in again.")
        }
        GatewayStatus::Expired => Some("Your T3 connection has expired. Reconnect to continue."),
        GatewayStatus::Revoked => Some("Your T3 connection was revoked. Reconnect to continue."),
        GatewayStatus::Unreachable { .. } => {
            Some("T3 is unreachable. Your history and retained messages are kept.")
        }
    }
}

impl ProjectView {
    fn render_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(native_manager)]
        self.attachment_restore_connection(_window, cx);
        #[cfg(native_manager)]
        self.sync_native_action_surface(cx);
        if self.local_setup.is_some() {
            return self.render_local_setup(cx).into_any_element();
        }
        let status = self
            .gateway
            .as_ref()
            .map(|g| g.status())
            .unwrap_or(GatewayStatus::NotConnected);
        let enabled = self.can_send();
        let note = status_note(&status);
        let compact = self.connection_is_compact(&status);
        #[cfg(native_manager)]
        let (enabled, note, compact) = if self.native.active() {
            (enabled, None, compact)
        } else if self.threads.enabled {
            (false, None, compact)
        } else {
            (enabled, note, compact)
        };
        #[cfg(native_manager)]
        let legacy_controls = !self.threads.enabled && !self.native.active();
        #[cfg(not(native_manager))]
        let legacy_controls = true;
        let expiry = self
            .gateway
            .as_ref()
            .and_then(|gateway| gateway.token_expires_at())
            .map(expiry_note);
        let renew = matches!(
            status,
            GatewayStatus::ExpiresSoon { .. } | GatewayStatus::Expired
        );
        let browser_help = self.busy
            || matches!(
                status,
                GatewayStatus::NotConnected | GatewayStatus::Expired | GatewayStatus::Revoked
            );
        let browser_origin = self
            .connection_environment(cx)
            .ok()
            .map(|environment| environment.origin());

        let _pending = self
            .chat
            .as_ref()
            .filter(|chat| chat.pending_question_error.is_none())
            .map(|chat| chat.pending_questions)
            .unwrap_or(0);
        #[cfg(native_manager)]
        let history = {
            self.typed_sync(cx);
            self.feed
                .update(cx, |feed, cx| feed.set_controls(enabled, self.busy, cx));
            let native_ready = !self.native.active()
                || (self.native.actions.read(cx).state.ready
                    && self.native.stream_error_is_clear());
            let can_steer = self.native.actions.read(cx).state.can_steer;
            let has_attachments = !self.draft_attachments().is_empty();
            let can_attach = self.attachment_can_add();
            self.composer.update(cx, |composer, cx| {
                composer.set_native(self.native.active(), can_steer, cx);
                composer.set_attachments(has_attachments, can_attach, cx);
                composer.set_history_blocked(_pending > 0);
                composer.set_controls(enabled && native_ready, self.busy, cx)
            });
            h_flex()
                .debug_selector(|| "project-chat-history".into())
                .flex_1()
                .min_h_0()
                .child(
                    div().flex_1().min_w_0().h_full().child(
                        self.feed
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    ),
                )
                .child(composer::TypedPanelSlot(self.composer.clone()))
        };
        #[cfg(not(native_manager))]
        let history = {
            let mut history = v_flex()
                .id("project-messages")
                .debug_selector(|| "project-chat-history".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .gap_4();
            if let Some(chat) = &self.chat {
                for item in chat.items.values() {
                    if !matches!(item.kind.as_str(), "user_message" | "assistant_message") {
                        continue;
                    }
                    let Some(text) = &item.text else {
                        continue;
                    };
                    history = history.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_color(brand::palette(cx).text_muted).child(
                                if item.kind == "user_message" {
                                    "You"
                                } else {
                                    "Manager"
                                },
                            ))
                            .child(
                                TextView::markdown(
                                    ("project-message", item.position as usize),
                                    text.clone(),
                                )
                                .selectable(true)
                                .style(reader_text_style(cx.theme())),
                            ),
                    );
                }
            }
            for (index, record) in self.records.iter().enumerate() {
                let projected = record.receipt.as_ref().is_some_and(|receipt| {
                    self.chat.as_ref().is_some_and(|chat| {
                        chat.items
                            .values()
                            .any(|item| item.message_id.as_ref() == Some(&receipt.message_id))
                    })
                });
                if projected {
                    continue;
                }
                let retry = record.clone();
                history = history.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_color(brand::palette(cx).text_muted).child(
                            if record.receipt.is_some() {
                                "You · queued"
                            } else {
                                "Message delivery unconfirmed"
                            },
                        ))
                        .when(record.receipt.is_some(), |row| {
                            row.child(
                                TextView::markdown(
                                    ("project-retained", index),
                                    record.message.text.clone(),
                                )
                                .selectable(true)
                                .style(reader_text_style(cx.theme())),
                            )
                        })
                        .when(record.receipt.is_none(), |row| {
                            row.child(
                                Button::new(("project-retry", index))
                                    .debug_selector(move || format!("project-retry-{index}"))
                                    .ghost()
                                    .icon(IconName::RotateCw)
                                    .tooltip(format!(
                                        "Retry retained message: {}",
                                        record.message.text
                                    ))
                                    .disabled(self.busy || !enabled)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.send(Some(retry.clone()), window, cx)
                                    })),
                            )
                        }),
                );
            }
            history
        };
        v_flex()
            .relative()
            .capture_key_down(cx.listener(|this,event:&gpui::KeyDownEvent,window,cx| { this.model_key(event,window,cx); }))
            .id("project-tab")
            .debug_selector(|| "project-tab".into())
            .when(cfg!(native_manager), |view| {
                #[cfg(native_manager)]
                let view = view.on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if this.active
                        && !this.busy
                        && this.native_actions_pinned()
                        && !this.model_open
                        && reader_features::enabled(reader_features::Feature::Agents, cx)
                        && !this.compose.read(cx).focus_handle(cx).is_focused(window)
                        && !this.origin.read(cx).focus_handle(cx).is_focused(window)
                    {
                        this.native.actions.update(cx, |actions, cx| {
                            actions.handle_question_key(event, window, cx);
                        });
                    }
                }));
                view
            })
            .size_full()
            .min_h_0()
            .text_size(px(design::CHROME))
            .line_height(px(design::LINE))
            .text_color(brand::palette(cx).text)
            .bg(brand::palette(cx).surface)
            .gap_2()
            .when(self.connection_details && !cfg!(native_manager), |view| view.child(self.render_chat_diagnostics(cx)))
            .when(
                self.binding
                    .as_ref()
                    .is_some_and(|b| b.manager.thread_id.is_none()),
                |view| {
                    view.child(div().child("Manager не запущен")).child(
                        Button::new("start-manager")
                            .debug_selector(|| "start-manager".into())
                            .label("Start manager")
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_manager(window, cx)),
                            ),
                    )
                },
            )
            .when(self.binding.is_some(),|view| {
                #[cfg(native_manager)] let view=view.child(self.render_native(_window, cx));
                view
            })
            .when_some(self.reason.clone(), |view, reason| {
                view.child(
                    div()
                        .text_color(brand::palette(cx).text_muted)
                        .child(reason),
                )
            })
            .when(self.area.is_some() && self.reason.as_deref() != Some("Loading project…") && self.binding.as_ref().is_none_or(|b| b.manager.project_id.is_none() && b.manager.thread_id.is_none()), |view| {
                view.child(Button::new("project-local-setup").debug_selector(|| "project-local-setup".into()).label("Set up local Manager")
                    .disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.open_local_setup(window, cx))))
            })
            .when_some(note, |view, note| {
                if compact { return view; }
                view.child(
                    div()
                        .debug_selector(|| "project-connection-status".into())
                        .text_color(brand::palette(cx).text_muted)
                        .child(note),
                )
            })
            .when(self.binding.is_some() && !compact, |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(Input::new(&self.origin).flex_1())
                        .child(
                            Button::new("project-connect")
                                .debug_selector(|| "project-connect".into())
                                .ghost()
                                .label("Reconnect")
                                .tooltip("Reconnect using the saved T3 connection")
                                .disabled(self.busy || self.model_busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.connect(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("project-sign-in-again")
                                .debug_selector(move || if renew { "project-renew".into() } else { "project-sign-in-again".into() })
                                .ghost()
                                .label(if renew { "Renew in Settings" } else { "Sign in to T3" })
                                .tooltip("Use the shared connection in Settings › T3")
                                .disabled(self.busy || self.model_busy)
                                .on_click(cx.listener(|this, _, window, cx| this.connect_mode(true, window, cx))),
                        ),
                )
            })
            .when_some(expiry.filter(|_| !compact), |view, expiry| view.child(div().w_full().min_w_0().whitespace_normal().debug_selector(|| "project-token-expiry".into()).text_color(brand::palette(cx).text_muted).child(expiry)))
            .when(self.connection_details && !cfg!(native_manager), |view| view.child(Button::new("project-connection-done")
                .debug_selector(|| "project-connection-done".into()).ghost().icon(IconName::Check).tooltip("Done with connection settings")
                .on_click(cx.listener(|this, _, _, cx| { this.connection_details = false; cx.notify(); }))))
            .when(self.binding.is_some() && browser_help && legacy_controls, |view| {
                view.child(v_flex().gap_1()
                    .child(div().w_full().min_w_0().whitespace_normal().child("Settings › T3 shows the shared sign-in state and can accept a browser callback URL."))
                    .child(Button::new("project-browser-sign-in").debug_selector(|| "project-browser-sign-in".into()).ghost().label("Open T3 in browser")
                        .disabled(browser_origin.is_none())
                        .on_click(move |_, _, cx| { if let Some(origin) = &browser_origin { cx.open_url(origin); } })))
            })
            .when_some(
                self.chat
                    .as_ref()
                    .and_then(|chat| chat.pending_question_error.clone()),
                |view, error| {
                    view.child(
                        div()
                            .debug_selector(|| "project-questions-unavailable".into())
                            .text_color(cx.theme().danger)
                            .child(if compact { "Questions unavailable. Open T3 to view them.".to_owned() } else { format!("Questions unavailable: {error}. Open T3 to view questions.") }),
                    )
                },
            )
            .when_some(self.error.clone().or_else(|| self.environment.as_ref().and_then(|env| reader_t3_settings::connection_error(&env.origin(), cx))), |view, error| {
                #[cfg(native_manager)]
                if self.binding.is_some() { return view; }
                #[cfg(native_manager)]
                if self.native.active() {
                    return if let Some(banner) = self.native_setup_error_banner(error, _window, cx) {
                        view.child(banner)
                    } else {
                        view
                    };
                }
                view.child(
                    div()
                        .debug_selector(|| "project-error".into())
                        .text_color(cx.theme().danger)
                        .child(if compact { "Could not connect. Open Details to retry.".to_owned() } else { error }),
                )
            })
            .when(self.launch.open, |view| view.child(self.render_launch(cx)))
            .child(if self.executors_open {
                self.render_executors(_window, cx)
            } else {
                history.into_any_element()
            })
            .when(!self.executors_open, |view| {
                #[cfg(native_manager)]
                let composer = self.composer.clone().into_any_element();
                #[cfg(not(native_manager))]
                let composer = h_flex()
                        .items_end()
                        .gap_2()
                        .child(Textarea::new(&self.compose).flex_1())
                        .child(
                            Button::new("project-send")
                                .debug_selector(|| "project-send".into())
                                .ghost()
                                .icon(IconName::ArrowUp)
                                .tooltip("Send to manager")
                                .disabled(
                                    self.busy
                                        || !enabled
                                        || self.compose.read(cx).value().trim().is_empty(),
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.send(None, window, cx)),
                                ),
                        ).into_any_element();
                #[cfg(native_manager)]
                let attachments = if self.native.active(){self.render_native_attachment_drafts(cx)}else{self.render_draft_attachments(cx)};
                #[cfg(native_manager)]
                let composer = v_flex()
                    .w_full().min_w_0().gap(px(design::GAP))
                    .when(self.native.actions.read(cx).has_visible_content(cx), |surface| {
                        surface.child(self.native.actions.clone())
                    })
                    .child(attachments)
                    .child(composer);
                #[cfg(not(native_manager))]
                let composer = v_flex().w_full().min_w_0().gap(px(design::GAP))
                    .child(self.render_model_controls(_window, cx)).child(composer);
                let palette = brand::palette(cx);
                view.child(
                    div().w_full().min_w_0().px(px(design::INSET)).pb(px(design::INSET))
                        .child(div()
                            .id("project-message-surface")
                            .debug_selector(|| "project-chat-composer".into())
                            .w_full().min_w_0()
                            .border_1().border_color(if self.compose.read(cx).focus_handle(cx).is_focused(_window) {
                                palette.focus
                            } else {
                                palette.border_subtle
                            })
                            .rounded(px(design::RADIUS))
                            .bg(palette.surface_raised)
                            .p(px(design::INSET))
                            .child(composer))
                )
            }).into_any_element()
    }
}

impl Render for ProjectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = reader_features::enabled(reader_features::Feature::Agents, cx);
        if !enabled {
            #[cfg(native_manager)]
            self.invalidate_rotation(cx);
            #[cfg(native_manager)]
            if self.composer.read(cx).typed_scope().is_some() {
                self.draft_suspend_typed(cx);
                self.typed_invalidate(false, cx);
            }
            return div().into_any_element();
        }
        #[cfg(native_manager)]
        if self.active {
            self.draft_restore(window, cx);
            self.typed_sync(cx);
        }
        #[cfg(native_manager)]
        if self.rotation_resolver.is_some()
            && Self::rotation_owner(self, cx) != self.rotation_expected_owner
        {
            self.invalidate_rotation(cx);
        }
        let content = if self.section == ProjectSection::Manager {
            let content = self.render_body(window, cx);
            #[cfg(native_manager)]
            let content = if self.threads.enabled {
                v_flex()
                    .size_full()
                    .min_h_0()
                    .child(self.render_manager_notices(cx))
                    .child(div().flex_1().min_h_0().child(content))
                    .into_any_element()
            } else {
                content
            };
            content
        } else {
            self.render_resource_section(cx)
        };
        // The mounted workspace owns the identity header. Section navigation
        // and notices follow it, so the conversation aligns with the panel top.
        #[cfg(native_manager)]
        let chrome = self
            .chrome
            .clone()
            .cached(
                StyleRefinement::default()
                    .w_full()
                    .h(px(40.) + window.rem_size() * 3.)
                    .flex_shrink_0(),
            )
            .into_any_element();
        #[cfg(not(native_manager))]
        let chrome = self.render_chrome(cx);
        let surface = v_flex().size_full().min_w_0().min_h_0().child(chrome);
        #[cfg(native_manager)]
        let surface = surface
            .when_some(self.rotation_notice.clone(), |surface, error| {
                surface.child(div().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.rotation.clone(), |surface, coordinator| {
                surface.child(coordinator)
            });
        #[cfg(native_manager)]
        let surface = surface.when(self.rotation_policy.is_open(), |surface| {
            surface.child(self.render_rotation_policy(window, cx))
        });
        surface
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(self.section == ProjectSection::Rules, |view| {
                        view.flex().flex_col().min_w_0().overflow_hidden()
                    })
                    .child(content),
            )
            .into_any_element()
    }
}

impl Reader {
    pub(super) fn refresh_project_resources(
        &mut self,
        changes: &okilum_core::Changes,
        cx: &mut Context<Self>,
    ) {
        if !changes.is_empty() {
            for records in self.project_navigation.records.values() {
                records.update(cx, |records, cx| records.changed(cx));
            }
        }
        if let Some(project) = self.project.as_ref().filter(|_| self.project_open) {
            project.update(cx, |view, cx| view.resources_changed(changes, cx));
        }
    }

    pub(super) fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_request = self.project_request.wrapping_add(1);
        if let Some(project) = self.project.as_ref().filter(|_| self.project_open) {
            project.update(cx, |project, cx| project.set_active(false, window, cx));
        }
        self.project_open = false;
    }

    #[cfg(test)]
    pub(super) fn toggle_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_open {
            self.close_project(window, cx);
            cx.notify();
            return;
        }
        let note = self.vault_root.join(&self.current_rel);
        let area = connections::area_for(&self.vault_root, &note)
            .unwrap_or_else(|_| self.vault_root.clone());
        self.open_project_area(area, window, cx);
    }

    pub(super) fn open_project_area(
        &mut self,
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        if let Some(previous) = &self.project {
            previous.update(cx, |view, cx| view.set_active(false, window, cx));
        }
        let opened = std::time::Instant::now();
        self.project_open = true;
        self.project = None;
        self.project_request = self.project_request.wrapping_add(1);
        let request = self.project_request;
        let unavailable_manager = self
            .project_navigation
            .statuses
            .get(&area)
            .filter(|status| status.restartable())
            .and_then(|_| self.project_navigation.status_managers.get(&area))
            .cloned();
        cx.spawn_in(window, async move |this, cx| {
            // Re-read the explicit pointer on every activation. A rotation gets
            // its own view; returning to an existing binding retains its draft.
            let binding = cx
                .background_executor()
                .spawn(async move {
                    let project = okilum_t3::projects::Project::read(&area)?;
                    let mut binding = project.native_binding()?;
                    if unavailable_manager.as_ref() == Some(&project.manager) {
                        if let Some(binding) = &mut binding {
                            binding.manager.thread_id = None;
                        }
                    }
                    let key = (area, serde_json::to_string(&binding)?);
                    Ok::<_, anyhow::Error>((key, binding))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.project_request != request || !this.project_open {
                    return;
                }
                let project = match binding {
                    Ok((key, binding)) => match this.project_views.get(&key).cloned() {
                        Some(project) => {
                            project.update(cx, |project, cx| {
                                project.set_active_since(true, opened, window, cx)
                            });
                            project
                        }
                        None => {
                            let state = this.session_directory.clone();
                            let project = cx.new(|cx| {
                                let mut view = ProjectView::new(
                                    binding,
                                    state,
                                    key.0.clone(),
                                    opened,
                                    window,
                                    cx,
                                );
                                view.context_path = key
                                    .0
                                    .strip_prefix(&this.vault_root)
                                    .unwrap_or(&key.0)
                                    .to_string_lossy()
                                    .replace('\\', "/");
                                view
                            });
                            #[cfg(native_manager)]
                            this.connect_manager_links(&project, window, cx);
                            #[cfg(native_manager)]
                            cx.subscribe_in(
                                &project,
                                window,
                                |this, _, _: &NativeConnected, window, cx| {
                                    // Discard pre-pairing snapshots before refreshing
                                    // project status and icon gateways with the saved bearer.
                                    this.project_navigation.gateway_generation =
                                        this.project_navigation.gateway_generation.wrapping_add(1);
                                    this.project_navigation.t3_gateways.clear();
                                    this.refresh_project_statuses(window, cx);
                                },
                            )
                            .detach();
                            cx.subscribe_in(
                                &project,
                                window,
                                |this, _, _: &ManagerStarted, window, cx| {
                                    if let Some(active) = this.project_navigation.active.clone() {
                                        this.enter_vault_project(active, window, cx);
                                    }
                                },
                            )
                            .detach();
                            this.project_views.insert(key, project.clone());
                            project
                        }
                    },
                    Err(error) => cx.new(|cx| {
                        let mut view =
                            ProjectView::empty(this.session_directory.clone(), window, cx);
                        view.reason = None;
                        view.error = Some(format!("{error:#}"));
                        view
                    }),
                };
                let root = this.vault_root.clone();
                let published = this.vault.clone();
                let publication_epoch = this.source_publication_epoch;
                let reader = cx.entity();
                project.update(cx, |view, cx| {
                    view.bind_resources(&reader, root, published, publication_epoch, cx)
                });
                let _resource_open = cx.subscribe_in(
                    &project,
                    window,
                    |this, project, request: &ProjectSkillOpen, window, cx| {
                        if this.project.as_ref() != Some(project)
                            || !this.project_open
                            || !reader_features::enabled(reader_features::Feature::Agents, cx)
                        {
                            return;
                        }
                        let relative = okilum_core::vault::note_path(&request.0);
                        this.open_note_landing(&relative, None, None, None, window, cx);
                    },
                );
                project.update(cx, |view, _| view._resource_open = Some(_resource_open));
                this.connect_project_board(project.clone(), window, cx);
                this.project = Some(project);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(any(test, feature = "project-ui-harness"))]
mod fixture;
#[cfg(test)]
mod tests;

#[cfg(feature = "project-ui-harness")]
pub(crate) fn open_harness(cx: &mut App) -> bool {
    fixture::open(cx)
}
