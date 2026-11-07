//! Canonical local policy authority. Checkpoints never allocate this identity.
//! One namespace lock serializes configuration and every coordinator CAS.
use crate::project::*;
use okilum_t3::rotation::{Checkpoint, Owner, Policy, PolicyStore, TzifResolver};
use okilum_t3::stream::NativeClient;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    fs,
    io::{Read, Write},
    sync::atomic::{AtomicBool, Ordering},
};

type Result<T> = std::result::Result<T, String>;
const MAX_RECORD: usize = 131_072;
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|e| e.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::project) struct Identity {
    pub origin: String,
    pub project: String,
    pub manager: String,
}
impl Identity {
    fn validate(&self) -> Result<()> {
        if [&self.origin, &self.project, &self.manager]
            .iter()
            .any(|v| v.is_empty() || v.len() > 4096 || v.chars().any(char::is_control))
        {
            return Err("Rotation owner identity is incomplete or malformed".into());
        }
        let environment = Environment::new("rotation", &self.origin).map_err(|e| e.to_string())?;
        if environment.origin() != self.origin {
            return Err("Rotation owner origin is not canonical".into());
        }
        Ok(())
    }
    fn owner(&self, revision: u64) -> Owner {
        Owner {
            origin: self.origin.clone(),
            project: self.project.clone(),
            manager: self.manager.clone(),
            revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::project) struct Configuration {
    pub enabled: bool,
    pub timezone: String,
    pub at: [String; 2],
    pub reviewed_source: Option<String>,
}
impl Configuration {
    fn policy(&self) -> Result<Policy> {
        if self.timezone.len() > 1024
            || self
                .reviewed_source
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.len() > 65_536 || s.contains('\0'))
        {
            return Err("Rotation policy or reviewed draft text is malformed or too large".into());
        }
        Policy::new(self.enabled, &self.timezone, [&self.at[0], &self.at[1]]).map_err(str::to_owned)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Preparing,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::project) struct Record {
    schema: u32,
    pub sequence: u64,
    /// Allocated only by explicit configuration, independently of checkpoint CAS.
    pub lifecycle_revision: u64,
    identity: Identity,
    pub configuration: Configuration,
    phase: Phase,
}
impl Record {
    pub fn owner(&self) -> Owner {
        self.identity.owner(self.lifecycle_revision)
    }
}

pub(in crate::project) struct Loaded {
    pub authority: Arc<Store>,
    pub record: Record,
    pub policy: Arc<PolicyStore>,
    pub checkpoint: Checkpoint,
}

#[derive(Clone)]
pub(in crate::project) struct Store {
    state: PathBuf,
    directory: PathBuf,
    identity: Identity,
}
struct IdentityLock(PathBuf);
impl Drop for IdentityLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
impl Store {
    pub fn new(state: &Path, identity: Identity) -> Result<Self> {
        identity.validate()?;
        // JSON tuple encoding supplies unambiguous component boundaries. The
        // full identity is also checked in every immutable record after hashing.
        let encoded = serde_json::to_vec(&[&identity.origin, &identity.project, &identity.manager])
            .map_err(|e| e.to_string())?;
        let key = format!("{:x}", Sha256::digest(encoded));
        Ok(Self {
            state: state.to_owned(),
            directory: state.join("manager-rotation-v1").join(key),
            identity,
        })
    }
    pub fn policy_store(&self) -> Arc<PolicyStore> {
        Arc::new(PolicyStore::new(self.directory.join("checkpoints")))
    }
    fn directories(&self, create: bool) -> Result<bool> {
        let namespace = self.state.join("manager-rotation-v1");
        for path in [&self.state, &namespace, &self.directory] {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
                Ok(_) => return Err("Rotation namespace is not an owned plain directory".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                    let mut builder = fs::DirBuilder::new();
                    builder.recursive(path == &self.state);
                    #[cfg(unix)]
                    builder.mode(0o700);
                    io(builder.create(path))?;
                    let metadata = io(fs::symlink_metadata(path))?;
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err("Rotation namespace changed while opening".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.to_string()),
            }
        }
        if let Ok(metadata) = fs::symlink_metadata(self.directory.join("checkpoints")) {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("Rotation checkpoint namespace is not a plain directory".into());
            }
        }
        Ok(true)
    }
    fn lock(&self) -> Result<IdentityLock> {
        self.directories(true)?;
        let path = self.directory.join("identity.lock");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&path).map_err(|e| {
            format!("Rotation state is busy or needs interrupted-write recovery: {e}")
        })?;
        let lock = IdentityLock(path);
        io(file.write_all(uuid::Uuid::new_v4().to_string().as_bytes()))?;
        io(file.sync_all())?;
        Ok(lock)
    }
    fn latest(&self) -> Result<Option<Record>> {
        if !self.directories(false)? {
            return Ok(None);
        }
        let mut records = Vec::new();
        for entry in io(fs::read_dir(&self.directory))? {
            let entry = io(entry)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Invalid rotation authority filename")?;
            if let Some(number) = name.strip_suffix(".owner") {
                let n: u64 = number
                    .parse()
                    .map_err(|_| "Invalid rotation authority sequence")?;
                if n.to_string() != number {
                    return Err("Ambiguous rotation authority sequence".into());
                }
                records.push((n, entry.path()));
            } else if name != "checkpoints"
                && name != "identity.lock"
                && !(name.starts_with(".owner-") && name.ends_with(".tmp"))
            {
                return Err("Unrecognized rotation authority state; nothing was replaced".into());
            }
        }
        records.sort_by_key(|(n, _)| *n);
        let mut previous: Option<Record> = None;
        for (index, (sequence, path)) in records.into_iter().enumerate() {
            if sequence != index as u64 + 1 {
                return Err("Rotation authority history is incomplete".into());
            }
            let metadata = io(fs::symlink_metadata(&path))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > MAX_RECORD as u64
            {
                return Err("Rotation authority record is not a bounded plain file".into());
            }
            let mut bytes = Vec::new();
            io(io(fs::File::open(path))?
                .take(MAX_RECORD as u64 + 1)
                .read_to_end(&mut bytes))?;
            if bytes.len() > MAX_RECORD {
                return Err("Rotation authority record is too large".into());
            }
            let record: Record = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Rotation authority is corrupt: {e}"))?;
            if record.schema != 1 || record.sequence != sequence || record.identity != self.identity
            {
                return Err("Rotation authority identity or schema does not match".into());
            }
            record.configuration.policy()?;
            let valid = match (previous.as_ref(), record.phase) {
                (None, Phase::Preparing) => record.lifecycle_revision == 1,
                (Some(before), Phase::Preparing) => {
                    before.lifecycle_revision.checked_add(1) == Some(record.lifecycle_revision)
                }
                (Some(before), Phase::Ready) => {
                    before.phase == Phase::Preparing
                        && before.lifecycle_revision == record.lifecycle_revision
                        && before.configuration == record.configuration
                }
                _ => false,
            };
            if !valid {
                return Err("Rotation authority lifecycle history is invalid".into());
            }
            previous = Some(record);
        }
        if previous.is_none() && self.policy_store().load()?.is_some() {
            return Err("Rotation checkpoint exists without canonical owner authority".into());
        }
        Ok(previous)
    }
    fn publish(&self, expected: u64, record: &Record) -> Result<()> {
        if self.latest()?.as_ref().map_or(0, |r| r.sequence) != expected
            || expected.checked_add(1) != Some(record.sequence)
        {
            return Err("Rotation authority changed; reload before saving".into());
        }
        let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_RECORD {
            return Err("Rotation authority record is too large".into());
        }
        let temporary = self
            .directory
            .join(format!(".owner-{}.tmp", uuid::Uuid::new_v4()));
        struct Temporary(PathBuf);
        impl Drop for Temporary {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let cleanup = Temporary(temporary);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = io(options.open(&cleanup.0))?;
        io(file.write_all(&bytes))?;
        io(file.sync_all())?;
        drop(file);
        io(fs::hard_link(
            &cleanup.0,
            self.directory.join(format!("{}.owner", record.sequence)),
        ))?;
        // Match PolicyStore's platform contract: flush immutable file bytes on
        // every OS, and the directory entry where directory fsync is supported.
        #[cfg(unix)]
        io(fs::File::open(&self.directory).and_then(|d| d.sync_all()))?;
        Ok(())
    }
    fn bound(&self, record: Record) -> Result<Loaded> {
        if record.phase != Phase::Ready {
            return Err(
                "Rotation configuration was interrupted before Ready; authority is held".into(),
            );
        }
        let policy = self.policy_store();
        let checkpoint = policy
            .load()?
            .ok_or("Canonical rotation owner has no policy checkpoint")?;
        if checkpoint.owner != record.owner()
            || checkpoint.policy != record.configuration.policy()?
        {
            return Err("Canonical rotation owner and checkpoint do not match".into());
        }
        Ok(Loaded {
            authority: Arc::new(self.clone()),
            record,
            policy,
            checkpoint,
        })
    }
    /// Read-only: opening/restarting never creates an owner or advances a revision.
    pub fn load(&self) -> Result<Option<Loaded>> {
        self.latest()?.map(|r| self.bound(r)).transpose()
    }

    pub fn configure(
        &self,
        expected_sequence: u64,
        configuration: Configuration,
        now: i64,
        resolver: &TzifResolver,
        valid: impl Fn() -> bool,
    ) -> Result<Loaded> {
        let policy = configuration.policy()?;
        let day = resolver.local_day(policy.timezone(), now)?;
        policy.preview(&[day], now, resolver)?;
        let _lock = self.lock()?;
        let previous = self.latest()?;
        if previous.as_ref().map_or(0, |r| r.sequence) != expected_sequence {
            return Err("Rotation policy changed; reload before saving".into());
        }
        let checkpoint = self.policy_store().load()?;
        if checkpoint.as_ref().is_some_and(|c| c.pending.is_some()) {
            return Err(
                "Review or cancel the pending rotation claim before changing policy".into(),
            );
        }
        if checkpoint.as_ref().is_some_and(|c| c.through > now) {
            return Err("Clock moved behind the saved rotation cursor".into());
        }
        if let Some(previous) = &previous {
            if previous.phase == Phase::Ready {
                self.bound(previous.clone())?;
            } else if checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.owner != previous.owner()
                    || previous.configuration.policy().as_ref() != Ok(&checkpoint.policy)
            }) {
                return Err(
                    "Interrupted authority and checkpoint disagree; explicit recovery is required"
                        .into(),
                );
            }
        }
        if !valid() {
            return Err("Rotation configuration owner was revoked".into());
        }
        let revision = previous
            .as_ref()
            .map_or(0, |r| r.lifecycle_revision)
            .checked_add(1)
            .ok_or("Rotation policy-owner revision exhausted")?;
        let preparing = Record {
            schema: 1,
            sequence: expected_sequence
                .checked_add(1)
                .ok_or("Authority sequence exhausted")?,
            lifecycle_revision: revision,
            identity: self.identity.clone(),
            configuration,
            phase: Phase::Preparing,
        };
        self.publish(expected_sequence, &preparing)?;
        if !valid() {
            return Err("Rotation configuration revoked while Preparing; authority held".into());
        }
        self.policy_store().configure(
            checkpoint.as_ref().map_or(0, |c| c.revision),
            preparing.owner(),
            policy,
            now,
            resolver,
        )?;
        if !valid() {
            return Err("Rotation configuration revoked before Ready; authority held".into());
        }
        let ready = Record {
            sequence: preparing
                .sequence
                .checked_add(1)
                .ok_or("Authority sequence exhausted")?,
            phase: Phase::Ready,
            ..preparing
        };
        self.publish(ready.sequence - 1, &ready)?;
        self.bound(ready)
    }
    /// The lock is retained through the synchronous operation, including its
    /// existing checkpoint CAS. It must never cross an await or a UI callback.
    pub fn with_current<T>(
        &self,
        expected: &Owner,
        operation: impl FnOnce(&PolicyStore) -> Result<T>,
    ) -> Result<T> {
        let _lock = self.lock()?;
        let record = self
            .latest()?
            .ok_or("Canonical rotation owner is missing")?;
        let loaded = self.bound(record)?;
        if &loaded.record.owner() != expected {
            return Err("Canonical rotation owner was replaced".into());
        }
        operation(&loaded.policy)
    }
}

// The UI below owns only form state and cancellation leases. All durable policy
// semantics, checkpoint transitions and draft reconciliation remain in the
// existing Policy/PolicyStore/Coordinator implementations.
struct Form {
    identity: Identity,
    sequence: u64,
    enabled: bool,
    timezone: Entity<InputState>,
    first: Entity<InputState>,
    second: Entity<InputState>,
    source: Entity<TextareaState>,
}
#[derive(Default)]
pub(in crate::project) struct State {
    form: Option<Form>,
    loaded: Option<Loaded>,
    request: u64,
    pending: Option<Arc<AtomicBool>>,
    pending_client: Option<Arc<NativeClient>>,
    error: Option<String>,
}
impl State {
    pub fn is_open(&self) -> bool {
        self.form.is_some()
    }
    pub fn revoke_requests(&mut self) {
        self.pending_client = None;
        if let Some(valid) = self.pending.take() {
            valid.store(false, Ordering::Release);
            self.request = self
                .request
                .checked_add(1)
                .expect("Rotation UI request generation exhausted");
        }
    }
    pub fn authority_for(&self, owner: &Owner, policy: &Arc<PolicyStore>) -> Result<Arc<Store>> {
        let loaded = self
            .loaded
            .as_ref()
            .ok_or("Canonical rotation authority has not been loaded")?;
        if loaded.record.owner() != *owner || !Arc::ptr_eq(&loaded.policy, policy) {
            return Err("Rotation authority does not own this exact policy store".into());
        }
        Ok(loaded.authority.clone())
    }
}

struct MenuTicket {
    identity: Identity,
    generation: u64,
    connection: u64,
    client: Arc<NativeClient>,
}
impl MenuTicket {
    fn capture(view: &ProjectView, cx: &App) -> Result<Self> {
        Ok(Self {
            identity: exact_identity(view, cx)?,
            generation: view.generation,
            connection: view.native.connection_epoch,
            client: view
                .native
                .model_client()
                .ok_or("Current Manager client is missing")?,
        })
    }
    fn current(&self, view: &ProjectView, cx: &App) -> bool {
        self.generation == view.generation
            && self.connection == view.native.connection_epoch
            && exact_identity(view, cx).as_ref() == Ok(&self.identity)
            && self.client.access_allowed()
            && view
                .native
                .model_client()
                .is_some_and(|client| Arc::ptr_eq(&client, &self.client))
    }
    fn open(&self, view: &mut ProjectView, window: &mut Window, cx: &mut Context<ProjectView>) {
        if self.current(view, cx) {
            view.open_rotation_policy(window, cx);
        } else {
            view.rotation_notice = Some("Manager changed or access was revoked. Reopen its menu before configuring rotation.".into());
            cx.notify();
        }
    }
}
pub(in crate::project) fn menu_item(
    owner: WeakEntity<ProjectView>,
    cx: &mut App,
) -> gpui_component::menu::PopupMenuItem {
    let ticket = owner
        .upgrade()
        .ok_or("Project view closed".to_string())
        .and_then(|view| MenuTicket::capture(view.read(cx), cx));
    gpui_component::menu::PopupMenuItem::new("Rotation policy…").on_click(move |_, window, cx| {
        let _ = owner.update(cx, |view, cx| {
            match &ticket {
                Ok(ticket) => ticket.open(view, window, cx),
                _ => {
                    view.rotation_notice = Some("Manager changed or access was revoked. Reopen its menu before configuring rotation.".into());
                    cx.notify();
                }
            }
        });
    })
}

fn exact_identity(view: &ProjectView, cx: &App) -> Result<Identity> {
    if !view.active
        || view.section != ProjectSection::Manager
        || view.executors_open
        || !view.draft_handoff_surface_is_visible()
        || !view.draft_handoff_owner_ready(cx)
        || view.typed_scope().is_none()
        || view.native.deleted()
        || !reader_features::enabled(reader_features::Feature::Agents, cx)
    {
        return Err("Open the current ready Manager surface before configuring rotation".into());
    }
    let client = view
        .native
        .model_client()
        .ok_or("Current Manager access is unavailable")?;
    if !client.access_allowed()
        || view
            .threads
            .handoff_revoked
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &client))
    {
        return Err("Current Manager access was revoked".into());
    }
    let environment = view
        .environment
        .as_ref()
        .ok_or("Manager environment is missing")?;
    if reader_t3_settings::signed_out(&environment.origin(), cx) {
        return Err("Manager is signed out".into());
    }
    let binding = view.binding.as_ref().ok_or("Manager binding is missing")?;
    let identity = Identity {
        origin: environment.origin(),
        project: binding
            .manager
            .project_id
            .clone()
            .ok_or("Project identity is missing")?,
        manager: binding
            .manager
            .thread_id
            .clone()
            .ok_or("Manager identity is missing")?,
    };
    if view.chat.as_ref().map(|chat| chat.thread_id.as_str()) != Some(identity.manager.as_str()) {
        return Err("Selected Manager no longer matches its binding".into());
    }
    identity.validate()?;
    Ok(identity)
}
pub(in crate::project) fn canonical_owner(view: &ProjectView, cx: &App) -> Option<Owner> {
    let identity = exact_identity(view, cx).ok()?;
    let loaded = view.rotation_policy.loaded.as_ref()?;
    (loaded.record.identity == identity && loaded.record.phase == Phase::Ready)
        .then(|| loaded.record.owner())
}
fn now() -> Result<i64> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "Clock is outside the supported range".into())
}
fn resolver() -> TzifResolver {
    TzifResolver::new("/usr/share/zoneinfo")
}
fn input(value: &str, placeholder: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()));
    state.update(cx, |state, cx| {
        state.set_value(value.to_owned(), window, cx)
    });
    state
}
impl Form {
    fn new(identity: Identity, loaded: Option<&Loaded>, window: &mut Window, cx: &mut App) -> Self {
        let configuration =
            loaded
                .map(|l| l.record.configuration.clone())
                .unwrap_or(Configuration {
                    enabled: false,
                    timezone: String::new(),
                    at: ["08:30".into(), "20:30".into()],
                    reviewed_source: None,
                });
        let source = cx.new(|cx| TextareaState::new(window, cx));
        source.update(cx, |state, cx| {
            state.set_value(
                configuration.reviewed_source.clone().unwrap_or_default(),
                window,
                cx,
            )
        });
        Self {
            identity,
            sequence: loaded.map_or(0, |l| l.record.sequence),
            enabled: configuration.enabled,
            timezone: input(
                &configuration.timezone,
                "IANA timezone, e.g. Asia/Jerusalem",
                window,
                cx,
            ),
            first: input(&configuration.at[0], "HH:MM", window, cx),
            second: input(&configuration.at[1], "HH:MM", window, cx),
            source,
        }
    }
    fn configuration(&self, cx: &App) -> Configuration {
        let source = self.source.read(cx).value().to_string();
        Configuration {
            enabled: self.enabled,
            timezone: self.timezone.read(cx).value().to_string(),
            at: [
                self.first.read(cx).value().to_string(),
                self.second.read(cx).value().to_string(),
            ],
            reviewed_source: (!source.is_empty()).then_some(source),
        }
    }
}

impl ProjectView {
    pub(in crate::project) fn open_rotation_policy(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = exact_identity(self, cx).and_then(|identity| {
            let state = self
                .state
                .as_deref()
                .ok_or("Durable application state is unavailable")?;
            Ok((identity.clone(), Store::new(state, identity)?))
        });
        let (identity, store) = match target {
            Ok(target) => target,
            Err(error) => {
                self.rotation_notice = Some(error);
                cx.notify();
                return;
            }
        };
        self.invalidate_rotation(cx);
        // Retain edits when reopening the same form; an explicit Reload is the
        // only operation that replaces an already-authored configuration draft.
        let fresh_form = self
            .rotation_policy
            .form
            .as_ref()
            .is_none_or(|form| form.identity != identity);
        if fresh_form {
            self.rotation_policy.form = Some(Form::new(identity.clone(), None, window, cx));
        }
        self.rotation_policy.error = None;
        let initial_form = self
            .rotation_policy
            .form
            .as_ref()
            .map(|form| form.configuration(cx));
        let request = self.rotation_policy.request;
        let generation = self.generation;
        let connection = self.native.connection_epoch;
        let valid = Arc::new(AtomicBool::new(true));
        self.rotation_policy.pending = Some(valid.clone());
        self.rotation_policy.pending_client = self.native.model_client();
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { store.load() })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                if !view.rotation_request_current(
                    request, generation, connection, &identity, &valid, cx,
                ) {
                    return;
                }
                view.rotation_policy.pending = None;
                match loaded {
                    Ok(loaded) => {
                        if let Some(form) = &view.rotation_policy.form {
                            if fresh_form
                                && form.sequence == 0
                                && loaded.is_some()
                                && Some(form.configuration(cx)) == initial_form
                            {
                                view.rotation_policy.form =
                                    Some(Form::new(identity, loaded.as_ref(), window, cx));
                            }
                        }
                        view.rotation_loaded(loaded, window, cx);
                    }
                    Err(error) => {
                        view.rotation_policy.loaded = None;
                        view.rotation_policy.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn rotation_request_current(
        &self,
        request: u64,
        generation: u64,
        connection: u64,
        identity: &Identity,
        valid: &AtomicBool,
        cx: &App,
    ) -> bool {
        valid.load(Ordering::Acquire)
            && self.rotation_policy.request == request
            && self.generation == generation
            && self.native.connection_epoch == connection
            && exact_identity(self, cx).as_ref() == Ok(identity)
            && self
                .rotation_policy
                .pending_client
                .as_ref()
                .is_some_and(|expected| {
                    expected.access_allowed()
                        && self
                            .native
                            .model_client()
                            .is_some_and(|client| Arc::ptr_eq(expected, &client))
                })
    }
    fn rotation_loaded(
        &mut self,
        loaded: Option<Loaded>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rotation_policy.error = None;
        let pending = loaded
            .as_ref()
            .filter(|l| l.checkpoint.pending.is_some())
            .map(|l| {
                (
                    l.policy.clone(),
                    l.record.configuration.reviewed_source.clone(),
                )
            });
        self.rotation_policy.loaded = loaded;
        if let Some((policy, source)) = pending {
            self.mount_pending_rotation(policy, source, canonical_owner, window, cx);
        }
    }
    fn save_rotation_policy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prepared = (|| {
            let identity = exact_identity(self, cx)?;
            let form = self
                .rotation_policy
                .form
                .as_ref()
                .ok_or("Open rotation policy first")?;
            if form.identity != identity {
                return Err("Configuration belongs to a different Manager".into());
            }
            let configuration = form.configuration(cx);
            configuration.policy()?;
            let state = self
                .state
                .as_deref()
                .ok_or("Durable application state is unavailable")?;
            Ok((
                identity.clone(),
                Store::new(state, identity)?,
                form.sequence,
                configuration,
                now()?,
            ))
        })();
        let (identity, store, sequence, configuration, at) = match prepared {
            Ok(value) => value,
            Err(error) => {
                self.rotation_policy.error = Some(error);
                cx.notify();
                return;
            }
        };
        if self.rotation_policy.pending.is_some() {
            return;
        }
        self.invalidate_rotation(cx);
        self.rotation_policy.loaded = None;
        self.rotation_policy.error = None;
        let request = self.rotation_policy.request;
        let generation = self.generation;
        let connection = self.native.connection_epoch;
        let valid = Arc::new(AtomicBool::new(true));
        self.rotation_policy.pending = Some(valid.clone());
        self.rotation_policy.pending_client = self.native.model_client();
        let client = self
            .rotation_policy
            .pending_client
            .clone()
            .expect("validated current native client");
        let write_valid = valid.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move {
                    store.configure(sequence, configuration, at, &resolver(), || {
                        write_valid.load(Ordering::Acquire) && client.access_allowed()
                    })
                })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                if !view.rotation_request_current(
                    request, generation, connection, &identity, &valid, cx,
                ) {
                    return;
                }
                view.rotation_policy.pending = None;
                match saved {
                    Ok(loaded) => {
                        if let Some(form) = &mut view.rotation_policy.form {
                            form.sequence = loaded.record.sequence;
                        }
                        view.rotation_loaded(Some(loaded), window, cx);
                    }
                    Err(error) => view.rotation_policy.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn review_rotation_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prepared = (|| {
            let identity = exact_identity(self, cx)?;
            let loaded = self
                .rotation_policy
                .loaded
                .as_ref()
                .ok_or("Save an explicit policy before reviewing a window")?;
            if loaded.record.identity != identity || !loaded.record.configuration.enabled {
                return Err("Enable the policy for this exact Manager first".into());
            }
            if self.busy {
                return Err(
                    "Wait for the current Manager operation before reviewing a window".into(),
                );
            }
            if self.native.status_label(cx) == "Working"
                || self.chat.as_ref().is_some_and(|chat| {
                    chat.pending_questions != 0 || chat.pending_question_error.is_some()
                })
            {
                return Err(
                    "Resolve the active operation or pending questions before reviewing a window"
                        .into(),
                );
            }
            if self
                .rotation_policy
                .form
                .as_ref()
                .is_none_or(|form| form.configuration(cx) != loaded.record.configuration)
            {
                return Err(
                    "Save or reload configuration edits before reviewing the saved policy".into(),
                );
            }
            Ok((
                identity,
                loaded.authority.clone(),
                loaded.record.owner(),
                loaded.checkpoint.clone(),
                now()?,
            ))
        })();
        let (identity, authority, owner, checkpoint, at) = match prepared {
            Ok(value) => value,
            Err(error) => {
                self.rotation_policy.error = Some(error);
                cx.notify();
                return;
            }
        };
        if self.rotation_policy.pending.is_some() {
            return;
        }
        self.invalidate_rotation(cx);
        let request = self.rotation_policy.request;
        let generation = self.generation;
        let connection = self.native.connection_epoch;
        let valid = Arc::new(AtomicBool::new(true));
        self.rotation_policy.pending = Some(valid.clone());
        self.rotation_policy.pending_client = self.native.model_client();
        let client = self
            .rotation_policy
            .pending_client
            .clone()
            .expect("validated current native client");
        let read_valid = valid.clone();
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    authority.with_current(&owner, |policy| {
                        if !read_valid.load(Ordering::Acquire) || !client.access_allowed() {
                            return Err("Rotation request was revoked".into());
                        }
                        let current = policy.load()?.ok_or("Rotation checkpoint is missing")?;
                        if current != checkpoint {
                            return Err(
                                "Rotation window changed; reopen policy before reviewing".into()
                            );
                        }
                        if current.pending.is_none() {
                            policy.claim(current.revision, &owner, at, &resolver(), None)?;
                        }
                        Ok(())
                    })?;
                    authority
                        .load()?
                        .ok_or("Canonical rotation authority is missing".into())
                })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                if !view.rotation_request_current(
                    request, generation, connection, &identity, &valid, cx,
                ) {
                    return;
                }
                view.rotation_policy.pending = None;
                match loaded {
                    Ok(loaded) => view.rotation_loaded(Some(loaded), window, cx),
                    Err(error) => view.rotation_policy.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn close_rotation_policy(&mut self, cx: &mut Context<Self>) {
        self.invalidate_rotation(cx);
        self.rotation_policy.form = None;
        cx.notify();
    }
    fn reload_rotation_policy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_rotation(cx);
        self.rotation_policy.form = None;
        self.open_rotation_policy(window, cx);
    }
    pub(in crate::project) fn render_rotation_policy(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(form) = &self.rotation_policy.form else {
            return div().into_any_element();
        };
        let busy = self.rotation_policy.pending.is_some();
        let enabled = form.enabled;
        let dirty = self
            .rotation_policy
            .loaded
            .as_ref()
            .is_none_or(|loaded| form.configuration(cx) != loaded.record.configuration);
        v_flex()
            .id("rotation-policy")
            .w_full()
            .p_3()
            .gap_2()
            .child("Local rotation recovery")
            .child("Review each rotation draft and send it yourself. No scheduled task is created.")
            .child(
                Button::new("rotation-policy-enabled")
                    .ghost()
                    .label(if enabled {
                        "Enabled"
                    } else {
                        "Disabled — opt in explicitly"
                    })
                    .selected(enabled)
                    .disabled(busy)
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(form) = &mut view.rotation_policy.form {
                            form.enabled = !form.enabled;
                        }
                        cx.notify();
                    })),
            )
            .child(Input::new(&form.timezone))
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&form.first))
                    .child(Input::new(&form.second)),
            )
            .child("Reviewed rotation draft text (optional)")
            .child(Textarea::new(&form.source))
            .when_some(self.rotation_policy.error.clone(), |surface, error| {
                surface.child(div().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("rotation-policy-save")
                            .ghost()
                            .label("Save policy")
                            .disabled(busy)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.save_rotation_policy(window, cx)
                            })),
                    )
                    .child(
                        Button::new("rotation-policy-review")
                            .ghost()
                            .label("Review missed window")
                            .disabled(
                                busy || dirty
                                    || self
                                        .rotation_policy
                                        .loaded
                                        .as_ref()
                                        .is_none_or(|l| !l.record.configuration.enabled),
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.review_rotation_window(window, cx)
                            })),
                    )
                    .child(
                        Button::new("rotation-policy-reload")
                            .ghost()
                            .label("Reload saved policy")
                            .disabled(busy)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.reload_rotation_policy(window, cx)
                            })),
                    )
                    .child(
                        Button::new("rotation-policy-close")
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|view, _, _, cx| view.close_rotation_policy(cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "policy_owner_tests.rs"]
pub(in crate::project) mod tests;
