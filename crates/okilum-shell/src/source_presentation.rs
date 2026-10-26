//! App-owned raw classifier bridge: classify off-thread, compose reviewed source mappings.
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

use gpui_component::input::projection::{
    ActiveSource, ConcealBias, LineScale, MarkerKind, ProjectedByte, ProjectionBlock,
    ProjectionMarker, ProjectionProvider, ProjectionStyle, SourceByte, SourceProjection,
    SourceSnapshot,
};
use okilum_core::{
    source_classifier::{self, Classification, RetainedPresentation, Style},
    source_projection::{Active, Bias, Projection, Snapshot},
};

#[allow(dead_code)] // Full authored metadata is separate from display backend selection.
#[path = "source_presentation/navigation_inventory.rs"]
pub(crate) mod navigation_inventory;

#[allow(dead_code)] // Explicitly unselected until the consumer activation contract is proven.
#[path = "source_presentation/publication_adapter.rs"]
pub(crate) mod publication_adapter;

pub const BODY_FONT: &str = "Noto Sans";
pub const CODE_FONT: &str = "Cascadia Code";

/// App theme colors; they do not participate in parsing or source revisions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectionColors {
    pub heading: gpui::Hsla,
    pub link: gpui::Hsla,
    /// Quiet rows such as reference definitions.
    pub muted: gpui::Hsla,
}

// The standalone projection fixture imports this module without app theme colors.
#[allow(dead_code)]
struct ColoredProvider {
    provider: Arc<CachedProvider>,
    colors: ProjectionColors,
}
impl ProjectionProvider for ColoredProvider {
    fn compose(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        self.provider
            .compose_colored(source, active, Some(self.colors))
    }
}

pub struct CachedProvider {
    source: SourceSnapshot,
    classified: Classification,
    retained: Mutex<RetainedPresentation>,
    links: Vec<source_classifier::NoteLink>,
    /// Rendered blocks (tables, images) of the classified revision, with their
    /// exact bytes, so a later revision can find them before it is classified
    /// (S7, #936).
    tables: Vec<(Range<usize>, String)>,
}

impl CachedProvider {
    /// Reuse the classification when the theme changes; never parse on the UI thread.
    // The standalone projection fixture deliberately uses the unthemed provider.
    #[allow(dead_code)]
    pub fn with_colors(self: Arc<Self>, colors: ProjectionColors) -> Arc<dyn ProjectionProvider> {
        Arc::new(ColoredProvider {
            provider: self,
            colors,
        })
    }
    /// Call only from a background classification task.
    pub fn classify(source: SourceSnapshot) -> Self {
        let links = if source.text.len() <= source_classifier::MAX_BYTES {
            okilum_core::document_links::parse(&source.text)
                .into_iter()
                .map(|link| source_classifier::NoteLink {
                    label: link.range.clone(),
                    range: link.range,
                    target: link.target,
                    wiki: link.wiki,
                })
                .collect()
        } else {
            Vec::new()
        };
        Self::from_links(source, links)
    }
    /// One authored-link producer for the Brain's actual worker. Reading a
    /// completion here checks its immutable input; UI admission remains separate.
    #[allow(dead_code)] // Standalone projection examples still use classify.
    pub(crate) fn classify_navigation(
        request: navigation_inventory::Request,
        input: navigation_inventory::Input,
    ) -> Result<(Self, navigation_inventory::Completion), navigation_inventory::Error> {
        let completion = request.compute();
        let links = completion.view(&input)?.legacy_links().to_vec();
        Ok((Self::from_links(input.source, links), completion))
    }
    fn from_links(source: SourceSnapshot, links: Vec<source_classifier::NoteLink>) -> Self {
        let snapshot = core_snapshot(&source);
        let classified = source_classifier::classify(&snapshot);
        let tables = classified
            .decorations_for(&snapshot)
            .unwrap_or_default()
            .iter()
            .filter(|m| is_block(&m.kind))
            .filter_map(|m| {
                Some((
                    m.range.clone(),
                    source.text.get(m.range.clone())?.to_owned(),
                ))
            })
            .collect();
        Self {
            source,
            retained: Mutex::new(RetainedPresentation::new(&classified)),
            classified,
            links,
            tables,
        }
    }
    // Shared with native_projection216, which only consumes presentation data.
    #[allow(dead_code)]
    pub fn note_links(&self) -> &[source_classifier::NoteLink] {
        &self.links
    }
    // The isolated native example has no limit-status control.
    #[allow(dead_code)]
    pub fn limited(&self) -> bool {
        self.classified.reasons().iter().any(|reason| {
            matches!(
                reason,
                source_classifier::SourceReason::InputLimit
                    | source_classifier::SourceReason::StructureLimit
            )
        })
    }
    pub fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    // Used by the isolated native evidence fixture.
    #[allow(dead_code)]
    pub fn reasons(&self) -> String {
        format!("{:?}", self.classified.reasons())
    }
}

fn core_snapshot(source: &SourceSnapshot) -> Snapshot {
    Snapshot::new(
        source.stamp.document.to_string(),
        source.stamp.generation,
        source.text.clone(),
    )
}
fn raw_range(range: &Range<SourceByte>) -> Range<usize> {
    range.start.0..range.end.0
}

impl ProjectionProvider for CachedProvider {
    fn compose(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        self.compose_colored(source, active, None)
    }
}
impl CachedProvider {
    fn compose_colored(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
        colors: Option<ProjectionColors>,
    ) -> Option<Arc<dyn SourceProjection>> {
        // #214's stricter cap is a presentation limit, not a buffer limit.
        if source.text.len() > source_classifier::MAX_BYTES {
            return None;
        }
        let current = core_snapshot(source);
        let mut retained = self.retained.lock().ok()?;
        if retained.snapshot() != &current {
            // An edit no reparse can prove keeps the blocks above it and
            // draws only the edited block and the rest below as source,
            // never the whole note (#1621).
            *retained = retained
                .remap(&current)
                .or_else(|| retained.remap_raw_tail(&current))?;
        }
        let selection = active.anchor.0.min(active.head.0)..active.anchor.0.max(active.head.0);
        let mut reveal = Active {
            selection: Some(selection),
            composition: active.composition.as_ref().map(raw_range),
        };
        // Independently validate exact native ranges before taking their union.
        // An enclosing union must never hide an invalid/subgrapheme endpoint.
        retained.validate_active(&reveal).ok()?;
        if let Some(replacement) = active.replacement.as_ref().map(raw_range) {
            retained
                .validate_active(&Active {
                    selection: Some(replacement.clone()),
                    composition: None,
                })
                .ok()?;
            reveal.composition = Some(match reveal.composition {
                Some(composition) => {
                    composition.start.min(replacement.start)..composition.end.max(replacement.end)
                }
                None => replacement,
            });
        }
        let reveal_snapshot = retained.prepare_reveal(&reveal).ok()?;
        let projection = reveal_snapshot.projection().clone();
        let styles = projected_styles(&projection, retained.styles(), colors)?;
        let line_scales = heading_line_scales(&projection, retained.styles());
        let blocks = table_blocks(
            &source.text,
            self.current_tables(&current, &source.text),
            std::iter::once(active.anchor.0.min(active.head.0)..active.anchor.0.max(active.head.0))
                .chain(active.composition.as_ref().map(raw_range))
                .chain(active.replacement.as_ref().map(raw_range)),
        );
        Some(Arc::new(MappedProjection {
            source: source.clone(),
            // Retained with the styles, so a revision not yet classified keeps
            // its quote bars, bullets and task boxes (#1295).
            markers: projection_markers(retained.decorations()),
            reveal: reveal_snapshot,
            styles,
            line_scales,
            blocks,
        }))
    }
}

fn projection_markers(
    decorations: &[source_classifier::decorations::Marker],
) -> Vec<ProjectionMarker> {
    decorations
        .iter()
        .filter(|m| !is_block(&m.kind))
        .map(|m| ProjectionMarker {
            range: SourceByte(m.range.start)..SourceByte(m.range.end),
            scope: SourceByte(m.scope.start)..SourceByte(m.scope.end),
            kind: match m.kind {
                source_classifier::decorations::Kind::Unordered { depth } => {
                    MarkerKind::Bullet { depth }
                }
                source_classifier::decorations::Kind::Quote { depth } => {
                    MarkerKind::Quote { depth }
                }
                source_classifier::decorations::Kind::ThematicBreak => MarkerKind::Rule,
                source_classifier::decorations::Kind::CodeBlock => MarkerKind::CodeBlock,
                source_classifier::decorations::Kind::Task { checked } => {
                    MarkerKind::Task { checked }
                }
                source_classifier::decorations::Kind::Table
                | source_classifier::decorations::Kind::Image
                | source_classifier::decorations::Kind::Callout => {
                    unreachable!("filtered")
                }
            },
        })
        .collect()
}

/// Decorations Live Preview draws as rendered blocks: tables, images and
/// callouts.
fn is_block(kind: &source_classifier::decorations::Kind) -> bool {
    matches!(
        kind,
        source_classifier::decorations::Kind::Table
            | source_classifier::decorations::Kind::Image
            | source_classifier::decorations::Kind::Callout
    )
}

impl CachedProvider {
    /// Rendered block ranges (tables, images) in `text`, the current revision. The classified revision
    /// gives them exactly; a newer one, not yet classified, finds each table
    /// by its exact bytes nearest its old place, so tables stay rendered while
    /// typing elsewhere. A table that changed is not found and shows raw.
    fn current_tables(&self, current: &Snapshot, text: &str) -> Vec<Range<usize>> {
        if let Ok(markers) = self.classified.decorations_for(current) {
            return markers
                .iter()
                .filter(|m| is_block(&m.kind))
                .map(|m| m.range.clone())
                .collect();
        }
        let line_start = |at: usize| at == 0 || text.as_bytes()[at - 1] == b'\n';
        let line_end = |at: usize| at == text.len() || matches!(text.as_bytes()[at], b'\n' | b'\r');
        let mut found: Vec<Range<usize>> = self
            .tables
            .iter()
            .filter_map(|(old, bytes)| {
                text.match_indices(bytes.as_str())
                    .map(|(at, _)| at..at + bytes.len())
                    .filter(|range| line_start(range.start) && line_end(range.end))
                    .min_by_key(|range| range.start.abs_diff(old.start))
            })
            .collect();
        found.sort_by_key(|range| range.start);
        found.dedup_by(|later, earlier| later.start < earlier.end);
        found
    }
}

/// Rendered blocks: every table or image not touched by the caret, selection,
/// composition or replacement (edges included), by whole projected lines.
fn table_blocks(
    text: &str,
    tables: Vec<Range<usize>>,
    active: impl Iterator<Item = Range<usize>> + Clone,
) -> Vec<ProjectionBlock> {
    use std::hash::{Hash, Hasher};
    let newlines: Vec<usize> = text.match_indices('\n').map(|(at, _)| at).collect();
    let line_of = |at: usize| newlines.partition_point(|&nl| nl < at);
    tables
        .into_iter()
        .filter(|table| {
            !active
                .clone()
                .any(|range| range.start <= table.end && table.start <= range.end)
        })
        .filter_map(|table| {
            let bytes = text.get(table.clone())?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut hasher);
            Some(ProjectionBlock {
                lines: line_of(table.start)..line_of(table.end) + 1,
                source: SourceByte(table.start)..SourceByte(table.end),
                key: hasher.finish(),
            })
        })
        .collect()
}

/// Where a press on rendered table cell (`row`, `column`) lands in the table's
/// Markdown `source`: the start of the cell's text (#1067). Row 0 is the header
/// row; body rows follow the delimiter row. Cells split on unescaped `|`, as in
/// GFM (also inside code spans). None when the cell is not written out.
#[allow(dead_code)] // The bidi and marker examples include this file without the editor.
pub(crate) fn table_cell_offset(source: &str, row: usize, column: usize) -> Option<usize> {
    let line_ix = if row == 0 { 0 } else { row + 1 };
    let mut start = 0;
    let mut line = None;
    for (ix, text) in source.split_inclusive('\n').enumerate() {
        if ix == line_ix {
            line = Some(text.trim_end_matches(['\n', '\r']));
            break;
        }
        start += text.len();
    }
    let line = line?;
    // Unescaped pipes split the row; a leading and a trailing one are borders.
    let bytes = line.as_bytes();
    let mut pipes = Vec::new();
    let mut escaped = false;
    for (at, &b) in bytes.iter().enumerate() {
        match b {
            b'\\' => escaped = !escaped,
            b'|' if !escaped => pipes.push(at),
            _ => escaped = false,
        }
        if b != b'\\' {
            escaped = false;
        }
    }
    let content_start = line.len() - line.trim_start().len();
    let content_end = line.trim_end().len();
    let mut bounds = vec![content_start];
    for &pipe in &pipes {
        bounds.push(pipe);
    }
    bounds.push(content_end);
    let mut cells: Vec<std::ops::Range<usize>> = bounds
        .windows(2)
        .map(|pair| {
            let from = if pair[0] == content_start && pipes.first() != Some(&pair[0]) {
                pair[0]
            } else {
                pair[0] + 1
            };
            from..pair[1].max(from)
        })
        .collect();
    if pipes.first() == Some(&content_start) {
        cells.remove(0);
    }
    if pipes.last().is_some_and(|&pipe| pipe + 1 == content_end) {
        cells.pop();
    }
    let cell = cells.get(column)?;
    let text = &line[cell.clone()];
    let lead = text.len() - text.trim_start().len();
    let at = if text.trim().is_empty() {
        cell.start + text.len().min(1)
    } else {
        cell.start + lead
    };
    Some(start + at)
}

/// Heading sizes in units of the body font size, shared by the Reader and Live
/// Preview so a note reads the same in both (#1076). docs/design/reader.md:
/// 15.5 body -> H1 30, H2 21, H3 17, H4-H6 body size.
pub(crate) fn heading_scale(level: u8) -> f32 {
    const BODY: f32 = 15.5;
    match level {
        1 => 30. / BODY,
        2 => 21. / BODY,
        3 => 17. / BODY,
        _ => 1.,
    }
}

// Shared conversion consumes a checked read view, not mutable parser authority.
trait PresentationMap {
    fn display_text(&self) -> &str;
    fn map_source(&self, offset: usize) -> Option<usize>;
}
impl PresentationMap for Projection {
    fn display_text(&self) -> &str {
        self.display()
    }
    fn map_source(&self, offset: usize) -> Option<usize> {
        self.source_to_display(self.snapshot(), offset).ok()
    }
}
impl<T: PresentationMap> PresentationMap for Arc<T> {
    fn display_text(&self) -> &str {
        self.as_ref().display_text()
    }
    fn map_source(&self, offset: usize) -> Option<usize> {
        self.as_ref().map_source(offset)
    }
}
impl PresentationMap for okilum_core::source_classifier::presentation::View<'_> {
    fn display_text(&self) -> &str {
        self.display()
    }
    fn map_source(&self, offset: usize) -> Option<usize> {
        self.source_to_display(offset).ok()
    }
}

/// One entry per projected heading line: its scale, and the revealed marker
/// (`## `) that hangs in the margin so the heading text does not move when the
/// caret reveals it.
fn heading_line_scales(
    projection: &impl PresentationMap,
    styles: &[source_classifier::StyleSpan],
) -> Vec<LineScale> {
    let display = projection.display_text();
    let newlines: Vec<usize> = display.match_indices('\n').map(|(at, _)| at).collect();
    let mut scales: Vec<LineScale> = styles
        .iter()
        .filter_map(|style| match style.style {
            Style::Heading(level) => {
                let start = projection.map_source(style.range.start)?;
                let line = newlines.partition_point(|&at| at < start);
                let line_start = line.checked_sub(1).map_or(0, |prev| newlines[prev] + 1);
                let scale = heading_scale(level);
                // Only a bare revealed marker hangs: a quote or list prefix
                // before it stays in the text so per-row markers keep aligning.
                let prefix = &display[line_start..start];
                let marker = prefix.trim_end_matches([' ', '\t']);
                let bare = (1..=6).contains(&marker.len())
                    && marker.bytes().all(|b| b == b'#')
                    && marker.len() < prefix.len();
                let hang = if bare { prefix.len() } else { 0 };
                (scale > 1. || hang > 0).then_some(LineScale { line, scale, hang })
            }
            _ => None,
        })
        .collect();
    scales.sort_by_key(|scale| scale.line);
    scales.dedup_by_key(|scale| scale.line);
    scales
}

struct MappedProjection {
    markers: Vec<ProjectionMarker>,
    source: SourceSnapshot,
    reveal: source_classifier::RevealSnapshot,
    styles: Vec<ProjectionStyle>,
    line_scales: Vec<LineScale>,
    blocks: Vec<ProjectionBlock>,
}
impl SourceProjection for MappedProjection {
    fn source_boundary(&self, current: &SourceSnapshot, offset: SourceByte) -> Option<bool> {
        if self.source.stamp != current.stamp
            || (!Arc::ptr_eq(&self.source.text, &current.text) && self.source.text != current.text)
        {
            return None;
        }
        // Remap/reveal already validated this immutable source index. A mapping
        // query checks it without rebuilding a second whole-source index for
        // marker painting. Source boundaries do not depend on active/reveal.
        let projection = self.reveal.projection();
        match projection.source_to_display(projection.snapshot(), offset.0) {
            Ok(_) => Some(true),
            Err(okilum_core::source_projection::MapError::InvalidBoundary) => Some(false),
            Err(_) => None,
        }
    }

    fn blocks(&self) -> &[ProjectionBlock] {
        &self.blocks
    }
    fn markers(&self) -> &[ProjectionMarker] {
        &self.markers
    }
    fn line_scales(&self) -> &[LineScale] {
        &self.line_scales
    }
    fn marker_scope_is_raw(&self, scope: &Range<SourceByte>) -> bool {
        self.reveal
            .is_raw(self.reveal.projection().snapshot(), &raw_range(scope))
    }

    fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    fn text(&self) -> &str {
        self.reveal.projection().display()
    }
    fn styles(&self) -> &[ProjectionStyle] {
        &self.styles
    }
    fn to_projected(&self, source: SourceByte, _bias: ConcealBias) -> Option<ProjectedByte> {
        self.reveal
            .projection()
            .source_to_display(self.reveal.projection().snapshot(), source.0)
            .ok()
            .map(ProjectedByte)
    }
    fn to_source(&self, projected: ProjectedByte, bias: ConcealBias) -> Option<SourceByte> {
        let bias = match bias {
            ConcealBias::Left => Bias::Left,
            ConcealBias::Right => Bias::Right,
        };
        self.reveal
            .projection()
            .display_to_source(self.reveal.projection().snapshot(), projected.0, bias)
            .ok()
            .map(SourceByte)
    }
}

/// Sweep canonical nested style intervals into disjoint projected metric runs.
fn projected_styles(
    projection: &impl PresentationMap,
    styles: &[source_classifier::StyleSpan],
    colors: Option<ProjectionColors>,
) -> Option<Vec<ProjectionStyle>> {
    let mut events = Vec::new();
    for style in styles {
        let slot = match style.style {
            Style::Strong => 0,
            Style::Heading(_) => 4,
            Style::Emphasis => 1,
            Style::Strike => 2,
            Style::Code => 3,
            Style::Link | Style::WikiLink => 5,
            Style::Definition => 6,
        };
        let start = projection.map_source(style.range.start)?;
        let end = projection.map_source(style.range.end)?;
        if start < end {
            events.extend([(start, slot, 1i32), (end, slot, -1)]);
        }
    }
    events.sort_unstable();
    let mut counts = [0i32; 7];
    let mut runs = Vec::new();
    let mut index = 0;
    while index < events.len() {
        let start = events[index].0;
        while index < events.len() && events[index].0 == start {
            counts[events[index].1] += events[index].2;
            index += 1;
        }
        if counts.iter().any(|&n| n < 0) {
            return None;
        }
        let Some(&(end, _, _)) = events.get(index) else {
            break;
        };
        if start < end && counts.iter().any(|&n| n != 0) {
            runs.push(ProjectionStyle {
                range: ProjectedByte(start)..ProjectedByte(end),
                font_family: (counts[3] > 0).then(|| CODE_FONT.to_owned()),
                color: colors.and_then(|colors| {
                    // Links retain their affordance inside bold headings.
                    if counts[5] > 0 {
                        Some(colors.link)
                    } else if counts[4] > 0 {
                        Some(colors.heading)
                    } else if counts[6] > 0 {
                        Some(colors.muted)
                    } else {
                        None
                    }
                }),
                bold: counts[0] > 0 || counts[4] > 0,
                italic: counts[1] > 0,
                strikethrough: counts[2] > 0,
            });
        }
    }
    Some(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::input::projection::SourceStamp;
    fn source(text: &str) -> SourceSnapshot {
        SourceSnapshot {
            stamp: SourceStamp {
                document: 5,
                generation: 9,
            },
            text: Arc::from(text),
        }
    }
    fn inactive(source: &SourceSnapshot) -> ActiveSource {
        ActiveSource {
            anchor: SourceByte(source.text.len()),
            head: SourceByte(source.text.len()),
            ..ActiveSource::default()
        }
    }

    #[test]
    fn boundary_reuse_preserves_exact_ranges_across_reveal_and_unpainted_edits() {
        use unicode_segmentation::UnicodeSegmentation;

        let original = source("prefix **e\u{301}** 🧠\r\n- [ ] task\n> quote\n");
        let provider = CachedProvider::classify(original.clone());
        let pinned = provider.compose(&original, &inactive(&original)).unwrap();
        let old_display = pinned.text().to_owned();
        let old_markers = format!("{:?}", pinned.markers());
        let mut current = original.clone();
        // No layout/paint between successive revisions: each remap must retain
        // the previous revision's exact semantic ranges, never its boundary lease.
        for prefix in ["x", "xx"] {
            current.stamp.generation += 1;
            current.text = Arc::from(format!("{prefix}{}", original.text));
            let fresh = CachedProvider::classify(current.clone());
            let hidden = provider.compose(&current, &inactive(&current)).unwrap();
            let at = current.text.find("e\u{301}").unwrap();
            let active = ActiveSource {
                anchor: SourceByte(at),
                head: SourceByte(at),
                composition: Some(SourceByte(at)..SourceByte(at + "e\u{301}".len())),
                ..ActiveSource::default()
            };
            let revealed = provider.compose(&current, &active).unwrap();
            assert_ne!(
                hidden.text(),
                revealed.text(),
                "active reveal positive control"
            );
            let boundaries: Vec<_> = current
                .text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain(std::iter::once(current.text.len()))
                .collect();
            for (actual, state) in [(&hidden, inactive(&current)), (&revealed, active)] {
                let expected = fresh.compose(&current, &state).unwrap();
                assert_eq!(actual.text(), expected.text());
                assert_eq!(
                    format!("{:?}", actual.markers()),
                    format!("{:?}", expected.markers())
                );
                for offset in 0..=current.text.len() + 1 {
                    let valid = boundaries.contains(&offset);
                    assert_eq!(
                        actual.source_boundary(&current, SourceByte(offset)),
                        Some(valid)
                    );
                    for bias in [ConcealBias::Left, ConcealBias::Right] {
                        assert_eq!(
                            actual.to_projected(SourceByte(offset), bias),
                            expected.to_projected(SourceByte(offset), bias)
                        );
                    }
                }
                for offset in 0..=actual.text().len() + 1 {
                    for bias in [ConcealBias::Left, ConcealBias::Right] {
                        assert_eq!(
                            actual.to_source(ProjectedByte(offset), bias),
                            expected.to_source(ProjectedByte(offset), bias)
                        );
                    }
                }
                for foreign in [
                    SourceSnapshot {
                        stamp: gpui_component::input::projection::SourceStamp {
                            document: current.stamp.document + 1,
                            ..current.stamp
                        },
                        ..current.clone()
                    },
                    SourceSnapshot {
                        stamp: gpui_component::input::projection::SourceStamp {
                            generation: current.stamp.generation + 1,
                            ..current.stamp
                        },
                        ..current.clone()
                    },
                    SourceSnapshot {
                        text: Arc::from("different bytes"),
                        ..current.clone()
                    },
                ] {
                    assert_eq!(actual.source_boundary(&foreign, SourceByte(0)), None);
                }
                assert_eq!(pinned.source_boundary(&current, SourceByte(0)), None);
            }
            // Retained paint data and its boundary authority remain immutable.
            assert_eq!(pinned.text(), old_display);
            assert_eq!(format!("{:?}", pinned.markers()), old_markers);
            assert_eq!(pinned.source_boundary(&original, SourceByte(0)), Some(true));
            let emoji = original.text.find('🧠').unwrap();
            assert_eq!(
                pinned.source_boundary(&original, SourceByte(emoji + 1)),
                Some(false)
            );
        }
    }
    #[test]
    fn decoration_inventory_follows_edits_and_stays_pinned() {
        let source = source("- list\n\n> quote\n\n***\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let idle = inactive(&source);
        let projection = provider.compose(&source, &idle).unwrap();
        assert_eq!(projection.markers().len(), 3);
        assert_eq!(projection.text(), source.text.as_ref());
        let old_ranges: Vec<_> = projection
            .markers()
            .iter()
            .map(|m| m.range.clone())
            .collect();
        for (step, text) in [
            "- list\n\n> quote\n\n***\n\nend",
            "- list\n\n> quote\n\n***\n\nended",
            "- listing\n\n> quote\n\n***\n\nended",
        ]
        .into_iter()
        .enumerate()
        {
            // Each step is one edit on top of the previous revision.
            let mut current = source.clone();
            current.stamp.generation += 1 + step as u64;
            current.text = Arc::from(text);
            // An unclassified revision draws the markers a fresh parse would.
            let updated = provider.compose(&current, &inactive(&current)).unwrap();
            let fresh = CachedProvider::classify(current.clone());
            let expected = fresh.compose(&current, &inactive(&current)).unwrap();
            let ranges = |p: &Arc<dyn SourceProjection>| {
                p.markers()
                    .iter()
                    .map(|m| (m.range.clone(), m.scope.clone(), format!("{:?}", m.kind)))
                    .collect::<Vec<_>>()
            };
            assert_eq!(ranges(&updated), ranges(&expected), "{text:?}");
            // The pinned earlier projection never changes.
            assert_eq!(
                projection
                    .markers()
                    .iter()
                    .map(|m| m.range.clone())
                    .collect::<Vec<_>>(),
                old_ranges
            );
        }
    }

    /// #1295: every keystroke in a list drew one frame with the quote above it
    /// and the list as raw Markdown, until the background parse came back.
    /// Replay the Hedva sequence frame by frame without adoption: each frame
    /// keeps the markers a fresh parse of that revision would draw.
    #[test]
    fn list_edits_keep_neighbouring_markers_on_every_frame_before_adoption() {
        let quote: String = (0..5)
            .map(|i| format!("> Quote line {i} with some words.\n"))
            .collect();
        let base = format!(
            "# Title\n\nParagraph before.\n\n{quote}\n- List item one at depth one\n  - Nested item at depth two\n    - Deep item at depth three\n    - [ ] A task at depth three\n  - Back at depth two\n- Last item at depth one\n\nClosing line.\n"
        );
        let anchor = "  - Nested item at depth two";
        let at = base.find(anchor).unwrap() + anchor.len();
        // Enter keeps the line's indent, then `**x**`, Backspace twice.
        let mut texts = vec![base.clone()];
        let mut text = base.clone();
        text.insert_str(at, "\n  ");
        texts.push(text.clone());
        let mut caret = at + 3;
        for c in ["*", "*", "x", "*", "*"] {
            text.insert_str(caret, c);
            caret += 1;
            texts.push(text.clone());
        }
        for _ in 0..2 {
            caret -= 1;
            text.remove(caret);
            texts.push(text.clone());
        }
        // Undo back to the original, then Redo forward again.
        let undo: Vec<_> = texts.iter().rev().skip(1).cloned().collect();
        let redo: Vec<_> = texts.iter().skip(1).cloned().collect();
        let frames: Vec<_> = texts.iter().cloned().chain(undo).chain(redo).collect();
        let original = source(&frames[0]);
        let provider = CachedProvider::classify(original.clone());
        let markers = |p: &Arc<dyn SourceProjection>| {
            p.markers()
                .iter()
                .map(|m| (m.range.clone(), m.kind.clone()))
                .collect::<Vec<_>>()
        };
        // Positive control: the classified revision draws quote and list markers.
        let first = markers(&provider.compose(&original, &inactive(&original)).unwrap());
        assert!(first
            .iter()
            .any(|(_, k)| matches!(k, MarkerKind::Quote { .. })));
        assert!(first
            .iter()
            .any(|(_, k)| matches!(k, MarkerKind::Bullet { .. })));
        assert!(first
            .iter()
            .any(|(_, k)| matches!(k, MarkerKind::Task { .. })));
        for (step, text) in frames.iter().enumerate().skip(1) {
            let current = SourceSnapshot {
                stamp: SourceStamp {
                    generation: original.stamp.generation + step as u64,
                    ..original.stamp
                },
                text: Arc::from(text.as_str()),
            };
            let frame = provider.compose(&current, &inactive(&current)).unwrap();
            let fresh = CachedProvider::classify(current.clone());
            let expected = fresh.compose(&current, &inactive(&current)).unwrap();
            let debug = |m: Vec<(Range<SourceByte>, MarkerKind)>| format!("{m:?}");
            assert_eq!(
                debug(markers(&frame)),
                debug(markers(&expected)),
                "frame {step}: {text:?}"
            );
        }
    }

    /// #1266: Enter at the end of a list's last item fell back to a full
    /// reclassification and drew the whole note raw for a frame: under a
    /// nested item (the new line keeps its indent) and on a task list right
    /// under an ordered one. Each frame before adoption must project, with
    /// the markers of a fresh parse of that revision and its text on every
    /// line but the one being typed (its own syntax waits for the parse).
    #[test]
    fn enter_at_the_end_of_a_list_projects_every_frame_before_adoption() {
        let base = include_str!("../examples/decoration784/fixture.md");
        for (item, indent) in [
            ("    - Third level", "    "),
            ("- [ ] Task stays unchanged", ""),
        ] {
            let at = base.find(item).unwrap() + item.len();
            let mut texts = vec![base.to_string()];
            let mut text = base.to_string();
            text.insert_str(at, &format!("\n{indent}"));
            texts.push(text.clone());
            let mut caret = at + 1 + indent.len();
            for c in ["*", "*", "x", "*", "*"] {
                text.insert_str(caret, c);
                caret += 1;
                texts.push(text.clone());
            }
            for _ in 0..2 {
                caret -= 1;
                text.remove(caret);
                texts.push(text.clone());
            }
            let undo: Vec<_> = texts.iter().rev().skip(1).cloned().collect();
            let frames: Vec<_> = texts.iter().cloned().chain(undo).collect();
            let original = source(&frames[0]);
            let provider = CachedProvider::classify(original.clone());
            // Projection keeps every source line, so line numbers match.
            let edited = base[..at].matches('\n').count() + 1;
            let shown = |p: &Arc<dyn SourceProjection>| {
                let markers: Vec<_> = p
                    .markers()
                    .iter()
                    .map(|m| (m.range.clone(), m.kind.clone()))
                    .collect();
                let lines: Vec<_> = p
                    .text()
                    .split('\n')
                    .enumerate()
                    .filter(|(line, _)| *line != edited)
                    .map(|(_, text)| text)
                    .collect();
                format!("{lines:?}\n{markers:?}")
            };
            for (step, text) in frames.iter().enumerate().skip(1) {
                let current = SourceSnapshot {
                    stamp: SourceStamp {
                        generation: original.stamp.generation + step as u64,
                        ..original.stamp
                    },
                    text: Arc::from(text.as_str()),
                };
                let frame = provider
                    .compose(&current, &inactive(&current))
                    .unwrap_or_else(|| panic!("{item:?} frame {step} drew raw: {text:?}"));
                let fresh = CachedProvider::classify(current.clone());
                let expected = fresh.compose(&current, &inactive(&current)).unwrap();
                assert_eq!(shown(&frame), shown(&expected), "{item:?} frame {step}");
            }
        }
    }

    /// #1323 (stable gate, muninn): a heading, a quote and a list ending the
    /// note. Typing after the last item, Enter, more text and Undo drew the
    /// whole note raw for a frame: the edit lay past the last block's end.
    #[test]
    fn typing_after_the_last_item_at_the_note_end_projects_every_frame() {
        let base = "# Quote and list\n\n> A quote before the list.\n\n- first item\n- last item";
        for trailing in ["", "\n"] {
            let mut text = format!("{base}{trailing}");
            let mut caret = base.len();
            let mut texts = vec![text.clone()];
            for typed in [" typed", "\n", "new", "\n", "next"] {
                for c in typed.chars() {
                    text.insert(caret, c);
                    caret += c.len_utf8();
                    texts.push(text.clone());
                }
            }
            let undo: Vec<_> = texts.iter().rev().skip(1).cloned().collect();
            let frames: Vec<_> = texts.iter().cloned().chain(undo).collect();
            let original = source(&frames[0]);
            let provider = CachedProvider::classify(original.clone());
            for (step, text) in frames.iter().enumerate().skip(1) {
                let current = SourceSnapshot {
                    stamp: SourceStamp {
                        generation: original.stamp.generation + step as u64,
                        ..original.stamp
                    },
                    text: Arc::from(text.as_str()),
                };
                let frame = provider
                    .compose(&current, &inactive(&current))
                    .unwrap_or_else(|| panic!("frame {step} drew raw: {text:?}"));
                // Heading, quote and first item keep their presentation.
                let fresh = CachedProvider::classify(current.clone());
                let expected = fresh.compose(&current, &inactive(&current)).unwrap();
                let head = |p: &Arc<dyn SourceProjection>| {
                    let lines: Vec<_> = p.text().split('\n').take(5).map(str::to_owned).collect();
                    let markers: Vec<_> = p
                        .markers()
                        .iter()
                        .filter(|m| m.range.start.0 < base.find("- last").unwrap())
                        .map(|m| (m.range.clone(), m.kind.clone()))
                        .collect();
                    format!("{lines:?}\n{markers:?}")
                };
                assert_eq!(head(&frame), head(&expected), "frame {step}: {text:?}");
            }
        }
    }

    /// #1332 (night QA, muninn): a heading, a paragraph, a quote, a list and
    /// another block after the list. Typing after the last item, Enter and
    /// Undo drew the whole note raw for a frame. Undo replays the coalesced
    /// groups (the Enter, then the typed run) as well as single steps.
    #[test]
    fn enter_and_undo_after_a_list_followed_by_a_block_project_every_frame() {
        for after in ["\n\n[[linked]]\n", "\n\nplain after\n"] {
            let base = format!("# Heading\n\npara\n\n> quote\n\n- one\n- two{after}");
            let at = base.find("- two").unwrap() + "- two".len();
            let mut text = base.clone();
            let mut typed = vec![text.clone()];
            for (i, c) in " EDIT\n".chars().enumerate() {
                text.insert(at + i, c);
                typed.push(text.clone());
            }
            let single: Vec<_> = typed.iter().rev().skip(1).cloned().collect();
            let grouped = vec![typed[typed.len() - 2].clone(), base.clone()];
            for undo in [single, grouped] {
                let frames: Vec<_> = typed.iter().cloned().chain(undo).collect();
                let original = source(&frames[0]);
                let provider = CachedProvider::classify(original.clone());
                for (step, text) in frames.iter().enumerate().skip(1) {
                    let current = SourceSnapshot {
                        stamp: SourceStamp {
                            generation: original.stamp.generation + step as u64,
                            ..original.stamp
                        },
                        text: Arc::from(text.as_str()),
                    };
                    let frame = provider
                        .compose(&current, &inactive(&current))
                        .unwrap_or_else(|| panic!("{after:?} frame {step} drew raw: {text:?}"));
                    // Everything above the list item keeps its presentation.
                    let fresh = CachedProvider::classify(current.clone());
                    let expected = fresh.compose(&current, &inactive(&current)).unwrap();
                    let head = |p: &Arc<dyn SourceProjection>| {
                        let lines: Vec<_> =
                            p.text().split('\n').take(7).map(str::to_owned).collect();
                        let markers: Vec<_> = p
                            .markers()
                            .iter()
                            .filter(|m| m.range.start.0 < at - "- two".len())
                            .map(|m| (m.range.clone(), m.kind.clone()))
                            .collect();
                        format!("{lines:?}\n{markers:?}")
                    };
                    assert_eq!(
                        head(&frame),
                        head(&expected),
                        "{after:?} frame {step}: {text:?}"
                    );
                }
            }
        }
    }

    /// #1334 (Windows gate, VM 150): the full parse after a fallback ran for
    /// several frames on a slow host, so every fallback showed. Composing
    /// without ever adopting a parse stands in for an arbitrarily slow one.
    /// The gate's recipe in a note ending with a wiki link paragraph: type
    /// at the end, Enter, Undo, Redo, then Enter before the link.
    #[test]
    fn wiki_link_paragraph_edits_project_every_frame_however_slow_the_parse() {
        let base = "# Heading preview\n\n> Quote neighbour stays rendered.\n\n- First item\n- Last item\n\n[[Linked1]] tail";
        let mut text = base.to_string();
        let mut frames = vec![text.clone()];
        for c in " typed12815".chars() {
            text.push(c);
            frames.push(text.clone());
        }
        let typed = text.clone();
        text.push('\n');
        frames.extend([text.clone(), typed, text.clone()]);
        text.insert(text.find("[[Linked1]]").unwrap(), '\n');
        frames.push(text);
        let original = source(&frames[0]);
        let provider = CachedProvider::classify(original.clone());
        let list = base.find("- Last item").unwrap();
        for (step, text) in frames.iter().enumerate().skip(1) {
            let current = SourceSnapshot {
                stamp: SourceStamp {
                    generation: original.stamp.generation + step as u64,
                    ..original.stamp
                },
                text: Arc::from(text.as_str()),
            };
            let frame = provider
                .compose(&current, &inactive(&current))
                .unwrap_or_else(|| panic!("frame {step} drew raw: {text:?}"));
            // Heading, quote and list keep their presentation.
            let fresh = CachedProvider::classify(current.clone());
            let expected = fresh.compose(&current, &inactive(&current)).unwrap();
            let head = |p: &Arc<dyn SourceProjection>| {
                let lines: Vec<_> = p.text().split('\n').take(6).map(str::to_owned).collect();
                let markers: Vec<_> = p
                    .markers()
                    .iter()
                    .filter(|m| m.range.start.0 <= list)
                    .map(|m| (m.range.clone(), m.kind.clone()))
                    .collect();
                format!("{lines:?}\n{markers:?}")
            };
            assert_eq!(head(&frame), head(&expected), "frame {step}: {text:?}");
        }
    }

    #[test]
    fn quote_markers_reveal_as_their_own_element_not_the_whole_quote() {
        use gpui_component::input::projection::{LayoutStamp, MarkerKind, PinnedProjection};
        let source = source("> first **line**\n> second line\n");
        let provider = CachedProvider::classify(source.clone());
        let stamp = LayoutStamp {
            source: source.stamp,
            presentation: 1,
        };
        let raw_quotes = |caret: usize| {
            let active = ActiveSource {
                anchor: SourceByte(caret),
                head: SourceByte(caret),
                ..ActiveSource::default()
            };
            let pin = PinnedProjection {
                stamp,
                projection: provider.compose(&source, &active).unwrap(),
            };
            pin.projection
                .markers()
                .iter()
                .filter(|m| matches!(m.kind, MarkerKind::Quote { .. }))
                .map(|m| pin.marker_scope_is_raw(stamp, &source, &m.scope, &active))
                .collect::<Vec<_>>()
        };
        let second = source.text.find("> second").unwrap();
        // A caret inside quote text reveals no delimiter, even in a long quote.
        assert_eq!(raw_quotes(second + 6), [false, false]);
        // At its own `> ` element only that delimiter is revealed.
        assert_eq!(raw_quotes(second), [false, true]);
        assert_eq!(raw_quotes(second + 2), [false, true]);
        assert_eq!(raw_quotes(0), [true, false]);
    }

    #[test]
    fn pinned_marker_policy_uses_its_projection_and_only_allows_raw_safety_override() {
        use gpui_component::input::projection::{LayoutStamp, PinnedProjection};
        let source = source("[label](destination) tail e\u{301}");
        let provider = CachedProvider::classify(source.clone());
        let idle = inactive(&source);
        let stamp = LayoutStamp {
            source: source.stamp,
            presentation: 17,
        };
        let pin = PinnedProjection {
            stamp,
            projection: provider.compose(&source, &idle).unwrap(),
        };
        let scope = SourceByte(0)..SourceByte(20);
        let text = pin.projection.text().to_owned();
        assert!(!pin.marker_scope_is_raw(stamp, &source, &scope, &idle));
        let active = ActiveSource {
            anchor: SourceByte(2),
            head: SourceByte(3),
            ..idle.clone()
        };
        assert!(pin.marker_scope_is_raw(stamp, &source, &scope, &active));
        for safety in [
            ActiveSource {
                composition: Some(SourceByte(2)..SourceByte(3)),
                ..idle.clone()
            },
            ActiveSource {
                replacement: Some(SourceByte(2)..SourceByte(3)),
                ..idle.clone()
            },
            ActiveSource {
                composition: Some(SourceByte(usize::MAX)..SourceByte(usize::MAX)),
                ..idle.clone()
            },
        ] {
            assert!(pin.marker_scope_is_raw(stamp, &source, &scope, &safety));
        }
        let next = provider.compose(&source, &active).unwrap();
        assert_ne!(next.text(), text);
        // Adoption elsewhere cannot replace this frame's policy or hit map.
        assert_eq!(pin.projection.text(), text);
        assert!(!pin.marker_scope_is_raw(stamp, &source, &scope, &idle));
        assert!(pin.marker_scope_is_raw(
            LayoutStamp {
                presentation: 18,
                ..stamp
            },
            &source,
            &scope,
            &idle
        ));
        let mut stale = source.clone();
        stale.text = Arc::from("same stamp but wrong source");
        assert!(pin.marker_scope_is_raw(stamp, &stale, &scope, &idle));
        stale = source.clone();
        stale.stamp.generation += 1;
        assert!(pin.marker_scope_is_raw(stamp, &stale, &scope, &idle));
        let inside_grapheme = SourceByte(source.text.len() - 2)..SourceByte(source.text.len());
        assert!(pin.marker_scope_is_raw(stamp, &source, &inside_grapheme, &idle));
    }

    #[test]
    fn cached_projection_maps_raw_source_and_nested_styles() {
        let source = source("***nested*** and `code`\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let projection = provider.compose(&source, &inactive(&source)).unwrap();
        assert_eq!(projection.text(), "nested and code\n\nend");
        assert_eq!(
            projection.styles()[0].range,
            ProjectedByte(0)..ProjectedByte(6)
        );
        assert!(projection.styles()[0].bold && projection.styles()[0].italic);
        assert_eq!(
            projection.styles()[1].font_family.as_deref(),
            Some(CODE_FONT)
        );
        assert_eq!(
            projection.to_source(ProjectedByte(0), ConcealBias::Left),
            Some(SourceByte(0))
        );
        assert_eq!(
            projection.to_source(ProjectedByte(0), ConcealBias::Right),
            Some(SourceByte(3))
        );
    }
    #[test]
    fn themed_links_and_headings_preserve_exact_mapping_and_nested_weight() {
        let source = source("# [Heading](https://example.com)\n\n**[web](https://example.com)** and [[Note|wiki]]\n\nend");
        let provider = Arc::new(CachedProvider::classify(source.clone()));
        let active = inactive(&source);
        let plain = provider.compose(&source, &active).unwrap();
        for colors in [
            ProjectionColors {
                heading: gpui::rgb(0x18202a).into(),
                link: gpui::rgb(0x005ca8).into(),
                muted: gpui::rgb(0x6b7280).into(),
            },
            ProjectionColors {
                heading: gpui::rgb(0xf2f4f8).into(),
                link: gpui::rgb(0x8dc8ff).into(),
                muted: gpui::rgb(0x6b7280).into(),
            },
        ] {
            let projection = provider
                .clone()
                .with_colors(colors)
                .compose(&source, &active)
                .unwrap();
            assert_eq!(projection.text(), plain.text());
            assert_eq!(projection.source().text.as_ref(), source.text.as_ref());
            for label in ["Heading", "web", "wiki"] {
                let offset = projection.text().find(label).unwrap();
                let style = projection
                    .styles()
                    .iter()
                    .find(|style| style.range.start.0 <= offset && offset < style.range.end.0)
                    .unwrap();
                assert_eq!(style.color, Some(colors.link));
                if label != "wiki" {
                    assert!(style.bold);
                }
            }
            for (offset, _) in projection.text().char_indices() {
                for bias in [ConcealBias::Left, ConcealBias::Right] {
                    assert_eq!(
                        projection.to_source(ProjectedByte(offset), bias),
                        plain.to_source(ProjectedByte(offset), bias)
                    );
                }
            }
        }
        let source = self::source("# Plain heading\n\nend");
        let colors = ProjectionColors {
            heading: gpui::rgb(0x18202a).into(),
            link: gpui::rgb(0x005ca8).into(),
            muted: gpui::rgb(0x6b7280).into(),
        };
        let projection = Arc::new(CachedProvider::classify(source.clone()))
            .with_colors(colors)
            .compose(&source, &inactive(&source))
            .unwrap();
        assert!(projection
            .styles()
            .iter()
            .any(|style| style.bold && style.color == Some(colors.heading)));
    }

    #[test]
    fn heading_lines_scale_like_the_reader_and_follow_reveal() {
        let text = "# One\n\nbody\n## Two\n### Three\n#### Four\n##### Five\n###### Six\n";
        let source = source(text);
        let provider = CachedProvider::classify(source.clone());
        let expected = [1u8, 2, 3].map(heading_scale);
        let expected = [(0, expected[0]), (3, expected[1]), (4, expected[2])];
        let scales = |projection: &dyn SourceProjection| {
            projection
                .line_scales()
                .iter()
                .filter(|s| s.scale > 1.)
                .map(|s| (s.line, s.scale))
                .collect::<Vec<_>>()
        };
        let hangs = |projection: &dyn SourceProjection| {
            projection
                .line_scales()
                .iter()
                .filter(|s| s.hang > 0)
                .map(|s| (s.line, s.hang))
                .collect::<Vec<_>>()
        };
        // Concealed markers: the same lines, at the Reader's sizes.
        let idle = provider.compose(&source, &inactive(&source)).unwrap();
        assert_ne!(idle.text(), text, "markers are concealed");
        assert_eq!(scales(idle.as_ref()), expected);
        assert!(
            hangs(idle.as_ref()).is_empty(),
            "a concealed marker has nothing to hang"
        );
        // Revealing `## ` on the caret line keeps every line where it was.
        let caret = SourceByte(text.find("Two").unwrap());
        let revealed = provider
            .compose(
                &source,
                &ActiveSource {
                    anchor: caret,
                    head: caret,
                    ..ActiveSource::default()
                },
            )
            .unwrap();
        assert!(revealed.text().contains("## Two"));
        assert_eq!(scales(revealed.as_ref()), expected);
        // The revealed `## ` hangs in the margin; nothing else does.
        assert_eq!(hangs(revealed.as_ref()), [(3, "## ".len())]);
    }

    #[test]
    fn reference_definitions_keep_their_rows_and_render_muted() {
        let source = source("- see [docs][Id]\n\n[Id]: https://example.com\n");
        let colors = ProjectionColors {
            heading: gpui::rgb(0x18202a).into(),
            link: gpui::rgb(0x005ca8).into(),
            muted: gpui::rgb(0x6b7280).into(),
        };
        let projection = Arc::new(CachedProvider::classify(source.clone()))
            .with_colors(colors)
            .compose(&source, &inactive(&source))
            .unwrap();
        assert_eq!(
            projection.text(),
            "- see docs\n\n[Id]: https://example.com\n"
        );
        let color_at = |label: &str| {
            let offset = projection.text().find(label).unwrap();
            projection
                .styles()
                .iter()
                .find(|style| style.range.start.0 <= offset && offset < style.range.end.0)
                .and_then(|style| style.color)
        };
        assert_eq!(color_at("docs"), Some(colors.link));
        assert_eq!(color_at("[Id]:"), Some(colors.muted));
    }

    #[test]
    fn themed_native_fixture_projects_without_fallback() {
        let source = source(
            r###"# A quiet place to write

Okilum keeps **the exact Markdown** while showing *readable formatting*.

## Links and code

Visit [[Target|another note]] or [the project](https://github.com/BeFeast/okilum).
Keep `inline code`, ~~finished thoughts~~ and Unicode: Привет 🧠 é.

## Plain files, safe edits

Selection, copy and Undo use your original text. Source is one click away.

[This intentionally long link label stays blue while wrapping across the editor width, so the second line must preserve the same color and selection behavior](https://example.com). Ordinary text follows.

---

This paragraph remains ordinary Markdown.
"###,
        );
        let provider = Arc::new(CachedProvider::classify(source.clone()));
        let projection = provider
            .with_colors(ProjectionColors {
                heading: gpui::black(),
                link: gpui::rgb(0x005ca8).into(),
                muted: gpui::rgb(0x6b7280).into(),
            })
            .compose(&source, &inactive(&source))
            .expect("native fixture must project");
        assert!(!projection.text().contains("[[Target|"));
        assert!(!projection.text().contains("**the exact Markdown**"));
    }

    #[test]
    fn foreign_or_inconsistent_identity_falls_back() {
        let source = source("**bold**\n\nend");
        let provider = CachedProvider::classify(source.clone());
        for changed in [
            SourceSnapshot {
                stamp: SourceStamp {
                    document: 6,
                    ..source.stamp
                },
                ..source.clone()
            },
            SourceSnapshot {
                stamp: SourceStamp {
                    generation: 8,
                    ..source.stamp
                },
                ..source.clone()
            },
            SourceSnapshot {
                text: Arc::from("**other**\n\nend"),
                ..source.clone()
            },
        ] {
            assert!(provider.compose(&changed, &inactive(&changed)).is_none());
        }
    }
    #[test]
    fn edits_keep_unchanged_projection_until_background_adoption() {
        let original = source("TOP text\n\n**bold** [label](destination)\n\nend");
        let provider = CachedProvider::classify(original.clone());
        for step in 1..=100 {
            let changed = SourceSnapshot {
                stamp: SourceStamp {
                    generation: original.stamp.generation + step,
                    ..original.stamp
                },
                text: Arc::from(
                    original
                        .text
                        .replace("TOP text", &format!("TOP текст{step}")),
                ),
            };
            let projection = provider.compose(&changed, &inactive(&changed)).unwrap();
            assert!(projection.text().contains("bold label"));
            assert_eq!(projection.source().stamp, changed.stamp);
            assert_eq!(projection.source().text, changed.text);
            let fresh = CachedProvider::classify(changed.clone());
            assert_eq!(
                projection.text(),
                fresh.compose(&changed, &inactive(&changed)).unwrap().text()
            );
        }
    }

    /// #1621: an edit the background classification has not seen never
    /// draws the whole note as source. Blocks above the edited one keep their
    /// rendered form, so their rows keep their height.
    #[test]
    fn unclassified_edits_keep_rendered_blocks_above_the_edit() {
        let note = "# Days Gone\n\n![[cover.png]]\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n\
            - item\n\n```css\n.x { color: red; }\n\n[hidden] { display: none; }\n```\n\n";
        let original = source(note);
        let provider = CachedProvider::classify(original.clone());
        let table = note.find("| a |").unwrap();
        let fence_end = note.rfind("```").unwrap() + 3;
        let list_end = note.find("- item").unwrap() + "- item".len();
        // Enter after the fence, on its closing line, after the list, and at
        // the end of the table's last row.
        for (step, at) in [
            note.len(),
            fence_end,
            list_end,
            note.find("| 1 | 2 |").unwrap() + 9,
        ]
        .into_iter()
        .enumerate()
        {
            let mut text = note.to_owned();
            text.insert(at, '\n');
            let changed = SourceSnapshot {
                stamp: SourceStamp {
                    generation: original.stamp.generation + 1 + step as u64,
                    ..original.stamp
                },
                text: Arc::from(text.as_str()),
            };
            let caret = ActiveSource {
                anchor: SourceByte(at + 1),
                head: SourceByte(at + 1),
                ..ActiveSource::default()
            };
            let projection = provider
                .compose(&changed, &caret)
                .unwrap_or_else(|| panic!("Enter at {at} drew the whole note raw"));
            assert!(projection.text().starts_with("Days Gone\n"), "{at}");
            assert!(
                projection
                    .line_scales()
                    .iter()
                    .any(|s| s.line == 0 && s.scale > 1.),
                "{at}: the heading keeps its size"
            );
            let rendered: Vec<_> = projection
                .blocks()
                .iter()
                .map(|b| b.source.start.0)
                .collect();
            if at > table + 30 {
                assert!(rendered.contains(&table), "{at}: the table stays rendered");
            }
        }
    }

    #[test]
    fn explicit_replacement_reveals_without_changing_selection() {
        let source = source("**first**\n\n**second**\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let active = ActiveSource {
            replacement: Some(SourceByte(3)..SourceByte(4)),
            ..inactive(&source)
        };
        let projection = provider.compose(&source, &active).unwrap();
        assert_eq!(projection.text(), "**first**\n\nsecond\n\nend");
        assert_eq!(active.anchor, SourceByte(source.text.len()));
    }
    #[test]
    fn subgrapheme_composition_cannot_be_hidden_by_replacement_union() {
        let source = source("**e\u{301}**\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let active = ActiveSource {
            composition: Some(SourceByte(3)..SourceByte(5)),
            replacement: Some(SourceByte(0)..SourceByte(7)),
            ..inactive(&source)
        };
        assert!(provider.compose(&source, &active).is_none());
    }
    #[test]
    fn larger_input_is_full_source_fallback() {
        let source = source(&"x".repeat(source_classifier::MAX_BYTES + 1));
        let provider = CachedProvider::classify(source.clone());
        assert!(provider.compose(&source, &inactive(&source)).is_none());
        assert_eq!(source.text.len(), source_classifier::MAX_BYTES + 1);
    }

    #[test]
    fn a_table_cell_press_lands_at_the_start_of_that_cells_text() {
        let table = "| Name | Value |\n| --- | ---: |\n| alpha | `a\\|b` |\n|  | last |\n";
        let at = |row, column| table_cell_offset(table, row, column).map(|at| &table[at..]);
        assert!(at(0, 0).unwrap().starts_with("Name |"));
        assert!(at(0, 1).unwrap().starts_with("Value |"));
        assert!(at(1, 0).unwrap().starts_with("alpha |"));
        // An escaped pipe stays inside its cell.
        assert!(at(1, 1).unwrap().starts_with("`a\\|b` |"));
        // An empty cell lands inside it, after the first space.
        let empty = table_cell_offset(table, 2, 0).unwrap();
        assert_eq!(&table[empty - 2..empty], "| ");
        assert_eq!(&table[empty..empty + 3], " | ");
        assert!(at(2, 1).unwrap().starts_with("last |"));
        assert_eq!(table_cell_offset(table, 2, 2), None, "no third column");
        assert_eq!(table_cell_offset(table, 3, 0), None, "no fourth row");
        // Without border pipes.
        let bare = "a | b\n--|--\nc | d\n";
        assert!(bare[table_cell_offset(bare, 1, 1).unwrap()..].starts_with("d\n"));
        assert!(bare[table_cell_offset(bare, 0, 0).unwrap()..].starts_with("a |"));
    }

    #[test]
    fn shared_navigation_worker_keeps_normal_factory_output_with_one_authored_producer() {
        use super::navigation_inventory as nav;
        for text in [
            "# Heading\n\n[**label**](target.md 'title') [[note|alias]]\n\n- item\n\nend\n",
            "[r]: /u\npara [a](x.md) [b](y.md)\n",
            "| a | b |\n| - | - |\n| [[note|alias]] | [literal](path \"[[not|wiki]]\") |\n\nend\n",
        ] {
            let source = source(text);
            let owner = nav::Owner::new(gpui::EntityId::from(1_u64));
            let input = nav::Input {
                binding: nav::Binding {
                    owner: owner.clone(),
                    workspace: serde_json::json!({"brain_id":"test","root":"/isolated"}),
                    path: "note.md".into(),
                },
                source: source.clone(),
            };
            let mut session = nav::Session::new(owner);
            let request = session.begin(Some(input.clone())).unwrap();
            let ((provider, completion), report) = okilum_core::open_diagnostics::capture(|| {
                CachedProvider::classify_navigation(request, input.clone()).unwrap()
            });
            let (legacy, baseline_work) =
                okilum_core::open_diagnostics::capture(|| CachedProvider::classify(source.clone()));
            assert_eq!(report.counts["link_parse_bytes"], text.len() as u64);
            assert_eq!(report.counts, baseline_work.counts);
            assert_eq!(
                report.phases["parse"].calls,
                baseline_work.phases["parse"].calls
            );
            assert!(report.phases["parse"].calls > 0);
            assert_eq!(
                completion.view(&input).unwrap().parsed_links(),
                okilum_core::document_links::parse(text)
            );
            assert_eq!(provider.note_links(), legacy.note_links());
            assert_eq!(provider.limited(), legacy.limited());
            let active = inactive(&source);
            let actual = provider.compose(&source, &active).unwrap();
            let expected = legacy.compose(&source, &active).unwrap();
            assert_eq!(actual.text(), expected.text());
            assert_eq!(actual.styles(), expected.styles());
            assert_eq!(actual.line_scales(), expected.line_scales());
            assert_eq!(actual.blocks(), expected.blocks());
            assert_eq!(
                format!("{:?}", actual.markers()),
                format!("{:?}", expected.markers())
            );
        }
    }
}
