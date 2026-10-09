//! Rebuildable, revision-aware lexical and real embedding retrieval for one brain.
//! Heavy scans, indexing and embedding calls never acquire the service/Runner lock.
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use notify::{EventKind, RecursiveMode, Watcher};
use okilum_core::{
    search::{SearchDocument, Searcher},
    source::{SourceSnapshot, SourceStore},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{mpsc, Arc, Mutex, RwLock, Weak},
    time::Duration,
};
use uuid::Uuid;

// v4 excludes unverified proposals, including brain-owned unplanned Inbox drafts.
// v5 adds complete, bounded incoming references from original source snapshots.
// v6 includes ordinary Markdown note links using the shared document resolver.
// Reject previous generations rather than reusing their derived chunks or vectors.
const INDEX_SCHEMA: &str = "okilum-brain-index/v6";
const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHUNKS: usize = 32_000;
const CHUNK_BYTES: usize = 2048;
const MAX_EXCERPT_BYTES: usize = 8192;
// Covers the maximum 8192-byte passage and model template tokens without truncation.
const EMBEDDING_CONTEXT_TOKENS: usize = 16_384;
const QUERY_PREFIX: &str =
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: ";

#[derive(Debug)]
pub struct RetrievalError {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for RetrievalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RetrievalError {}
pub fn error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    RetrievalError {
        code,
        message: message.into(),
    }
    .into()
}
pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|p| matches!(p,Component::Normal(n) if !n.to_string_lossy().starts_with('.')))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceMetadata {
    #[serde(default)]
    pub record_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub owner_goal_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Citation {
    pub citation_id: String,
    pub path: String,
    pub revision: String,
    pub start_line: usize,
    pub end_line: usize,
    pub locator: String,
    pub excerpt: String,
    #[serde(default)]
    pub metadata: SourceMetadata,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchScope {
    pub goal_id: String,
    #[serde(default = "default_scope")]
    pub mode: String,
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub include_paths: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    pub scope: SearchScope,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_excerpt")]
    pub max_excerpt_bytes: usize,
}
fn default_scope() -> String {
    "project".into()
}
fn default_mode() -> String {
    "hybrid".into()
}
fn default_limit() -> usize {
    10
}
fn default_excerpt() -> usize {
    4096
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    #[serde(flatten)]
    pub citation: Citation,
    pub title: String,
    pub rank: usize,
    pub score: f32,
    pub lexical_score: Option<f32>,
    pub semantic_score: Option<f32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexStatus {
    pub status: String,
    pub generation: Option<String>,
    pub observed_at: Option<String>,
    pub documents: usize,
    pub chunks: usize,
    pub semantic_status: String,
    pub model: Option<ModelMetadata>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}
impl Default for IndexStatus {
    fn default() -> Self {
        Self {
            status: "indexing".into(),
            generation: None,
            observed_at: None,
            documents: 0,
            chunks: 0,
            semantic_status: "unconfigured".into(),
            model: None,
            error: None,
            warnings: vec![],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchResult>,
    pub mode_requested: String,
    pub mode_used: String,
    pub index: IndexStatus,
    pub freshness: String,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingSettings {
    pub base_url: String,
    pub model: String,
    pub digest: String,
    #[serde(default = "default_dimensions")]
    pub dimensions: usize,
}
fn default_dimensions() -> usize {
    1024
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelMetadata {
    pub backend: String,
    pub name: String,
    pub digest: String,
    pub dimensions: usize,
    pub query_prefix: String,
    pub document_prefix: String,
    pub normalized: bool,
    pub context_tokens: usize,
}
impl EmbeddingSettings {
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            backend: "ollama".into(),
            name: self.model.clone(),
            digest: self.digest.clone(),
            dimensions: self.dimensions,
            query_prefix: QUERY_PREFIX.into(),
            document_prefix: String::new(),
            normalized: true,
            context_tokens: EMBEDDING_CONTEXT_TOKENS,
        }
    }
    fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.base_url)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none(),
            "invalid embedding address"
        );
        ensure!(
            !self.model.is_empty()
                && self.model.len() <= 128
                && self.digest.len() == 64
                && self.digest.chars().all(|c| c.is_ascii_hexdigit()),
            "pin a real embedding model and SHA256 digest"
        );
        ensure!(
            (1..=4096).contains(&self.dimensions),
            "invalid embedding dimensions"
        );
        Ok(())
    }
}
#[derive(Clone)]
pub struct OllamaEmbedder {
    settings: EmbeddingSettings,
    client: reqwest::blocking::Client,
}
impl OllamaEmbedder {
    pub fn new(settings: EmbeddingSettings) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            settings,
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(180))
                .connect_timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    fn verify_model(&self) -> Result<()> {
        let v: Value = self
            .client
            .get(format!(
                "{}/api/tags",
                self.settings.base_url.trim_end_matches('/')
            ))
            .send()?
            .error_for_status()?
            .json()?;
        ensure!(v["models"].as_array().is_some_and(|models|models.iter().any(|m|m["name"]==self.settings.model && m["digest"]==self.settings.digest)),"embedding model digest changed or pinned model unavailable; rebuild with the intended model");
        Ok(())
    }
    pub fn embed(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
        ensure!(
            texts.len() <= 16 && texts.iter().all(|s| s.len() <= MAX_EXCERPT_BYTES),
            "embedding input budget exceeded"
        );
        self.verify_model()?;
        let input: Vec<String> = texts
            .iter()
            .map(|t| {
                if query {
                    format!("{QUERY_PREFIX}{t}")
                } else {
                    t.clone()
                }
            })
            .collect();
        let v:Value=self.client.post(format!("{}/api/embed",self.settings.base_url.trim_end_matches('/'))).json(&json!({"model":self.settings.model,"input":input,"truncate":false,"keep_alive":"5m","options":{"num_ctx":EMBEDDING_CONTEXT_TOKENS,"num_thread":4}})).send()?.error_for_status()?.json()?;
        let mut vectors: Vec<Vec<f32>> = serde_json::from_value(v["embeddings"].clone())?;
        ensure!(vectors.len() == texts.len(), "embedding count mismatch");
        for vector in &mut vectors {
            ensure!(
                vector.len() == self.settings.dimensions && vector.iter().all(|x| x.is_finite()),
                "invalid embedding vector shape"
            );
            let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            ensure!(norm > 0.000001, "empty embedding vector");
            for value in vector {
                *value /= norm;
            }
        }
        self.verify_model()?;
        Ok(vectors)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct IndexedChunk {
    citation: Citation,
    title: String,
    owner_goal_id: Option<String>,
    vector: Option<Vec<f32>>,
}
#[derive(Clone, Serialize, Deserialize)]
struct CachedIndex {
    schema: String,
    brain_id: String,
    root: String,
    records_dir: String,
    generation: String,
    observed_at: String,
    model: Option<ModelMetadata>,
    documents: BTreeMap<String, String>,
    chunks: Vec<IndexedChunk>,
    #[serde(default)]
    incoming: crate::incoming_references::Graph,
    warnings: Vec<String>,
}
struct Generation {
    cache: CachedIndex,
    lexical: Searcher,
    directory: PathBuf,
    embedder: Option<OllamaEmbedder>,
}
struct View {
    epoch: u64,
    status: IndexStatus,
    generation: Option<Arc<Generation>>,
}
pub struct BrainIndex {
    brain_id: String,
    root: PathBuf,
    records_dir: String,
    operational: PathBuf,
    cache_root: PathBuf,
    source: SourceStore,
    view: RwLock<View>,
    trigger: mpsc::Sender<bool>,
    _watcher: Mutex<Option<notify::RecommendedWatcher>>,
}
impl BrainIndex {
    /// Construct only immutable source handles here; first scan runs in its own
    /// backend-owned worker after this function returns.
    pub fn start(
        brain_id: String,
        root: PathBuf,
        records_dir: String,
        operational: PathBuf,
        _managed: bool,
    ) -> Result<Arc<Self>> {
        let cache_root = operational.join("derived/brain-index-v1");
        fs::create_dir_all(&cache_root)?;
        ensure!(
            !fs::symlink_metadata(&cache_root)?.file_type().is_symlink(),
            "derived index cannot be a symlink"
        );
        let source = SourceStore::open_read_only(&brain_id, &root, &operational.join("source"))?;
        let (tx, rx) = mpsc::channel();
        let index = Arc::new(Self {
            brain_id,
            root,
            records_dir,
            operational,
            cache_root,
            source,
            view: RwLock::new(View {
                epoch: 0,
                status: IndexStatus::default(),
                generation: None,
            }),
            trigger: tx,
            _watcher: Mutex::new(None),
        });
        let weak = Arc::downgrade(&index);
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Some(index) = weak.upgrade() {
                    // Directory create/remove/rename must invalidate too. Read-only
                    // access events never dirty the index or cause an indexing loop.
                    if event.as_ref().is_ok_and(|e| !index.relevant_event(e)) {
                        return;
                    }
                    if let Ok(mut view) = index.view.write() {
                        view.epoch += 1;
                        view.status.status = "stale".into();
                    }
                    let _ = index.trigger.send(false);
                }
            })?;
        watcher.watch(&index.root, RecursiveMode::Recursive)?;
        *index._watcher.lock().unwrap() = Some(watcher);
        let weak = Arc::downgrade(&index);
        std::thread::spawn(move || Self::worker(weak, rx));
        index.trigger.send(false)?;
        Ok(index)
    }
    fn relevant_event(&self, event: &notify::Event) -> bool {
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }
        if event.paths.is_empty() {
            return true;
        }
        let directory_event = matches!(
            event.kind,
            EventKind::Create(notify::event::CreateKind::Folder)
                | EventKind::Remove(notify::event::RemoveKind::Folder)
        ) || event.paths.iter().any(|p| p.is_dir());
        event.paths.iter().any(|p| {
            let Ok(rel) = p.strip_prefix(&self.root) else {
                return false;
            };
            if rel.components().any(|c| {
                c.as_os_str()
                    .to_str()
                    .is_some_and(|s| s.starts_with('.') || matches!(s, "target" | "node_modules"))
            }) {
                return false;
            }
            directory_event || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
        })
    }
    fn worker(weak: Weak<Self>, rx: mpsc::Receiver<bool>) {
        loop {
            let Ok(mut force) = rx.recv() else { return };
            // Coalesce save/rename bursts. No periodic scan or index writes while idle.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                let remaining = deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(300));
                match rx.recv_timeout(remaining) {
                    Ok(value) => force |= value,
                    Err(_) => break,
                }
            }
            let Some(index) = weak.upgrade() else { return };
            let epoch = {
                let mut view = index.view.write().unwrap();
                view.status.status = "indexing".into();
                view.status.error = None;
                view.epoch
            };
            if let Err(e) = index.refresh_at(force, epoch) {
                let mut view = index.view.write().unwrap();
                // An obsolete refresh must not overwrite a watcher/rebuild invalidation.
                if view.epoch == epoch {
                    if view.status.status == "ready" {
                        view.status.semantic_status = "unavailable".into();
                        view.status
                            .warnings
                            .push(format!("Semantic enrichment unavailable: {e}"));
                    } else {
                        view.status.status = "error".into();
                        view.status.error = Some(e.to_string());
                    }
                }
            }
        }
    }
    pub fn status(&self) -> IndexStatus {
        self.view.read().unwrap().status.clone()
    }
    pub fn rebuild(&self) -> Result<IndexStatus> {
        {
            let mut view = self.view.write().unwrap();
            view.epoch += 1;
            view.status.status = "indexing".into();
        }
        self.trigger.send(true)?;
        Ok(self.status())
    }
    fn settings(&self) -> Result<Option<EmbeddingSettings>> {
        let path = self.operational.join("retrieval-settings.json");
        if !path.exists() {
            return Ok(None);
        }
        let settings: EmbeddingSettings = serde_json::from_slice(&fs::read(path)?)?;
        settings.validate()?;
        Ok(Some(settings))
    }
    fn inventory(&self) -> Result<Vec<SourceSnapshot>> {
        fn walk(
            index: &BrainIndex,
            path: &Path,
            out: &mut Vec<SourceSnapshot>,
            bytes: &mut usize,
        ) -> Result<()> {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let ty = entry.file_type()?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if ty.is_symlink()
                    || name.starts_with('.')
                    || matches!(name.as_ref(), "node_modules" | "target")
                {
                    continue;
                }
                if ty.is_dir() {
                    walk(index, &entry.path(), out, bytes)?;
                } else if ty.is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("md"))
                {
                    ensure!(out.len() < 10_000, "brain index document budget exceeded");
                    let path = entry
                        .path()
                        .strip_prefix(&index.root)?
                        .to_string_lossy()
                        .replace('\\', "/");
                    let snapshot = index.source.read_bounded(&path, MAX_SOURCE_BYTES)?;
                    *bytes += STANDARD.decode(&snapshot.content_base64)?.len();
                    ensure!(
                        *bytes <= MAX_TOTAL_BYTES,
                        "brain index byte budget exceeded"
                    );
                    out.push(snapshot);
                }
            }
            Ok(())
        }
        let mut sources = vec![];
        let mut bytes = 0;
        walk(self, &self.root, &mut sources, &mut bytes)?;
        sources.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(sources)
    }
    fn configuration_revision(&self) -> Option<String> {
        match fs::read(self.operational.join("retrieval-settings.json")) {
            Ok(bytes) => Some(sha(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            // Optional configuration access errors must not block lexical readiness.
            // settings() reports the actionable warning; keep an identity fence here.
            Err(e) => Some(format!("unreadable:{:?}", e.kind())),
        }
    }
    #[cfg(test)]
    fn refresh(&self, force: bool) -> Result<()> {
        let epoch = self.view.read().unwrap().epoch;
        self.refresh_at(force, epoch)
    }
    fn refresh_at(&self, force: bool, epoch: u64) -> Result<()> {
        let configuration = self.configuration_revision();
        let sources = self.inventory()?;
        let markdown_vault = okilum_core::Vault::scan_metadata(&self.root)?;
        let docs: BTreeMap<_, _> = sources
            .iter()
            .map(|s| (s.path.clone(), s.revision.clone()))
            .collect();
        let (settings, mut warnings) = match self.settings() {
            Ok(v) => (v, vec![]),
            Err(e) => (
                None,
                vec![format!("Embedding configuration unavailable: {e}")],
            ),
        };
        let model = settings.as_ref().map(EmbeddingSettings::metadata);
        let current = self.view.read().unwrap().generation.clone();
        let retained = current.as_ref().map(|g| &g.cache);
        let disk_cache = if retained.is_none() && !force {
            self.read_cache().ok()
        } else {
            None
        };
        let previous = if force {
            None
        } else {
            retained.or(disk_cache.as_ref())
        };
        if !force
            && previous.is_some_and(|p| {
                p.documents == docs
                    && p.incoming.matches_inventory(&markdown_vault)
                    && p.model == model
                    && (model.is_none() || p.chunks.iter().all(|c| c.vector.is_some()))
            })
        {
            if let Some(current) = &current {
                let same_settings =
                    current.embedder.as_ref().map(|e| &e.settings) == settings.as_ref();
                if same_settings {
                    self.validate_refresh(
                        epoch,
                        &configuration,
                        &docs,
                        &current.cache.incoming,
                        None,
                    )?;
                    let mut view = self.view.write().unwrap();
                    ensure!(view.epoch == epoch, "index refresh superseded");
                    view.status.status = "ready".into();
                    view.status.observed_at = Some(now());
                    return Ok(());
                }
            }
        }
        let reusable: BTreeMap<_, _> = previous
            .filter(|p| p.model == model)
            .map(|p| {
                p.chunks
                    .iter()
                    .filter_map(|c| {
                        c.vector
                            .as_ref()
                            .map(|v| (c.citation.citation_id.clone(), v.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        drop(current);
        let incoming =
            crate::incoming_references::Graph::build(&sources, &self.records_dir, &markdown_vault);
        let mut chunks = vec![];
        for source in &sources {
            let (mut next, mut notes) = chunks_for(source, &self.records_dir)?;
            chunks.append(&mut next);
            warnings.append(&mut notes);
            ensure!(
                chunks.len() <= MAX_CHUNKS,
                "brain index chunk budget exceeded"
            );
        }
        let embedder = match settings.map(OllamaEmbedder::new).transpose() {
            Ok(embedder) => embedder,
            Err(e) => {
                warnings.push(format!("Semantic embeddings unavailable: {e}"));
                None
            }
        };
        for chunk in &mut chunks {
            chunk.vector = reusable.get(&chunk.citation.citation_id).cloned();
        }
        let missing: Vec<usize> = chunks
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.vector.is_none().then_some(i))
            .collect();
        let semantic_status = match (&embedder, &model, missing.is_empty()) {
            (Some(_), _, false) => "indexing",
            (Some(_), _, true) => "ready",
            (None, Some(_), _) => "unavailable",
            (None, None, _) => "unconfigured",
        };
        let mut cache = CachedIndex {
            schema: INDEX_SCHEMA.into(),
            brain_id: self.brain_id.clone(),
            root: self.root.to_string_lossy().into_owned(),
            records_dir: self.records_dir.clone(),
            generation: String::new(),
            observed_at: now(),
            model,
            documents: docs,
            chunks,
            incoming,
            warnings,
        };
        // Publish complete lexical/search ownership before any optional provider RPC.
        let phase_one = self.publish_generation(
            cache.clone(),
            embedder.clone(),
            semantic_status,
            epoch,
            &configuration,
            None,
        )?;
        let Some(embedder) = embedder else {
            return Ok(());
        };
        if missing.is_empty() {
            return Ok(());
        }
        let mut semantic_status = "ready";
        for batch in embedding_batches(&missing, &cache.chunks) {
            self.check_enrichment(epoch, &configuration, &phase_one)?;
            let texts: Vec<String> = batch
                .iter()
                .map(|i| cache.chunks[*i].citation.excerpt.clone())
                .collect();
            let result = embedder.embed(&texts, false);
            // In-flight HTTP may finish, but cannot start an obsolete tail or publish it.
            self.check_enrichment(epoch, &configuration, &phase_one)?;
            match result {
                Ok(vectors) => {
                    for (i, vector) in batch.iter().zip(vectors) {
                        cache.chunks[*i].vector = Some(vector);
                    }
                }
                Err(e) => {
                    semantic_status = "unavailable";
                    cache
                        .warnings
                        .push(format!("Semantic embeddings unavailable: {e}"));
                    break;
                }
            }
        }
        self.publish_generation(
            cache,
            Some(embedder),
            semantic_status,
            epoch,
            &configuration,
            Some(&phase_one),
        )?;
        Ok(())
    }
    fn check_enrichment(
        &self,
        epoch: u64,
        configuration: &Option<String>,
        generation: &str,
    ) -> Result<()> {
        {
            let view = self.view.read().unwrap();
            ensure!(
                view.epoch == epoch && view.status.generation.as_deref() == Some(generation),
                "index refresh superseded"
            );
        }
        if &self.configuration_revision() != configuration {
            let mut view = self.view.write().unwrap();
            if view.epoch == epoch {
                view.epoch += 1;
                view.status.status = "stale".into();
                let _ = self.trigger.send(false);
            }
            return Err(error(
                "index_changed",
                "Embedding settings changed while indexing",
            ));
        }
        Ok(())
    }
    fn validate_refresh(
        &self,
        epoch: u64,
        configuration: &Option<String>,
        documents: &BTreeMap<String, String>,
        incoming: &crate::incoming_references::Graph,
        expected_generation: Option<&str>,
    ) -> Result<()> {
        let owned = |view: &View| {
            view.epoch == epoch
                && expected_generation
                    .is_none_or(|id| view.status.generation.as_deref() == Some(id))
        };
        if !owned(&self.view.read().unwrap()) {
            return Err(error("index_changed", "Index refresh superseded"));
        }
        let sources = self.inventory()?;
        let after: BTreeMap<_, _> = sources.into_iter().map(|s| (s.path, s.revision)).collect();
        if &after != documents
            || &self.configuration_revision() != configuration
            || !incoming.matches_inventory(&okilum_core::Vault::scan_metadata(&self.root)?)
        {
            let mut view = self.view.write().unwrap();
            if owned(&view) {
                view.epoch += 1;
                view.status.status = "stale".into();
                let _ = self.trigger.send(false);
            }
            return Err(error(
                "index_changed",
                "Brain or embedding settings changed while indexing",
            ));
        }
        Ok(())
    }
    fn publish_generation(
        &self,
        mut cache: CachedIndex,
        embedder: Option<OllamaEmbedder>,
        semantic_status: &str,
        epoch: u64,
        configuration: &Option<String>,
        expected_generation: Option<&str>,
    ) -> Result<String> {
        let generation = Uuid::new_v4().to_string();
        let directory = self.cache_root.join(&generation);
        fs::create_dir(&directory)?;
        let published = (|| -> Result<_> {
            let documents: Vec<_> = cache
                .chunks
                .iter()
                .map(|c| SearchDocument {
                    path: c.citation.citation_id.clone(),
                    title: format!("{} {}", c.title, c.citation.path),
                    text: c.citation.excerpt.clone(),
                })
                .collect();
            let lexical = Searcher::build_documents(&documents, &directory.join("lexical"))?;
            cache.generation = generation.clone();
            cache.observed_at = now();
            let cache_bytes = serde_json::to_vec(&cache)?;
            fs::write(directory.join("index.json"), &cache_bytes)?;
            self.validate_refresh(
                epoch,
                configuration,
                &cache.documents,
                &cache.incoming,
                expected_generation,
            )?;
            let status = IndexStatus {
                status: "ready".into(),
                generation: Some(generation.clone()),
                observed_at: Some(cache.observed_at.clone()),
                documents: cache.documents.len(),
                chunks: cache.chunks.len(),
                semantic_status: semantic_status.into(),
                model: cache.model.clone(),
                error: None,
                warnings: cache.warnings.clone(),
            };
            let replacement = Arc::new(Generation {
                cache,
                lexical,
                directory: directory.clone(),
                embedder,
            });
            let mut view = self.view.write().unwrap();
            ensure!(
                view.epoch == epoch
                    && expected_generation
                        .is_none_or(|id| view.status.generation.as_deref() == Some(id)),
                "index refresh superseded"
            );
            // The pointer and View share the invalidation lock: stale events cannot
            // be overwritten between committing disk identity and visible readiness.
            atomic_json(
                &self.cache_root.join("current.json"),
                &json!({"generation":generation,"content_sha256":sha(&cache_bytes)}),
            )?;
            view.status = status;
            Ok(view.generation.replace(replacement))
        })();
        let old = match published {
            Ok(old) => old,
            Err(e) => {
                let _ = fs::remove_dir_all(&directory);
                return Err(e);
            }
        };
        if let Some(old) = old {
            if Arc::strong_count(&old) == 1 {
                let dir = old.directory.clone();
                drop(old);
                let _ = fs::remove_dir_all(dir);
            }
        }
        Ok(generation)
    }
    fn read_cache(&self) -> Result<CachedIndex> {
        let v: Value = serde_json::from_slice(&fs::read(self.cache_root.join("current.json"))?)?;
        let id = v["generation"]
            .as_str()
            .context("missing index generation")?;
        Uuid::parse_str(id)?;
        let bytes = fs::read(self.cache_root.join(id).join("index.json"))?;
        ensure!(
            v["content_sha256"].as_str() == Some(sha(&bytes).as_str()),
            "derived cache content checksum mismatch"
        );
        let cache: CachedIndex = serde_json::from_slice(&bytes)?;
        validate_cached_vectors(&cache)?;
        ensure!(
            cache.generation == id
                && cache.schema == INDEX_SCHEMA
                && cache.brain_id == self.brain_id
                && cache.root == self.root.to_string_lossy()
                && cache.records_dir == self.records_dir,
            "derived cache identity mismatch"
        );
        Ok(cache)
    }
    pub fn source_backlinks(
        &self,
        request: crate::incoming_references::Request,
    ) -> Result<crate::incoming_references::Response> {
        request.validate()?;
        let (generation, status) = {
            let view = self.view.read().unwrap();
            (view.generation.clone(), view.status.clone())
        };
        let generation = generation.ok_or_else(|| {
            error(
                "index_not_ready",
                "Brain index is not ready; retry after indexing",
            )
        })?;
        if status.status != "ready" {
            return Err(error(
                "index_stale",
                "Brain index is updating; retry after indexing",
            ));
        }
        if generation.cache.documents.get(&request.path) != Some(&request.expected_revision) {
            return Err(error(
                "source_stale",
                "The target revision is not indexed; reload references after indexing",
            ));
        }
        let (rows, next_cursor) = generation
            .cache
            .incoming
            .page(&request, &generation.cache.generation)?;
        let mut checked = BTreeSet::new();
        for (path, revision) in std::iter::once((&request.path, &request.expected_revision))
            .chain(rows.iter().map(|r| (&r.path, &r.revision)))
        {
            if !checked.insert(path) {
                continue;
            }
            let source = self
                .source
                .read_bounded(path, MAX_SOURCE_BYTES)
                .map_err(|_| {
                    error(
                        "source_stale",
                        "An incoming-reference source is unavailable; reload after indexing",
                    )
                })?;
            if source.revision != *revision {
                let _ = self.trigger.send(false);
                return Err(error(
                    "source_stale",
                    "An incoming-reference source changed; reload after indexing",
                ));
            }
        }
        let current = self.status();
        if current.status != "ready" || current.generation != status.generation {
            return Err(error(
                "index_stale",
                "Brain changed while reading incoming references; reload current index",
            ));
        }
        Ok(crate::incoming_references::Response {
            target: crate::incoming_references::Target {
                path: request.path,
                revision: request.expected_revision,
            },
            index: status,
            freshness: "current_at_read".into(),
            rows,
            next_cursor,
            warnings: vec![],
        })
    }

    pub fn search(&self, request: SearchRequest) -> Result<SearchResponse> {
        ensure!(
            !request.query.trim().is_empty() && request.query.len() <= 2048,
            "query must contain 1–2048 bytes"
        );
        ensure!(
            (1..=20).contains(&request.limit)
                && (64..=MAX_EXCERPT_BYTES).contains(&request.max_excerpt_bytes),
            "search budget exceeded"
        );
        ensure!(
            matches!(request.mode.as_str(), "lexical" | "semantic" | "hybrid"),
            "invalid search mode"
        );
        validate_scope(&request.scope)?;
        let (generation, status) = {
            let view = self.view.read().unwrap();
            (view.generation.clone(), view.status.clone())
        };
        let generation = generation.ok_or_else(|| {
            error(
                "index_not_ready",
                "Brain index is not ready; retry after indexing",
            )
        })?;
        if status.status != "ready" {
            return Err(error(
                "index_stale",
                "Brain index is updating; retry after indexing",
            ));
        }
        let mut warnings = vec![];
        let mut mode_used = request.mode.clone();
        let allowed: Vec<_> = generation
            .cache
            .chunks
            .iter()
            .filter(|c| in_scope(c, &request.scope))
            .collect();
        let lexical: Vec<_> = if request.mode != "semantic" {
            generation.lexical.search_in_paths(
                &natural_lexical_query(&request.query),
                request.limit * 8,
                &allowed
                    .iter()
                    .map(|c| c.citation.citation_id.clone())
                    .collect::<Vec<_>>(),
            )?
        } else {
            vec![]
        };
        let mut semantic: Vec<(String, f32)> = vec![];
        if request.mode != "lexical" {
            let embedded = if status.semantic_status == "ready" {
                generation
                    .embedder
                    .as_ref()
                    .context("embedding backend unconfigured")
                    .and_then(|e| e.embed(std::slice::from_ref(&request.query), true))
            } else {
                Err(anyhow::anyhow!("embedding index unavailable"))
            };
            match embedded {
                Ok(v) => {
                    for chunk in &allowed {
                        if let Some(vector) = &chunk.vector {
                            semantic.push((
                                chunk.citation.citation_id.clone(),
                                v[0].iter().zip(vector).map(|(a, b)| a * b).sum(),
                            ));
                        }
                    }
                    semantic.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                }
                Err(e) => {
                    if request.mode == "semantic" {
                        return Err(error(
                            "semantic_unavailable",
                            format!("Semantic retrieval unavailable: {e}"),
                        ));
                    }
                    mode_used = "lexical".into();
                    warnings.push(format!(
                        "Semantic retrieval unavailable; lexical results only: {e}"
                    ));
                }
            }
        }
        let mut scores: BTreeMap<String, (f32, Option<f32>, Option<f32>)> = BTreeMap::new();
        for (rank, hit) in lexical.iter().enumerate() {
            let e = scores.entry(hit.path.clone()).or_default();
            e.0 += reciprocal_rank(rank);
            e.1 = Some(hit.score);
        }
        for (rank, (id, score)) in semantic.iter().enumerate() {
            let e = scores.entry(id.clone()).or_default();
            e.0 += reciprocal_rank(rank);
            e.2 = Some(*score);
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        let mut hits = vec![];
        let mut checked = BTreeSet::new();
        for (id, (score, lexical_score, semantic_score)) in ranked {
            let Some(chunk) = allowed.iter().find(|c| c.citation.citation_id == id) else {
                continue;
            };
            if chunk.citation.excerpt.len() > request.max_excerpt_bytes {
                warnings.push(format!(
                    "Excerpt exceeds requested budget: {}",
                    chunk.citation.path
                ));
                continue;
            }
            if checked.insert(chunk.citation.path.clone()) {
                let source = self
                    .source
                    .read_bounded(&chunk.citation.path, MAX_SOURCE_BYTES)
                    .map_err(|_| {
                        error(
                            "source_stale",
                            "A retrieved source disappeared or is unavailable; refresh the index",
                        )
                    })?;
                if source.revision != chunk.citation.revision {
                    let _ = self.trigger.send(false);
                    return Err(error(
                        "source_stale",
                        "A retrieved source changed; refresh the index",
                    ));
                }
            }
            hits.push(SearchResult {
                citation: chunk.citation.clone(),
                title: chunk.title.clone(),
                rank: hits.len() + 1,
                score,
                lexical_score,
                semantic_score,
            });
            if hits.len() == request.limit {
                break;
            }
        }
        let current = self.status();
        if current.status != "ready" || current.generation != status.generation {
            return Err(error(
                "index_stale",
                "Brain changed while searching; retry current index",
            ));
        }
        warnings.sort();
        warnings.dedup();
        Ok(SearchResponse {
            hits,
            mode_requested: request.mode,
            mode_used,
            index: status,
            freshness: "current_at_read".into(),
            warnings,
        })
    }
}

/// Natural-language brain questions use an OR BM25 baseline. The Reader's
/// expressive query parser and default conjunction are unchanged.
pub fn natural_lexical_query(query: &str) -> String {
    let words: BTreeSet<_> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .collect();
    words
        .into_iter()
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}
pub fn validate_scope(scope: &SearchScope) -> Result<()> {
    Uuid::parse_str(&scope.goal_id).context("search requires a valid goal identity")?;
    ensure!(
        matches!(scope.mode.as_str(), "goal" | "project"),
        "invalid knowledge scope"
    );
    if let Some(prefix) = &scope.path_prefix {
        ensure!(
            valid_path(prefix.trim_end_matches('/')),
            "invalid scope prefix"
        );
    }
    ensure!(
        scope.include_paths.len() <= 100 && scope.exclude_paths.len() <= 100,
        "scope path budget exceeded"
    );
    ensure!(
        scope
            .include_paths
            .iter()
            .chain(&scope.exclude_paths)
            .all(|p| valid_path(p)),
        "invalid scope path"
    );
    Ok(())
}
fn in_scope(chunk: &IndexedChunk, scope: &SearchScope) -> bool {
    source_in_scope(&chunk.citation.path, &chunk.citation.metadata, scope)
}
pub(crate) fn source_in_scope(path: &str, metadata: &SourceMetadata, scope: &SearchScope) -> bool {
    record_visible(metadata, &scope.goal_id, &scope.mode)
        && scope.path_prefix.as_ref().is_none_or(|p| {
            path == p.trim_end_matches('/')
                || path.starts_with(&format!("{}/", p.trim_end_matches('/')))
        })
        && (scope.include_paths.is_empty() || scope.include_paths.iter().any(|p| p == path))
        && !scope.exclude_paths.iter().any(|p| p == path)
}
/// A YAML document delimiter must start in column zero. Indented dividers may
/// belong to literal values (including serialized citation excerpts).
pub(crate) fn frontmatter_delimiter(line: &str) -> bool {
    line.trim_end_matches(['\r', '\n', ' ', '\t']) == "---"
}
pub(crate) fn metadata(text: &str) -> (Value, usize) {
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    if lines
        .first()
        .is_some_and(|l| frontmatter_delimiter(l.strip_prefix('\u{feff}').unwrap_or(l)))
    {
        if let Some(end) = lines
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, l)| frontmatter_delimiter(l).then_some(i))
        {
            let value =
                serde_yaml::from_str::<Value>(&lines[1..end].concat()).unwrap_or(Value::Null);
            return (value, end + 1);
        }
    }
    (Value::Null, 0)
}
fn string(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v[*k].as_str().filter(|s| s.len() <= 256).map(str::to_owned))
}
pub fn source_metadata(text: &str) -> SourceMetadata {
    let (m, _) = metadata(text);
    SourceMetadata {
        record_type: string(&m, &["record_type", "type"]),
        status: string(&m, &["status"]),
        verification: string(&m, &["verification"])
            .or_else(|| string(&m["outcome"], &["verification"])),
        observed_at: string(
            &m,
            &[
                "observed_at",
                "received_at",
                "created_at",
                "created",
                "date",
                "updated",
            ],
        ),
        owner_goal_id: if m["record_type"] == "goal" {
            string(&m, &["id"])
        } else {
            string(&m, &["goal_id"])
        },
    }
}
pub fn source_owner(
    text: &str,
    path: &str,
    records_dir: &str,
    brain_id: &str,
) -> Result<Option<String>> {
    let (m, _) = metadata(text);
    if m["record_type"] == "inbox" {
        crate::inbox::validate_record(text, path, records_dir, brain_id)?;
        return Ok(None);
    }
    if m["record_type"] == "proposal" {
        let record: crate::proposal::Record = serde_json::from_value(m)?;
        ensure!(
            record.schema == crate::proposal::SCHEMA
                && record.record_type == "proposal"
                && record.brain_id == brain_id
                && record.goal_id == record.trigger.goal_id
                && record.id == record.trigger.validate(brain_id)?
                && record.verification == "unverified"
                && path == format!("{records_dir}/proposal-{}.md", record.id),
            "invalid canonical proposal ownership"
        );
        return Ok(record.goal_id);
    }
    let owner = if m["record_type"] == "goal" {
        m["id"].as_str()
    } else {
        m["goal_id"].as_str()
    }
    .map(str::to_owned);
    if path.starts_with(&format!("{records_dir}/")) && owner.is_none() {
        bail!("Canonical record has no goal owner: {path}");
    }
    Ok(owner)
}
pub fn record_visible(metadata: &SourceMetadata, goal_id: &str, scope_mode: &str) -> bool {
    let kind = metadata
        .record_type
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        kind.as_str(),
        "inbox"
            | "proposal"
            | "stage"
            | "context"
            | "reviewed-context"
            | "conversation"
            | "operation"
            | "session"
            | "dispatch"
    ) {
        return false;
    }
    match &metadata.owner_goal_id {
        None => true,
        Some(owner) if owner == goal_id => true,
        Some(_) => scope_mode == "project" && matches!(kind.as_str(), "result" | "decision"),
    }
}
pub fn citation_id(path: &str, revision: &str, start: usize, end: usize) -> String {
    format!(
        "c_{}",
        sha(format!("{path}\n{revision}\n{start}\n{end}").as_bytes())
    )
}
fn chunks_for(
    source: &SourceSnapshot,
    records_dir: &str,
) -> Result<(Vec<IndexedChunk>, Vec<String>)> {
    let bytes = STANDARD.decode(&source.content_base64)?;
    let text = String::from_utf8(bytes).context("indexed Markdown must be UTF-8")?;
    let (_, body_start) = metadata(&text);
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let title = lines
        .iter()
        .skip(body_start)
        .find_map(|l| l.strip_prefix("# "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&source.path)
        .to_string();
    let owner = source_owner(&text, &source.path, records_dir, &source.brain_id)?;
    let meta = source_metadata(&text);
    if matches!(
        meta.record_type
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "inbox"
            | "proposal"
            | "stage"
            | "context"
            | "reviewed-context"
            | "conversation"
            | "operation"
            | "session"
            | "dispatch"
    ) {
        return Ok((vec![], vec![]));
    }
    let mut out = vec![];
    let mut warnings = vec![];
    let mut start = body_start;
    while start < lines.len() {
        if lines[start].len() > MAX_EXCERPT_BYTES {
            warnings.push(format!(
                "Oversized source line omitted: {}:{}",
                source.path,
                start + 1
            ));
            start += 1;
            continue;
        }
        let mut end = start;
        let mut size = 0;
        while end < lines.len() && end - start < 16 {
            if size > 0 && size + lines[end].len() > CHUNK_BYTES {
                break;
            }
            size += lines[end].len();
            end += 1;
            if size >= 256 && lines[end - 1].trim().is_empty() {
                break;
            }
        }
        if end == start {
            end += 1;
        }
        let excerpt = lines[start..end].concat();
        if !excerpt.trim().is_empty() {
            out.push(IndexedChunk {
                citation: Citation {
                    citation_id: citation_id(&source.path, &source.revision, start + 1, end),
                    path: source.path.clone(),
                    revision: source.revision.clone(),
                    start_line: start + 1,
                    end_line: end,
                    locator: format!("L{}-L{end}", start + 1),
                    excerpt,
                    metadata: meta.clone(),
                },
                title: title.clone(),
                owner_goal_id: owner.clone(),
                vector: None,
            });
        }
        start = end;
    }
    Ok((out, warnings))
}
pub fn validate_citation(
    source: &SourceSnapshot,
    citation: &Citation,
    goal_id: &str,
    scope_mode: &str,
    records_dir: &str,
) -> Result<()> {
    ensure!(
        valid_path(&citation.path)
            && Path::new(&citation.path)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            && source.path == citation.path
            && source.revision == citation.revision,
        "citation source revision changed"
    );
    ensure!(
        citation.start_line > 0
            && citation.end_line >= citation.start_line
            && citation.excerpt.len() <= MAX_EXCERPT_BYTES,
        "invalid citation bounds"
    );
    ensure!(
        citation.citation_id
            == citation_id(
                &citation.path,
                &citation.revision,
                citation.start_line,
                citation.end_line
            ),
        "citation identity mismatch"
    );
    let bytes = STANDARD.decode(&source.content_base64)?;
    let text = String::from_utf8(bytes)?;
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    ensure!(
        citation.end_line <= lines.len()
            && lines[citation.start_line - 1..citation.end_line].concat() == citation.excerpt,
        "citation excerpt does not match original source lines"
    );
    ensure!(
        citation.locator == format!("L{}-L{}", citation.start_line, citation.end_line)
            && citation.metadata == source_metadata(&text),
        "citation provenance mismatch"
    );
    source_owner(&text, &citation.path, records_dir, &source.brain_id)?;
    ensure!(
        record_visible(&citation.metadata, goal_id, scope_mode),
        "source record is outside the selected knowledge scope"
    );
    Ok(())
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

// Bound total tokenization work as well as per-passage context. Keep original
// passages whole: each accepted passage already fits MAX_EXCERPT_BYTES.
fn embedding_batches<'a>(missing: &'a [usize], chunks: &[IndexedChunk]) -> Vec<&'a [usize]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (offset, index) in missing.iter().enumerate() {
        let len = chunks[*index].citation.excerpt.len();
        if offset > start && (offset - start >= 8 || bytes + len > MAX_EXCERPT_BYTES) {
            batches.push(&missing[start..offset]);
            start = offset;
            bytes = 0;
        }
        bytes += len;
    }
    if start < missing.len() {
        batches.push(&missing[start..]);
    }
    batches
}

fn validate_cached_vectors(cache: &CachedIndex) -> Result<()> {
    if let Some(model) = &cache.model {
        ensure!(
            (1..=4096).contains(&model.dimensions),
            "derived cache model dimensions invalid"
        );
        for chunk in &cache.chunks {
            let Some(vector) = chunk.vector.as_ref() else {
                continue;
            };
            ensure!(
                vector.len() == model.dimensions && vector.iter().all(|v| v.is_finite()),
                "derived cache vector shape invalid"
            );
            let squared_norm = vector.iter().map(|v| v * v).sum::<f32>();
            ensure!(
                (squared_norm - 1.0).abs() < 0.001,
                "derived cache vector normalization invalid"
            );
        }
    }
    Ok(())
}

// Rank-zero API uses one-based reciprocal ranks. No corpus-dependent constant:
// rank 1 evidence must not be buried solely by weak agreement in both tails.
fn reciprocal_rank(zero_based: usize) -> f32 {
    1.0 / (zero_based as f32 + 1.0)
}
#[cfg(test)]
mod fusion_tests {
    use super::*;
    #[test]
    fn incompatible_or_corrupted_cached_vectors_cannot_be_reused() {
        let model = EmbeddingSettings {
            base_url: "http://127.0.0.1:1".into(),
            model: "fixture".into(),
            digest: "a".repeat(64),
            dimensions: 2,
        }
        .metadata();
        let citation = Citation {
            citation_id: "c_fixture".into(),
            path: "note.md".into(),
            revision: "sha256:fixture".into(),
            start_line: 1,
            end_line: 1,
            locator: "L1-L1".into(),
            excerpt: "fixture\n".into(),
            metadata: SourceMetadata::default(),
        };
        let mut cache = CachedIndex {
            schema: INDEX_SCHEMA.into(),
            brain_id: Uuid::new_v4().to_string(),
            root: "/fixture".into(),
            records_dir: "records".into(),
            generation: Uuid::new_v4().to_string(),
            observed_at: now(),
            model: Some(model),
            documents: BTreeMap::new(),
            chunks: vec![IndexedChunk {
                citation,
                title: "fixture".into(),
                owner_goal_id: None,
                vector: Some(vec![1.0, 0.0]),
            }],
            incoming: Default::default(),
            warnings: vec![],
        };
        assert!(validate_cached_vectors(&cache).is_ok());
        let mut chunks = vec![cache.chunks[0].clone(); 19];
        for (i, chunk) in chunks.iter_mut().enumerate() {
            chunk.citation.excerpt = "x".repeat(if i == 4 { 8192 } else { 1300 });
        }
        let selected = (0..chunks.len()).collect::<Vec<_>>();
        let batches = embedding_batches(&selected, &chunks);
        assert_eq!(batches.concat(), selected);
        assert!(batches.iter().all(|b| b.len() <= 8
            && b.iter()
                .map(|i| chunks[*i].citation.excerpt.len())
                .sum::<usize>()
                <= 8192));
        assert!(
            batches.iter().any(|b| *b == [4]),
            "maximum accepted passage must remain whole"
        );
        cache.chunks[0].vector = None;
        assert!(
            validate_cached_vectors(&cache).is_ok(),
            "a partial cache may retain its valid vectors"
        );
        for invalid in [
            Some(vec![1.0]),
            Some(vec![0.0, 0.0]),
            Some(vec![f32::NAN, 0.0]),
            Some(vec![4.0, 0.0]),
        ] {
            cache.chunks[0].vector = invalid;
            assert!(validate_cached_vectors(&cache).is_err());
        }
    }
    fn source_fixture(path: &str, text: &str, brain: &str) -> SourceSnapshot {
        SourceSnapshot {
            schema: "ai-brain/v1".into(),
            brain_id: brain.into(),
            path: path.into(),
            revision: format!("sha256:{}", sha(text.as_bytes())),
            content_base64: STANDARD.encode(text.as_bytes()),
            media_type: "text/markdown".into(),
        }
    }
    #[test]
    fn indented_yaml_dividers_preserve_raw_exclusion_and_saved_result_provenance() {
        let goal = Uuid::new_v4().to_string();
        let brain = Uuid::new_v4().to_string();
        for newline in ["\n", "\r\n"] {
            for bom in ["", "\u{feff}"] {
                for kind in ["context", "reviewed-context", "dispatch", "result"] {
                    let text = format!("{bom}--- \t\nsummary: |\n    nested Markdown\n    ---\n    retained summary\nrecord_type: {kind}\ngoal_id: {goal}\nverification: verified\n---\t \n# Saved evidence\nEvidence body.\n").replace('\n', newline);
                    let source = source_fixture("records/record.md", &text, &brain);
                    let meta = source_metadata(&text);
                    assert_eq!(meta.record_type.as_deref(), Some(kind));
                    assert_eq!(meta.owner_goal_id.as_deref(), Some(goal.as_str()));
                    assert_eq!(
                        source_owner(&text, &source.path, "records", &source.brain_id).unwrap(),
                        Some(goal.clone())
                    );
                    let chunks = chunks_for(&source, "records").unwrap().0;
                    if kind == "result" {
                        assert_eq!(chunks.len(), 1, "saved-result positive control");
                        assert_eq!(chunks[0].citation.start_line, 10);
                        assert_eq!(
                            chunks[0].citation.excerpt,
                            format!("# Saved evidence{newline}Evidence body.{newline}")
                        );
                        assert_eq!(
                            chunks[0].citation.metadata.verification.as_deref(),
                            Some("verified")
                        );
                        validate_citation(
                            &source,
                            &chunks[0].citation,
                            &goal,
                            "project",
                            "records",
                        )
                        .unwrap();
                    } else {
                        assert!(chunks.is_empty());
                        let citation = Citation {
                            citation_id: citation_id(&source.path, &source.revision, 10, 11),
                            path: source.path.clone(),
                            revision: source.revision.clone(),
                            start_line: 10,
                            end_line: 11,
                            locator: "L10-L11".into(),
                            excerpt: format!("# Saved evidence{newline}Evidence body.{newline}"),
                            metadata: SourceMetadata::default(),
                        };
                        assert!(
                            validate_citation(&source, &citation, &goal, "project", "records")
                                .is_err()
                        );
                    }
                }
            }
        }
        let malformed = source_fixture(
            "records/broken.md",
            "---\ngoal_id: [invalid\n---\n# Body\n",
            &brain,
        );
        assert!(
            chunks_for(&malformed, "records").is_err(),
            "malformed canonical owner must fail closed"
        );
    }
    #[test]
    fn pre_boundary_fix_cache_is_rejected_and_rebuilt_with_actual_records_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        let operational = temp.path().join("runtime");
        let cache_root = operational.join("derived/brain-index-v1");
        fs::create_dir_all(root.join("records")).unwrap();
        fs::create_dir_all(&cache_root).unwrap();
        fs::create_dir_all(operational.join("source")).unwrap();
        let brain = Uuid::new_v4().to_string();
        let goal = Uuid::new_v4().to_string();
        let raw = format!("---\nsummary: |\n    nested\n    ---\n    tail\nrecord_type: context\ngoal_id: {goal}\n---\n# Raw context\nRAW-CONTEXT-SENTINEL\n");
        let result = format!("---\nrecord_type: result\ngoal_id: {goal}\nverification: verified\n---\n# Saved result\nRESULT-POSITIVE-CONTROL\n");
        fs::write(root.join("records/context.md"), &raw).unwrap();
        fs::write(root.join("records/result.md"), &result).unwrap();
        let source = SourceStore::open(
            &brain,
            &root,
            &operational.join("source"),
            okilum_core::source::WriteBoundary::Managed,
        )
        .unwrap();
        let (trigger, _rx) = mpsc::channel();
        let index = BrainIndex {
            brain_id: brain.clone(),
            root: root.clone(),
            records_dir: "records".into(),
            operational,
            cache_root: cache_root.clone(),
            source,
            view: RwLock::new(View {
                epoch: 0,
                status: IndexStatus::default(),
                generation: None,
            }),
            trigger,
            _watcher: Mutex::new(None),
        };
        let generation = Uuid::new_v4().to_string();
        let sources = index.inventory().unwrap();
        let raw_source = sources
            .iter()
            .find(|s| s.path == "records/context.md")
            .unwrap();
        let cache = CachedIndex {
            schema: "okilum-brain-index/v1".into(),
            brain_id: brain,
            root: root.to_string_lossy().into_owned(),
            records_dir: "records".into(),
            generation: generation.clone(),
            observed_at: now(),
            model: None,
            documents: sources
                .iter()
                .map(|s| (s.path.clone(), s.revision.clone()))
                .collect(),
            chunks: vec![IndexedChunk {
                citation: Citation {
                    citation_id: "old-leaked-chunk".into(),
                    path: raw_source.path.clone(),
                    revision: raw_source.revision.clone(),
                    start_line: 10,
                    end_line: 10,
                    locator: "L10-L10".into(),
                    excerpt: "RAW-CONTEXT-SENTINEL\n".into(),
                    metadata: SourceMetadata::default(),
                },
                title: "Raw context".into(),
                owner_goal_id: None,
                vector: None,
            }],
            incoming: Default::default(),
            warnings: vec![],
        };
        let bytes = serde_json::to_vec(&cache).unwrap();
        fs::create_dir(cache_root.join(&generation)).unwrap();
        fs::write(cache_root.join(&generation).join("index.json"), &bytes).unwrap();
        atomic_json(
            &cache_root.join("current.json"),
            &json!({"generation":generation,"content_sha256":sha(&bytes)}),
        )
        .unwrap();
        assert!(index
            .read_cache()
            .err()
            .unwrap()
            .to_string()
            .contains("identity mismatch"));
        index.refresh(false).unwrap();
        let rebuilt = index.read_cache().unwrap();
        assert_eq!(rebuilt.schema, INDEX_SCHEMA);
        assert_ne!(rebuilt.generation, generation);
        assert_eq!(rebuilt.chunks.len(), 1);
        assert_eq!(rebuilt.chunks[0].citation.path, "records/result.md");
        assert_eq!(
            rebuilt.chunks[0].citation.metadata.owner_goal_id.as_deref(),
            Some(goal.as_str())
        );
        assert!(rebuilt.chunks[0]
            .citation
            .excerpt
            .contains("RESULT-POSITIVE-CONTROL"));
    }
    #[test]
    fn raw_dispatch_records_cannot_be_searched_or_supplied_as_forged_citations() {
        let goal = Uuid::new_v4().to_string();
        let text=format!("---\nrecord_type: dispatch\ngoal_id: {goal}\n---\n# Internal dispatch\nDo not retrieve.\n");
        let source = SourceSnapshot {
            schema: "ai-brain/v1".into(),
            brain_id: Uuid::new_v4().to_string(),
            path: "records/dispatch.md".into(),
            revision: format!("sha256:{}", sha(text.as_bytes())),
            content_base64: STANDARD.encode(text.as_bytes()),
            media_type: "text/markdown".into(),
        };
        assert!(chunks_for(&source, "records").unwrap().0.is_empty());
        let citation = Citation {
            citation_id: citation_id(&source.path, &source.revision, 5, 6),
            path: source.path.clone(),
            revision: source.revision.clone(),
            start_line: 5,
            end_line: 6,
            locator: "L5-L6".into(),
            excerpt: "# Internal dispatch\nDo not retrieve.\n".into(),
            metadata: source_metadata(&text),
        };
        assert!(validate_citation(&source, &citation, &goal, "goal", "records").is_err());
        assert!(validate_citation(&source, &citation, &goal, "project", "records").is_err());
        let mut result = citation.metadata;
        result.record_type = Some("result".into());
        assert!(
            record_visible(&result, &goal, "project"),
            "saved-result positive control"
        );
    }
    #[test]
    fn strong_single_modality_beats_weak_dual_tail_and_top_agreement_wins() {
        let semantic_only = reciprocal_rank(0);
        let weak_dual = reciprocal_rank(4) + reciprocal_rank(5);
        let strong_dual = reciprocal_rank(0) + reciprocal_rank(1);
        assert!(strong_dual > semantic_only && semantic_only > weak_dual);
    }
}

#[cfg(test)]
mod inbox_ownership_tests {
    use super::*;
    #[test]
    fn inbox_has_no_chunks_and_even_exact_citation_is_forbidden_with_knowledge_controls() {
        let brain = Uuid::new_v4().to_string();
        let capture = Uuid::new_v4().to_string();
        let goal = Uuid::new_v4().to_string();
        let record = crate::inbox::Record {
            schema: crate::SCHEMA.into(),
            record_type: "inbox".into(),
            brain_id: brain.clone(),
            id: capture.clone(),
            status: "captured".into(),
            received_at: "2026-09-07T00:00:00Z".into(),
            source: crate::inbox::SourceIdentity {
                channel: "native".into(),
                instance_id: Uuid::new_v4().to_string(),
                account_id: "local".into(),
                actor_id: "operator".into(),
                chat_id: None,
                topic_id: None,
                message_id: Uuid::new_v4().to_string(),
                update_id: Uuid::new_v4().to_string(),
                uri: None,
            },
        };
        let text = format!(
            "---\n{}---\nInbox must not become accepted evidence.\n",
            serde_yaml::to_string(&record).unwrap()
        );
        let snapshot = |path: String, text: &str| SourceSnapshot {
            schema: crate::SCHEMA.into(),
            brain_id: brain.clone(),
            path,
            revision: format!("sha256:{}", sha(text.as_bytes())),
            content_base64: STANDARD.encode(text),
            media_type: "text/markdown".into(),
        };
        let source = snapshot(format!("records/inbox-{capture}.md"), &text);
        assert!(chunks_for(&source, "records").unwrap().0.is_empty());
        let line = text.lines().count();
        let forged = Citation {
            citation_id: citation_id(&source.path, &source.revision, line, line),
            path: source.path.clone(),
            revision: source.revision.clone(),
            start_line: line,
            end_line: line,
            locator: format!("L{line}-L{line}"),
            excerpt: "Inbox must not become accepted evidence.\n".into(),
            metadata: source_metadata(&text),
        };
        assert!(
            validate_citation(&source, &forged, &goal, "project", "records")
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
        for source in [snapshot("note.md".into(),"# Ordinary knowledge\nPositive control.\n"),snapshot("records/result-control.md".into(),&format!("---\nrecord_type: result\ngoal_id: {goal}\n---\n# Saved result\nPositive control.\n"))] {
            let chunks=chunks_for(&source,"records").unwrap().0; assert!(!chunks.is_empty());
            validate_citation(&source,&chunks[0].citation,&goal,"project","records").unwrap();
        }
        for source in [
            snapshot(
                format!("records/inbox-{capture}.md"),
                "---\nrecord_type: decision\n---\nNo goal owner\n",
            ),
            snapshot(
                format!("records/inbox-{capture}.md"),
                &text.replace(&brain, &Uuid::new_v4().to_string()),
            ),
            snapshot("records/other.md".into(), &text),
        ] {
            assert!(chunks_for(&source, "records").is_err());
        }
    }
}

// Compile the same production shell helper only in tests. Serialize its real
// output into the backend wire type; no shell -> backend production dependency.
#[cfg(test)]
#[path = "../../okilum-shell/src/brain/source_context.rs"]
mod source_context_shell;
#[cfg(test)]
mod source_context_oracle {
    use super::*;
    fn observation_runner(brain: &str, goal: &str) -> (tempfile::TempDir, crate::Runner) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        fs::create_dir(temp.path().join("state")).unwrap();
        let mut runner = crate::Runner::open(crate::RunnerConfig {
            brain_id: brain.into(),
            root: temp.path().join("brain"),
            operational_dir: temp.path().join("state"),
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        })
        .unwrap();
        runner
            .create_goal(
                crate::Goal {
                    id: goal.into(),
                    title: "Observation context oracle".into(),
                    status: "active".into(),
                    criteria: vec![crate::Criterion {
                        id: "C1".into(),
                        description: "Inspect historical evidence".into(),
                        requires_human: false,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: std::collections::BTreeMap::new(),
                },
                "# Goal\n".into(),
            )
            .unwrap();
        (temp, runner)
    }
    fn observation_oracle(runner: &crate::Runner, goal: &str, view: &Value) {
        let workspace = runner.workspace_identity();
        assert_eq!(view["history"][0]["active"], false);
        for path in view["source_paths"]["observations"]
            .as_object()
            .unwrap()
            .values()
        {
            let source = runner.read_source(path.as_str().unwrap()).unwrap();
            let snapshot = serde_json::to_value(&source).unwrap();
            let scope = json!({"goal_id":goal,"mode":"project","path_prefix":null,"include_paths":[],"exclude_paths":[]});
            let history =
                source_context_shell::observation_history(&snapshot, &workspace, goal, view)
                    .unwrap();
            let actual = source_context_shell::citation_with_history(
                &snapshot, &workspace, goal, &scope, &history,
            )
            .unwrap();
            let citation: Citation =
                serde_json::from_slice(&serde_json::to_vec(&actual).unwrap()).unwrap();
            validate_citation(&source, &citation, goal, "project", "records").unwrap();
            let typed_scope: SearchScope = serde_json::from_value(scope.clone()).unwrap();
            assert_eq!(
                crate::context::selected_citations(
                    runner,
                    goal,
                    &typed_scope,
                    std::slice::from_ref(&citation)
                )
                .unwrap(),
                vec![source.clone()]
            );
            assert_eq!(
                STANDARD.decode(&source.content_base64).unwrap(),
                citation.excerpt.as_bytes()
            );
            assert_eq!(
                citation.metadata.verification.as_deref(),
                Some("unverified")
            );
            assert!(validate_citation(
                &source,
                &citation,
                &Uuid::new_v4().to_string(),
                "project",
                "records"
            )
            .is_err());
            assert_eq!(
                source_context_shell::preflight(
                    &actual,
                    std::slice::from_ref(&actual),
                    "manual guidance"
                )
                .unwrap(),
                source_context_shell::Addition::AlreadyIncluded
            );
        }
    }
    #[test]
    fn source_context_current_real_maestro_producer_passes_backend_oracle() {
        let goal = Uuid::new_v4().to_string();
        let (_temp, mut runner) = observation_runner(&Uuid::new_v4().to_string(), &goal);
        let mut discovery = crate::maestro::tests::discovery();
        discovery.projects[0].issues[0]
            .approvals
            .push(crate::maestro::Approval {
                id: "current-approval".into(),
                action: "merge_pr".into(),
                status: "pending".into(),
                summary: "Transient approval prose".into(),
                dashboard_url: Some("https://example.test/approval".into()),
            });
        let project = &discovery.projects[0];
        let issue = &project.issues[0];
        runner
            .maestro_link(
                crate::maestro_links::LinkRequest {
                    operation_id: Uuid::new_v4().to_string(),
                    goal_id: goal.clone(),
                    selection_guard: crate::maestro::selection_guard(
                        &discovery.instance,
                        project,
                        issue,
                    ),
                    project_id: project.project_id.clone(),
                    project_name: project.name.clone(),
                    repo: project.repo.clone(),
                    issue_number: issue.number,
                },
                &discovery,
            )
            .unwrap();
        let active = runner.maestro_link_view(&goal).unwrap();
        runner
            .maestro_unlink(crate::maestro_links::UnlinkRequest {
                operation_id: Uuid::new_v4().to_string(),
                goal_id: goal.clone(),
                expected_link_id: active["link"]["id"].as_str().unwrap().into(),
            })
            .unwrap();
        let view = runner.maestro_link_view(&goal).unwrap();
        observation_oracle(&runner, &goal, &view);
        for path in view["source_paths"]["observations"]
            .as_object()
            .unwrap()
            .values()
        {
            let source = runner.read_source(path.as_str().unwrap()).unwrap();
            let raw = String::from_utf8(STANDARD.decode(source.content_base64).unwrap()).unwrap();
            assert!(raw.contains("dashboard_url: null"));
            assert!(!raw.contains("Transient approval prose"));
            assert!(!raw.contains("https://example.test/approval"));
        }
    }
    #[test]
    fn source_context_original_152_producer_golden_passes_backend_oracle() {
        // Generated by original 193812c Runner::maestro_link -> maestro_observe_inner
        // -> queue/flush_writes, then maestro_unlink. No authored shape substitution.
        // Generation evidence: maestro-context267-historical193812c-golden.json
        // SHA256: e3a165a774d3c4187b00ec016106f3fadb6dbdbcd2e2b6fa14ff4011da7525d7
        let golden: Value = serde_json::from_str(HISTORICAL_GOLDEN).unwrap();
        let brain = golden["brain_id"].as_str().unwrap();
        let goal = golden["goal_id"].as_str().unwrap();
        let (temp, runner) = observation_runner(brain, goal);
        let raw = golden["raw"].as_str().unwrap();
        assert!(!raw.contains("dashboard_url"));
        fs::write(
            temp.path()
                .join("brain")
                .join(golden["path"].as_str().unwrap()),
            raw,
        )
        .unwrap();
        observation_oracle(&runner, goal, &golden["view"]);
        let source = runner
            .read_source(golden["path"].as_str().unwrap())
            .unwrap();
        let frontmatter = raw
            .strip_prefix("---\n")
            .unwrap()
            .split_once("---\n")
            .unwrap()
            .0;
        let m: serde_yaml::Value = serde_yaml::from_str(frontmatter).unwrap();
        let parsed: okilum_core::maestro_observation::Observation =
            serde_json::from_value(serde_json::to_value(m).unwrap()).unwrap();
        assert_eq!(parsed.issue.approvals.len(), 1);
        assert_eq!(parsed.issue.approvals[0].dashboard_url, None);
        assert_eq!(source.content_base64, STANDARD.encode(raw));
    }
    const HISTORICAL_GOLDEN: &str = r###"{
  "brain_id": "e7bcaa49-77ce-44b4-8d44-e41345f0adcd",
  "goal_id": "01000000-0000-4000-8000-000000000042",
  "path": "records/maestro-observation-66b1ade9-5ca8-5889-a2a9-88b80368bc55.md",
  "producer_commit": "193812c",
  "raw": "---\nid: 66b1ade9-5ca8-5889-a2a9-88b80368bc55\nlink_id: c49f3f63-80a6-42e1-b0db-687423fd6a82\ngoal_id: 01000000-0000-4000-8000-000000000042\nobserved_at: 2026-09-07T01:00:01Z\nremote_at: 2026-09-07T01:00:00Z\npaused: true\nissue:\n  number: 42\n  title: Observed fixture\n  url: https://example.test/fixture/test/issues/42\n  attempts:\n  - slot: test-1\n    generation: 1\n    started_at: 2026-09-07T00:59:00Z\n    status: running\n    live: true\n    needs_attention: false\n    reason: ''\n    pr_number: null\n    pr_url: null\n  approvals:\n  - id: historical-approval\n    action: merge_pr\n    status: pending\n    summary: ''\nverification: unverified\nschema: ai-brain/v1\nrecord_type: maestro-observation\nbrain_id: e7bcaa49-77ce-44b4-8d44-e41345f0adcd\n---\n# Observed Maestro work — fixture/test #42\n\nObserved at 2026-09-07T01:00:01Z. Verification: unverified.\n\nProject: fixture. This is linked external work, not a Okilum dispatch or completed goal.\n\n[Original issue](<https://example.test/fixture/test/issues/42>)\n\n- Attempt test-1 / generation Some(1): running.\n",
  "view": {
    "controls_enabled": false,
    "goal_id": "01000000-0000-4000-8000-000000000042",
    "history": [
      {
        "active": false,
        "attention_generation": 0,
        "created_at": "2026-09-07T01:00:01Z",
        "error": null,
        "goal_id": "01000000-0000-4000-8000-000000000042",
        "id": "c49f3f63-80a6-42e1-b0db-687423fd6a82",
        "instance": {
          "base_url": "http://127.0.0.1:8786",
          "instance_id": "01000000-0000-4000-8000-000000000151"
        },
        "issue_number": 42,
        "last_remote": "2026-09-07T01:00:00Z",
        "last_seen": "2026-09-07T01:00:01Z",
        "latest": {
          "goal_id": "01000000-0000-4000-8000-000000000042",
          "id": "66b1ade9-5ca8-5889-a2a9-88b80368bc55",
          "issue": {
            "approvals": [
              {
                "action": "merge_pr",
                "id": "historical-approval",
                "status": "pending",
                "summary": "Transient original approval prose"
              }
            ],
            "attempts": [
              {
                "generation": 1,
                "live": true,
                "needs_attention": false,
                "pr_number": null,
                "pr_url": null,
                "reason": "",
                "slot": "test-1",
                "started_at": "2026-09-07T00:59:00Z",
                "status": "running"
              }
            ],
            "number": 42,
            "title": "Observed fixture",
            "url": "https://example.test/fixture/test/issues/42"
          },
          "link_id": "c49f3f63-80a6-42e1-b0db-687423fd6a82",
          "observed_at": "2026-09-07T01:00:01Z",
          "paused": true,
          "remote_at": "2026-09-07T01:00:00Z",
          "verification": "unverified"
        },
        "latest_semantic": "sha256:01d8482662275cc5967271e9c5536951476a47a3596934c24aca663c0f51f0a0",
        "observation_ids": [
          "66b1ade9-5ca8-5889-a2a9-88b80368bc55"
        ],
        "project_id": "01000000-0000-4000-8000-000000000152",
        "project_name": "fixture",
        "repo": "fixture/test",
        "status": "unlinked",
        "transition": 1
      }
    ],
    "link": null,
    "recovery_required": false,
    "schema": "okilum-maestro-observation/v1",
    "source_paths": {
      "link": null,
      "observations": {
        "66b1ade9-5ca8-5889-a2a9-88b80368bc55": "records/maestro-observation-66b1ade9-5ca8-5889-a2a9-88b80368bc55.md"
      }
    }
  }
}"###;
    #[test]
    fn source_context_actual_shell_json_passes_existing_backend_oracle() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::create_dir_all(root.join("records")).unwrap();
        fs::create_dir(temp.path().join("state")).unwrap();
        let brain = Uuid::new_v4().to_string();
        let goal = Uuid::new_v4().to_string();
        let runner = crate::Runner::open(crate::RunnerConfig {
            brain_id: brain.clone(),
            root: root.clone(),
            operational_dir: temp.path().join("state"),
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        })
        .unwrap();
        let workspace = runner.workspace_identity();
        let scope = json!({"goal_id":goal,"mode":"goal","path_prefix":"notes","include_paths":["notes/input.md"],"exclude_paths":[]});
        for raw in [format!("---\ntype: Note\ngoal_id: {goal}\nstatus: Active\n---\nAuthored λ"),format!("\u{feff}--- \t\r\ntype: nOtE\r\ngoal_id: {goal}\r\nverification: unverified\r\nreceived_at: '2026-09-08T12:00:00Z'\r\nassistant_origin: {{model: synthetic}}\r\nsummary: |\r\n    exact nested\r\n    ---\r\n    divider\r\n---\t \r\n  Assistant-derived λ\r\n"),"Unowned project note\n".into()] {
            fs::write(root.join("notes/input.md"),&raw).unwrap();
            let source=SourceSnapshot {schema:"ai-brain/v1".into(),brain_id:brain.clone(),path:"notes/input.md".into(),revision:format!("sha256:{}",sha(raw.as_bytes())),content_base64:STANDARD.encode(&raw),media_type:"text/markdown".into()};
            let actual=source_context_shell::citation(&serde_json::to_value(&source).unwrap(),&workspace,&goal,&scope).unwrap();
            let wire=serde_json::to_vec(&actual).unwrap();
            let citation: Citation=serde_json::from_slice(&wire).unwrap();
            validate_citation(&source,&citation,&goal,"goal","records").unwrap();
            let typed_scope: SearchScope=serde_json::from_value(scope.clone()).unwrap();
            let selected=crate::context::selected_citations(&runner,&goal,&typed_scope,std::slice::from_ref(&citation)).unwrap();
            assert_eq!(selected,vec![source.clone()]);assert_eq!(citation.excerpt,raw);
            let mut excluded=typed_scope.clone();excluded.exclude_paths=vec![source.path.clone()];assert!(crate::context::selected_citations(&runner,&goal,&excluded,std::slice::from_ref(&citation)).is_err());
            let other=Uuid::new_v4().to_string();
            if citation.metadata.owner_goal_id.is_some() {assert!(validate_citation(&source,&citation,&other,"project","records").is_err());}
            let mut changed=source.clone();changed.revision="sha256:changed".into();assert!(validate_citation(&changed,&citation,&goal,"goal","records").is_err());
        }
    }
    #[test]
    fn source_selection_lines_use_production_context_source_admission() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::create_dir_all(root.join("records")).unwrap();
        fs::create_dir(temp.path().join("state")).unwrap();
        let brain = Uuid::new_v4().to_string();
        let goal = Uuid::new_v4().to_string();
        let runner = crate::Runner::open(crate::RunnerConfig {
            brain_id: brain.clone(),
            root: root.clone(),
            operational_dir: temp.path().join("state"),
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        })
        .unwrap();
        let workspace = runner.workspace_identity();
        let scope = json!({"goal_id":goal,"mode":"project","path_prefix":"notes"});
        for size in [45 * 1024, 1024 * 1024, 1024 * 1024 + 1] {
            let prefix =
                "---\r\ntype: Note\r\nverification: unverified\r\n---\r\nSelected 😀 λ\r\n";
            let raw = format!("{prefix}{}", "x".repeat(size - prefix.len()));
            fs::write(root.join("notes/input.md"), &raw).unwrap();
            let source = SourceSnapshot {
                schema: "ai-brain/v1".into(),
                brain_id: brain.clone(),
                path: "notes/input.md".into(),
                revision: format!("sha256:{}", sha(raw.as_bytes())),
                content_base64: STANDARD.encode(&raw),
                media_type: "text/markdown".into(),
            };
            let start = raw.find("Selected").unwrap();
            let result = source_context_shell::selected_lines(
                &serde_json::to_value(&source).unwrap(),
                &workspace,
                &goal,
                &scope,
                start + 1..prefix.len(),
            );
            let typed_scope: SearchScope = serde_json::from_value(scope.clone()).unwrap();
            if size > 1024 * 1024 {
                assert!(result.unwrap_err().contains("1 MiB"));
                let citation = Citation {
                    citation_id: citation_id(&source.path, &source.revision, 5, 5),
                    path: source.path.clone(),
                    revision: source.revision.clone(),
                    start_line: 5,
                    end_line: 5,
                    locator: "L5-L5".into(),
                    excerpt: raw[start..prefix.len()].into(),
                    metadata: source_metadata(&raw),
                };
                validate_citation(&source, &citation, &goal, "project", "records").unwrap();
                assert!(crate::context::selected_citations(
                    &runner,
                    &goal,
                    &typed_scope,
                    &[citation]
                )
                .is_err());
            } else {
                let citation: Citation = serde_json::from_value(result.unwrap()).unwrap();
                validate_citation(&source, &citation, &goal, "project", "records").unwrap();
                let selected = crate::context::selected_citations(
                    &runner,
                    &goal,
                    &typed_scope,
                    std::slice::from_ref(&citation),
                )
                .unwrap();
                assert_eq!(selected, vec![source]);
                assert_eq!(citation.excerpt, "Selected 😀 λ\r\n");
                assert_eq!(
                    citation.metadata.verification.as_deref(),
                    Some("unverified")
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "incoming_references_index_tests.rs"]
mod incoming_reference_tests;

#[cfg(test)]
#[path = "retrieval_readiness_tests.rs"]
mod readiness_tests;
