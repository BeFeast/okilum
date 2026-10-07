use anyhow::{Context as _, Result};
use serde::Serialize;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{Query, QueryParser};
use tantivy::schema::{
    Field, IndexRecordOption, JsonObjectOptions, Schema, TextFieldIndexing, TextOptions, Value,
    FAST, INDEXED, STORED, STRING, TEXT,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::Directory;
use tantivy::DocSet;
use tantivy::{doc, Index, TantivyDocument};

use crate::analyzer;
use crate::facets;
use crate::vault::{Resolution, Vault};

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub score: f32,
    /// Snippet with <b>..</b> around matched terms.
    pub snippet_html: String,
}

/// A caller-read, revision-checked document. Brain retrieval indexes bounded
/// source excerpts through the same schema/analyzer as the read-only Reader.
#[derive(Debug, Clone)]
pub struct SearchDocument {
    pub path: String,
    pub title: String,
    pub text: String,
}

/// A query the engine could not make sense of.
///
/// Separate from "no results" on purpose. The old code caught a parse failure,
/// stripped every non-alphanumeric character and retried, so `tag:widget`
/// against a schema with no tag field quietly became the words `tag` and
/// `widget` and returned confident nonsense — the reader could not tell a
/// filter that matched from a filter that had been thrown away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryError {
    /// What could not be understood, in the reader's own words.
    pub message: String,
    /// Present when the query named a field that does not exist.
    pub unknown_field: Option<String>,
    /// Every field this index actually has, so the caller can say so.
    pub known_fields: Vec<String>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for QueryError {}

/// Fields a reader is never meant to name directly: internal machinery.
const INTERNAL_FIELDS: [&str; 1] = ["path_id"];

/// Every schema field, so one place builds a document whether the index is
/// being created or a single note is being replaced.
#[derive(Clone, Copy)]
struct Fields {
    path: Field,
    /// The exact, untokenized path — what a hit reports and what a single-note
    /// delete keys on.
    path_id: Field,
    title: Field,
    body: Field,
    tag: Field,
    heading: Field,
    links: Field,
    fm: Field,
    date: Field,
}

impl Fields {
    fn from_schema(schema: &Schema) -> Result<Fields> {
        Ok(Fields {
            path: schema.get_field("path")?,
            path_id: schema.get_field("path_id")?,
            title: schema.get_field("title")?,
            body: schema.get_field("body")?,
            tag: schema.get_field("tag")?,
            heading: schema.get_field("heading")?,
            links: schema.get_field("links_to")?,
            fm: schema.get_field("frontmatter")?,
            date: schema.get_field("date")?,
        })
    }
}

pub struct Searcher {
    index: Index,
    f: Fields,
    /// Lazily opened: a read-only reader never pays for a writer, and a writer
    /// takes an exclusive lock on the index directory.
    writer: std::sync::Mutex<Option<tantivy::IndexWriter>>,
    /// Drops after index and writer so Windows handles close before cleanup.
    session: Option<tempfile::TempDir>,
    /// Caller-owned liveness pin for the directory this index was opened from.
    /// Drops last, after every index handle, so a collector never sees the
    /// directory unpinned while this searcher can still read it.
    pin: Option<Box<dyn std::any::Any + Send + Sync>>,
}

/// How much of a body the snippet generator sees. Four times the snippet
/// length on either side of the first match is enough to pick a good fragment
/// and small enough that thirty of them cost nothing.
const SNIPPET_WINDOW: usize = 1500;

/// A `window`-byte slice of `body`, centred on the first occurrence of any of
/// `terms` (case-insensitive), or the head of the body if none is found.
/// Always cut on char boundaries.
fn snippet_window<'a>(body: &'a str, terms: &[String], window: usize) -> &'a str {
    if body.len() <= window {
        return body;
    }
    let lower = body.to_lowercase();
    let hit = terms.iter().filter_map(|t| lower.find(t.as_str())).min();
    let start = match hit {
        // The lowercase copy may differ in byte length from the original for
        // non-ASCII text, so the offset is a hint, clamped and boundary-fixed.
        Some(pos) => pos.saturating_sub(window / 2).min(body.len()),
        None => 0,
    };
    let mut start = start;
    while start > 0 && !body.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + window).min(body.len());
    while end < body.len() && !body.is_char_boundary(end) {
        end += 1;
    }
    &body[start..end]
}

/// Path split into searchable segments, so `path:archive` matches
/// `archive/gamma.md` without the caller writing a prefix query.
fn path_terms(rel: &str) -> String {
    rel.trim_end_matches(".md").replace(['/', '-', '_'], " ")
}

impl Searcher {
    /// Finish all indexing and merge work before publishing an immutable build.
    /// Dropping an IndexWriter only joins indexing workers; merging can retain
    /// directory/file handles after drop, which prevents Windows directory moves.
    pub fn finish_build(mut self) -> Result<()> {
        if let Some(writer) = self
            .writer
            .get_mut()
            .map_err(|_| anyhow::anyhow!("index writer poisoned"))?
            .take()
        {
            writer.wait_merging_threads()?;
        }
        Ok(())
    }

    /// Build (or rebuild) the persistent index from the vault.
    pub fn build(vault: &Vault, index_dir: &Path) -> Result<Searcher> {
        Self::build_cancellable(vault, index_dir, &mut |_| Ok(()))
    }

    /// Build in a caller-owned directory, checking cancellation between documents.
    pub fn build_cancellable(
        vault: &Vault,
        index_dir: &Path,
        checkpoint: &mut impl FnMut(usize) -> Result<()>,
    ) -> Result<Searcher> {
        let checkpoint = std::cell::RefCell::new(checkpoint);
        Self::build_with_checked(
            Some(index_dir),
            |fields| {
                vault
                    .notes
                    .iter()
                    .enumerate()
                    .filter_map(|(count, note)| {
                        if let Err(error) = checkpoint.borrow_mut()(count) {
                            return Some(Err(error));
                        }
                        Self::document_for(fields, vault, &note.path, &note.title).map(Ok)
                    })
                    .collect()
            },
            &mut |count| checkpoint.borrow_mut()(count),
        )
    }

    /// Build the Reader index from the exact bytes used for its cache identity.
    /// The existing resolver supplies `links_to`, just as in `build`.
    pub fn build_snapshot(
        vault: &Vault,
        documents: &[SearchDocument],
        index_dir: &Path,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<Searcher> {
        Self::build_snapshot_at(vault, documents, Some(index_dir), checkpoint)
    }

    /// Rebuildable fallback when the external disk cache cannot be written.
    pub fn build_snapshot_in_memory(
        vault: &Vault,
        documents: &[SearchDocument],
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<Searcher> {
        Self::build_snapshot_at(vault, documents, None, checkpoint)
    }

    fn build_snapshot_at(
        vault: &Vault,
        documents: &[SearchDocument],
        index_dir: Option<&Path>,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<Searcher> {
        let checkpoint = std::cell::RefCell::new(checkpoint);
        Self::build_with_checked(
            index_dir,
            |fields| {
                documents
                    .iter()
                    .filter(|doc| !crate::vault::service_path(Path::new(&doc.path)))
                    .enumerate()
                    .map(|(count, doc)| {
                        checkpoint.borrow_mut()("Parsing searchable text", count)?;
                        Ok(Self::document_for_text(
                            fields, vault, &doc.path, &doc.title, &doc.text,
                        ))
                    })
                    .collect()
            },
            &mut |count| checkpoint.borrow_mut()("Writing search", count),
        )
    }

    /// Index already-read text without reopening canonical files. This lets a
    /// managed source owner enforce revision and no-symlink checks itself.
    pub fn build_documents(documents: &[SearchDocument], index_dir: &Path) -> Result<Searcher> {
        Self::build_with(index_dir, |fields| {
            Ok(documents
                .iter()
                .filter(|doc| !crate::vault::service_path(Path::new(&doc.path)))
                .map(|doc| Self::document_from_text(fields, &doc.path, &doc.title, &doc.text))
                .collect())
        })
    }

    fn build_with(
        index_dir: &Path,
        documents: impl FnOnce(Fields) -> Result<Vec<TantivyDocument>>,
    ) -> Result<Searcher> {
        Self::build_with_checked(Some(index_dir), documents, &mut |_| Ok(()))
    }

    fn build_with_checked(
        index_dir: Option<&Path>,
        documents: impl FnOnce(Fields) -> Result<Vec<TantivyDocument>>,
        checkpoint: &mut impl FnMut(usize) -> Result<()>,
    ) -> Result<Searcher> {
        checkpoint(0)?;
        let location = index_dir
            .map(crate::vault::display_path)
            .unwrap_or_else(|| "memory".into());
        if let Some(index_dir) = index_dir {
            if index_dir.exists() {
                std::fs::remove_dir_all(index_dir)
                    .with_context(|| format!("Remove owned search staging directory {location}"))?;
            }
            std::fs::create_dir_all(index_dir)
                .with_context(|| format!("Create search staging directory {location}"))?;
        }
        let mut schema = Schema::builder();
        // Prose fields go through the bilingual stemming analyzer; identifier
        // fields (paths, tags, links) keep the plain tokenizer — a stemmed tag
        // would turn `tag:archived` into a search for `archiv`.
        let stemmed = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(analyzer::TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        // `path` is indexed twice on purpose. The tokenized copy is what a
        // reader queries — `path:archive` should find a subtree, which an
        // untokenized field can never do. The exact copy keeps one term per
        // note, which is what a single-note update deletes on.
        schema.add_text_field("path", TEXT | STORED);
        schema.add_text_field("path_id", STRING | STORED);
        schema.add_text_field("title", stemmed.clone() | STORED);
        schema.add_text_field("body", stemmed.clone() | STORED);
        schema.add_text_field("tag", TEXT);
        schema.add_text_field("heading", stemmed);
        schema.add_text_field("links_to", TEXT);
        // Frontmatter as JSON, so `frontmatter.title:x` addresses one key rather
        // than matching any note that merely contains the word "title".
        //
        // Named for what the acceptance test asked for, which was written before
        // this existed. A shorter `fm` would have meant editing the spec to fit
        // the code — the wrong direction.
        schema.add_json_field(
            "frontmatter",
            JsonObjectOptions::from(TEXT).set_expand_dots_enabled(),
        );
        schema.add_date_field("date", INDEXED | FAST | STORED);
        let schema = schema.build();

        let index = match index_dir {
            Some(path) => Index::create_in_dir(path, schema)
                .with_context(|| format!("Create Tantivy index metadata in {location}"))?,
            None => Index::create_in_ram(schema),
        };
        index
            .tokenizers()
            .register(analyzer::TOKENIZER, analyzer::analyzer());
        let f = Fields::from_schema(&index.schema())?;
        let mut writer = index.writer(64_000_000).with_context(|| {
            format!("Open Tantivy writer and .tantivy-writer.lock in {location}")
        })?;
        let documents = documents(f)?;
        let count = documents.len();
        for (at, document) in documents.into_iter().enumerate() {
            checkpoint(at)?;
            writer
                .add_document(document)
                .with_context(|| format!("Write searchable document in {location}"))?;
        }
        checkpoint(count)?;
        writer
            .commit()
            .with_context(|| format!("Commit search segments and meta.json in {location}"))?;
        checkpoint(count)?;
        Ok(Searcher {
            index,
            f,
            writer: std::sync::Mutex::new(Some(writer)),
            session: None,
            pin: None,
        })
    }

    /// The index document for one note — the single place that knows how a
    /// note becomes fields. Both the full build and a single-note update go
    /// through here, so they cannot disagree about what gets indexed.
    fn document_for(f: Fields, vault: &Vault, rel: &str, title: &str) -> Option<TantivyDocument> {
        let raw = vault.read_note(rel).ok()?;
        Some(Self::document_for_text(f, vault, rel, title, &raw))
    }

    fn document_for_text(
        f: Fields,
        vault: &Vault,
        rel: &str,
        title: &str,
        raw: &str,
    ) -> TantivyDocument {
        let mut d = Self::document_from_text(f, rel, title, raw);
        // What this note links to, as the resolver actually decided it —
        // not re-derived here, or the index and the reader would disagree
        // about an ambiguous link.
        for target in Vault::outbound_links_in(raw) {
            match vault.resolve_from(&target, rel) {
                Resolution::Resolved { path } => d.add_text(f.links, &path),
                Resolution::Ambiguous { candidates } => {
                    for c in candidates {
                        d.add_text(f.links, &c);
                    }
                }
                Resolution::Unresolved => {}
            }
        }
        d
    }

    fn document_from_text(f: Fields, rel: &str, title: &str, raw: &str) -> TantivyDocument {
        let facets = facets::extract(raw);

        let mut d = doc!(
            f.path => path_terms(rel),
            f.path_id => rel.to_string(),
            f.title => title.to_string(),
            f.body => raw.to_owned(),
        );
        for t in &facets.tags {
            d.add_text(f.tag, t);
        }
        for h in &facets.headings {
            d.add_text(f.heading, h);
        }
        if !facets.frontmatter.is_empty() {
            let map: std::collections::BTreeMap<String, tantivy::schema::OwnedValue> = facets
                .frontmatter
                .iter()
                .map(|(k, v)| (k.clone(), tantivy::schema::OwnedValue::Str(v.clone())))
                .collect();
            d.add_object(f.fm, map);
        }
        if let Some(day) = &facets.date {
            if let Ok(dt) = time::Date::parse(
                day,
                &time::macros::format_description!("[year]-[month]-[day]"),
            ) {
                let odt = dt.with_hms(0, 0, 0).unwrap().assume_utc();
                d.add_date(f.date, tantivy::DateTime::from_utc(odt));
            }
        }
        d
    }

    /// Copy search bytes once per Reader session, never canonical sources or a
    /// completed generation. Call only on a worker with no concurrent writer.
    pub fn fork_session(&self) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("tessera-search-session-")
            .tempdir()?;
        self.copy_committed_to(directory.path())?;
        let mut fork = Self::open(directory.path())?;
        fork.session = Some(directory);
        Ok(fork)
    }

    /// Copy committed search files to a caller-owned fresh directory. No parsing
    /// or canonical note I/O; segment footers and metadata remain unchanged.
    pub fn copy_committed_to(&self, destination: &Path) -> Result<()> {
        // In-memory fallback/session indexes can still have merging workers.
        let writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("index writer poisoned"))?
            .take();
        if let Some(writer) = writer {
            writer.wait_merging_threads()?;
        }
        let managed = self.index.directory();
        let paths = managed.list_managed_files();
        for path in &paths {
            anyhow::ensure!(
                path.components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                    && path.components().count() == 1,
                "Invalid search segment identity"
            );
            if path == Path::new("meta.json") {
                continue;
            }
            // atomic_read preserves the segment footer; open_read strips it.
            std::fs::write(destination.join(path), managed.atomic_read(path)?)?;
        }
        std::fs::write(
            destination.join("meta.json"),
            managed.atomic_read(Path::new("meta.json"))?,
        )?;
        std::fs::write(
            destination.join(".managed.json"),
            serde_json::to_vec(&paths)?,
        )?;
        Ok(())
    }

    pub fn is_session(&self) -> bool {
        self.session.is_some()
    }

    /// Keep `pin` alive exactly as long as this searcher. Forks never inherit
    /// it: a session owns a private copy of the committed files.
    pub fn pinned(mut self, pin: impl std::any::Any + Send + Sync) -> Self {
        self.pin = Some(Box::new(pin));
        self
    }

    /// Commit exactly one source batch using already-read canonical bytes.
    pub fn update_snapshot_batch(
        &self,
        vault: &Vault,
        documents: &[SearchDocument],
        removed: &[String],
    ) -> Result<()> {
        let parsed: Vec<_> = documents
            .iter()
            .map(|document| {
                Self::document_for_text(
                    self.f,
                    vault,
                    &document.path,
                    &document.title,
                    &document.text,
                )
            })
            .collect();
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("index writer poisoned"))?;
        if guard.is_none() {
            // Reader batches are usually one document. Starting one worker per
            // CPU adds latency/memory without parallel source work to perform.
            *guard = Some(self.index.writer_with_num_threads(1, 32_000_000)?);
        }
        let writer = guard.as_mut().unwrap();
        let result = (|| -> Result<()> {
            for path in removed
                .iter()
                .map(String::as_str)
                .chain(documents.iter().map(|document| document.path.as_str()))
            {
                writer.delete_term(tantivy::Term::from_field_text(self.f.path_id, path));
            }
            for document in parsed {
                writer.add_document(document)?;
            }
            writer.commit()?;
            Ok(())
        })();
        if result.is_err() {
            writer.rollback()?;
        }
        result
    }

    /// Re-index one note in place: delete whatever the index holds for `rel`,
    /// add the current file, commit. No rebuild, no rescan of the rest.
    ///
    /// This is what a file watcher calls on every save. At 3949 notes a full
    /// rebuild is 1.35 s — survivable once, not on every keystroke in an
    /// external editor.
    ///
    /// `vault` must already reflect the file on disk: resolution of the note's
    /// outbound links is read from it, and a stale scan would index stale
    /// links. Rescanning the vault is the caller's decision (#6), not this
    /// method's — a watcher knows which change it is reacting to.
    pub fn update_note(&self, vault: &Vault, rel: &str) -> Result<()> {
        let title = vault.note_title(rel);
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("index writer poisoned"))?;
        if guard.is_none() {
            *guard = Some(self.index.writer(32_000_000)?);
        }
        let writer = guard.as_mut().unwrap();
        writer.delete_term(tantivy::Term::from_field_text(self.f.path_id, rel));
        if let Some(document) = Self::document_for(self.f, vault, rel, &title) {
            writer.add_document(document)?;
        }
        writer.commit()?;
        Ok(())
    }

    /// Why `rel` matched `query`: the engine's own scoring explanation, as text.
    ///
    /// `None` when the note is not a hit for that query at all. This is the
    /// difference between "the ranking is wrong" being an argument and being a
    /// shrug — a surprising order can now be read off the terms and fields that
    /// produced it.
    pub fn explain(&self, query: &str, rel: &str) -> Result<Option<String>> {
        let widened = Self::widen_bare_dates(query);
        let parsed = self
            .parser()
            .parse_query(&widened)
            .map_err(|e| anyhow::anyhow!("could not parse the query: {e}"))?;
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        // Find the document address for this note by its exact path term.
        let by_path = tantivy::query::TermQuery::new(
            tantivy::Term::from_field_text(self.f.path_id, rel),
            tantivy::schema::IndexRecordOption::Basic,
        );
        let Some((_, addr)) = searcher
            .search(&by_path, &TopDocs::with_limit(1).order_by_score())?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        // Build the weight against the SAME searcher the address came from.
        // `Query::explain` does this internally too, but going through the
        // weight directly lets a "does not match" come back as `None` instead
        // of tripping a docset assertion deep inside tantivy — which is what the
        // first version of this did.
        let weight = parsed.weight(tantivy::query::EnableScoring::enabled_from_searcher(
            &searcher,
        ))?;
        let segment = searcher.segment_reader(addr.segment_ord);
        // A weight explains only documents it matches; check first. Walk with
        // `advance`, not `seek`: `seek` requires the target to be at or beyond
        // the scorer's current doc, and a fresh scorer whose first match lies
        // past our doc trips tantivy's `debug_assert!(doc <= target)`.
        let mut scorer = weight.scorer(segment, 1.0)?;
        let mut doc = scorer.doc();
        while doc < addr.doc_id {
            doc = scorer.advance();
        }
        if doc != addr.doc_id {
            return Ok(None);
        }
        Ok(Some(weight.explain(segment, addr.doc_id)?.to_pretty_json()))
    }

    /// Remove one note from the index. A deleted file has nothing to add back.
    pub fn remove_note(&self, rel: &str) -> Result<()> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("index writer poisoned"))?;
        if guard.is_none() {
            *guard = Some(self.index.writer(32_000_000)?);
        }
        let writer = guard.as_mut().unwrap();
        writer.delete_term(tantivy::Term::from_field_text(self.f.path_id, rel));
        writer.commit()?;
        Ok(())
    }

    /// Open an existing index (fast startup path).
    pub fn open(index_dir: &Path) -> Result<Searcher> {
        let index = Index::open_in_dir(index_dir)?;
        // The schema names the analyzer; the code has to supply it every time
        // the index is opened, or every stemmed query silently finds nothing.
        index
            .tokenizers()
            .register(analyzer::TOKENIZER, analyzer::analyzer());
        let f = Fields::from_schema(&index.schema())?;
        Ok(Searcher {
            index,
            f,
            writer: std::sync::Mutex::new(None),
            session: None,
            pin: None,
        })
    }

    /// Open if present, else build.
    pub fn open_or_build(vault: &Vault, index_dir: &Path) -> Result<Searcher> {
        if index_dir.join("meta.json").exists() {
            if let Ok(s) = Self::open(index_dir) {
                return Ok(s);
            }
        }
        Self::build(vault, index_dir)
    }

    fn parser(&self) -> QueryParser {
        let mut p = QueryParser::for_index(&self.index, vec![self.f.title, self.f.body]);
        p.set_field_boost(self.f.title, 3.0);
        // Space means AND, as it does in every search box a reader has used.
        // tantivy defaults to OR, which turns `path:archive widget` into
        // "in archive, OR mentions widget anywhere" — a filter that widens the
        // result instead of narrowing it is not a filter.
        p.set_conjunction_by_default();
        p
    }

    /// The parser with one edit of slack on the prose fields.
    ///
    /// Used only as a **fallback** when the exact query finds nothing. Fuzzy
    /// on every query was measured and rejected: it doubled p50 (10 -> 21 ms),
    /// pushed p99 into the gray band (26 -> 79 ms), and — worse — a fuzzy
    /// term query carries no exact term for the snippet generator or
    /// explain() to work from, so every snippet came back blank and every
    /// explanation lost the word that matched. Correctness capabilities C11
    /// and C12 broke to buy C8. A fallback keeps all three.
    ///
    /// Identifier fields stay exact: `tag:archive` fuzzing into `archived` is
    /// a different tag, and paths are exact by definition.
    fn fuzzy_parser(&self) -> QueryParser {
        let mut p = self.parser();
        p.set_field_fuzzy(self.f.title, false, 1, true);
        p.set_field_fuzzy(self.f.body, false, 1, true);
        p
    }

    /// The fields a query may name, read off the live schema.
    ///
    /// Derived, never hand-maintained: a hardcoded list drifts from the schema
    /// and then reports a real field as unknown — which is precisely the
    /// dishonesty this error path exists to remove. It did exactly that for
    /// `date` before this was fixed.
    pub fn queryable_fields(&self) -> Vec<String> {
        self.index
            .schema()
            .fields()
            .map(|(_, e)| e.name().to_string())
            .filter(|n| !INTERNAL_FIELDS.contains(&n.as_str()))
            .collect()
    }

    /// tantivy wants RFC 3339 on both ends of a date range; a reader writes
    /// `date:[2026-01-01 TO 2026-12-31]`. Widen the bare form rather than
    /// making the reader learn the storage format.
    ///
    /// No look-ahead: Rust's `regex` has none, and the first version of this
    /// used `(?![T\d])`, which is a *runtime* pattern error — compiled on every
    /// call and unwrapped, so it panicked every single query. Hence both the
    /// manual boundary check and the compile-once static.
    fn widen_bare_dates(query: &str) -> String {
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = RE.get_or_init(|| regex::Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap());
        let bytes = query.as_bytes();
        let mut out = String::with_capacity(query.len());
        let mut last = 0;
        for m in re.find_iter(query) {
            // Already a datetime (`…T…`) or part of a longer number: leave it.
            let next = bytes.get(m.end()).copied();
            let widen = !matches!(next, Some(b'T') | Some(b't'))
                && !next.is_some_and(|c| c.is_ascii_digit());
            out.push_str(&query[last..m.end()]);
            if widen {
                out.push_str("T00:00:00Z");
            }
            last = m.end();
        }
        out.push_str(&query[last..]);
        out
    }

    /// Search, or say why the query could not be understood.
    ///
    /// A query naming a field this index does not have is an error, not an
    /// excuse to match on the words it was made of.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_scoped(query, limit, None)
    }

    /// Apply exact path ownership before ranking, not after a global top-N cut.
    pub fn search_in_paths(
        &self,
        query: &str,
        limit: usize,
        paths: &[String],
    ) -> Result<Vec<SearchHit>> {
        if paths.is_empty() {
            return Ok(vec![]);
        }
        self.search_scoped(query, limit, Some(paths))
    }

    fn search_scoped(
        &self,
        query: &str,
        limit: usize,
        paths: Option<&[String]>,
    ) -> Result<Vec<SearchHit>> {
        let widened = Self::widen_bare_dates(query);
        let parsed = match self.parser().parse_query(&widened) {
            Ok(q) => q,
            Err(e) => {
                let known = self.queryable_fields();
                let msg = e.to_string();
                // `field:value` where the field is unknown. Reported by name so
                // the caller can suggest the real ones.
                let unknown = query
                    .split_whitespace()
                    .filter_map(|tok| tok.split_once(':').map(|(f, _)| f))
                    .map(|f| f.trim_start_matches(['-', '+', '(']))
                    .find(|f| {
                        !f.is_empty()
                            && !known.iter().any(|k| k == f)
                            && !f
                                .split_once('.')
                                .is_some_and(|(base, _)| known.iter().any(|k| k == base))
                    })
                    .map(str::to_string);
                let message = match &unknown {
                    Some(f) => {
                        format!(
                            "no field named `{f}` — this vault has: {}",
                            known.join(", ")
                        )
                    }
                    None => format!("could not parse the query: {msg}"),
                };
                return Err(QueryError {
                    message,
                    unknown_field: unknown,
                    known_fields: known,
                }
                .into());
            }
        };

        // A fresh reader per search sees the latest commit, which is what an
        // update_note() followed by a search must guarantee. Readers are cheap;
        // a cached one would need reload plumbing for no measured gain yet.
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let scoped = |query: Box<dyn Query>| -> Box<dyn Query> {
            if let Some(paths) = paths {
                let filter = tantivy::query::TermSetQuery::new(
                    paths
                        .iter()
                        .map(|p| tantivy::Term::from_field_text(self.f.path_id, p)),
                );
                Box::new(tantivy::query::BooleanQuery::new(vec![
                    (tantivy::query::Occur::Must, query),
                    (tantivy::query::Occur::Must, Box::new(filter)),
                ]))
            } else {
                query
            }
        };
        let filtered = scoped(parsed.box_clone());
        let mut top = searcher.search(&filtered, &TopDocs::with_limit(limit).order_by_score())?;
        if top.is_empty() {
            // Nothing exact: allow one typo. The exact query is still what the
            // snippet generator highlights against, so a fuzzy hit shows the
            // word as written in the note rather than nothing at all.
            if let Ok(fuzzy) = self.fuzzy_parser().parse_query(&widened) {
                top = searcher
                    .search(&scoped(fuzzy), &TopDocs::with_limit(limit).order_by_score())?;
            }
        }
        let mut snippets = SnippetGenerator::create(&searcher, &*parsed, self.f.body)?;
        snippets.set_max_num_chars(180);
        // Lowercased query words, for locating the snippet window. Stemming
        // is deliberately not applied here: the window only needs to land
        // near a match, and the generator does the precise highlighting.
        let terms_lower: Vec<String> = query
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| w.len() >= 2 && !w.contains(':'))
            .collect();
        let mut hits = Vec::new();
        for (score, addr) in top {
            let retrieved: TantivyDocument = searcher.doc(addr)?;
            let path = retrieved
                .get_first(self.f.path_id)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let title = retrieved
                .get_first(self.f.title)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // Snippet from a bounded window of the body, not the whole note.
            // Measured: search(term, 30) was 20 ms while search(term, 1) was
            // 0.27 ms — the cost was 30 notes' worth of full-body tokenizing
            // through the stemmer, some of them 130 KB, to pick 180 chars.
            // The window is centred on the first occurrence of any query
            // term when one is found, so the snippet still comes from the
            // matching region; otherwise the head of the note.
            let body = retrieved
                .get_first(self.f.body)
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let window = snippet_window(body, &terms_lower, SNIPPET_WINDOW);
            let snippet = snippets.snippet(window);
            hits.push(SearchHit {
                path,
                title,
                score,
                snippet_html: snippet.to_html(),
            });
        }
        Ok(hits)
    }
}
