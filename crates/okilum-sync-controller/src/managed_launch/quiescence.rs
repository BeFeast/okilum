//! A scoped transport proof composed with a drained local writer capability.
//! Dropping this value never releases the durable local hold or resumes Sync.
use super::*;
use okilum_write_authority::Hold;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    Pausing,
    Quiescent,
    ResumeAuthorized,
    Complete,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    operation: String,
    epoch: String,
    root: PathBuf,
    folder: String,
    generation: Generation,
    cursor: u64,
    paused_event: Option<u64>,
    pause_label: String,
    before: Value,
    phase: Phase,
}

/// The gate prevents a new managed generation/configuration from entering while
/// publication uses this proof. A crash releases the OS gate, but the local hold
/// keeps future launches fenced. Ordinary transfers of other folders continue.
pub struct ScopedHold {
    hold: Hold,
    daemon: DaemonIdentity,
    reference: Reference,
    gate: Arc<File>,
    path: PathBuf,
    record: Record,
}

fn events(value: &Value) -> Result<&[Value]> {
    let values = value.as_array().context("pause event inventory missing")?;
    ensure!(values.len() <= 256, "pause event inventory exceeds bound");
    let mut previous = 0;
    for event in values {
        let id = event["id"].as_u64().context("pause event cursor missing")?;
        ensure!(
            id > previous && event["type"] == "FolderPaused" && event["data"]["id"].is_string(),
            "pause event inventory malformed or unordered"
        );
        previous = id;
    }
    Ok(values)
}
fn paused(record: &Record) -> Value {
    let mut result = record.before.clone();
    result["paused"] = json!(true);
    result["label"] = json!(record.pause_label);
    result
}
fn same_generation(expected: &Generation, actual: &Generation) -> Result<()> {
    ensure!(
        actual == expected,
        "transport generation changed; recovery required"
    );
    Ok(())
}
impl ScopedHold {
    /// Cancellation before ANY merge publication needs no exclusive transport
    /// receipt: it restores normal Sync, never publishes a canonical byte.
    pub fn cancel_unpublished(daemon: &DaemonIdentity, folder: &str, hold: Hold) -> Result<()> {
        hold.validate_held(hold.token())?;
        let home = daemon
            .config_file
            .parent()
            .context("managed home missing")?;
        let reference = existing(home)?;
        let gate = lock_file(&reference.directory.join("gate.lock"), false, false)?;
        let binding = reference.load()?;
        let registered = binding
            .folders
            .get(folder)
            .context("managed folder missing")?;
        ensure!(
            registered.root == hold.root(),
            "unpublished cancellation root changed"
        );
        let path = reference
            .directory
            .join("scoped-holds")
            .join(format!("{:x}.json", Sha256::digest(folder.as_bytes())));
        let has_record = path.try_exists()?;
        if has_record {
            let mut record: Record = serde_json::from_slice(&private::read(&path)?)?;
            ensure!(
                record.operation == hold.token().operation_id
                    && record.epoch == hold.token().epoch
                    && matches!(
                        record.phase,
                        Phase::Pausing | Phase::Quiescent | Phase::ResumeAuthorized
                    ),
                "unpublished cancellation transport identity differs"
            );
            record.phase = Phase::ResumeAuthorized;
            private::write(&path, &serde_json::to_vec(&record)?)?;
        } else {
            ensure!(
                daemon.connect()?.folder(folder)?["paused"] == false,
                "unpublished folder has an unknown pause; review before cancellation"
            );
        }
        let operation = hold.token().operation_id.clone();
        let token = hold.token().clone();
        hold.release(&token)?;
        drop(gate);
        if has_record {
            Self::resume_committed(daemon, folder, &operation)?;
        }
        Ok(())
    }
    pub fn configuration_before_pause(&self) -> Result<Value> {
        self.validate()?;
        let mut config = self.daemon.connect()?.config()?;
        let folder = config["folders"]
            .as_array_mut()
            .context("folder inventory missing")?
            .iter_mut()
            .find(|folder| folder["id"] == self.record.folder)
            .context("held folder missing")?;
        *folder = self.record.before.clone();
        Ok(config)
    }
    pub fn runtime_started(&self) -> Option<&str> {
        self.record.generation.rest_start_time.as_deref()
    }
    /// Explicit recovery after the distributed journal durably records both
    /// publications. This resumes only the exact saved configuration; it grants
    /// no canonical publication authority and never reruns a completed merge.
    pub fn resume_committed(daemon: &DaemonIdentity, folder: &str, operation: &str) -> Result<()> {
        let home = daemon
            .config_file
            .parent()
            .context("managed home missing")?;
        let reference = existing(home)?;
        let gate = lock_file(&reference.directory.join("gate.lock"), false, false)?;
        let binding = reference.load()?;
        let directory = reference.directory.join("scoped-holds");
        let path = directory.join(format!("{:x}.json", Sha256::digest(folder.as_bytes())));
        let mut record: Record = serde_json::from_slice(&private::read(&path)?)?;
        ensure!(
            record.operation == operation
                && record.folder == folder
                && matches!(
                    record.phase,
                    Phase::Quiescent | Phase::ResumeAuthorized | Phase::Complete
                ),
            "first Sync resume has no committed transport hold"
        );
        if record.phase == Phase::Complete {
            return Ok(());
        }
        let registered = binding
            .folders
            .get(folder)
            .context("managed folder binding missing")?;
        ensure!(
            registered.root == record.root,
            "committed first Sync root changed"
        );
        live_generation(home)?;
        let api = daemon.connect()?;
        let current = api.folder(folder)?;
        ensure!(
            current == paused(&record) || current == record.before,
            "committed resume configuration changed"
        );
        record.phase = Phase::ResumeAuthorized;
        private::write(&path, &serde_json::to_vec(&record)?)?;
        let authority = registered.authority.open()?;
        let observed = authority.snapshot()?;
        if observed.phase != okilum_write_authority::Phase::Open {
            ensure!(
                observed.operation_id.as_deref() == Some(operation)
                    && observed.epoch == record.epoch,
                "another operation owns the committed first Sync root"
            );
            let mut hold = authority.recover(&observed)?;
            let token = hold.token().clone();
            ensure!(
                hold.try_hold(&token)?,
                "committed first Sync still drains writers"
            );
            let token = hold.token().clone();
            hold.release(&token)?;
        }
        drop(gate);
        let _admission = crate::admission::configuration(daemon, folder, &record.root)?;
        if current != record.before {
            ensure!(
                api.folder(folder)? == current,
                "first Sync resume changed before admission"
            );
            api.patch_folder(
                folder,
                &json!({"paused":false,"label":record.before["label"]}),
            )?;
        }
        record.phase = Phase::Complete;
        private::write(&path, &serde_json::to_vec(&record)?)
    }
    /// Caller drains local writers first. Recovery is explicit and requires a
    /// Hold adopted through the authority's exact observed-state recovery API.
    pub fn acquire(
        daemon: DaemonIdentity,
        folder: &str,
        hold: Hold,
        recovery: bool,
    ) -> Result<Self> {
        hold.validate_held(hold.token())?;
        let home = daemon
            .config_file
            .parent()
            .context("managed home missing")?;
        let reference = existing(home)?;
        let binding = reference.load()?;
        let gate = Arc::new(lock_file(
            &reference.directory.join("gate.lock"),
            false,
            false,
        )?);
        let binding_after = reference.load()?;
        ensure!(binding.id == binding_after.id, "managed binding changed");
        ensure!(
            crate::daemon::managed_transport_identity(
                home.parent().context("preparation missing")?
            )? == daemon,
            "external runtime cannot grant scoped ownership"
        );
        let registered = binding_after
            .folders
            .get(folder)
            .context("managed folder binding missing")?;
        ensure!(
            registered.root == hold.root()
                && registered.authority.open()?.root() == hold.root()
                && identity(hold.root())? == registered.identity,
            "transport and local ownership name different roots"
        );
        let generation = live_generation(home)?;
        let api = daemon.connect()?;
        ensure!(
            api.version()?["version"] == crate::daemon::CLIENT_VERSION,
            "scoped quiescence requires the pinned runtime"
        );
        let directory = reference.directory.join("scoped-holds");
        private::directory(&directory)?;
        let path = directory.join(format!("{:x}.json", Sha256::digest(folder.as_bytes())));
        let before = api.folder(folder)?;
        ensure!(
            before["id"] == folder
                && Path::new(before["path"].as_str().context("folder path missing")?)
                    .canonicalize()?
                    == hold.root(),
            "managed folder identity changed"
        );
        let mut record = if recovery {
            let record: Record = serde_json::from_slice(&private::read(&path)?)?;
            ensure!(
                record.schema == 1
                    && record.operation == hold.token().operation_id
                    && record.epoch == hold.token().epoch
                    && record.root == hold.root()
                    && record.folder == folder
                    && matches!(record.phase, Phase::Pausing | Phase::Quiescent),
                "stale or completed transport recovery refused"
            );
            ensure!(
                before == paused(&record)
                    || (record.phase == Phase::Pausing && before == record.before),
                "held folder configuration changed"
            );
            if before == record.before {
                // The prior process may have died after persisting its cursor
                // but before submitting PATCH. Never infer acknowledgement.
                api.patch_folder(folder, &json!({"paused":true,"label":record.pause_label}))?;
            }
            record
        } else {
            if path.try_exists()? {
                let old: Record = serde_json::from_slice(&private::read(&path)?)?;
                ensure!(
                    old.phase == Phase::Complete,
                    "unfinished transport hold requires recovery"
                );
            }
            ensure!(
                before["paused"] == false,
                "already paused folder requires explicit recovery proof"
            );
            let history = api.folder_pause_events(0)?;
            let cursor = events(&history)?
                .last()
                .map(|e| e["id"].as_u64().unwrap())
                .unwrap_or(0);
            let record = Record {
                schema: 1,
                operation: hold.token().operation_id.clone(),
                epoch: hold.token().epoch.clone(),
                root: hold.root().to_owned(),
                folder: folder.into(),
                generation: generation.clone(),
                cursor,
                paused_event: None,
                // FolderPaused includes the committed label. This binds the
                // event to THIS pause, even if an older event was buffered late.
                pause_label: format!("Okilum first Sync {}", Uuid::new_v4()),
                before,
                phase: Phase::Pausing,
            };
            // The cursor and intent must survive a lost PATCH reply or holder.
            private::write(&path, &serde_json::to_vec(&record)?)?;
            api.patch_folder(folder, &json!({"paused":true,"label":record.pause_label}))?;
            record
        };
        if record.generation != generation {
            // Only the owned launcher proves an already-paused startup. A
            // historical event from another process generation is never reused.
            ensure!(
                recovery && generation.paused_at_start.iter().any(|id| id == folder),
                "new generation has no startup fence for held folder"
            );
            record.generation = generation.clone();
            record.paused_event = None;
        } else if record.phase == Phase::Pausing {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                same_generation(&generation, &live_generation(home)?)?;
                let history = api.folder_pause_events(record.cursor)?;
                if let Some(event) = events(&history)?.iter().find(|e| {
                    e["id"].as_u64().unwrap() > record.cursor
                        && e["data"]["id"] == folder
                        && e["data"]["label"] == record.pause_label
                }) {
                    record.paused_event = event["id"].as_u64();
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "folder drain acknowledgement missing; durable hold retained for recovery"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        ensure!(
            api.folder(folder)? == paused(&record),
            "folder changed during drain"
        );
        same_generation(&generation, &live_generation(home)?)?;
        hold.validate_held(hold.token())?;
        record.phase = Phase::Quiescent;
        private::write(&path, &serde_json::to_vec(&record)?)?;
        Ok(Self {
            hold,
            daemon,
            reference,
            gate,
            path,
            record,
        })
    }
    pub fn root(&self) -> &Path {
        self.hold.root()
    }
    /// Call immediately before publication and after readback. It never creates
    /// authority or accepts an offline REST endpoint as a stop proof.
    pub fn validate(&self) -> Result<()> {
        self.hold.validate_held(self.hold.token())?;
        let home = self
            .daemon
            .config_file
            .parent()
            .context("managed home missing")?;
        same_generation(&self.record.generation, &live_generation(home)?)?;
        ensure!(
            self.record.phase == Phase::Quiescent
                && self.daemon.connect()?.folder(&self.record.folder)? == paused(&self.record),
            "quiescence scope changed"
        );
        let saved: Record = serde_json::from_slice(&private::read(&self.path)?)?;
        ensure!(
            serde_json::to_vec(&saved)? == serde_json::to_vec(&self.record)?,
            "transport hold record changed"
        );
        self.reference.load()?;
        Ok(())
    }
    /// The distributed owner calls this ONLY after both durable completions.
    /// Local/transport admission never opens automatically on timeout or Drop.
    pub fn release_and_resume(mut self) -> Result<()> {
        self.validate()?;
        self.record.phase = Phase::ResumeAuthorized;
        private::write(&self.path, &serde_json::to_vec(&self.record)?)?;
        let token = self.hold.token().clone();
        self.hold.release(&token)?;
        drop(self.gate);
        let _admission =
            crate::admission::configuration(&self.daemon, &self.record.folder, &self.record.root)?;
        let api = self.daemon.connect()?;
        ensure!(
            api.folder(&self.record.folder)? == paused(&self.record),
            "resume configuration changed"
        );
        api.patch_folder(
            &self.record.folder,
            &json!({"paused":false,"label":self.record.before["label"]}),
        )?;
        self.record.phase = Phase::Complete;
        private::write(&self.path, &serde_json::to_vec(&self.record)?)
    }
}

/// An ordinary destination has no transport publisher. Hold its local writers
/// and the managed configuration/launch gate so it cannot become enrolled while
/// the merge publishes. Other already-configured folders keep transferring.
pub struct OrdinaryHold {
    hold: Hold,
    _gate: File,
    daemon: DaemonIdentity,
    generation: Generation,
    config: Value,
}
impl OrdinaryHold {
    pub fn acquire(daemon: DaemonIdentity, hold: Hold) -> Result<Self> {
        hold.validate_held(hold.token())?;
        let home = daemon
            .config_file
            .parent()
            .context("managed home missing")?;
        let reference = existing(home)?;
        let gate = lock_file(&reference.directory.join("gate.lock"), false, false)?;
        let generation = live_generation(home)?;
        let config = daemon.connect()?.config()?;
        for folder in config["folders"]
            .as_array()
            .context("folder inventory missing")?
        {
            let root = Path::new(folder["path"].as_str().context("folder path missing")?)
                .canonicalize()?;
            ensure!(
                !root.starts_with(hold.root()) && !hold.root().starts_with(root),
                "ordinary first Sync destination overlaps a transport folder"
            );
        }
        Ok(Self {
            hold,
            _gate: gate,
            daemon,
            generation,
            config,
        })
    }
    pub fn configuration(&self) -> &Value {
        &self.config
    }
    pub fn runtime_started(&self) -> Option<&str> {
        self.generation.rest_start_time.as_deref()
    }
    pub fn validate(&self) -> Result<()> {
        self.hold.validate_held(self.hold.token())?;
        same_generation(
            &self.generation,
            &live_generation(
                self.daemon
                    .config_file
                    .parent()
                    .context("managed home missing")?,
            )?,
        )?;
        ensure!(
            self.daemon.connect()?.config()? == self.config,
            "ordinary destination runtime configuration changed"
        );
        Ok(())
    }
    pub fn release(self) -> Result<()> {
        let token = self.hold.token().clone();
        self.hold.release(&token)
    }
}
