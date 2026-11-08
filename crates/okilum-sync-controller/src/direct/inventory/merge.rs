//! Explicit first-merge transactions over the existing approved-peer channel.
//! Staging/preservation do not authorize publication. Both complete revisions
//! are checked under local/transport ownership before either canonical write.
use super::*;
use crate::managed_launch::{OrdinaryHold, ScopedHold};
use okilum_write_authority::{Authority, Hold};
use std::time::{Duration, Instant};
mod storage;
use storage::{Binding as JournalBinding, Metadata, Phase, Store};

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::direct) enum Request {
    Stage {
        operation: Uuid,
        header: Box<Header>,
        metadata_hash: String,
        metadata_size: usize,
        recover: bool,
    },
    Chunk {
        operation: Uuid,
        blob: Option<String>,
        offset: usize,
        hex: String,
    },
    Seal {
        operation: Uuid,
        metadata_hash: String,
    },
    Freeze {
        operation: Uuid,
        metadata_hash: String,
        recover: bool,
    },
    Publish {
        operation: Uuid,
        metadata_hash: String,
    },
    Finish {
        operation: Uuid,
        metadata_hash: String,
    },
    Cancel {
        operation: Uuid,
        metadata_hash: String,
    },
    Status {
        operation: Uuid,
    },
}
impl Request {
    fn operation(&self) -> Uuid {
        match self {
            Self::Stage { operation, .. }
            | Self::Chunk { operation, .. }
            | Self::Seal { operation, .. }
            | Self::Freeze { operation, .. }
            | Self::Publish { operation, .. }
            | Self::Finish { operation, .. }
            | Self::Cancel { operation, .. }
            | Self::Status { operation } => *operation,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::direct) enum Response {
    Staged {
        operation: Uuid,
    },
    Chunk {
        operation: Uuid,
        offset: usize,
    },
    Prepared {
        operation: Uuid,
        metadata_hash: String,
    },
    Frozen {
        operation: Uuid,
        metadata_hash: String,
    },
    Published {
        operation: Uuid,
        metadata_hash: String,
    },
    Complete {
        operation: Uuid,
        metadata_hash: String,
    },
    Cancelled {
        operation: Uuid,
    },
    Status {
        operation: Uuid,
        metadata_hash: String,
        phase: Phase,
    },
    Absent {
        operation: Uuid,
    },
}
pub(in crate::direct) struct Session {
    store: Store,
    scope: Option<ScopedHold>,
}

/// Disposable integration-test injection. Release/default builds expose no
/// hook setter and perform no environment-driven interruption.
#[cfg(feature = "first-sync-fixture")]
pub mod fixture {
    use super::*;
    type Hook = Box<dyn Fn(&str, Uuid) + Send + Sync>;
    static HOOK: std::sync::OnceLock<Hook> = std::sync::OnceLock::new();
    pub fn install(hook: impl Fn(&str, Uuid) + Send + Sync + 'static) {
        assert!(
            HOOK.set(Box::new(hook)).is_ok(),
            "fixture hook already installed"
        );
    }
    pub(super) fn checkpoint(name: &str, operation: Uuid) {
        eprintln!("FIRST_SYNC_FIXTURE_CHECKPOINT name={name} operation={operation}");
        if let Some(hook) = HOOK.get() {
            hook(name, operation);
        }
    }
}
pub(in crate::direct) fn checkpoint(name: &str, operation: Uuid) {
    #[cfg(feature = "first-sync-fixture")]
    fixture::checkpoint(name, operation);
    #[cfg(not(feature = "first-sync-fixture"))]
    let _ = (name, operation);
}

fn drain(authority: &Authority, operation: Uuid, recover: bool) -> Result<Hold> {
    let mut hold = if recover {
        let observed = authority.snapshot()?;
        ensure!(
            observed.operation_id.as_deref() == Some(operation.to_string().as_str()),
            "another operation owns this vault"
        );
        authority.recover(&observed)?
    } else {
        authority.begin_drain(&operation.to_string())?
    };
    let token = hold.token().clone();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !hold.try_hold(&token)? {
        ensure!(
            Instant::now() < deadline,
            "local writers still draining; recovery retained"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(hold)
}
impl Controller {
    pub(in crate::direct) fn validate_first_sync_completion(
        &self,
        receipt: &CompletedMerge,
        check_files: bool,
    ) -> Result<()> {
        let store = Store::open(&self.first_sync_state()?, receipt.operation)?;
        ensure!(
            store.binding.phase == Phase::PeerComplete
                && !store.binding.remote_side
                && store.binding.peer == receipt.peer
                && store.binding.source.folder_id == receipt.vault
                && store.binding.source.path == receipt.path
                && store.binding.source.daemon == receipt.daemon,
            "non-empty attachment has no completed two-computer merge"
        );
        ensure!(
            receipt.authority.open()?.root() == receipt.path,
            "first Sync destination authority changed"
        );
        if check_files {
            let root = tree::open_root(&receipt.path)?;
            let current = Tree::collect(&root, &receipt.path, true)?;
            ensure!(
                current.files == *store.plan()?.merged_files(),
                "merged destination changed before Sync handoff"
            );
        }
        Ok(())
    }
    fn first_sync_state(&self) -> Result<PathBuf> {
        let path = self.shared.state.join("first-sync");
        private::directory(&path)?;
        Ok(path)
    }
    fn first_sync_call(&self, peer: Uuid, request: Request) -> Result<Response> {
        let paired = self.inspection_peer(peer)?;
        match self.call(
            paired.endpoint,
            peer,
            &paired.key,
            &paired.public,
            super::super::Operation::FirstSync {
                request: Box::new(request),
            },
        )? {
            Reply::FirstSync { response } => Ok(response),
            _ => anyhow::bail!(
                "first Sync stopped or needs recovery; no new operation is authorized"
            ),
        }
    }
    pub(in crate::direct) fn first_sync_request(
        &self,
        peer: Uuid,
        signer: &[u8],
        request: Request,
    ) -> Result<Response> {
        self.check()?;
        ensure!(
            self.inspection_peer(peer)?.public == signer,
            "first Sync computer identity differs"
        );
        let operation = request.operation();
        ensure!(!operation.is_nil(), "first Sync operation identity missing");
        let state = self.first_sync_state()?;
        if matches!(&request, Request::Status { .. })
            && !state.join(operation.to_string()).try_exists()?
        {
            return Ok(Response::Absent { operation });
        }
        if matches!(&request, Request::Cancel { .. })
            && !state.join(operation.to_string()).try_exists()?
        {
            // A remote hold cannot precede its durable operation directory.
            return Ok(Response::Cancelled { operation });
        }
        if let Request::Stage {
            header,
            metadata_hash,
            metadata_size,
            recover,
            ..
        } = request
        {
            // Serialize first creation with all competing Stage requests. An
            // authenticated replay cannot replace a journal before insertion.
            let mut sessions = self.shared.first_sync.lock().unwrap();
            let header = *header;
            let (source, seed) = self.inspection_authority(peer, signer, &header.binding.vault)?;
            if state.join(operation.to_string()).try_exists()? {
                let saved = Store::open(&state, operation)?;
                ensure!(
                    saved.binding.peer == peer
                        && saved.binding.remote_side
                        && saved.binding.preview == header
                        && existing::same_binding(&saved.binding.source, &source)
                        && saved.binding.metadata_hash == metadata_hash
                        && saved.binding.metadata_size == metadata_size
                        && !matches!(saved.binding.phase, Phase::Complete | Phase::Cancelled),
                    "replayed first Sync staging differs"
                );
                return Ok(Response::Staged { operation });
            }
            let original = {
                let mut cache = self.shared.inventory.lock().unwrap();
                cache.prune();
                cache
                    .entries
                    .get(&(peer, header.binding.vault.clone(), header.revision.clone()))
                    .cloned()
            };
            if let Some(original) = original {
                ensure!(
                    header == original.local.header
                        && existing::same_binding(&source, &original.source),
                    "preview identity changed"
                );
                self.validate_local(&original.local, &source.daemon, true)?;
            } else {
                ensure!(
                    recover && header.auth == header_auth(&self.shared.inventory_key, &header)?,
                    "preview expired or substituted; refresh before first Sync"
                );
                let root = authorized_root(&source)?;
                ensure!(
                    header.binding.pair == peer
                        && header.binding.remote
                        && header.binding.owner == wire::fingerprint(&wire::public(&seed)?)
                        && header.binding.reader == wire::fingerprint(signer)
                        && header.binding.device == source.daemon.device_id
                        && header.binding.certificate == source.daemon.certificate_hash
                        && header.binding.root_binding
                            == opaque_root(&self.shared.inventory_key, peer, &source.path, &root)?,
                    "recovery preview identity or authorized root differs"
                );
                let current = Tree::collect(&root, &source.path, true)?;
                ensure!(
                    digest(&(&current.stamps, manifest(&current)))? == header.revision
                        && current.policy == header.binding.policy,
                    "source changed before preservation; cancel and review a new preview"
                );
            }
            let mut unfinished = 0;
            for entry in std::fs::read_dir(&state)? {
                let id = Uuid::parse_str(
                    entry?
                        .file_name()
                        .to_str()
                        .context("invalid first Sync journal name")?,
                )?;
                let saved = Store::open(&state, id)?;
                if !matches!(saved.binding.phase, Phase::Complete | Phase::Cancelled) {
                    unfinished += 1;
                }
            }
            ensure!(
                unfinished < 4,
                "finish or cancel pending first Sync operations before starting another"
            );
            let authority = Authority::for_root(&source.path)?.durable_binding()?;
            let journal = JournalBinding {
                schema: 1,
                operation,
                peer,
                source,
                authority,
                remote_side: true,
                revision: header.revision.clone(),
                metadata_hash,
                metadata_size,
                own_inventory: header.clone(),
                preview: header,
                phase: Phase::Staging,
            };
            let store = Store::create(&state, journal)?;
            sessions.insert(
                operation,
                Arc::new(Mutex::new(Session { store, scope: None })),
            );
            checkpoint("source-staged", operation);
            return Ok(Response::Staged { operation });
        }
        let session = {
            let mut sessions = self.shared.first_sync.lock().unwrap();
            if let Some(session) = sessions.get(&operation) {
                session.clone()
            } else {
                let store = Store::open(&state, operation)?;
                ensure!(
                    store.binding.peer == peer && store.binding.remote_side,
                    "operation belongs to another computer"
                );
                let session = Arc::new(Mutex::new(Session { store, scope: None }));
                sessions.insert(operation, session.clone());
                session
            }
        };
        let mut session = session.lock().unwrap();
        let (authorized, _) =
            self.inspection_authority(peer, signer, &session.store.binding.source.folder_id)?;
        ensure!(
            session.store.binding.peer == peer
                && session.store.binding.remote_side
                && existing::same_binding(&session.store.binding.source, &authorized),
            "first Sync authorization or root changed"
        );
        let metadata_hash = session.store.binding.metadata_hash.clone();
        match request {
            Request::Stage { .. } => unreachable!(),
            Request::Chunk {
                blob, offset, hex, ..
            } => {
                ensure!(
                    hex.len() <= CHUNK_BYTES * 2,
                    "first Sync chunk exceeds bound"
                );
                let bytes = decode_chunk(&hex)?;
                let uploaded = session.store.chunk(blob.as_deref(), offset, &bytes)?;
                if blob.is_some() && offset == 0 && bytes.len() == CHUNK_BYTES {
                    checkpoint("source-partial-blob", operation);
                }
                Ok(Response::Chunk {
                    operation,
                    offset: uploaded,
                })
            }
            Request::Status { .. } => Ok(Response::Status {
                operation,
                metadata_hash,
                phase: session.store.binding.phase,
            }),
            Request::Seal {
                metadata_hash: expected,
                ..
            } => {
                ensure!(expected == metadata_hash, "first Sync consent differs");
                let meta = session.store.metadata()?;
                ensure!(
                    meta.remote == session.store.binding.preview
                        && meta.remote.binding.pair == peer
                        && meta.remote.binding.vault == authorized.folder_id
                        && meta.remote.revision == session.store.binding.revision
                        && meta.there.scope == digest(&meta.remote.binding)?
                        && meta.there.revision == meta.remote.revision,
                    "staged remote inventory differs from preview"
                );
                session.store.compatible(false)?;
                session.store.prepare()?;
                checkpoint("source-prepared", operation);
                Ok(Response::Prepared {
                    operation,
                    metadata_hash,
                })
            }
            Request::Freeze {
                metadata_hash: expected,
                recover,
                ..
            } => {
                ensure!(expected == metadata_hash, "first Sync consent differs");
                ensure!(
                    matches!(
                        session.store.binding.phase,
                        Phase::Prepared | Phase::Publishing | Phase::Published
                    ),
                    "first Sync is not prepared"
                );
                if session.scope.is_none() {
                    let authority = session.store.binding.authority.open()?;
                    let adopting = recover
                        && authority.snapshot()?.phase != okilum_write_authority::Phase::Open;
                    let hold = drain(&authority, operation, adopting)?;
                    let scope = ScopedHold::acquire(
                        authorized.daemon.clone(),
                        &authorized.folder_id,
                        hold,
                        adopting,
                    )?;
                    session.scope = Some(scope);
                }
                session.scope.as_ref().unwrap().validate()?;
                if !recover {
                    let scope = session.scope.as_ref().unwrap();
                    let binding = &session.store.binding.preview.binding;
                    ensure!(
                        scope.runtime_started() == Some(binding.started.as_str())
                            && digest(&scope.configuration_before_pause()?)? == binding.config,
                        "runtime changed since first Sync preview"
                    );
                }
                session.store.compatible(recover)?;
                checkpoint("source-frozen", operation);
                Ok(Response::Frozen {
                    operation,
                    metadata_hash,
                })
            }
            Request::Publish {
                metadata_hash: expected,
                ..
            } => {
                ensure!(expected == metadata_hash, "first Sync consent differs");
                let Session { store, scope } = &mut *session;
                let scope = scope
                    .as_ref()
                    .context("transport ownership missing; recover first Sync")?;
                store.publish(|| {
                    self.check()?;
                    scope.validate()
                })?;
                Ok(Response::Published {
                    operation,
                    metadata_hash,
                })
            }
            Request::Finish {
                metadata_hash: expected,
                ..
            } => {
                ensure!(expected == metadata_hash, "first Sync completion differs");
                ensure!(
                    matches!(
                        session.store.binding.phase,
                        Phase::Published | Phase::PeerComplete | Phase::Complete
                    ),
                    "first Sync has no completed publication"
                );
                if session.store.binding.phase != Phase::Complete {
                    session.store.verify_published()?;
                    session.store.binding.phase = Phase::PeerComplete;
                    session.store.save()?;
                    if let Some(scope) = session.scope.take() {
                        scope.release_and_resume()?;
                    } else {
                        ScopedHold::resume_committed(
                            &authorized.daemon,
                            &authorized.folder_id,
                            &operation.to_string(),
                        )?;
                    }
                    session.store.binding.phase = Phase::Complete;
                    session.store.save()?;
                }
                checkpoint("source-complete", operation);
                // Terminal journals remain durable; their in-memory manifests
                // need not accumulate for the rest of the application's life.
                self.shared.first_sync.lock().unwrap().remove(&operation);
                Ok(Response::Complete {
                    operation,
                    metadata_hash,
                })
            }
            Request::Cancel {
                metadata_hash: expected,
                ..
            } => {
                if expected == metadata_hash && session.store.binding.phase == Phase::Cancelled {
                    return Ok(Response::Cancelled { operation });
                }
                ensure!(
                    expected == metadata_hash
                        && matches!(
                            session.store.binding.phase,
                            Phase::Staging | Phase::Prepared
                        ),
                    "a published first Sync requires recovery"
                );
                if let Some(scope) = session.scope.take() {
                    scope.release_and_resume()?;
                } else {
                    let authority = session.store.binding.authority.open()?;
                    let observed = authority.snapshot()?;
                    if observed.phase != okilum_write_authority::Phase::Open {
                        let hold = drain(&authority, operation, true)?;
                        ScopedHold::cancel_unpublished(
                            &authorized.daemon,
                            &authorized.folder_id,
                            hold,
                        )?;
                    }
                }
                session.store.binding.phase = Phase::Cancelled;
                session.store.save()?;
                self.shared.first_sync.lock().unwrap().remove(&operation);
                Ok(Response::Cancelled { operation })
            }
        }
    }
}
fn decode_chunk(hex: &str) -> Result<Vec<u8>> {
    ensure!(hex.len().is_multiple_of(2), "invalid first Sync chunk");
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Ok(b - b'0'),
                b'a'..=b'f' => Ok(b - b'a' + 10),
                _ => anyhow::bail!("invalid first Sync byte"),
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}

/// Display-only durable operation identity. Recovery always reloads and checks
/// its journal, root, paired identity and both preimages before publication.
#[derive(Clone, Debug)]
pub struct PendingMerge {
    pub operation: Uuid,
    pub path: PathBuf,
    pub vault: String,
    pub peer: Uuid,
    pub remote_side: bool,
}

pub(in crate::direct) struct CompletedMerge {
    pub operation: Uuid,
    pub peer: Uuid,
    pub vault: String,
    pub path: PathBuf,
    pub daemon: daemon::DaemonIdentity,
    pub authority: okilum_write_authority::DurableBinding,
}

struct Apply {
    controller: Controller,
    preview: Preview,
}
impl merge_preview::Backend for Apply {
    fn apply_if_unchanged(
        &mut self,
        here: &merge_preview::Snapshot,
        there: &merge_preview::Snapshot,
        merged: &BTreeMap<String, Vec<u8>>,
    ) -> std::result::Result<(), merge_preview::Error> {
        ensure_plan(&self.preview, here, there, merged)?;
        self.controller
            .begin_first_sync(&self.preview)
            .map_err(|error| merge_preview::Error::Backend(format!("{error:#}")))
    }
}
fn ensure_plan(
    preview: &Preview,
    here: &merge_preview::Snapshot,
    there: &merge_preview::Snapshot,
    merged: &BTreeMap<String, Vec<u8>>,
) -> std::result::Result<(), merge_preview::Error> {
    if preview.plan.originals() != (here, there) || preview.plan.merged_files() != merged {
        return Err(merge_preview::Error::Stale);
    }
    Ok(())
}
impl Controller {
    /// Constructing an adapter never acquires a hold or writes a note. The
    /// existing Plan/Consent boundary invokes it only after explicit confirmation.
    pub fn first_sync_backend(&self, preview: Preview) -> Box<dyn merge_preview::Backend> {
        Box::new(Apply {
            controller: self.clone(),
            preview,
        })
    }
    fn ordinary_first_sync_destination(&self, path: &Path) -> Result<()> {
        let inventory = daemon::discover(&self.configuration_paths()?);
        let folders = inventory.folders()?;
        ensure!(
            inventory.candidates.len() == 1,
            "choose one managed Sync connection first"
        );
        for (_, root) in folders {
            ensure!(
                !path.starts_with(&root) && !root.starts_with(path),
                "first Sync destination overlaps an enrolled folder"
            );
        }
        Ok(())
    }
    fn begin_first_sync(&self, preview: &Preview) -> Result<()> {
        let _serial = self.shared.transfer_lock.lock().unwrap();
        self.check()?;
        self.validate_remote_preview(preview)?;
        self.ordinary_first_sync_destination(&preview.local.path)?;
        let operation = Uuid::new_v4();
        let meta = Metadata::from_preview(operation, preview)?;
        let metadata = meta.encoded()?;
        let metadata_hash = wire::fingerprint(&metadata);
        let state = self.first_sync_state()?;
        let configs = self.configuration_paths()?;
        ensure!(configs.len() == 1, "first Sync runtime changed");
        let daemon = managed(&configs[0])?;
        let authority = Authority::for_root(&preview.local.path)?.durable_binding()?;
        let source = ExistingVault {
            daemon: daemon.clone(),
            version: daemon::CLIENT_VERSION.into(),
            folder_id: preview.remote.binding.vault.clone(),
            path: preview.local.path.clone(),
            label: "First Sync destination".into(),
            paused: true,
            folder_type: "ordinary".into(),
            peers: vec![],
            root_identity: Some(tree::identity(&preview.local.root)?),
            stable_root_identity: Some(tree::stable_identity(&preview.local.root)?),
        };
        let mut store = Store::create(
            &state,
            JournalBinding {
                schema: 1,
                operation,
                peer: preview.peer,
                source,
                authority,
                remote_side: false,
                revision: preview.local.header.revision.clone(),
                metadata_hash: metadata_hash.clone(),
                metadata_size: metadata.len(),
                preview: preview.remote.clone(),
                own_inventory: preview.local.header.clone(),
                phase: Phase::Staging,
            },
        )?;
        store.put(None, &metadata)?;
        let (here, there) = preview.plan.originals();
        let blobs: BTreeMap<_, _> = here
            .files
            .values()
            .chain(there.files.values())
            .map(|bytes| (wire::fingerprint(bytes), bytes))
            .collect();
        for (hash, bytes) in &blobs {
            store.put(Some(hash), bytes)?;
        }
        store.prepare()?;
        checkpoint("destination-prepared", operation);
        let response = self.first_sync_call(
            preview.peer,
            Request::Stage {
                operation,
                header: Box::new(preview.remote.clone()),
                metadata_hash: metadata_hash.clone(),
                metadata_size: metadata.len(),
                recover: false,
            },
        )?;
        ensure!(
            matches!(response,Response::Staged{operation:id} if id==operation),
            "first Sync staging acknowledgement differs"
        );
        self.upload_first_sync(preview.peer, operation, None, &metadata)?;
        for (hash, bytes) in blobs {
            self.upload_first_sync(preview.peer, operation, Some(hash), bytes)?;
        }
        let response = self.first_sync_call(
            preview.peer,
            Request::Seal {
                operation,
                metadata_hash: metadata_hash.clone(),
            },
        )?;
        ensure!(
            matches!(response,Response::Prepared{operation:id,metadata_hash:h} if id==operation && h==metadata_hash),
            "remote first Sync preimages not acknowledged"
        );
        self.continue_first_sync(&mut store, false)
    }
    fn upload_first_sync(
        &self,
        peer: Uuid,
        operation: Uuid,
        blob: Option<String>,
        bytes: &[u8],
    ) -> Result<()> {
        let chunks = bytes.chunks(CHUNK_BYTES);
        if bytes.is_empty() {
            let response = self.first_sync_call(
                peer,
                Request::Chunk {
                    operation,
                    blob: blob.clone(),
                    offset: 0,
                    hex: String::new(),
                },
            )?;
            ensure!(
                matches!(response,Response::Chunk{operation:id,offset:0} if id==operation),
                "empty first Sync file acknowledgement differs"
            );
        }
        for (index, chunk) in chunks.enumerate() {
            let offset = index * CHUNK_BYTES;
            let response = self.first_sync_call(
                peer,
                Request::Chunk {
                    operation,
                    blob: blob.clone(),
                    offset,
                    hex: wire::hex(chunk),
                },
            )?;
            ensure!(
                matches!(response,Response::Chunk{operation:id,offset:end} if id==operation && end>=offset+chunk.len() && end<=bytes.len()),
                "first Sync upload acknowledgement differs"
            );
        }
        Ok(())
    }
    fn continue_first_sync(&self, store: &mut Store, recover: bool) -> Result<()> {
        let operation = store.binding.operation;
        let peer = store.binding.peer;
        let hash = store.binding.metadata_hash.clone();
        self.inspection_peer(peer)?;
        ensure!(
            !store.binding.remote_side
                && !matches!(store.binding.phase, Phase::Complete | Phase::Cancelled),
            "first Sync operation is terminal"
        );
        if recover {
            if store.binding.phase == Phase::Staging {
                store.plan()?;
                store.prepare()?;
            }
            let remote = self.restore_remote_preservation(store)?;
            if matches!(remote, Phase::PeerComplete | Phase::Complete) {
                ensure!(
                    matches!(store.binding.phase, Phase::Published | Phase::PeerComplete),
                    "remote completion has no local publication receipt"
                );
                if store.binding.phase == Phase::Published {
                    store.verify_published()?;
                    store.binding.phase = Phase::PeerComplete;
                    store.save()?;
                }
            }
        }
        let authority = store.binding.authority.open()?;
        let observed = authority.snapshot()?;
        let hold = if store.binding.phase == Phase::PeerComplete
            && observed.phase == okilum_write_authority::Phase::Open
        {
            None
        } else {
            Some(OrdinaryHold::acquire(
                store.binding.source.daemon.clone(),
                drain(
                    &authority,
                    operation,
                    recover && observed.phase != okilum_write_authority::Phase::Open,
                )?,
            )?)
        };
        if store.binding.phase != Phase::PeerComplete {
            self.ordinary_first_sync_destination(&store.binding.source.path)?;
            let response = self.first_sync_call(
                peer,
                Request::Freeze {
                    operation,
                    metadata_hash: hash.clone(),
                    recover,
                },
            )?;
            ensure!(
                matches!(response,Response::Frozen{operation:id,metadata_hash:h} if id==operation && h==hash),
                "first Sync transport ownership not acknowledged"
            );
            store.compatible(recover)?;
            hold.as_ref().unwrap().validate()?;
            if !recover {
                let owner = hold.as_ref().unwrap();
                let binding = &store.binding.own_inventory.binding;
                ensure!(
                    owner.runtime_started() == Some(binding.started.as_str())
                        && digest(owner.configuration())? == binding.config,
                    "local runtime changed since first Sync preview"
                );
            }
            let response = self.first_sync_call(
                peer,
                Request::Publish {
                    operation,
                    metadata_hash: hash.clone(),
                },
            )?;
            ensure!(
                matches!(response,Response::Published{operation:id,metadata_hash:h} if id==operation && h==hash),
                "remote first Sync completion not acknowledged"
            );
            let owner = hold.as_ref().unwrap();
            store.publish(|| {
                self.check()?;
                owner.validate()
            })?;
            // This is the durable acknowledgement of BOTH exact readbacks.
            store.binding.phase = Phase::PeerComplete;
            store.save()?;
        }
        let response = self.first_sync_call(
            peer,
            Request::Finish {
                operation,
                metadata_hash: hash.clone(),
            },
        )?;
        ensure!(
            matches!(response,Response::Complete{operation:id,metadata_hash:h} if id==operation && h==hash),
            "first Sync handoff acknowledgement missing"
        );
        if let Some(hold) = hold {
            hold.release()?;
        }
        checkpoint("destination-released", operation);
        let receipt = CompletedMerge {
            operation,
            peer,
            vault: store.binding.source.folder_id.clone(),
            path: store.binding.source.path.clone(),
            daemon: store.binding.source.daemon.clone(),
            authority: store.binding.authority.clone(),
        };
        self.attach_completed_first_sync(&receipt)?;
        store.binding.phase = Phase::Complete;
        store.save()
    }
    fn restore_remote_preservation(&self, store: &Store) -> Result<Phase> {
        let operation = store.binding.operation;
        let peer = store.binding.peer;
        let hash = &store.binding.metadata_hash;
        let remote = self.first_sync_call(peer, Request::Status { operation })?;
        let phase = match remote {
            Response::Absent { operation: id } if id == operation => {
                let response = self.first_sync_call(
                    peer,
                    Request::Stage {
                        operation,
                        header: Box::new(store.binding.preview.clone()),
                        metadata_hash: hash.clone(),
                        metadata_size: store.binding.metadata_size,
                        recover: true,
                    },
                )?;
                ensure!(
                    matches!(response,Response::Staged{operation:id} if id==operation),
                    "recovery staging not acknowledged"
                );
                Phase::Staging
            }
            Response::Status {
                operation: id,
                metadata_hash: expected,
                phase,
            } if id == operation && expected == *hash => phase,
            _ => anyhow::bail!("first Sync recovery identity differs"),
        };
        ensure!(
            phase != Phase::Cancelled,
            "source cancelled this unpublished merge; cancel locally before another preview"
        );
        if phase != Phase::Staging {
            return Ok(phase);
        }
        self.upload_first_sync(peer, operation, None, &store.metadata()?.encoded()?)?;
        let plan = store.plan()?;
        let (here, there) = plan.originals();
        let blobs: BTreeMap<_, _> = here
            .files
            .values()
            .chain(there.files.values())
            .map(|bytes| (wire::fingerprint(bytes), bytes))
            .collect();
        for (hash, bytes) in blobs {
            self.upload_first_sync(peer, operation, Some(hash), bytes)?;
        }
        let response = self.first_sync_call(
            peer,
            Request::Seal {
                operation,
                metadata_hash: hash.clone(),
            },
        )?;
        ensure!(
            matches!(response,Response::Prepared{operation:id,metadata_hash:expected} if id==operation && expected==*hash),
            "recovery preimages not acknowledged"
        );
        Ok(Phase::Prepared)
    }
    /// Interrupted work is never resumed by ordinary refresh/startup. The UI
    /// presents this inventory and asks for an explicit recovery/cancellation.
    pub fn pending_first_sync(&self) -> Result<Vec<PendingMerge>> {
        let state = self.first_sync_state()?;
        let mut result = vec![];
        for entry in std::fs::read_dir(&state)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                anyhow::bail!("invalid first Sync journal name")
            };
            let operation = Uuid::parse_str(&name)?;
            let store = Store::open(&state, operation)?;
            if !matches!(store.binding.phase, Phase::Complete | Phase::Cancelled) {
                result.push(PendingMerge {
                    operation,
                    path: store.binding.source.path,
                    vault: store.binding.source.folder_id,
                    peer: store.binding.peer,
                    remote_side: store.binding.remote_side,
                });
            }
        }
        Ok(result)
    }
    pub fn recover_first_sync(&self, operation: Uuid) -> Result<()> {
        let _serial = self.shared.transfer_lock.lock().unwrap();
        let mut store = Store::open(&self.first_sync_state()?, operation)?;
        self.continue_first_sync(&mut store, true)
    }
    pub fn cancel_first_sync(&self, operation: Uuid) -> Result<()> {
        let _serial = self.shared.transfer_lock.lock().unwrap();
        let mut store = Store::open(&self.first_sync_state()?, operation)?;
        ensure!(
            !store.binding.remote_side
                && matches!(store.binding.phase, Phase::Staging | Phase::Prepared),
            "first Sync already published files; recover it to keep both versions"
        );
        let response = self.first_sync_call(
            store.binding.peer,
            Request::Cancel {
                operation,
                metadata_hash: store.binding.metadata_hash.clone(),
            },
        )?;
        ensure!(
            matches!(response,Response::Cancelled{operation:id} if id==operation),
            "source cancellation was not acknowledged"
        );
        let authority = store.binding.authority.open()?;
        let observed = authority.snapshot()?;
        if observed.phase != okilum_write_authority::Phase::Open {
            let hold = drain(&authority, operation, true)?;
            let token = hold.token().clone();
            hold.release(&token)?;
        }
        store.binding.phase = Phase::Cancelled;
        store.save()
    }
}
