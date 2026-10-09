//! Pure in-memory target preparation shared by adoption preflight and acceptance.
//! Preparation does not write targets, reserve operations or create receipts.
use super::*;
use okilum_core::source::SourceWrite;

const SCHEMA: &str = "tessera-frozen-context-target/v1";
const MAX_FROZEN_BYTES: usize = 1024 * 1024;
// Must fit the existing context::read source budget, including all metadata.
const MAX_CANONICAL_BYTES: usize = 256 * 1024;

/// Complete destination form after the operator has inspected it. Guidance is
/// already edited text, not an instruction to regenerate the ordinary template.
#[derive(Clone, Debug)]
pub(crate) struct Form {
    pub goal_id: String,
    pub expected_goal_revision: String,
    pub query: String,
    pub scope: SearchScope,
    pub citations: Vec<Citation>,
    pub pinned_citation_ids: Vec<String>,
    pub guidance: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema: String,
    packet_id: String,
    goal_id: String,
    created_at: String,
    revision: String,
    source_write: SourceWrite,
}

/// Only preparation or checked restore can construct a target. Serialize exact
/// source bytes; do not reconstruct them from packet metadata on replay.
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub(crate) struct FrozenTarget(Stored);
impl FrozenTarget {
    pub fn packet_id(&self) -> &str {
        &self.0.packet_id
    }
    pub fn goal_id(&self) -> &str {
        &self.0.goal_id
    }
    pub fn created_at(&self) -> &str {
        &self.0.created_at
    }
    pub fn revision(&self) -> &str {
        &self.0.revision
    }
    pub fn source_write(&self) -> &SourceWrite {
        &self.0.source_write
    }
    pub fn packet(&self) -> Result<ReviewedPacket> {
        from_source(&SourceSnapshot {
            schema: crate::SCHEMA.into(),
            brain_id: self.0.source_write.brain_id.clone(),
            path: self.0.source_write.path.clone(),
            revision: self.0.revision.clone(),
            content_base64: self.0.source_write.content_base64.clone(),
            media_type: "text/markdown".into(),
        })
    }
    /// Restore validates the frozen owner and internal consistency only. It has
    /// no Runner, clock, current goal/source read or ID generator. Freshness for
    /// a *new* adoption operation remains the future coordinator's responsibility.
    pub fn restore(bytes: &[u8], brain_id: &str, records_dir: &str, goal_id: &str) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_FROZEN_BYTES,
            "frozen context target exceeds bound"
        );
        let target = Self(serde_json::from_slice(bytes)?);
        target.validate(brain_id, records_dir, goal_id)?;
        Ok(target)
    }
    fn validate(&self, brain: &str, records: &str, goal: &str) -> Result<()> {
        let stored = &self.0;
        let write = &stored.source_write;
        for id in [brain, goal, &stored.packet_id, &write.operation_id] {
            ensure!(
                Uuid::parse_str(id)?.to_string() == id,
                "invalid frozen target UUID"
            );
        }
        ensure!(
            stored.schema == SCHEMA
                && stored.goal_id == goal
                && write.schema == crate::SCHEMA
                && write.brain_id == brain
                && write.expected_revision.is_none(),
            "frozen target owner/schema/create-only mismatch"
        );
        ensure!(
            !records.is_empty()
                && records
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | ".."))
                && !records.contains(['\\', ':'])
                && write.path == format!("{records}/reviewed-context-{}.md", stored.packet_id),
            "invalid frozen context target path"
        );
        let bytes = STANDARD.decode(&write.content_base64)?;
        crate::proposal::publication_size(
            bytes.len(),
            MAX_CANONICAL_BYTES,
            "frozen context target exceeds canonical bound",
        )?;
        ensure!(
            STANDARD.encode(&bytes) == write.content_base64
                && stored.revision == format!("sha256:{}", retrieval::sha(&bytes)),
            "frozen context target bytes/revision mismatch"
        );
        let text = std::str::from_utf8(&bytes)?;
        let lines: Vec<_> = text.split_inclusive('\n').collect();
        ensure!(
            lines
                .first()
                .is_some_and(|line| retrieval::frontmatter_delimiter(line)),
            "frozen metadata missing"
        );
        let end = lines
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, line)| retrieval::frontmatter_delimiter(line).then_some(i))
            .context("frozen metadata incomplete")?;
        let raw: Value = serde_yaml::from_str(&lines[1..end].concat())?;
        ensure!(
            raw["schema"] == crate::SCHEMA
                && raw["reviewed"] == false
                && raw["reviewed_content_sha256"].is_null(),
            "frozen target must be explicitly unreviewed"
        );
        let packet = self.packet()?;
        ensure!(
            packet.id == stored.packet_id
                && packet.goal_id == goal
                && packet.scope.goal_id == goal
                && packet.created_at == stored.created_at
                && packet.updated_at == stored.created_at
                && !packet.reviewed
                && packet.reviewed_content_sha256.is_none(),
            "frozen context packet metadata mismatch"
        );
        let at = time::OffsetDateTime::parse(
            &stored.created_at,
            &time::format_description::well_known::Rfc3339,
        )?;
        ensure!(
            at.offset() == time::UtcOffset::UTC,
            "frozen context time must be UTC"
        );
        validate_form_fields(
            &packet.query,
            &packet.scope,
            &packet.citations,
            &packet.pinned_citation_ids,
            &packet.text,
        )?;
        Ok(())
    }
}

pub(crate) fn prepare(runner: &Runner, form: Form) -> Result<FrozenTarget> {
    prepare_with_clock(runner, form, retrieval::now)
}
fn prepare_with_clock(
    runner: &Runner,
    form: Form,
    clock: impl FnOnce() -> String,
) -> Result<FrozenTarget> {
    let goal_source = runner.goal_source()?;
    ensure!(
        goal_source.revision == form.expected_goal_revision,
        "goal source changed before context preparation"
    );
    let goal = runner.snapshot()?.goal.context("unknown context goal")?;
    ensure!(
        goal.id == form.goal_id && form.scope.goal_id == goal.id,
        "context form belongs to another goal"
    );
    validate_form_fields(
        &form.query,
        &form.scope,
        &form.citations,
        &form.pinned_citation_ids,
        &form.guidance,
    )?;
    selected_citations(runner, &goal.id, &form.scope, &form.citations)?;
    ensure!(
        runner.goal_source()?.revision == goal_source.revision,
        "goal changed during context preparation"
    );
    let at = clock();
    let packet = ReviewedPacket {
        id: Uuid::new_v4().to_string(),
        revision: String::new(),
        goal_id: goal.id.clone(),
        goal_revision: goal_source.revision,
        goal_definition_sha256: goal_definition(&goal)?,
        query: form.query,
        text: form.guidance,
        citations: form.citations,
        scope: form.scope,
        pinned_citation_ids: form.pinned_citation_ids,
        created_at: at.clone(),
        updated_at: at.clone(),
        reviewed: false,
        reviewed_content_sha256: None,
        stale: false,
        stale_reason: None,
    };
    let owner = runner.workspace_identity();
    let brain = owner["brain_id"]
        .as_str()
        .context("missing brain identity")?;
    let records = owner["records_dir"]
        .as_str()
        .context("missing records directory")?;
    let mut metadata = packet.metadata()?;
    metadata["schema"] = json!(crate::SCHEMA);
    metadata["record_type"] = json!("reviewed-context");
    metadata["brain_id"] = json!(brain);
    let bytes = format!(
        "---\n{}---\n{}",
        serde_yaml::to_string(&metadata)?,
        packet.text
    )
    .into_bytes();
    let target = FrozenTarget(Stored {
        schema: SCHEMA.into(),
        packet_id: packet.id.clone(),
        goal_id: goal.id.clone(),
        created_at: at,
        revision: format!("sha256:{}", retrieval::sha(&bytes)),
        source_write: SourceWrite {
            schema: crate::SCHEMA.into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: brain.into(),
            path: runner.path("reviewed-context", &packet.id),
            expected_revision: None,
            content_base64: STANDARD.encode(bytes),
        },
    });
    target.validate(brain, records, &goal.id)?;
    crate::proposal::publication_size(
        serde_json::to_vec(&target)?.len(),
        MAX_FROZEN_BYTES,
        "frozen context target exceeds bound",
    )?;
    Ok(target)
}
pub(crate) fn validate_form_fields(
    query: &str,
    scope: &SearchScope,
    citations: &[Citation],
    pins: &[String],
    guidance: &str,
) -> Result<()> {
    ensure!(
        !query.trim().is_empty() && query.len() <= 2048,
        "context query is required and bounded"
    );
    retrieval::validate_scope(scope)?;
    ensure!(citations.len() <= 20, "select at most twenty citations");
    for citation in citations {
        ensure!(
            retrieval::valid_path(&citation.path)
                && citation.start_line > 0
                && citation.end_line >= citation.start_line
                && citation.excerpt.len() <= 8192
                && citation.citation_id
                    == retrieval::citation_id(
                        &citation.path,
                        &citation.revision,
                        citation.start_line,
                        citation.end_line
                    )
                && citation.locator == format!("L{}-L{}", citation.start_line, citation.end_line),
            "invalid frozen citation identity/bounds"
        );
        ensure!(
            retrieval::record_visible(&citation.metadata, &scope.goal_id, &scope.mode),
            "frozen citation is outside goal scope"
        );
        ensure!(
            scope
                .path_prefix
                .as_ref()
                .is_none_or(|p| citation.path == p.trim_end_matches('/')
                    || citation
                        .path
                        .starts_with(&format!("{}/", p.trim_end_matches('/'))))
                && (scope.include_paths.is_empty() || scope.include_paths.contains(&citation.path))
                && !scope.exclude_paths.contains(&citation.path),
            "frozen citation is excluded by scope"
        );
    }
    let ids: BTreeSet<_> = citations.iter().map(|c| &c.citation_id).collect();
    ensure!(
        ids.len() == citations.len()
            && pins.iter().all(|id| ids.contains(id))
            && pins.iter().collect::<BTreeSet<_>>().len() == pins.len(),
        "pins must be a unique subset of exact citation identities"
    );
    ensure!(
        !guidance.trim().is_empty()
            && citations
                .iter()
                .try_fold(guidance.len(), |size, c| size.checked_add(c.excerpt.len()))
                .is_some_and(|size| size <= MAX_PACKET_BYTES),
        "context guidance/excerpts exceed 64 KiB or guidance is empty"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
