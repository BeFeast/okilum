//! Question-local uploads. Only confirmed references reach immutable responses;
//! local sources and prepared bytes never enter the main composer or journal.
use super::*;
use crate::project::attachments::{prepare_with_preview, uploaded_references, Prepared, Source};
use okilum_t3::{
    actions::QuestionAttachments,
    chat_drafts::{Attachment, AttachmentKind as DraftKind, AttachmentState},
    stream::{
        AttachmentKind, NativeEnvironmentCapabilities, NativeEnvironmentProfile, MAX_ATTACHMENTS,
    },
};

type Key = (String, String);

#[derive(Clone)]
pub(super) struct AttachmentContext {
    pub origin: String,
    pub project: String,
    pub thread: String,
    pub lease: Arc<()>,
    pub client: Arc<NativeClient>,
    pub confirmed_environment: Option<String>,
}
impl AttachmentContext {
    fn same_scope(&self, other: &Self) -> bool {
        self.origin == other.origin
            && self.project == other.project
            && self.thread == other.thread
            && Arc::ptr_eq(&self.lease, &other.lease)
    }
    fn same_client(&self, other: &Self) -> bool {
        self.same_scope(other) && Arc::ptr_eq(&self.client, &other.client)
    }
}
struct Entry {
    row: Attachment,
    source: Source,
    prepared: Option<Arc<Prepared>>,
    environment: String,
    request_row: serde_json::Value,
    revision: uuid::Uuid,
    job: Option<uuid::Uuid>,
}
#[derive(Default)]
pub(super) struct Runtime {
    context: Option<AttachmentContext>,
    profile: Option<NativeEnvironmentProfile>,
    loading: Option<uuid::Uuid>,
    epoch: uuid::Uuid,
    entries: HashMap<Key, Vec<Entry>>,
    notice: Option<String>,
}
impl Runtime {
    pub(super) fn set_context(&mut self, context: Option<AttachmentContext>) {
        if self
            .context
            .as_ref()
            .zip(context.as_ref())
            .is_some_and(|(old, new)| old.same_client(new))
        {
            let expected = context
                .as_ref()
                .and_then(|context| context.confirmed_environment.clone());
            if self.profile.as_ref().is_some_and(|profile| {
                expected
                    .as_ref()
                    .is_some_and(|expected| expected != &profile.identity.environment_id)
            }) {
                self.invalidate_jobs();
                self.profile = None;
                self.notice =
                    Some("The T3 environment changed. Reconnect before attaching files.".into());
            }
            self.context = context;
            return;
        }
        let same_scope = self
            .context
            .as_ref()
            .zip(context.as_ref())
            .is_some_and(|(old, new)| old.same_scope(new));
        if same_scope {
            self.invalidate_jobs();
            self.profile = None;
        } else {
            *self = Self::default();
        }
        self.context = context;
    }
    pub(super) fn invalidate_jobs(&mut self) {
        self.epoch = uuid::Uuid::new_v4();
        self.loading = None;
        for entry in self.entries.values_mut().flatten() {
            if entry.job.take().is_some() {
                entry.row.state = AttachmentState::Error {
                    message: "Connection changed. Retry this file when ready.".into(),
                };
            }
        }
    }
    pub(super) fn reconcile(&mut self, state: &State) {
        if state.ready {
            self.entries.retain(|(request, question), entries| {
                let Some(candidate) = state.requests.iter().find(|candidate| {
                    candidate.id == *request
                        && candidate.item["questions"]
                            .as_array()
                            .is_some_and(|questions| questions.iter().any(|q| q["id"] == *question))
                }) else {
                    return false;
                };
                entries.retain(|entry| entry.request_row == candidate.item["id"]);
                !entries.is_empty()
            });
        }
    }
    pub(super) fn capabilities(&self) -> Option<&NativeEnvironmentCapabilities> {
        self.profile.as_ref().map(|profile| &profile.capabilities)
    }
    pub(super) fn references(
        &self,
        request: &str,
        question: Option<&str>,
    ) -> anyhow::Result<QuestionAttachments> {
        let collections = self
            .entries
            .iter()
            .filter(|((r, q), rows)| {
                r == request && question.is_none_or(|question| q == question) && !rows.is_empty()
            })
            .collect::<Vec<_>>();
        if collections.is_empty() {
            return Ok(BTreeMap::new());
        }
        let profile = self
            .profile
            .as_ref()
            .context("Confirm question attachment support before sending")?;
        anyhow::ensure!(
            profile.capabilities.question_attachments_available(),
            "Question attachments are unavailable on this server"
        );
        let mut result = BTreeMap::new();
        for ((_, question), entries) in collections {
            anyhow::ensure!(
                entries
                    .iter()
                    .all(|entry| entry.environment == profile.identity.environment_id),
                "This question file belongs to another environment; select it again"
            );
            let rows = entries
                .iter()
                .map(|entry| entry.row.clone())
                .collect::<Vec<_>>();
            let references = uploaded_references(&rows, &profile.identity.environment_id)?;
            for reference in &references {
                if reference.metadata.kind == AttachmentKind::File {
                    anyhow::ensure!(
                        profile
                            .capabilities
                            .effective_file_max_upload_bytes()
                            .is_some_and(|limit| reference.metadata.size_bytes <= limit),
                        "This file exceeds the server's supported file limit"
                    );
                }
            }
            result.insert(question.clone(), references);
        }
        okilum_t3::actions::validate_question_attachments(&result)?;
        Ok(result)
    }
    pub(super) fn ready_references(&self, request: &str) -> QuestionAttachments {
        let mut result = BTreeMap::new();
        for (r, q) in self.entries.keys().filter(|(r, _)| r == request) {
            if let Ok(ready) = self.references(r, Some(q)) {
                result.extend(ready);
            }
        }
        result
    }
    fn count(&self, request: &str) -> usize {
        self.entries
            .iter()
            .filter(|((r, _), _)| r == request)
            .map(|(_, rows)| rows.len())
            .sum()
    }
}
#[derive(Clone)]
enum UserAction {
    Pick,
    Add(Vec<(Source, String)>),
    Retry(String),
    Check,
}
#[derive(Clone)]
struct UploadJob {
    context: AttachmentContext,
    environment: String,
    epoch: uuid::Uuid,
    key: Key,
    row: serde_json::Value,
    question: serde_json::Value,
    id: String,
    revision: uuid::Uuid,
    job: uuid::Uuid,
}

impl Actions {
    fn question_file_context_current(&self, context: &AttachmentContext) -> bool {
        self.enabled
            && self
                .question_files
                .context
                .as_ref()
                .is_some_and(|current| current.same_client(context))
            && context.client.can_operate()
    }
    fn question_file_allowed(&self, control: &QuestionControl) -> bool {
        self.question_control_current(control)
            && self.question_enabled(&control.transition.request)
            && self
                .state
                .requests
                .iter()
                .find(|request| request.id == control.transition.request)
                .is_some_and(|request| {
                    matches!(request.response_mode.as_deref(), Some("live" | "message"))
                        && request.item["questions"]
                            .as_array()
                            .is_some_and(|questions| {
                                questions
                                    .iter()
                                    .any(|q| q["id"] == control.transition.question)
                            })
                })
    }
    fn question_file_action(
        &mut self,
        control: QuestionControl,
        action: UserAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let support_only = matches!(action, UserAction::Check);
        if !(self.question_file_allowed(&control)
            || (support_only
                && self.question_control_current(&control)
                && self.enabled
                && self.state.ready
                && !self.busy))
        {
            return;
        }
        let Some(context) = self
            .question_files
            .context
            .clone()
            .filter(|context| self.question_file_context_current(context))
        else {
            return;
        };
        if self.question_files.profile.is_some() {
            self.question_file_action_ready(control, action, window, cx);
            return;
        }
        if self.question_files.loading.is_some() {
            return;
        }
        let job = uuid::Uuid::new_v4();
        let epoch = self.question_files.epoch;
        self.question_files.loading = Some(job);
        self.question_files.notice = None;
        let client = context.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = super::super::recovery::stream_io(cx.background_executor(), move || {
                client.environment_profile()
            })
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.question_files.loading != Some(job)
                    || this.question_files.epoch != epoch
                    || !this.question_file_context_current(&context)
                {
                    return;
                }
                this.question_files.loading = None;
                match result {
                    Ok(profile) => {
                        if this
                            .question_files
                            .context
                            .as_ref()
                            .and_then(|context| context.confirmed_environment.as_ref())
                            .is_some_and(|expected| expected != &profile.identity.environment_id)
                        {
                            this.question_files.notice = Some(
                                "The T3 environment changed. Reconnect before attaching files."
                                    .into(),
                            );
                            cx.notify();
                            return;
                        }
                        this.question_files.profile = Some(profile);
                        this.question_file_action_ready(control, action, window, cx);
                    }
                    Err(error) => this.question_files.notice = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn question_file_action_ready(
        &mut self,
        control: QuestionControl,
        action: UserAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .question_files
            .capabilities()
            .is_some_and(NativeEnvironmentCapabilities::question_attachments_available)
        {
            self.question_files.notice =
                Some("This server does not support files in question answers.".into());
            cx.notify();
            return;
        }
        if matches!(action, UserAction::Check) {
            cx.notify();
            return;
        }
        if !self.question_file_allowed(&control) {
            return;
        }
        match action {
            UserAction::Pick => {
                let context = self.question_files.context.clone();
                let epoch = self.question_files.epoch;
                let picker = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: true,
                    prompt: Some("Attach files to this question".into()),
                });
                cx.spawn_in(window, async move |this, cx| {
                    let selected = picker.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if !this.question_file_allowed(&control)
                            || this.question_files.epoch != epoch
                            || !context
                                .as_ref()
                                .is_some_and(|context| this.question_file_context_current(context))
                        {
                            return;
                        }
                        match selected {
                            Ok(Ok(Some(paths))) => {
                                let sources = paths
                                    .into_iter()
                                    .map(|path| {
                                        let name = path
                                            .file_name()
                                            .map(|name| name.to_string_lossy().into_owned())
                                            .unwrap_or_else(|| "attachment".into());
                                        (Source::File(path), name)
                                    })
                                    .collect();
                                this.question_add_sources(&control, sources, window, cx);
                            }
                            Ok(Ok(None)) => {}
                            _ => {
                                this.question_files.notice = Some(
                                    "Could not open the file chooser. Paste the file instead."
                                        .into(),
                                );
                                cx.notify();
                            }
                        }
                    });
                })
                .detach();
            }
            UserAction::Add(sources) => self.question_add_sources(&control, sources, window, cx),
            UserAction::Retry(id) => self.question_upload(&control, &id, window, cx),
            UserAction::Check => {}
        }
    }
    fn question_add_sources(
        &mut self,
        control: &QuestionControl,
        sources: Vec<(Source, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.question_file_allowed(control) {
            return;
        }
        let Some(environment) = self
            .question_files
            .profile
            .as_ref()
            .map(|profile| profile.identity.environment_id.clone())
        else {
            return;
        };
        let key = (
            control.transition.request.clone(),
            control.transition.question.clone(),
        );
        self.cancel_question_transition(&key.0);
        let mut added = Vec::new();
        for (source, name) in sources {
            if self.question_files.count(&key.0) >= MAX_ATTACHMENTS {
                self.question_files.notice =
                    Some("You can attach up to 8 files across all questions.".into());
                break;
            }
            let id = uuid::Uuid::new_v4().to_string();
            let image = matches!(source, Source::Image(_));
            self.question_files
                .entries
                .entry(key.clone())
                .or_default()
                .push(Entry {
                    row: Attachment {
                        id: id.clone(),
                        kind: if image {
                            DraftKind::Image
                        } else {
                            DraftKind::File
                        },
                        name,
                        mime_type: if image {
                            "image/png"
                        } else {
                            "application/octet-stream"
                        }
                        .into(),
                        size_bytes: 0,
                        source: None,
                        state: AttachmentState::Pending,
                    },
                    source,
                    prepared: None,
                    environment: environment.clone(),
                    request_row: self
                        .state
                        .requests
                        .iter()
                        .find(|request| request.id == key.0)
                        .unwrap()
                        .item["id"]
                        .clone(),
                    revision: uuid::Uuid::new_v4(),
                    job: None,
                });
            added.push(id);
        }
        for id in added {
            self.question_upload(control, &id, window, cx);
        }
        cx.notify();
    }
    fn question_upload_current(&self, job: &UploadJob) -> bool {
        self.question_files.epoch == job.epoch
            && self.question_file_context_current(&job.context)
            && self.state.ready
            && !self.request_retained(&job.key.0)
            && self.question_files.profile.as_ref().is_some_and(|profile| {
                profile.identity.environment_id == job.environment
                    && profile.capabilities.question_attachments_available()
            })
            && self.state.requests.iter().any(|request| {
                request.id == job.key.0
                    && request.item["id"] == job.row
                    && request.answerable
                    && matches!(request.response_mode.as_deref(), Some("live" | "message"))
                    && request.item["questions"]
                        .as_array()
                        .is_some_and(|questions| questions.iter().any(|q| q == &job.question))
            })
            && self
                .question_files
                .entries
                .get(&job.key)
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry.row.id == job.id
                            && entry.revision == job.revision
                            && entry.job == Some(job.job)
                    })
                })
    }
    fn question_release_stale(&mut self, job: &UploadJob, cx: &mut Context<Self>) {
        if let Some(entry) = self
            .question_files
            .entries
            .get_mut(&job.key)
            .and_then(|entries| {
                entries
                    .iter_mut()
                    .find(|entry| entry.row.id == job.id && entry.job == Some(job.job))
            })
        {
            entry.job = None;
            entry.row.state = AttachmentState::Error {
                message: "The question or connection changed. Retry this file when ready.".into(),
            };
            cx.notify();
        }
    }
    fn question_upload(
        &mut self,
        control: &QuestionControl,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.question_file_allowed(control) {
            return;
        }
        let Some(context) = self.question_files.context.clone() else {
            return;
        };
        let Some(profile) = self.question_files.profile.clone() else {
            return;
        };
        let key = (
            control.transition.request.clone(),
            control.transition.question.clone(),
        );
        let Some(request) = self
            .state
            .requests
            .iter()
            .find(|request| request.id == key.0)
        else {
            return;
        };
        let Some(question) = request.item["questions"]
            .as_array()
            .and_then(|questions| questions.iter().find(|q| q["id"] == key.1))
            .cloned()
        else {
            return;
        };
        let row = request.item["id"].clone();
        let other_images = self
            .question_files
            .entries
            .iter()
            .filter(|((r, _), _)| r == &key.0)
            .flat_map(|(_, entries)| entries)
            .filter(|entry| entry.row.id != id && entry.row.kind == DraftKind::Image)
            .fold(0u64, |total, entry| {
                total.saturating_add(entry.row.size_bytes)
            });
        let Some(entry) = self
            .question_files
            .entries
            .get_mut(&key)
            .and_then(|entries| entries.iter_mut().find(|entry| entry.row.id == id))
        else {
            return;
        };
        if entry.environment != profile.identity.environment_id {
            entry.row.state = AttachmentState::NeedsReselect;
            self.question_files.notice = Some(
                "This file belongs to another environment. Remove it and attach it again.".into(),
            );
            cx.notify();
            return;
        }
        if entry.job.is_some() || matches!(entry.row.state, AttachmentState::Uploaded { .. }) {
            return;
        }
        let job = UploadJob {
            context,
            environment: profile.identity.environment_id.clone(),
            epoch: self.question_files.epoch,
            key,
            row,
            question,
            id: id.into(),
            revision: entry.revision,
            job: uuid::Uuid::new_v4(),
        };
        entry.row.state = AttachmentState::Pending;
        entry.job = Some(job.job);
        let cached = entry.prepared.clone();
        let source = entry.source.clone();
        let name = entry.row.name.clone();
        self.cancel_question_transition(&job.key.0);
        cx.spawn_in(window, async move |this, cx| {
            let prepared = match cached {
                Some(prepared) => Ok(prepared),
                None => {
                    cx.background_executor()
                        .spawn(async move {
                            prepare_with_preview(&source, &name, other_images)
                                .map_err(|error| format!("{error:#}"))
                        })
                        .await
                }
            };
            let prepared = this
                .update_in(cx, |this, _, cx| {
                    if !this.question_upload_current(&job) {
                        this.question_release_stale(&job, cx);
                        return None;
                    }
                    let entry = this
                        .question_files
                        .entries
                        .get_mut(&job.key)
                        .unwrap()
                        .iter_mut()
                        .find(|entry| entry.row.id == job.id)
                        .unwrap();
                    match prepared {
                        Ok(prepared) => {
                            if prepared.metadata.kind == AttachmentKind::File
                                && !profile
                                    .capabilities
                                    .effective_file_max_upload_bytes()
                                    .is_some_and(|limit| prepared.metadata.size_bytes <= limit)
                            {
                                entry.row.state = AttachmentState::Error {
                                    message: "This file exceeds the server's supported file limit."
                                        .into(),
                                };
                                entry.job = None;
                                entry.prepared = Some(prepared);
                                cx.notify();
                                return None;
                            }
                            entry.row.kind = if prepared.metadata.kind == AttachmentKind::Image {
                                DraftKind::Image
                            } else {
                                DraftKind::File
                            };
                            entry.row.mime_type = prepared.metadata.mime_type.clone();
                            entry.row.size_bytes = prepared.metadata.size_bytes;
                            entry.prepared = Some(prepared.clone());
                            cx.notify();
                            Some(prepared)
                        }
                        Err(message) => {
                            entry.job = None;
                            entry.row.state = AttachmentState::Error { message };
                            cx.notify();
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            let Some(prepared) = prepared else {
                return;
            };
            let client = job.context.client.clone();
            let result = super::super::recovery::stream_io(cx.background_executor(), move || {
                client.upload_attachment(&prepared.metadata, prepared.bytes.clone())
            })
            .await;
            let cleanup = result.as_ref().ok().map(|asset| asset.id.clone());
            let applied = this
                .update_in(cx, |this, _, cx| {
                    if !this.question_upload_current(&job) {
                        this.question_release_stale(&job, cx);
                        return false;
                    }
                    let entry = this
                        .question_files
                        .entries
                        .get_mut(&job.key)
                        .unwrap()
                        .iter_mut()
                        .find(|entry| entry.row.id == job.id)
                        .unwrap();
                    entry.job = None;
                    match result {
                        Ok(asset) => {
                            entry.row.name = asset.metadata.name;
                            entry.row.source = asset.source;
                            entry.row.state = AttachmentState::Uploaded {
                                attachment_id: asset.id,
                                environment_id: job.environment.clone(),
                            };
                        }
                        Err(error) => {
                            entry.row.state = AttachmentState::Error {
                                message: error.to_string(),
                            }
                        }
                    }
                    this.cancel_question_transition(&job.key.0);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !applied {
                if let Some(id) = cleanup {
                    let client = job.context.client.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let _ = client.delete_attachment(&id);
                        })
                        .detach();
                }
            }
        })
        .detach();
        cx.notify();
    }
    fn question_remove_file(
        &mut self,
        control: &QuestionControl,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.question_file_allowed(control) {
            return;
        }
        let key = (
            control.transition.request.clone(),
            control.transition.question.clone(),
        );
        if let Some(entries) = self.question_files.entries.get_mut(&key) {
            if let Some(position) = entries.iter().position(|entry| entry.row.id == id) {
                let entry = entries.remove(position);
                if let AttachmentState::Uploaded { attachment_id, .. } = entry.row.state {
                    if let Some(context) = self.question_files.context.as_ref() {
                        let client = context.client.clone();
                        cx.background_executor()
                            .spawn(async move {
                                let _ = client.delete_attachment(&attachment_id);
                            })
                            .detach();
                    }
                }
            }
        }
        self.cancel_question_transition(&key.0);
        cx.notify();
    }
    pub(super) fn question_paste_files(
        &mut self,
        control: QuestionControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.question_file_allowed(&control) {
            return false;
        }
        use crate::note_paste::offer::PasteOffer;
        let offer = cx
            .try_global::<crate::note_paste::ManagedNoteClipboard>()
            .and_then(|clipboard| clipboard.0.read_chat_offer().ok());
        let sources = match offer {
            Some(PasteOffer::Files(paths)) => Some(
                paths
                    .into_iter()
                    .map(|path| {
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "attachment".into());
                        (Source::File(path), name)
                    })
                    .collect(),
            ),
            Some(PasteOffer::Image(image)) => Some(vec![(
                Source::Image(Arc::new(image.bytes)),
                "Clipboard.png".into(),
            )]),
            _ => cx.read_from_clipboard().and_then(|item| {
                item.entries().iter().find_map(|entry| match entry {
                    gpui::ClipboardEntry::Image(image) => Some(vec![(
                        Source::Image(Arc::new(image.bytes.clone())),
                        "Clipboard.png".into(),
                    )]),
                    gpui::ClipboardEntry::ExternalPaths(paths) => Some(
                        paths
                            .0
                            .iter()
                            .map(|path| {
                                let name = path
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "attachment".into());
                                (Source::File(path.clone()), name)
                            })
                            .collect(),
                    ),
                    _ => None,
                })
            }),
        };
        let Some(sources) = sources else {
            return false;
        };
        self.question_file_action(control, UserAction::Add(sources), window, cx);
        true
    }
    pub(super) fn render_question_files(
        &self,
        control: &QuestionControl,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let request = &control.transition.request;
        let question = &control.transition.question;
        let key = (request.clone(), question.clone());
        let unsupported = self
            .question_files
            .capabilities()
            .is_some_and(|caps| !caps.question_attachments_available());
        let loading = self.question_files.loading.is_some();
        let attach_control = control.clone();
        let paste_control = control.clone();
        let check_control = control.clone();
        let mut tray = v_flex().w_full().min_w_0().gap_1().debug_selector({
            let request = request.clone();
            let question = question.clone();
            move || format!("question-files-{request}-{question}")
        });
        let mut controls = h_flex()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!(
                    "question-attach-{request}-{question}"
                )))
                .debug_selector({
                    let request = request.clone();
                    let question = question.clone();
                    move || format!("question-attach-{request}-{question}")
                })
                .ghost()
                .label(if loading {
                    "Checking file support…"
                } else {
                    "Attach file"
                })
                .disabled(
                    disabled || unsupported || loading || self.question_files.context.is_none(),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.question_file_action(attach_control.clone(), UserAction::Pick, window, cx)
                })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "question-paste-{request}-{question}"
                )))
                .debug_selector({
                    let request = request.clone();
                    let question = question.clone();
                    move || format!("question-paste-{request}-{question}")
                })
                .ghost()
                .label("Paste attachment")
                .disabled(
                    disabled || unsupported || loading || self.question_files.context.is_none(),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.question_paste_files(paste_control.clone(), window, cx);
                })),
            );
        if self.question_files.profile.is_none() && self.request_retained(request) {
            controls = controls.child(
                Button::new(SharedString::from(format!(
                    "question-check-files-{request}"
                )))
                .ghost()
                .label("Check file support")
                .disabled(loading || !self.enabled || !self.state.ready || self.busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.question_file_action(check_control.clone(), UserAction::Check, window, cx)
                })),
            );
        }
        tray = tray.child(controls);
        if let Some(notice) = &self.question_files.notice {
            tray = tray.child(div().whitespace_normal().child(notice.clone()));
        }
        if let Some(entries) = self.question_files.entries.get(&key) {
            for entry in entries {
                let id = entry.row.id.clone();
                let retry_id = id.clone();
                let remove_id = id.clone();
                let retry_control = control.clone();
                let remove_control = control.clone();
                let status = match &entry.row.state {
                    AttachmentState::Pending => "Preparing or uploading…".to_owned(),
                    AttachmentState::Uploaded { .. } => "Ready".to_owned(),
                    AttachmentState::Error { message } => message.clone(),
                    AttachmentState::NeedsReselect => "Select this file again".into(),
                };
                let mut row = h_flex().w_full().min_w_0().gap_1().child(
                    div()
                        .min_w_0()
                        .whitespace_normal()
                        .child(format!("{} — {status}", entry.row.name)),
                );
                if let Some(preview) = entry
                    .prepared
                    .as_ref()
                    .and_then(|prepared| prepared.previews.as_ref())
                {
                    row = row.child(img(preview.0.clone()).w(px(40.)).h(px(32.)));
                }
                if matches!(
                    entry.row.state,
                    AttachmentState::Error { .. } | AttachmentState::NeedsReselect
                ) {
                    row = row.child(
                        Button::new(SharedString::from(format!("question-file-retry-{id}")))
                            .ghost()
                            .label("Retry")
                            .disabled(disabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.question_file_action(
                                    retry_control.clone(),
                                    UserAction::Retry(retry_id.clone()),
                                    window,
                                    cx,
                                )
                            })),
                    );
                }
                row = row.child(
                    Button::new(SharedString::from(format!("question-file-remove-{id}")))
                        .ghost()
                        .label("Remove")
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.question_remove_file(&remove_control, &remove_id, cx)
                        })),
                );
                tray = tray.child(row);
            }
        }
        tray.into_any_element()
    }
}

#[cfg(test)]
mod tests;
