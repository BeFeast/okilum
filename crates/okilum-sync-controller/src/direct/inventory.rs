//! Read-only, revision-bound inspection. This is NOT an exclusive mutation lease.
//! No enrollment, share grant, transport reconciliation or canonical publication.
use super::*;
use crate::{daemon, merge_preview};
use std::{fs::File, time::Instant};

mod tree;
use tree::Tree;
pub mod merge;

/// Bounds are refusals, never successful truncation. Snapshots are peer-isolated.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_FILES: usize = 16_384;
pub const PAGE_FILES: usize = 4;
/// Hex encoding keeps even worst-case nested JSON below the existing wire bound.
pub const CHUNK_BYTES: usize = 4096;
const LIFETIME_SECONDS: u64 = 120;
const MAX_SNAPSHOTS: usize = 4;
const MAX_PEER_SNAPSHOTS: usize = 2;
const MAX_ACTIVE: usize = 4;
const PAUSED: &str = "paused vault cannot be inspected";
const NOT_PROMOTED: &str = "inspection requires a promoted sendreceive vault";

pub(super) fn persistent_key(state: &Path) -> Result<[u8; 32]> {
    let path = state.join("inventory-key");
    if !path.try_exists()? {
        let operations = state.join("first-sync");
        ensure!(
            !operations.try_exists()?
                || std::fs::read_dir(operations)?.next().transpose()?.is_none(),
            "first Sync identity key missing; recovery required"
        );
        private::write(&path, &wire::random::<32>()?)?;
    }
    private::read(&path)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("first Sync identity key is invalid"))
}

pub(super) fn public_refusal(error: &anyhow::Error) -> Option<&'static str> {
    let message = error.to_string();
    [PAUSED, NOT_PROMOTED]
        .into_iter()
        .find(|public| *public == message)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    pair: Uuid,
    owner: String,
    reader: String,
    vault: String,
    remote: bool,
    root_binding: String,
    device: String,
    certificate: String,
    config: String,
    generation: Uuid,
    started: String,
    policy: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub size: usize,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Header {
    auth: String,
    id: Uuid,
    binding: Binding,
    revision: String,
    manifest: String,
    files: usize,
    bytes: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    Open {
        vault: String,
    },
    Page {
        header: Header,
        offset: usize,
    },
    Content {
        header: Header,
        index: usize,
        offset: usize,
    },
    Validate {
        header: Header,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Response {
    Open {
        header: Header,
    },
    Page {
        header: Header,
        offset: usize,
        entries: Vec<Entry>,
    },
    Content {
        header: Header,
        index: usize,
        offset: usize,
        sha256: String,
        hex: String,
    },
    Valid {
        header: Header,
    },
}
struct Local {
    path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
    tree: Tree,
    root: Arc<File>,
}
impl Local {
    fn matches(&self, fresh: &Tree, content: bool) -> bool {
        fresh.stamps == self.tree.stamps
            && fresh.policy == self.header.binding.policy
            && (!content || fresh.files == self.tree.files)
    }
}
struct Stored {
    local: Local,
    source: ExistingVault,
    expires: Instant,
}
type CacheKey = (Uuid, String, String);
#[derive(Default)]
pub(super) struct Cache {
    entries: BTreeMap<CacheKey, Arc<Stored>>,
    busy: BTreeMap<Uuid, Option<String>>,
}
impl Cache {
    pub(super) fn prune(&mut self) {
        self.entries
            .retain(|_, stored| stored.expires > Instant::now());
    }
    fn admit(&mut self, peer: Uuid, vault: Option<&str>) -> Result<()> {
        self.prune();
        ensure!(
            !self.busy.contains_key(&peer),
            "inspection already in progress for computer"
        );
        ensure!(
            self.busy.len() < MAX_ACTIVE,
            "inspection concurrency limit reached"
        );
        if let Some(vault) = vault {
            let existing = self
                .entries
                .keys()
                .any(|(p, v, _)| *p == peer && v == vault);
            let reservations = self
                .busy
                .iter()
                .filter(|(p, v)| {
                    v.as_ref().is_some_and(|v| {
                        !self
                            .entries
                            .keys()
                            .any(|(owner, name, _)| owner == *p && name == v)
                    })
                })
                .count();
            ensure!(
                existing || self.entries.len() + reservations < MAX_SNAPSHOTS,
                "inventory cache full; wait for expiry"
            );
            ensure!(
                existing
                    || self.entries.keys().filter(|(p, _, _)| *p == peer).count()
                        < MAX_PEER_SNAPSHOTS,
                "computer inventory limit reached; wait for expiry"
            );
        }
        self.busy.insert(peer, vault.map(str::to_owned));
        Ok(())
    }
    fn insert(&mut self, mut stored: Stored) -> Header {
        let h = &stored.local.header;
        let key = (h.binding.pair, h.binding.vault.clone(), h.revision.clone());
        // Reuse unchanged capabilities; refresh never evicts another peer/vault.
        if let Some(current) = self.entries.get(&key) {
            if current.local.header.binding == h.binding && current.expires > Instant::now() {
                // Open already collected and fully validated this new snapshot.
                // Keep the capability stable, but install that fresh witness and
                // its admission deadline instead of returning an almost-expired
                // old allocation. Ordinary page/content/validate never renew it.
                stored.local.header = current.local.header.clone();
            }
        }
        self.entries
            .retain(|(p, v, _), _| *p != key.0 || *v != key.1);
        let header = stored.local.header.clone();
        self.entries.insert(key, Arc::new(stored));
        header
    }
}
struct Permit {
    shared: Arc<Shared>,
    peer: Uuid,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.shared
            .inventory
            .lock()
            .unwrap()
            .busy
            .remove(&self.peer);
    }
}
fn disjoint(root: &Path, state: &Path) -> Result<()> {
    let state = state.canonicalize()?;
    ensure!(
        !root.starts_with(&state) && !state.starts_with(root),
        "inspection directory overlaps operational state"
    );
    Ok(())
}
fn inspectable(folder: &serde_json::Value) -> Result<()> {
    let paused = folder["paused"]
        .as_bool()
        .context("vault pause state missing")?;
    let mode = folder["type"].as_str().context("vault mode missing")?;
    ensure!(!paused, PAUSED);
    ensure!(mode == "sendreceive", NOT_PROMOTED);
    Ok(())
}
fn authorized_root(source: &ExistingVault) -> Result<Arc<File>> {
    let root = Arc::new(tree::open_root(&source.path)?);
    ensure!(
        Some(tree::identity(&root)?) == source.root_identity,
        "authorized root changed before collection"
    );
    Ok(root)
}
fn opaque_root(key: &[u8; 32], pair: Uuid, path: &Path, root: &File) -> Result<String> {
    let bytes = serde_json::to_vec(&(pair, path, tree::identity(root)?))?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    Ok(wire::hex(ring::hmac::sign(&key, &bytes).as_ref()))
}
/// Complete inspection with the ordinary Plan classification. The Plan remains
/// an observation: callers must revalidate, and #2506 must acquire writer fencing
/// before using any future apply endpoint. This type exposes no apply authority.
pub struct Preview {
    plan: merge_preview::Plan,
    local: Local,
    remote: Header,
    peer: Uuid,
}
impl Preview {
    pub fn plan(&self) -> &merge_preview::Plan {
        &self.plan
    }
    pub fn entries(&self) -> &[merge_preview::Entry] {
        self.plan.entries()
    }
}
fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(wire::fingerprint(&serde_json::to_vec(value)?))
}
fn manifest(tree: &Tree) -> Vec<Entry> {
    tree.files
        .iter()
        .map(|(path, bytes)| Entry {
            path: path.clone(),
            size: bytes.len(),
            sha256: wire::fingerprint(bytes),
        })
        .collect()
}
fn header_auth(key: &[u8; 32], header: &Header) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        header.id,
        &header.binding,
        &header.revision,
        &header.manifest,
        header.files,
        header.bytes,
    ))?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    Ok(wire::hex(ring::hmac::sign(&key, &bytes).as_ref()))
}
fn local(header_binding: Binding, path: PathBuf, root: Arc<File>, key: &[u8; 32]) -> Result<Local> {
    let tree = Tree::collect(&root, &path, true)?;
    ensure!(
        tree.policy == header_binding.policy,
        "ignore policy changed during inspection"
    );
    let entries = manifest(&tree);
    let revision = digest(&(&tree.stamps, &entries))?;
    let mut header = Header {
        auth: String::new(),
        id: Uuid::new_v4(),
        binding: header_binding,
        revision,
        manifest: digest(&entries)?,
        files: entries.len(),
        bytes: entries.iter().map(|e| e.size).sum(),
    };
    header.auth = header_auth(key, &header)?;
    Ok(Local {
        path,
        header,
        entries,
        tree,
        root,
    })
}
impl Controller {
    fn inspection_peer(&self, id: Uuid) -> Result<Paired> {
        self.check()?;
        self.shared
            .db
            .lock()
            .unwrap()
            .peers
            .get(&id)
            .cloned()
            .context("computer not approved")
    }
    fn inspection_call(&self, id: Uuid, request: Request) -> Result<Response> {
        let peer = self.inspection_peer(id)?;
        match self.call(
            peer.endpoint,
            id,
            &peer.key,
            &peer.public,
            Operation::Inspect {
                request: Box::new(request),
            },
        )? {
            Reply::Inspection { response } => Ok(response),
            Reply::Error { message } if message == PAUSED || message == NOT_PROMOTED => anyhow::bail!(message),
            _ => anyhow::bail!(
                "inspection unavailable or stale; check authorization, unpaused promoted vault, runtime and inventory limits, then refresh"
            ),
        }
    }
    /// Inspect a remote authorized vault against an existing non-empty ordinary
    /// local directory. Never enrolls that directory or changes either daemon.
    /// Blocking IO: run outside the UI thread, like the other direct operations.
    pub fn preview_remote_vault(&self, peer: Uuid, vault: &str, here: &Path) -> Result<Preview> {
        self.check()?;
        let canonical = here.canonicalize()?;
        disjoint(&canonical, &self.shared.state)?;
        let paired = self.inspection_peer(peer)?;
        let root = Arc::new(tree::open_root(here)?);
        let configs = self.configuration_paths()?;
        ensure!(
            configs.len() == 1,
            "inspection requires one selected managed runtime"
        );
        let daemon = managed(&configs[0])?;
        // Verify the checksum-pinned executable once at admission, not per page.
        let prepared = daemon
            .config_file
            .parent()
            .and_then(Path::parent)
            .context("managed state missing")?;
        disjoint(&canonical, prepared)?;
        ensure!(
            daemon::managed_identity(prepared)? == daemon,
            "managed runtime changed"
        );
        let (owner, reader) = {
            let db = self.shared.db.lock().unwrap();
            (
                wire::fingerprint(&wire::public(&db.seed)?),
                wire::fingerprint(&paired.public),
            )
        };
        let binding = self.inspection_binding(
            peer,
            &owner,
            &reader,
            "local-inspection",
            false,
            &canonical,
            &root,
            &daemon,
        )?;
        let here = local(binding, canonical, root, &self.shared.inventory_key)?;
        ensure!(
            !here.entries.is_empty(),
            "inspection requires a non-empty local directory"
        );
        let Response::Open { header } = self.inspection_call(
            peer,
            Request::Open {
                vault: vault.into(),
            },
        )?
        else {
            anyhow::bail!("incomplete inventory opening");
        };
        ensure!(
            header.binding.pair == peer
                && header.binding.owner == reader
                && header.binding.reader == owner
                && header.binding.vault == vault
                && header.binding.remote
                && !header.id.is_nil()
                && header.files > 0
                && header.files <= MAX_FILES
                && header.bytes <= MAX_BYTES,
            "inventory identity or bounds differ"
        );
        let mut collector = Collector::new(header.clone());
        while collector.entries.len() < header.files {
            let offset = collector.entries.len();
            let response = self.inspection_call(
                peer,
                Request::Page {
                    header: header.clone(),
                    offset,
                },
            )?;
            collector.page(response)?;
        }
        let entries = collector.finish()?;
        let mut files = BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let bytes = self.inspection_content(peer, &header, index, entry)?;
            files.insert(entry.path.clone(), bytes);
        }
        self.validate_remote(peer, &header)?;
        self.validate_local(&here, &daemon, true)?;
        let plan = merge_preview::Plan::preview(
            snapshot(&here)?,
            merge_preview::Snapshot {
                scope: digest(&header.binding)?,
                revision: header.revision.clone(),
                files,
            },
        )
        .map_err(|e| anyhow::anyhow!("cannot preview inventory: {e:?}"))?;
        Ok(Preview {
            plan,
            local: here,
            remote: header,
            peer,
        })
    }
    /// Recheck BOTH complete revisions; never makes this a writer lease.
    pub fn validate_remote_preview(&self, preview: &Preview) -> Result<()> {
        let configs = self.configuration_paths()?;
        ensure!(configs.len() == 1, "selected runtime changed");
        self.validate_local(&preview.local, &managed(&configs[0])?, true)?;
        self.validate_remote(preview.peer, &preview.remote)
    }
    /// Retrieve one exact file from a still-current preview. Binary content is
    /// assembled only after all offsets, hashes and both revisions validate.
    pub fn read_remote_preview_file(&self, preview: &Preview, path: &str) -> Result<Vec<u8>> {
        tree::validate_path(path)?;
        self.validate_remote_preview(preview)?;
        // Enumerate and validate the complete manifest; never trust a path from
        // the caller as filesystem authority on the other computer.
        let mut collector = Collector::new(preview.remote.clone());
        while collector.entries.len() < preview.remote.files {
            let offset = collector.entries.len();
            collector.page(self.inspection_call(
                preview.peer,
                Request::Page {
                    header: preview.remote.clone(),
                    offset,
                },
            )?)?;
        }
        let entries = collector.finish()?;
        let (index, entry) = entries
            .iter()
            .enumerate()
            .find(|(_, e)| e.path == path)
            .context("path absent from inventory")?;
        let bytes = self.inspection_content(preview.peer, &preview.remote, index, entry)?;
        self.validate_remote_preview(preview)?;
        Ok(bytes)
    }
    fn inspection_content(
        &self,
        peer: Uuid,
        header: &Header,
        index: usize,
        entry: &Entry,
    ) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(entry.size);
        while bytes.len() < entry.size {
            let offset = bytes.len();
            let response = self.inspection_call(
                peer,
                Request::Content {
                    header: header.clone(),
                    index,
                    offset,
                },
            )?;
            append_chunk(&mut bytes, header, index, entry, response)?;
        }
        ensure!(
            wire::fingerprint(&bytes) == entry.sha256,
            "content hash differs from inventory"
        );
        Ok(bytes)
    }
    fn validate_remote(&self, peer: Uuid, header: &Header) -> Result<()> {
        ensure!(
            matches!(self.inspection_call(peer, Request::Validate { header: header.clone() })?,
            Response::Valid { header: h } if h == *header),
            "inventory validation missing or substituted"
        );
        Ok(())
    }
    fn validate_local(
        &self,
        local: &Local,
        daemon: &daemon::DaemonIdentity,
        content: bool,
    ) -> Result<()> {
        self.check()?;
        let b = &local.header.binding;
        let current = self.inspection_binding(
            b.pair,
            &b.owner,
            &b.reader,
            &b.vault,
            b.remote,
            &local.path,
            &local.root,
            daemon,
        )?;
        ensure!(current == *b, "inspection binding changed");
        let fresh = Tree::collect(&local.root, &local.path, content)?;
        ensure!(
            local.matches(&fresh, content),
            "inventory changed; refresh preview"
        );
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn inspection_binding(
        &self,
        pair: Uuid,
        owner: &str,
        reader: &str,
        vault: &str,
        remote: bool,
        path: &Path,
        root: &File,
        daemon: &daemon::DaemonIdentity,
    ) -> Result<Binding> {
        let configs = self.configuration_paths()?;
        ensure!(
            configs.len() == 1 && managed(&configs[0])? == *daemon,
            "managed runtime selection changed"
        );
        let api = daemon.connect()?;
        let status = api.identity()?;
        let started = status["startTime"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("runtime generation missing")?
            .to_owned();
        ensure!(
            api.version()?["version"] == daemon::CLIENT_VERSION,
            "inspection requires the pinned managed runtime"
        );
        let config = api.config()?;
        if remote {
            let folder = config["folders"]
                .as_array()
                .context("runtime folders missing")?
                .iter()
                .find(|f| f["id"] == vault)
                .context("authorized vault unavailable")?;
            inspectable(folder)?;
            let configured = folder["path"]
                .as_str()
                .context("runtime folder path missing")?;
            ensure!(
                Path::new(configured).canonicalize()? == path,
                "authorized runtime folder path changed"
            );
            let ignores = api.ignores(vault)?;
            let lines = ignores["ignore"]
                .as_array()
                .context("runtime ignore policy missing")?;
            let lines = lines
                .iter()
                .map(|v| v.as_str().context("invalid runtime ignore policy"))
                .collect::<Result<Vec<_>>>()?;
            let lines: Vec<_> = lines
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && !s.starts_with("//"))
                .collect();
            ensure!(
                lines == source::IGNORES,
                "runtime ignore policy differs from canonical inspection policy"
            );
        }
        tree::verify_root(root, path)?;
        Ok(Binding {
            pair,
            owner: owner.into(),
            reader: reader.into(),
            vault: vault.into(),
            remote,
            root_binding: opaque_root(&self.shared.inventory_key, pair, path, root)?,
            device: daemon.device_id.clone(),
            certificate: daemon.certificate_hash.clone(),
            config: digest(&config)?,
            generation: self.shared.observation_generation,
            started,
            policy: tree::policy(root)?,
        })
    }
    fn inspection_authority(
        &self,
        scope: Uuid,
        signer: &[u8],
        vault: &str,
    ) -> Result<(ExistingVault, [u8; 32])> {
        let db = self.shared.db.lock().unwrap();
        let peer = db.peers.get(&scope).context("computer not approved")?;
        ensure!(
            peer.public == signer && peer.syncthing_device.is_some(),
            "inspection identity not approved"
        );
        ensure!(
            peer.allowed.iter().any(|v| v == vault),
            "vault not authorized"
        );
        let source = db
            .attached
            .iter()
            .find(|v| v.folder_id == vault)
            .context("authorized vault unavailable")?
            .clone();
        Ok((source, db.seed))
    }
    pub(super) fn inspect_request(
        &self,
        scope: Uuid,
        signer: &[u8],
        request: Request,
    ) -> Result<Response> {
        self.check()?;
        let vault = match &request {
            Request::Open { vault } => vault,
            Request::Page { header, .. }
            | Request::Content { header, .. }
            | Request::Validate { header } => &header.binding.vault,
        };
        let (source, seed) = self.inspection_authority(scope, signer, vault)?;
        let opening = matches!(&request, Request::Open { .. });
        self.shared
            .inventory
            .lock()
            .unwrap()
            .admit(scope, opening.then_some(vault.as_str()))?;
        let _permit = Permit {
            shared: self.shared.clone(),
            peer: scope,
        };
        #[cfg(test)]
        {
            let hook = self.shared.inspection_hook.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(scope);
            }
        }
        if let Request::Open { vault } = request {
            let fresh = inspect_existing(&self.configuration_paths()?, &source.path)?;
            ensure!(
                existing::same_binding(&source, &fresh),
                "authorized root changed"
            );
            let prepared = source
                .daemon
                .config_file
                .parent()
                .and_then(Path::parent)
                .context("managed state missing")?;
            ensure!(
                daemon::managed_identity(prepared)? == source.daemon,
                "source is not the pinned managed runtime"
            );
            let root = authorized_root(&source)?;
            let owner = wire::fingerprint(&wire::public(&seed)?);
            let reader = wire::fingerprint(signer);
            let binding = self.inspection_binding(
                scope,
                &owner,
                &reader,
                &vault,
                true,
                &source.path,
                &root,
                &source.daemon,
            )?;
            let collected = local(
                binding,
                source.path.clone(),
                root,
                &self.shared.inventory_key,
            )?;
            self.validate_local(&collected, &source.daemon, true)?;
            let (fresh, _) = self.inspection_authority(scope, signer, &vault)?;
            ensure!(
                existing::same_binding(&fresh, &source),
                "authorized replica changed"
            );
            let header = self.shared.inventory.lock().unwrap().insert(Stored {
                local: collected,
                source,
                expires: Instant::now() + Duration::from_secs(LIFETIME_SECONDS),
            });
            return Ok(Response::Open { header });
        }
        let header = match &request {
            Request::Page { header, .. }
            | Request::Content { header, .. }
            | Request::Validate { header } => header,
            Request::Open { .. } => unreachable!(),
        };
        let current = {
            let mut cache = self.shared.inventory.lock().unwrap();
            cache.prune();
            cache
                .entries
                .get(&(scope, header.binding.vault.clone(), header.revision.clone()))
                .cloned()
                .context("inventory expired or unavailable; refresh")?
        };
        ensure!(
            *header == current.local.header && header.binding.pair == scope,
            "stale or unauthorized inventory"
        );
        ensure!(
            existing::same_binding(&source, &current.source),
            "authorized replica changed"
        );
        // Chunks are immutable original bytes. Full byte revalidation brackets
        // the public preview/content operations, so even coarse timestamp
        // filesystems cannot turn an undetected same-size edit into success.
        let content = matches!(request, Request::Validate { .. });
        self.validate_local(&current.local, &current.source.daemon, content)?;
        let entries = &current.local.entries;
        let response = match request {
            Request::Page { header, offset } => {
                ensure!(
                    offset < entries.len() && offset % PAGE_FILES == 0,
                    "invalid inventory offset"
                );
                let end = (offset + PAGE_FILES).min(entries.len());
                Response::Page {
                    header,
                    offset,
                    entries: entries[offset..end].to_vec(),
                }
            }
            Request::Content {
                header,
                index,
                offset,
            } => {
                let entry = entries.get(index).context("file index outside inventory")?;
                let bytes = &current.local.tree.files[&entry.path];
                ensure!(
                    offset < bytes.len() && offset % CHUNK_BYTES == 0,
                    "invalid content offset"
                );
                let end = (offset + CHUNK_BYTES).min(bytes.len());
                Response::Content {
                    header,
                    index,
                    offset,
                    sha256: entry.sha256.clone(),
                    hex: wire::hex(&bytes[offset..end]),
                }
            }
            Request::Validate { header } => Response::Valid { header },
            Request::Open { .. } => unreachable!(),
        };
        // Only immutable cached data was sliced after the single validation.
        // Recheck revocation under a short lock before releasing the response.
        let (fresh, _) = self.inspection_authority(scope, signer, &current.source.folder_id)?;
        ensure!(
            existing::same_binding(&fresh, &current.source),
            "authorized replica changed"
        );
        ensure!(
            current.expires > Instant::now(),
            "inventory expired during collection"
        );
        Ok(response)
    }
}
fn snapshot(local: &Local) -> Result<merge_preview::Snapshot> {
    Ok(merge_preview::Snapshot {
        scope: digest(&local.header.binding)?,
        revision: local.header.revision.clone(),
        files: local.tree.files.clone(),
    })
}
fn managed(config: &Path) -> Result<daemon::DaemonIdentity> {
    let config = config.canonicalize()?;
    let prepared = config
        .parent()
        .and_then(Path::parent)
        .context("managed state missing")?;
    let identity = daemon::managed_transport_identity(prepared)?;
    ensure!(
        identity.config_file == config,
        "managed runtime identity substituted"
    );
    Ok(identity)
}
struct Collector {
    header: Header,
    entries: Vec<Entry>,
}
impl Collector {
    fn new(header: Header) -> Self {
        Self {
            header,
            entries: vec![],
        }
    }
    fn page(&mut self, response: Response) -> Result<()> {
        let Response::Page {
            header,
            offset,
            entries,
        } = response
        else {
            anyhow::bail!("missing inventory page");
        };
        ensure!(
            header == self.header
                && offset == self.entries.len()
                && entries.len() == PAGE_FILES.min(self.header.files.saturating_sub(offset))
                && !entries.is_empty(),
            "inventory page missing, truncated, reordered or from another generation"
        );
        for entry in &entries {
            tree::validate_path(&entry.path)?;
            ensure!(
                entry.size <= MAX_BYTES
                    && entry.sha256.len() == 64
                    && entry.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                    && self
                        .entries
                        .last()
                        .is_none_or(|last| last.path < entry.path),
                "invalid or duplicate inventory entry"
            );
            self.entries.push(entry.clone());
        }
        Ok(())
    }
    fn finish(self) -> Result<Vec<Entry>> {
        ensure!(
            self.entries.len() == self.header.files
                && digest(&self.entries)? == self.header.manifest
                && self
                    .entries
                    .iter()
                    .try_fold(0usize, |sum, e| sum.checked_add(e.size))
                    == Some(self.header.bytes),
            "incomplete or substituted inventory manifest"
        );
        Ok(self.entries)
    }
}
fn append_chunk(
    bytes: &mut Vec<u8>,
    expected: &Header,
    index: usize,
    entry: &Entry,
    response: Response,
) -> Result<()> {
    let Response::Content {
        header,
        index: got_index,
        offset,
        sha256,
        hex,
    } = response
    else {
        anyhow::bail!("missing content chunk");
    };
    let count = CHUNK_BYTES.min(entry.size - bytes.len());
    ensure!(
        header == *expected
            && index == got_index
            && offset == bytes.len()
            && sha256 == entry.sha256
            && hex.len() == count * 2
            && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "content chunk truncated, reordered or substituted"
    );
    for i in (0..hex.len()).step_by(2) {
        bytes.push(u8::from_str_radix(&hex[i..i + 2], 16)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
