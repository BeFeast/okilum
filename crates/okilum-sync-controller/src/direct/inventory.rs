//! Read-only, revision-bound inspection. This is NOT an exclusive mutation lease.
//! No enrollment, share grant, transport reconciliation or canonical publication.
use super::*;
use crate::{daemon, merge_preview};
use std::{fs::File, time::Instant};

mod tree;
use tree::Tree;

/// Bounds are refusals, never successful truncation. One server snapshot is kept
/// in memory; starting another inspection invalidates the previous capability.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_FILES: usize = 16_384;
pub const PAGE_FILES: usize = 4;
/// Hex encoding keeps even worst-case nested JSON below the existing wire bound.
pub const CHUNK_BYTES: usize = 4096;
const LIFETIME_SECONDS: u64 = 120;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    pair: Uuid,
    owner: String,
    reader: String,
    vault: String,
    root: PathBuf,
    root_identity: (u64, u64),
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
    header: Header,
    entries: Vec<Entry>,
    tree: Tree,
    root: Arc<File>,
}
pub(super) struct Stored {
    local: Local,
    source: ExistingVault,
    expires: Instant,
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
fn local(header_binding: Binding, root: Arc<File>) -> Result<Local> {
    let tree = Tree::collect(&root, &header_binding.root, true)?;
    ensure!(
        tree.policy == header_binding.policy,
        "ignore policy changed during inspection"
    );
    let entries = manifest(&tree);
    let header = Header {
        id: Uuid::new_v4(),
        binding: header_binding,
        revision: digest(&tree.stamps)?,
        manifest: digest(&entries)?,
        files: entries.len(),
        bytes: entries.iter().map(|e| e.size).sum(),
    };
    Ok(Local {
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
        match self.call(peer.endpoint, id, &peer.key, &peer.public, Operation::Inspect { request })? {
            Reply::Inspection { response } => Ok(response),
            _ => anyhow::bail!("inspection unavailable or stale; review authorization, runtime and vault, then refresh"),
        }
    }
    /// Inspect a remote authorized vault against an existing non-empty ordinary
    /// local directory. Never enrolls that directory or changes either daemon.
    /// Blocking IO: run outside the UI thread, like the other direct operations.
    pub fn preview_remote_vault(&self, peer: Uuid, vault: &str, here: &Path) -> Result<Preview> {
        self.check()?;
        let paired = self.inspection_peer(peer)?;
        let root = Arc::new(tree::open_root(here)?);
        let canonical = here.canonicalize()?;
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
            &canonical,
            &root,
            &daemon,
        )?;
        let here = local(binding, root)?;
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
        self.validate_local(&here, &daemon)?;
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
        self.validate_local(&preview.local, &managed(&configs[0])?)?;
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
    fn validate_local(&self, local: &Local, daemon: &daemon::DaemonIdentity) -> Result<()> {
        self.check()?;
        let b = &local.header.binding;
        let current = self.inspection_binding(
            b.pair,
            &b.owner,
            &b.reader,
            &b.vault,
            &b.root,
            &local.root,
            daemon,
        )?;
        ensure!(current == *b, "inspection binding changed");
        let fresh = Tree::collect(&local.root, &b.root, false)?;
        ensure!(
            fresh.stamps == local.tree.stamps && fresh.policy == b.policy,
            "inventory changed; refresh preview"
        );
        Ok(())
    }
    fn inspection_binding(
        &self,
        pair: Uuid,
        owner: &str,
        reader: &str,
        vault: &str,
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
        let config = digest(&api.config()?)?;
        tree::verify_root(root, path)?;
        Ok(Binding {
            pair,
            owner: owner.into(),
            reader: reader.into(),
            vault: vault.into(),
            root: path.into(),
            root_identity: tree::identity(root)?,
            device: daemon.device_id.clone(),
            certificate: daemon.certificate_hash.clone(),
            config,
            generation: self.shared.observation_generation,
            started,
            policy: tree::policy(root)?,
        })
    }
    pub(super) fn inspect_request(
        &self,
        db: &Database,
        scope: Uuid,
        signer: &[u8],
        request: Request,
    ) -> Result<Response> {
        self.check()?;
        let peer = db.peers.get(&scope).context("computer not approved")?;
        ensure!(
            peer.public == signer && peer.syncthing_device.is_some(),
            "inspection identity not approved"
        );
        let mut stored = self.shared.inventory.lock().unwrap();
        if let Request::Open { vault } = request {
            ensure!(peer.allowed.contains(&vault), "vault not authorized");
            let source = db
                .attached
                .iter()
                .find(|v| v.folder_id == vault)
                .context("authorized vault unavailable")?
                .clone();
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
            let root = Arc::new(tree::open_root(&source.path)?);
            let owner = wire::fingerprint(&wire::public(&db.seed)?);
            let reader = wire::fingerprint(signer);
            let binding = self.inspection_binding(
                scope,
                &owner,
                &reader,
                &vault,
                &source.path,
                &root,
                &source.daemon,
            )?;
            let collected = local(binding, root)?;
            self.validate_local(&collected, &source.daemon)?;
            let header = collected.header.clone();
            *stored = Some(Stored {
                local: collected,
                source,
                expires: Instant::now() + Duration::from_secs(LIFETIME_SECONDS),
            });
            return Ok(Response::Open { header });
        }
        let current = stored
            .as_ref()
            .context("inventory expired or replaced; refresh")?;
        let header = match &request {
            Request::Page { header, .. }
            | Request::Content { header, .. }
            | Request::Validate { header } => header,
            Request::Open { .. } => unreachable!(),
        };
        ensure!(
            *header == current.local.header
                && header.binding.pair == scope
                && current.expires > Instant::now()
                && peer.allowed.contains(&header.binding.vault),
            "stale or unauthorized inventory"
        );
        let fresh = db
            .attached
            .iter()
            .find(|v| v.folder_id == header.binding.vault)
            .context("vault authorization removed")?;
        ensure!(
            existing::same_binding(fresh, &current.source),
            "authorized replica changed"
        );
        self.validate_local(&current.local, &current.source.daemon)?;
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
        // The returned bytes are the immutable inventory bytes; metadata before
        // and after collection rejects changes rather than mixing generations.
        self.validate_local(&current.local, &current.source.daemon)?;
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
