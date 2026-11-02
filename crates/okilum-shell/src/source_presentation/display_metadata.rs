//! Canonical immutable display inventory. Cold work belongs to classification;
//! native revisions remap proven ranges and inspect only edited complete lines.
use super::*;
use okilum_core::source_projection::revision::{Edit, Text};

#[derive(Clone)]
pub(super) struct Inline {
    range: Range<usize>,
    scope: Range<usize>,
    rendered: Arc<tex::Linear>,
}
#[derive(Clone)]
struct Prose {
    range: Range<usize>,
    inventory: punctuation::Inventory,
}
pub(super) struct Metadata {
    snapshot: Snapshot,
    inline: Vec<Inline>,
    prose: Vec<Prose>,
    indents: Vec<(usize, usize)>,
    tables: Vec<(Range<usize>, u64)>,
    overlay: Arc<Mutex<Option<MathOverlay>>>,
}
fn moved(range: &Range<usize>, edit: &Edit) -> Option<Range<usize>> {
    if range.end <= edit.old.start {
        Some(range.clone())
    } else if range.start >= edit.old.end {
        Some(edit.forward(range.start)..edit.forward(range.end))
    } else {
        None
    }
}
impl Metadata {
    pub(super) fn reusing(mut self, previous: Option<&CachedProvider>) -> Self {
        if let Some(overlay) = previous.and_then(|p| {
            p.display_metadata
                .lock()
                .ok()
                .and_then(|m| m.as_ref().map(|m| m.overlay.clone()))
        }) {
            self.overlay = overlay;
        }
        self
    }
    pub(super) fn ranges_for(&self, source: &SourceSnapshot) -> Vec<Range<usize>> {
        let Some(edit) = source
            .text
            .edit_since(self.snapshot.text().id())
            .map(|e| Edit {
                previous: e.previous,
                old: e.old,
                inserted: e.inserted,
            })
        else {
            return Vec::new();
        };
        self.tables
            .iter()
            .filter_map(|(r, _)| moved(r, &edit))
            .collect()
    }
    pub(super) fn snapshot_id(&self) -> u64 {
        self.snapshot.text().id()
    }
    pub(super) fn new(provider: &CachedProvider, retained: &RetainedPresentation) -> Self {
        let snapshot = retained.snapshot().clone();
        let text = snapshot.text();
        let indents = indent_lines(text, 0..text.len());
        use std::hash::{Hash, Hasher};
        let tables = provider
            .tables
            .iter()
            .map(|(range, bytes)| {
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut hash);
                (range.clone(), hash.finish())
            })
            .collect();
        Self {
            inline: candidates(provider, retained, None),
            prose: retained
                .prose_blocks()
                .map(|(block, _)| Prose {
                    range: block.clone(),
                    inventory: punctuation::Inventory::new(
                        &text.slice(block.clone()),
                        block.start,
                        |range| boundary(retained, range),
                    ),
                })
                .collect(),
            snapshot,
            indents,
            tables,
            overlay: Default::default(),
        }
    }
    pub(super) fn advance(
        &mut self,
        provider: &CachedProvider,
        retained: &RetainedPresentation,
        edit: Option<Edit>,
    ) {
        let next = retained.snapshot();
        if self.snapshot == *next {
            return;
        }
        let Some(edit) = edit.filter(|edit| edit.previous == self.snapshot.text().id()) else {
            // Only standalone legacy callers lack native provenance. Native
            // providers reject before this point instead of discovering bytes.
            *self = Self::new(provider, retained);
            return;
        };
        self.inline = self
            .inline
            .iter()
            .filter_map(|item| {
                Some(Inline {
                    range: moved(&item.range, &edit)?,
                    scope: moved(&item.scope, &edit)?,
                    rendered: item.rendered.clone(),
                })
            })
            .collect();
        self.inline.extend(candidates(
            provider,
            retained,
            Some(edit.old.start..edit.new_end()),
        ));
        self.inline.sort_by_key(|item| item.range.start);
        self.inline.dedup_by(|a, b| a.range == b.range);
        let old_lines = self.snapshot.text().line_bounds(edit.old.start).start
            ..self.snapshot.text().line_bounds(edit.old.end).end;
        let new_lines = old_lines.start..old_lines.end - edit.old.end + edit.new_end();
        self.prose = retained
            .prose_blocks()
            .filter_map(|(block, _)| {
                let old = self.prose.iter().find(|p| {
                    let mapped = moved(&p.range, &edit).or_else(|| {
                        (p.range.start <= edit.old.start && edit.old.end <= p.range.end)
                            .then(|| p.range.start..p.range.end - edit.old.end + edit.new_end())
                    });
                    mapped.as_ref() == Some(block)
                });
                let inventory = if let Some(old) = old {
                    if old.range.end <= old_lines.start || old.range.start >= old_lines.end {
                        old.inventory.shifted(|at| edit.forward(at))
                    } else {
                        let dirty =
                            old_lines.start.max(old.range.start)..old_lines.end.min(old.range.end);
                        let new_end = dirty.end - edit.old.end + edit.new_end();
                        old.inventory.remap(
                            dirty.clone(),
                            &next.text().slice(dirty.start..new_end),
                            &edit,
                            |range| boundary(retained, range),
                        )
                    }
                } else {
                    // A new structural run has no reusable lexical proof. The
                    // retained classifier already bounds local reparses; leave an
                    // oversized new run raw until canonical worker adoption.
                    if block.len() > new_lines.len() + 4096 {
                        return None;
                    }
                    punctuation::Inventory::new(
                        &next.text().slice(block.clone()),
                        block.start,
                        |range| boundary(retained, range),
                    )
                };
                Some(Prose {
                    range: block.clone(),
                    inventory,
                })
            })
            .collect();
        self.indents = self
            .indents
            .iter()
            .filter_map(|&(at, prefix)| {
                if at < old_lines.start {
                    Some((at, prefix))
                } else if at >= old_lines.end {
                    Some((edit.forward(at), prefix))
                } else {
                    None
                }
            })
            .collect();
        self.indents.extend(indent_lines(next.text(), new_lines));
        self.indents.sort_unstable();
        self.indents.dedup();
        self.tables = self
            .tables
            .iter()
            .filter_map(|(range, key)| {
                let outside = range.end < edit.old.start || edit.old.end < range.start;
                let range = moved(range, &edit)?;
                // Receipt provenance also preserves unchanged table boundary
                // bytes. Only an edit at a table edge can invalidate the proof.
                if outside {
                    return Some((range, *key));
                }
                let text = next.text();
                ((range.start == 0 || text.byte(range.start - 1) == Some(b'\n'))
                    && (range.end == text.len()
                        || matches!(text.byte(range.end), Some(b'\r' | b'\n'))))
                .then_some((range, *key))
            })
            .collect();
        self.snapshot = next.clone();
    }
    pub(super) fn indents(&self, projection: &Projection) -> Vec<LineIndent> {
        self.indents
            .iter()
            .map(|&(at, prefix)| LineIndent {
                line: projection.source_line_at(at),
                prefix,
            })
            .collect()
    }
    pub(super) fn blocks(
        &self,
        projection: &Projection,
        active: &ActiveSource,
    ) -> Vec<ProjectionBlock> {
        let selection = active.anchor.0.min(active.head.0)..active.anchor.0.max(active.head.0);
        self.tables
            .iter()
            .filter(|(table, _)| {
                ![
                    Some(selection.clone()),
                    active.composition.as_ref().map(raw_range),
                    active.replacement.as_ref().map(raw_range),
                ]
                .into_iter()
                .flatten()
                .any(|range| range.start <= table.end && table.start <= range.end)
            })
            .map(|(range, key)| ProjectionBlock {
                source: SourceByte(range.start)..SourceByte(range.end),
                lines: projection.source_line_at(range.start)
                    ..projection.source_line_at(range.end) + 1,
                key: *key,
            })
            .collect()
    }
    pub(super) fn overlay(
        &mut self,
        projection: &Projection,
        reveal: &source_classifier::RevealSnapshot,
        retained: &RetainedPresentation,
    ) -> Option<MathOverlay> {
        let map = |at| projection.source_to_display(projection.snapshot(), at).ok();
        let mut formulas: Vec<_> = self
            .inline
            .iter()
            .filter(|item| !reveal.is_raw(projection.snapshot(), &item.scope))
            .filter_map(|item| {
                let (start, end) = (map(item.range.start)?, map(item.range.end)?);
                (start < end).then_some((start..end, item.rendered.clone()))
            })
            .collect();
        for prose in &self.prose {
            if reveal.is_raw(projection.snapshot(), &prose.range) {
                continue;
            }
            let Some((_, markers)) = retained
                .prose_blocks()
                .find(|(block, _)| **block == prose.range)
            else {
                continue;
            };
            formulas.extend(
                prose
                    .inventory
                    .replacements(|range| eligible(retained, markers, range))
                    .into_iter()
                    .filter_map(|(range, glyph)| {
                        let (start, end) = (map(range.start)?, map(range.end)?);
                        (start < end).then(|| {
                            (
                                start..end,
                                Arc::new(tex::Linear {
                                    text: glyph.into(),
                                    runs: Vec::new(),
                                }),
                            )
                        })
                    }),
            );
        }
        formulas.sort_by_key(|(range, _)| range.start);
        // An empty overlay still records its persistent display receipt, so
        // entering/leaving the last formula cannot force cold native layout.
        let mut last = self.overlay.lock().ok()?;
        let overlay =
            MathOverlay::new_persistent(projection.persistent_display(), formulas, last.as_ref());
        *last = Some(overlay.clone());
        Some(overlay)
    }
}
fn indent_lines(text: &Text, range: Range<usize>) -> Vec<(usize, usize)> {
    let fragment = text.slice(range.clone());
    let mut at = range.start;
    fragment
        .split_inclusive('\n')
        .filter_map(|line| {
            let start = at;
            at += line.len();
            let prefix = container_prefix(line.trim_end_matches(['\r', '\n']));
            (prefix > 0).then_some((start, prefix))
        })
        .collect()
}
fn candidates(
    provider: &CachedProvider,
    retained: &RetainedPresentation,
    dirty: Option<Range<usize>>,
) -> Vec<Inline> {
    let snapshot = retained.snapshot();
    let source = snapshot.text();
    let styles = retained.styles();
    let touched = |scope: &Range<usize>| {
        dirty
            .as_ref()
            .is_none_or(|d| d.start <= scope.end && scope.start <= d.end)
    };
    let mut out = Vec::new();
    for style in styles
        .iter()
        .filter(|s| s.style == Style::Math && touched(&s.range))
    {
        let scope = style.range.start - 1..style.range.end + 1;
        let Some(content) = source.get(style.range.clone()) else {
            continue;
        };
        let Some(authored) = source.get(scope.clone()) else {
            continue;
        };
        let rendered = provider.formula(&content).unwrap_or_else(|| {
            Arc::new(tex::Linear {
                text: authored.into_owned(),
                runs: Vec::new(),
            })
        });
        out.push(Inline {
            range: style.range.clone(),
            scope,
            rendered,
        });
    }
    out.sort_by_key(|item| item.range.start);
    out
}

fn boundary(retained: &RetainedPresentation, range: &Range<usize>) -> bool {
    retained
        .validate_active(&Active {
            selection: Some(range.clone()),
            composition: None,
        })
        .is_ok()
}
fn eligible(
    retained: &RetainedPresentation,
    markers: &[Range<usize>],
    range: &Range<usize>,
) -> bool {
    !markers
        .iter()
        .any(|m| m.start < range.end && range.start < m.end)
        && !retained.styles().iter().any(|s| {
            matches!(
                s.style,
                Style::Code | Style::CodeBlock | Style::Math | Style::Definition
            ) && s.range.start < range.end
                && range.start < s.range.end
        })
}
