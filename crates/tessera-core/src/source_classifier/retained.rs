//! Presentation-only edit mapping. Never reuse stale navigation or parser metadata.
use super::{Classification, StyleSpan, MAX_BYTES};
use crate::source_projection::{Active, Plan, Region, Snapshot};
use std::{ops::Range, sync::Arc};

/// One immutable reveal decision and its projection for an exact source revision.
/// Native adapters additionally bind this value to their layout/gesture epoch.
/// Paint must consume this value, never reconstruct policy from a live caret.
#[derive(Clone)]
pub struct RevealSnapshot {
    active: Active,
    projection: Arc<crate::source_projection::Projection>,
}

impl RevealSnapshot {
    pub fn projection(&self) -> &Arc<crate::source_projection::Projection> {
        &self.projection
    }

    /// Stale identities and invalid cluster boundaries conservatively paint raw.
    pub fn is_raw(&self, current: &Snapshot, scope: &Range<usize>) -> bool {
        current != self.projection.snapshot()
            || self
                .projection
                .validate_active(&Active {
                    selection: Some(scope.clone()),
                    composition: None,
                })
                .is_err()
            || self.touches(scope)
    }

    fn touches(&self, scope: &Range<usize>) -> bool {
        [
            self.active.selection.as_ref(),
            self.active.composition.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|r| {
            if r.is_empty() {
                scope.start <= r.start && r.start <= scope.end
            } else {
                r.start < scope.end && scope.start < r.end
            }
        })
    }
}

#[derive(Clone)]
pub struct RetainedPresentation {
    snapshot: Snapshot,
    base:
        Result<Arc<crate::source_projection::Projection>, crate::source_projection::SourceFallback>,
    plan: Plan,
    styles: Vec<StyleSpan>,
    marker_scopes: Vec<(Range<usize>, Range<usize>)>,
    contexts: Vec<Range<usize>>,
}

impl RetainedPresentation {
    pub fn new(classified: &Classification) -> Self {
        Self {
            snapshot: classified.snapshot.clone(),
            base: crate::source_projection::project(
                &classified.snapshot,
                &classified.plan,
                &Active::default(),
            )
            .map(Arc::new),
            plan: classified.plan.clone(),
            styles: classified.styles.clone(),
            marker_scopes: classified.marker_scopes.clone(),
            contexts: classified.contexts.clone(),
        }
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    #[cfg(test)]
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn styles(&self) -> &[StyleSpan] {
        &self.styles
    }

    /// Range validation must not construct another whole-document projection.
    pub fn validate_active(
        &self,
        active: &Active,
    ) -> Result<(), crate::source_projection::SourceFallback> {
        self.base
            .as_ref()
            .map_err(Clone::clone)?
            .validate_active(active)
    }

    /// Reveal the syntactic fragment touched by caret/selection/IME, not every
    /// link in its paragraph. Heading markers have their own small scope.
    pub fn project(
        &self,
        active: &Active,
    ) -> Result<Arc<crate::source_projection::Projection>, crate::source_projection::SourceFallback>
    {
        self.prepare_reveal(active).map(|reveal| reveal.projection)
    }

    /// Prepare projection and marker visibility together. Replacement ranges must
    /// already have been validated and merged into composition by the adapter.
    pub fn prepare_reveal(
        &self,
        active: &Active,
    ) -> Result<RevealSnapshot, crate::source_projection::SourceFallback> {
        self.validate_active(active)?;
        let mut reveal = RevealSnapshot {
            active: active.clone(),
            projection: self.base.clone()?,
        };
        let mut revealed = false;
        let regions = self
            .plan
            .regions()
            .iter()
            .map(|region| match region {
                Region::Source(r) => Region::Source(r.clone()),
                Region::Conceal { block, markers } => Region::Conceal {
                    block: block.clone(),
                    markers: markers
                        .iter()
                        .filter(|marker| {
                            let scope = self
                                .marker_scopes
                                .binary_search_by_key(&marker.start, |(m, _)| m.start)
                                .ok()
                                .map(|i| &self.marker_scopes[i].1)
                                .unwrap_or(marker);
                            let touched = reveal.touches(scope);
                            revealed |= touched;
                            !touched
                        })
                        .cloned()
                        .collect(),
                },
            })
            .collect();
        if !revealed {
            return Ok(reveal);
        }
        reveal.projection = Arc::new(crate::source_projection::project(
            &self.snapshot,
            &Plan::new(&self.snapshot, regions),
            &Active::default(),
        )?);
        Ok(reveal)
    }

    /// Map proven inline edits, or classify a bounded structural edit inside
    /// one accepted block. Nonlocal contexts still require fresh classification.
    /// Repeated edits map from the last retained revision, not the original parse.
    pub fn remap(&self, current: &Snapshot) -> Option<Self> {
        self.remap_inline(current)
            .or_else(|| self.reclassify_local_block(current))
    }

    /// Reclassify a bounded run of accepted top-level blocks and whitespace gaps.
    /// Unknown/global syntax remains a conservative fallback.
    fn reclassify_local_block(&self, current: &Snapshot) -> Option<Self> {
        const LOCAL_BYTES: usize = 4096;
        if self.snapshot.document() != current.document()
            || current.generation() <= self.snapshot.generation()
            || current.source().len() > MAX_BYTES
        {
            return None;
        }
        let old = self.snapshot.source();
        let new = current.source();
        let mut start = old
            .bytes()
            .zip(new.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
            start -= 1;
        }
        let mut tail = old[start..]
            .bytes()
            .rev()
            .zip(new[start..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - tail) || !new.is_char_boundary(new.len() - tail) {
            tail -= 1;
        }
        let end = old.len() - tail;
        let new_end = new.len() - tail;
        // Include both neighbors when an edit joins/splits their whitespace gap.
        // Never bridge unsupported containers hidden between accepted regions.
        let source_regions = self.plan.regions();
        let first = source_regions
            .iter()
            .rposition(|r| r.block().start <= start)?;
        let last = source_regions.iter().position(|r| r.block().end >= end)?;
        if last < first {
            return None;
        }
        // A top-level list/quote is the parse context of every line in it, and
        // can absorb an indented or lazy line across whitespace. Reparse whole
        // containers that the dirty run touches or borders (#868).
        let mut dirty = source_regions[first].block().start..source_regions[last].block().end;
        let mut included = vec![false; self.contexts.len()];
        while let Some(index) = self.contexts.iter().enumerate().position(|(i, c)| {
            !included[i]
                && (c.start <= dirty.end && dirty.start <= c.end
                    || old
                        .get(c.end..dirty.start)
                        .is_some_and(|gap| gap.trim().is_empty())
                    || old
                        .get(dirty.end..c.start)
                        .is_some_and(|gap| gap.trim().is_empty()))
        }) {
            included[index] = true;
            let c = &self.contexts[index];
            dirty = dirty.start.min(c.start)..dirty.end.max(c.end);
        }
        let first = source_regions
            .iter()
            .position(|r| r.block().end > dirty.start)?;
        let last = source_regions
            .iter()
            .rposition(|r| r.block().start < dirty.end)?;
        if last < first {
            return None;
        }
        let affected = &source_regions[first..=last];
        // Between accepted blocks only whitespace and quote prefixes may remain;
        // anything else is an unsupported block that this parse cannot prove.
        let container_gap = |gap: Option<&str>| {
            gap.is_some_and(|gap| gap.chars().all(|c| c.is_whitespace() || c == '>'))
        };
        if affected
            .iter()
            .any(|r| !matches!(r, Region::Conceal { .. }))
            || !container_gap(old.get(dirty.start..affected[0].block().start))
            || !container_gap(old.get(affected[affected.len() - 1].block().end..dirty.end))
            || affected
                .windows(2)
                .any(|pair| !container_gap(old.get(pair[0].block().end..pair[1].block().start)))
        {
            return None;
        }
        let shift = |offset: usize| offset.checked_add(new_end)?.checked_sub(end);
        let updated = dirty.start..shift(dirty.end)?;
        let fragment = new.get(updated.clone())?;
        let spans: Vec<_> = self
            .contexts
            .iter()
            .zip(&included)
            .filter(|(_, &included)| included)
            .filter_map(|(c, _)| {
                let span_start = if c.start <= start {
                    c.start
                } else {
                    shift(c.start)?
                };
                let span_end = if c.end >= end { shift(c.end)? } else { c.end };
                Some(span_start..span_end)
            })
            .collect();
        // Bound parser input before entering Comrak, not after a long parse.
        // Fences/HTML/definitions can change nonlocal parsing. Inside a whole
        // container fences and HTML end with it; definitions stay global.
        let lines: Vec<_> = fragment
            .split_inclusive('\n')
            .scan(0, |offset, line| {
                let at = *offset;
                *offset += line.len();
                Some((at, line))
            })
            .collect();
        let in_spans = |at: usize| {
            spans
                .iter()
                .any(|span| span.start <= updated.start + at && updated.start + at < span.end)
        };
        if fragment.contains('\0')
            || lines.iter().any(|&(at, line)| {
                if in_spans(at) {
                    container_content(line).starts_with('[')
                } else {
                    line.trim_start().starts_with(['`', '~', '<', '['])
                }
            })
        {
            return None;
        }
        // YAML and TOML frontmatter openers make the rest of the document raw.
        let opener = fragment.trim_start_matches('\u{feff}');
        if updated.start == 0 && (opener.starts_with("---") || opener.starts_with("+++")) {
            return None;
        }
        // Containers extend past the dirty run: an indented first line can join
        // a preceding list, and a new list item can absorb an indented neighbor.
        let indented = |text: &str, at: usize| {
            let line = text[..at].rfind('\n').map_or(0, |i| i + 1);
            text[line..].starts_with([' ', '\t'])
        };
        let next_indented = source_regions
            .get(last + 1)
            .is_some_and(|next| indented(old, next.block().start));
        if (indented(new, updated.start) && !new[..updated.start].trim().is_empty())
            || (next_indented && fragment.lines().any(opens_list_item))
        {
            return None;
        }
        let local = Snapshot::new(current.document(), current.generation(), fragment);
        // Oversized but context-local edits keep only their dirty run raw until
        // async adoption. Do not discard the already validated outer blocks.
        let classified = (dirty.len() <= LOCAL_BYTES && fragment.len() <= LOCAL_BYTES)
            .then(|| super::classify(&local));
        // Indented, tabbed and quote lines can attach to a container. Accept
        // them only inside a container of this parse, which includes every
        // container the run borders. A container reaching the run's end must
        // not continue lazily into an adjacent line outside it.
        let in_local = |at: usize| {
            classified
                .as_ref()
                .is_some_and(|c| c.contexts.iter().any(|ctx| ctx.start <= at && at < ctx.end))
        };
        let attaching = lines.iter().any(|&(at, line)| {
            let plain = line.trim_start();
            (line.len() - plain.len() >= 4 || line.starts_with('\t') || plain.starts_with('>'))
                && !in_spans(at)
                && !in_local(at)
        });
        let after = &new[updated.end..];
        let after = after
            .strip_prefix("\r\n")
            .or_else(|| after.strip_prefix('\n'))
            .unwrap_or(after);
        let next_blank = after
            .split('\n')
            .next()
            .is_none_or(|line| line.trim().is_empty());
        let lazy = !next_blank
            && classified
                .as_ref()
                .is_some_and(|c| c.contexts.iter().any(|ctx| ctx.end == fragment.len()));
        if attaching || lazy {
            return None;
        }
        let offset = |r: &Range<usize>| updated.start + r.start..updated.start + r.end;
        let map = |r: &Range<usize>| -> Option<Range<usize>> {
            if r.end <= dirty.start {
                Some(r.clone())
            } else if r.start >= dirty.end {
                Some(shift(r.start)?..shift(r.end)?)
            } else {
                None
            }
        };
        let mut regions = Vec::new();
        for (index, region) in source_regions.iter().enumerate() {
            if index == first {
                if let Some(classified) = &classified {
                    regions.extend(classified.plan.regions().iter().map(|r| match r {
                        Region::Source(r) => Region::Source(offset(r)),
                        Region::Conceal { block, markers } => Region::Conceal {
                            block: offset(block),
                            markers: markers.iter().map(offset).collect(),
                        },
                    }));
                } else {
                    // Empty conceal inventory preserves local-context provenance
                    // for subsequent edits before the async classifier returns.
                    regions.push(Region::Conceal {
                        block: updated.clone(),
                        markers: Vec::new(),
                    });
                }
            }
            if index < first || index > last {
                regions.push(match region {
                    Region::Source(r) => Region::Source(map(r)?),
                    Region::Conceal { block, markers } => Region::Conceal {
                        block: map(block)?,
                        markers: markers.iter().map(map).collect::<Option<Vec<_>>>()?,
                    },
                });
            }
        }
        let mut styles: Vec<_> = self
            .styles
            .iter()
            .filter_map(|s| {
                Some(StyleSpan {
                    range: map(&s.range)?,
                    style: s.style,
                })
            })
            .collect();
        styles.extend(
            classified
                .iter()
                .flat_map(|c| &c.styles)
                .map(|s| StyleSpan {
                    range: offset(&s.range),
                    style: s.style,
                }),
        );
        styles.sort_by_key(|s| s.range.start);
        let mut marker_scopes: Vec<_> = self
            .marker_scopes
            .iter()
            .filter_map(|(m, s)| Some((map(m)?, map(s)?)))
            .collect();
        marker_scopes.extend(
            classified
                .iter()
                .flat_map(|c| &c.marker_scopes)
                .map(|(m, s)| (offset(m), offset(s))),
        );
        marker_scopes.sort_by_key(|(m, _)| m.start);
        // An unclassified raw run has no proven containers: later edits in it
        // keep the strict outside-container line rules until adoption.
        let mut contexts: Vec<_> = self.contexts.iter().filter_map(map).collect();
        contexts.extend(classified.iter().flat_map(|c| &c.contexts).map(offset));
        contexts.sort_by_key(|c| c.start);
        let plan = Plan::new(current, regions);
        let base = crate::source_projection::project(current, &plan, &Active::default()).ok()?;
        Some(Self {
            snapshot: current.clone(),
            base: Ok(Arc::new(base)),
            plan,
            styles,
            marker_scopes,
            contexts,
        })
    }

    fn remap_inline(&self, current: &Snapshot) -> Option<Self> {
        if self.snapshot.document() != current.document()
            || current.generation() < self.snapshot.generation()
            || current.source().len() > MAX_BYTES
        {
            return None;
        }
        let old = self.snapshot.source();
        let new = current.source();
        if old == new {
            return Some(Self {
                snapshot: current.clone(),
                base: crate::source_projection::project(
                    current,
                    &Plan::new(current, self.plan.regions().to_vec()),
                    &Active::default(),
                )
                .map(Arc::new),
                plan: Plan::new(current, self.plan.regions().to_vec()),
                styles: self.styles.clone(),
                marker_scopes: self.marker_scopes.clone(),
                contexts: self.contexts.clone(),
            });
        }
        if current.generation() == self.snapshot.generation() {
            return None;
        }
        let mut start = old
            .bytes()
            .zip(new.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
            start -= 1;
        }
        let mut tail = old[start..]
            .bytes()
            .rev()
            .zip(new[start..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - tail) || !new.is_char_boundary(new.len() - tail) {
            tail -= 1;
        }
        let end = old.len() - tail;
        let new_end = new.len() - tail;
        if old[start..end].contains(['\r', '\n', '\0'])
            || new[start..new_end].contains(['\r', '\n', '\0'])
        {
            return None;
        }
        // Require an unchanged ordinary-text prefix on this source line. This
        // excludes edits that can turn the line into a fence, list, HTML block,
        // reference definition, frontmatter or setext underline.
        let line_start = old[..start].rfind('\n').map_or(0, |i| i + 1);
        let prefix = old[line_start..start].trim_start();
        let ordinary_start = |text: &str| {
            text[line_start..]
                .split('\n')
                .next()
                .unwrap_or("")
                .trim_start()
                .chars()
                .next()
                .is_some_and(char::is_alphabetic)
        };
        let fixed_prefix = prefix.chars().any(char::is_alphabetic)
            && !prefix.starts_with(['[', '<', '`', '~', '>']);
        let plain_edit = old[start..end]
            .chars()
            .chain(new[start..new_end].chars())
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '\t'));
        let line_end = old[start..].find('\n').map_or(old.len(), |i| start + i);
        let empty_line = old[line_start..line_end].trim().is_empty();
        if !fixed_prefix && !((ordinary_start(old) || empty_line) && ordinary_start(new)) {
            return None;
        }
        // Indentation decides container membership (list continuation, code).
        let indent = |text: &str| {
            let line = &text[line_start..];
            line.len() - line.trim_start_matches([' ', '\t']).len()
        };
        if indent(old) != indent(new) {
            return None;
        }
        let dirty = self
            .plan
            .regions()
            .iter()
            .find(|r| {
                let b = r.block();
                b.start <= start && end <= b.end
            })
            .map(|r| r.block().clone())
            .or_else(|| plain_edit.then_some(line_start..line_end))?;
        // Plain text outside all syntax fragments cannot change their inline
        // parse. Keep those fragments even within the edited paragraph.
        let preserve_inline = plain_edit
            && !self
                .marker_scopes
                .iter()
                .any(|(_, scope)| start <= scope.end && scope.start <= end);
        let shift = |offset: usize| offset.checked_add(new_end)?.checked_sub(end);
        let map = |r: &Range<usize>| -> Option<Range<usize>> {
            if preserve_inline {
                if r.end <= start {
                    Some(r.clone())
                } else if r.start >= end {
                    Some(shift(r.start)?..shift(r.end)?)
                } else if r.start <= start && r.end >= end {
                    Some(r.start..shift(r.end)?)
                } else {
                    None
                }
            } else if r.end <= dirty.start {
                Some(r.clone())
            } else if r.start >= dirty.end {
                Some(shift(r.start)?..shift(r.end)?)
            } else {
                None
            }
        };
        let mut regions = Vec::with_capacity(self.plan.regions().len());
        for region in self.plan.regions() {
            if region.block() == &dirty && !preserve_inline {
                regions.push(Region::Source(dirty.start..shift(dirty.end)?));
                continue;
            }
            regions.push(match region {
                Region::Source(r) => Region::Source(map(r)?),
                Region::Conceal { block, markers } => Region::Conceal {
                    block: map(block)?,
                    markers: markers.iter().map(map).collect::<Option<Vec<_>>>()?,
                },
            });
        }
        // An edit inside a container extends it, including typing at its end.
        let contexts = self
            .contexts
            .iter()
            .map(|c| {
                if c.start <= start && end <= c.end {
                    Some(c.start..shift(c.end)?)
                } else if c.end <= start {
                    Some(c.clone())
                } else if c.start >= end {
                    Some(shift(c.start)?..shift(c.end)?)
                } else {
                    None
                }
            })
            .collect::<Option<Vec<_>>>()?;
        let plan = Plan::new(current, regions);
        // Build and validate once per revision, then reuse during reveal/IME.
        let base = crate::source_projection::project(current, &plan, &Active::default()).ok()?;
        let result = Self {
            contexts,
            snapshot: current.clone(),
            base: Ok(Arc::new(base)),
            plan,
            marker_scopes: self
                .marker_scopes
                .iter()
                .filter_map(|(marker, scope)| Some((map(marker)?, map(scope)?)))
                .collect(),
            styles: self
                .styles
                .iter()
                .filter_map(|s| {
                    Some(StyleSpan {
                        range: map(&s.range)?,
                        style: s.style,
                    })
                })
                .collect(),
        };
        // This also checks grapheme boundaries after Unicode edits. No rounding
        // of an IME range or stale byte offset is ever accepted.
        Some(result)
    }
}

/// CommonMark list item opener: up to three spaces, then a bullet or an
/// ordinal of at most nine digits, followed by whitespace or end of line.
fn opens_list_item(line: &str) -> bool {
    let plain = line.trim_start_matches(' ');
    if line.len() - plain.len() > 3 {
        return false;
    }
    let digits = plain.bytes().take_while(u8::is_ascii_digit).count();
    let rest = match plain.as_bytes().get(digits) {
        Some(b'-' | b'*' | b'+') if digits == 0 => &plain[1..],
        Some(b'.' | b')') if (1..=9).contains(&digits) => &plain[digits + 1..],
        _ => return false,
    };
    rest.is_empty() || rest.starts_with([' ', '\t'])
}

/// Line content after container prefixes: indentation, quote markers and
/// list/task markers.
fn container_content(line: &str) -> &str {
    let mut rest = line;
    loop {
        rest = rest.trim_start_matches([' ', '\t']);
        if let Some(after) = rest.strip_prefix('>') {
            rest = after;
            continue;
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        // `[x]:` stays: that is a reference definition, not a task marker.
        let marker = if rest.starts_with(['-', '+', '*']) {
            1
        } else if ["[ ]", "[x]", "[X]"]
            .iter()
            .any(|task| rest.starts_with(task))
        {
            3
        } else if (1..=9).contains(&digits) && rest[digits..].starts_with(['.', ')']) {
            digits + 1
        } else {
            return rest;
        };
        match rest[marker..].chars().next() {
            Some(' ' | '\t') => rest = &rest[marker..],
            _ => return rest,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reveal_snapshot_keeps_projection_and_marker_policy_on_one_revision() {
        let text = "plain [first](one) and [second](two)";
        let source = snapshot(1, text);
        let retained = RetainedPresentation::new(&classify(&source));
        let first = text.find("[first]").unwrap()..text.find(" and").unwrap();
        let second = text.find("[second]").unwrap()..text.len();
        let mut active = Active {
            selection: Some(first.start + 2..first.start + 2),
            composition: None,
        };
        let frozen = retained.prepare_reveal(&active).unwrap();
        assert!(frozen.is_raw(&source, &first));
        assert!(!frozen.is_raw(&source, &second));
        assert_eq!(
            frozen.projection().display(),
            "plain [first](one) and second"
        );
        // Mutating the caller's selection cannot reinterpret an in-flight frame.
        active.selection = Some(second.start + 2..second.start + 2);
        let next = retained.prepare_reveal(&active).unwrap();
        assert!(frozen.is_raw(&source, &first));
        assert!(!frozen.is_raw(&source, &second));
        assert!(!next.is_raw(&source, &first));
        assert!(next.is_raw(&source, &second));
        active.composition = Some(first.clone());
        let ime = retained.prepare_reveal(&active).unwrap();
        assert_eq!(ime.projection().display(), text);
        assert!(ime.is_raw(&source, &first));
        assert!(ime.is_raw(&source, &second));
        for stale in [
            snapshot(2, text),
            snapshot(1, "different"),
            Snapshot::new("another-document", 1, text),
        ] {
            assert!(frozen.is_raw(&stale, &second));
        }
        assert!(frozen.is_raw(&source, &(0..usize::MAX)));
    }

    #[test]
    fn reveal_snapshot_rejects_subgrapheme_scope_and_caret() {
        let source = snapshot(1, "e\u{301} [label](target)");
        let retained = RetainedPresentation::new(&classify(&source));
        let reveal = retained.prepare_reveal(&Active::default()).unwrap();
        assert!(reveal.is_raw(&source, &(1..3)));
        assert!(!reveal.is_raw(&source, &(0..3)));
        assert!(retained
            .prepare_reveal(&Active {
                selection: Some(1..1),
                composition: None,
            })
            .is_err());
    }

    use super::*;
    use crate::source_classifier::classify;
    use crate::source_projection::{project, Bias};
    fn snapshot(generation: u64, text: &str) -> Snapshot {
        Snapshot::new("note", generation, text)
    }
    #[test]
    fn structural_edit_in_one_block_matches_fresh_classification() {
        for (before, after) in [
            ("plain text", "plain\ntext"),
            ("plain text", "plain\n\ntext"),
            ("plain\ntext", "plaintext"),
            ("plain text", "# plain text"),
            ("plain text", "- plain text"),
            ("plain text", "plain\nnew pasted paragraph\ntext"),
            ("plain שלום text", "plain שלום\r\ntext"),
        ] {
            let old = snapshot(
                1,
                &format!("**BEFORE**\n\n{before}\n\n**AFTER** [label](destination)"),
            );
            let current = snapshot(
                2,
                &format!("**BEFORE**\n\n{after}\n\n**AFTER** [label](destination)"),
            );
            let retained = RetainedPresentation::new(&classify(&old))
                .remap(&current)
                .unwrap();
            let actual = retained.project(&Active::default()).unwrap();
            let fresh = RetainedPresentation::new(&classify(&current))
                .project(&Active::default())
                .unwrap();
            assert_eq!(actual.display(), fresh.display(), "{after:?}");
            assert!(actual.display().starts_with("BEFORE\n"));
            assert!(actual.display().ends_with("AFTER label"));
            assert_eq!(
                current.copy_source(0..current.source().len()).unwrap(),
                current.source()
            );
        }
    }

    #[test]
    fn structural_cross_block_edits_match_full_parse_and_keep_outer_neighbors() {
        for (before, after) in [
            ("TOP text\n\n**bold**", "TOP text\n**bold**"),
            ("plain **one**\n\nother *two*", "plain **one**other *two*"),
            ("plain **one**\n\nother *two*", "plain **one**\nother *two*"),
            ("plain **one**\n\nother *two*", "plain replacement *two*"),
            (
                "plain **one**\r\n\r\nother *two*",
                "plain **one** other *two*",
            ),
            (
                "שלום **one**\n\nother *two*",
                "שלום **one**\nnew\nother *two*",
            ),
        ] {
            let old = snapshot(1, &format!("# BEFORE\n\n{before}\n\n# AFTER"));
            let new = snapshot(2, &format!("# BEFORE\n\n{after}\n\n# AFTER"));
            let retained = RetainedPresentation::new(&classify(&old))
                .remap(&new)
                .unwrap();
            let fresh = RetainedPresentation::new(&classify(&new));
            for caret in [
                0,
                new.source().find("plain").unwrap_or(12),
                new.source().len(),
            ] {
                let active = Active {
                    selection: Some(caret..caret),
                    composition: None,
                };
                let a = retained.project(&active).unwrap();
                let b = fresh.project(&active).unwrap();
                assert_eq!(a.display(), b.display(), "{before:?} -> {after:?}");
                for offset in 0..=new.source().len() {
                    assert_eq!(
                        a.source_to_display(&new, offset),
                        b.source_to_display(&new, offset)
                    );
                }
            }
            assert_eq!(retained.styles(), fresh.styles());
        }
    }

    #[test]
    fn structural_cross_block_edit_cannot_bridge_unsupported_source() {
        let old = snapshot(1, "plain **one**\n\n```\nopaque\n```\n\nother *two*");
        let new = snapshot(2, "plain replacement *two*");
        assert!(RetainedPresentation::new(&classify(&old))
            .remap(&new)
            .is_none());
    }

    #[test]
    fn structural_local_parse_refuses_global_context() {
        let old = snapshot(1, "plain text\n\n**AFTER**");
        for new in [
            "---\nplain text\n\n**AFTER**",
            "plain\n```\ntext\n\n**AFTER**",
            "plain\n[id]: destination\ntext\n\n**AFTER**",
            "plain\n<div>\ntext\n\n**AFTER**",
        ] {
            assert!(RetainedPresentation::new(&classify(&old))
                .remap(&snapshot(2, new))
                .is_none());
        }
    }

    /// Retained output may fall back to raw, but must never keep presentation
    /// that full classification no longer grants (#832), in either direction.
    #[test]
    fn local_edits_that_extend_document_or_container_context_match_full_parse() {
        for (before, after) in [
            ("plain\n\n**AFTER**", "+++\nplain\n\n**AFTER**"),
            (
                "\u{feff}plain\n\n**AFTER**",
                "\u{feff}+++\nplain\n\n**AFTER**",
            ),
            ("plain\n\n  **AFTER**", "- plain\n\n  **AFTER**"),
            ("plain\n\n  **AFTER**", "1. plain\n\n  **AFTER**"),
            ("plain\n\n   **AFTER**", "10) plain\n\n   **AFTER**"),
            ("- item\n\nplain *x*", "- item\n\n  plain *x*"),
        ] {
            for (from, to) in [(before, after), (after, before)] {
                let old = snapshot(1, from);
                let new = snapshot(2, to);
                let fresh = RetainedPresentation::new(&classify(&new))
                    .project(&Active::default())
                    .unwrap();
                let retained = RetainedPresentation::new(&classify(&old));
                if let Some(remapped) = retained.remap(&new) {
                    let actual = remapped.project(&Active::default()).unwrap();
                    assert_eq!(actual.display(), fresh.display(), "{from:?} -> {to:?}");
                    // Reversing before adoption maps from the retained revision.
                    if let Some(back) = remapped.remap(&snapshot(3, from)) {
                        let expected = RetainedPresentation::new(&classify(&old))
                            .project(&Active::default())
                            .unwrap();
                        let actual = back.project(&Active::default()).unwrap();
                        assert_eq!(actual.display(), expected.display(), "{to:?} -> {from:?}");
                    }
                } else {
                    // Fallback keeps the last accepted revision; undoing the
                    // edit before adoption must restore exactly its output.
                    let back = retained.remap(&snapshot(3, from)).unwrap();
                    assert_eq!(
                        back.project(&Active::default()).unwrap().display(),
                        retained.project(&Active::default()).unwrap().display()
                    );
                }
                assert_eq!(
                    new.copy_source(0..new.source().len()).unwrap(),
                    new.source()
                );
            }
        }
    }

    #[test]
    fn structural_edits_in_containers_reparse_the_whole_container() {
        for (before, after) in [
            // Nested list, three depths, quote inside a list item.
            (
                "- a **b**\n  - c *d*\n- e",
                "- a **b**\n  - c *d*\n  - new **x**\n- e",
            ),
            (
                "1. a\n   - b\n     > c **d**",
                "1. a\n   - b\n     > c **d**\n     > e *f*",
            ),
            ("> a **b**\n> c", "> a **b**\n> new *x*\n> c"),
            ("> a **b**\n>\n> c", "> a **b**\n>\n> c\nlazy *x*"),
            ("- [ ] a **b**", "- [ ] a **b**\n- [x] c ~~d~~"),
            // A paragraph after a list is reparsed with the list it borders.
            ("- a\n\nplain *p*", "- a\n\nplain *p*\nmore **m**"),
            ("- a\n\nplain *p*", "- a\n\n  plain *p*\n  more"),
            (
                "\u{feff}> a **b**\r\n> c",
                "\u{feff}> a **b**\r\n> ש *x*\r\n> c",
            ),
            // A new quote is parsed whole; a blank line ends it.
            ("plain *p*", "plain *p*\n> q **r**"),
        ] {
            let old = snapshot(1, &format!("**BEFORE**\n\n{before}\n\n**AFTER** [l](d)"));
            let current = snapshot(2, &format!("**BEFORE**\n\n{after}\n\n**AFTER** [l](d)"));
            let retained = RetainedPresentation::new(&classify(&old))
                .remap(&current)
                .unwrap_or_else(|| panic!("local container reparse: {after:?}"));
            let fresh = RetainedPresentation::new(&classify(&current));
            assert_eq!(
                retained.project(&Active::default()).unwrap().display(),
                fresh.project(&Active::default()).unwrap().display(),
                "{after:?}"
            );
            assert_eq!(retained.styles(), fresh.styles(), "{after:?}");
            assert_eq!(retained.contexts, fresh.contexts, "{after:?}");
        }
    }

    #[test]
    fn container_reparse_refuses_definitions_and_unsupported_content() {
        for (before, after) in [
            ("> a **b**\n> c", "> a **b**\n> [id]: /u\n> c"),
            ("- a **b**\n- c", "- a **b**\n- [id]: /u\n- c"),
            // An unsupported block inside the container cannot be proven.
            (
                "- a **b**\n\n      code\n- c",
                "- a **b**\n  more\n\n      code\n- c",
            ),
            // Indented blocks outside containers keep the conservative refusal.
            ("plain *p*", "plain *p*\n\n    code"),
            // A new quote would continue lazily into the adjacent paragraph.
            ("# H **b**\ntext *t*", "> H **b**\ntext *t*"),
        ] {
            let old = snapshot(1, &format!("{before}\n\n**AFTER**"));
            let current = snapshot(2, &format!("{after}\n\n**AFTER**"));
            assert!(
                RetainedPresentation::new(&classify(&old))
                    .remap(&current)
                    .is_none(),
                "{after:?}"
            );
        }
    }

    #[test]
    fn container_prefixes_are_stripped_before_the_definition_check() {
        for (line, content) in [
            ("> > - [ ] [x]: y", "[x]: y"),
            ("  10) text", "text"),
            ("-\t[X] task", "task"),
            ("-no space", "-no space"),
            ("[x]: def", "[x]: def"),
        ] {
            assert_eq!(container_content(line), content, "{line:?}");
        }
    }

    #[test]
    #[ignore = "manual same-host timing probe; run with --ignored --nocapture"]
    fn structural_edit_timing_probe() {
        use std::time::Instant;
        for (count, padding) in [(1, 0), (100, 0), (600, 0), (600, 40_000)] {
            let original = format!(
                "plain text\n\n{}{}",
                "**bold** [label](destination)\n\n".repeat(count),
                "x".repeat(padding)
            );
            let base = RetainedPresentation::new(&classify(&snapshot(1, &original)));
            let mut local_times = Vec::new();
            let mut full_times = Vec::new();
            for iteration in 0..100 {
                let current = snapshot(2, &original.replacen("plain text", "plain\ntext", 1));
                let started = Instant::now();
                let mapped = base.remap(&current).unwrap();
                std::hint::black_box(mapped.project(&Active::default()).unwrap());
                local_times.push(started.elapsed().as_micros());
                let started = Instant::now();
                let fresh = RetainedPresentation::new(&classify(&current));
                std::hint::black_box(fresh.project(&Active::default()).unwrap());
                full_times.push(started.elapsed().as_micros());
                assert_eq!(
                    mapped.project(&Active::default()).unwrap().display(),
                    fresh.project(&Active::default()).unwrap().display(),
                    "iteration {iteration}"
                );
            }
            local_times.sort_unstable();
            full_times.sort_unstable();
            eprintln!("blocks={count} bytes={} local_us p50={} p95={} max={} full_us p50={} p95={} max={}",
                original.len(), local_times[50], local_times[95], local_times[99],
                full_times[50], full_times[95], full_times[99]);
        }
    }

    #[test]
    fn oversized_local_edits_keep_outer_projection_until_adoption() {
        let original = format!(
            "**BEFORE**\n\nplain {}\n\n**AFTER** [label](destination)",
            "x".repeat(4097)
        );
        let mut retained = RetainedPresentation::new(&classify(&snapshot(1, &original)));
        for generation in 2..6 {
            let text = original.replace(
                "plain ",
                &format!("plain{}", "\n".repeat(generation as usize)),
            );
            let current = snapshot(generation, &text);
            retained = retained.remap(&current).expect("bounded raw dirty run");
            let projected = retained.project(&Active::default()).unwrap();
            assert!(projected.display().starts_with("BEFORE\n\n"));
            assert!(projected.display().ends_with("AFTER label"));
            let label = projected.display().rfind("label").unwrap();
            let source = projected
                .display_to_source(&current, label, Bias::Right)
                .unwrap();
            assert_eq!(&text[source..source + 5], "label");
            assert_eq!(
                projected.display(),
                RetainedPresentation::new(&classify(&current))
                    .project(&Active::default())
                    .unwrap()
                    .display()
            );
        }
        let global = snapshot(6, &original.replace("plain ", "plain\n```\n"));
        assert!(
            retained.remap(&global).is_none(),
            "global context still invalidates retention"
        );
    }

    #[test]
    fn unchanged_fragments_reuse_projection_but_validate_each_active_range() {
        let source = snapshot(1, "plain e\u{301} text [label](destination)");
        let retained = RetainedPresentation::new(&classify(&source));
        let base = retained.project(&Active::default()).unwrap();
        for caret in [0, 1, 4] {
            let projected = retained
                .project(&Active {
                    selection: Some(caret..caret),
                    composition: None,
                })
                .unwrap();
            assert!(Arc::ptr_eq(&base, &projected));
        }
        // A scalar boundary inside a grapheme must not pass the cached path.
        assert!(retained
            .project(&Active {
                selection: Some(7..7),
                composition: None
            })
            .is_err());
        assert!(retained
            .validate_active(&Active {
                selection: None,
                composition: Some(usize::MAX..usize::MAX)
            })
            .is_err());
        let edited = snapshot(2, "Xplain e\u{301} text [label](destination)");
        let mapped = retained.remap(&edited).unwrap();
        let next = mapped.project(&Active::default()).unwrap();
        assert!(!Arc::ptr_eq(&base, &next));
        assert_eq!(next.snapshot(), &edited);
        assert!(mapped
            .project(&Active {
                selection: Some(8..8),
                composition: None
            })
            .is_err());
    }

    #[test]
    fn caret_reveals_only_its_fragment_not_all_links_in_paragraph() {
        let text = "plain [first](long-one) and [second](long-two) end";
        let retained = RetainedPresentation::new(&classify(&snapshot(1, text)));
        for caret in [0, text.find(" and").unwrap() + 2, text.len()] {
            assert_eq!(
                retained
                    .project(&Active {
                        selection: Some(caret..caret),
                        composition: None
                    })
                    .unwrap()
                    .display(),
                "plain first and second end"
            );
        }
        let caret = text.find("first").unwrap() + 2;
        assert_eq!(
            retained
                .project(&Active {
                    selection: Some(caret..caret),
                    composition: None
                })
                .unwrap()
                .display(),
            "plain [first](long-one) and second end"
        );
        let second = text.find("second").unwrap();
        assert_eq!(
            retained
                .project(&Active {
                    selection: None,
                    composition: Some(second..second + 1)
                })
                .unwrap()
                .display(),
            "plain first and [second](long-two) end"
        );
    }

    #[test]
    fn repeated_unicode_edits_retain_other_blocks_and_exact_maps() {
        let original = "TOP text\n\n**שלום** [label](long-destination)\n\nTAIL";
        let mut retained = RetainedPresentation::new(&classify(&snapshot(1, original)));
        for generation in 2..102 {
            let text = original.replace("TOP text", &format!("TOP текст{generation}é"));
            let current = snapshot(generation, &text);
            retained = retained.remap(&current).unwrap();
            let p = project(&current, retained.plan(), &Active::default()).unwrap();
            assert!(p.display().contains("שלום label"));
            let label = p.display().find("label").unwrap();
            let source_label = p.display_to_source(&current, label, Bias::Right).unwrap();
            assert_eq!(&text[source_label..source_label + 5], "label");
            assert_eq!(current.copy_source(0..text.len()).unwrap(), text);
        }
    }
    #[test]
    fn dirty_block_only_is_raw_and_styles_follow_suffix() {
        let old = snapshot(1, "TOP text\n\n**bold**\n\nTAIL");
        let current = snapshot(2, "TOP text more\n\n**bold**\n\nTAIL");
        let retained = RetainedPresentation::new(&classify(&old))
            .remap(&current)
            .unwrap();
        assert_eq!(
            project(&current, retained.plan(), &Active::default())
                .unwrap()
                .display(),
            "TOP text more\n\nbold\n\nTAIL"
        );
        assert_eq!(
            &current.source()[retained.styles()[0].range.clone()],
            "bold"
        );
        // Positive control: changing the bold block invalidates its projection.
        let next = snapshot(3, "TOP text more\n\n**bold!**\n\nTAIL");
        let retained = retained.remap(&next).unwrap();
        assert!(project(&next, retained.plan(), &Active::default())
            .unwrap()
            .display()
            .contains("**bold!**"));
    }
    #[test]
    fn typing_between_links_retains_the_same_paragraph() {
        let old = snapshot(1, "plain [first](long-one) and [second](long-two) end");
        let current = snapshot(2, "plain [first](long-one) andש [second](long-two) end");
        let retained = RetainedPresentation::new(&classify(&old))
            .remap(&current)
            .unwrap();
        assert_eq!(
            retained.project(&Active::default()).unwrap().display(),
            "plain first andש second end"
        );
        assert_eq!(
            retained.project(&Active::default()).unwrap().display(),
            RetainedPresentation::new(&classify(&current))
                .project(&Active::default())
                .unwrap()
                .display()
        );
    }

    #[test]
    fn starting_a_plain_paragraph_in_a_blank_line_retains_other_blocks() {
        let old = snapshot(1, "**bold**\n\n\nTAIL");
        let current = snapshot(2, "**bold**\n\nz\nTAIL");
        let retained = RetainedPresentation::new(&classify(&old))
            .remap(&current)
            .unwrap();
        assert_eq!(
            retained.project(&Active::default()).unwrap().display(),
            "bold\n\nz\nTAIL"
        );
        let next = snapshot(3, "**bold**\n\nzש\nTAIL");
        assert!(retained
            .remap(&next)
            .unwrap()
            .project(&Active::default())
            .unwrap()
            .display()
            .starts_with("bold\n"));
    }

    #[test]
    fn rejects_context_changes_stale_identity_and_invalid_revision() {
        for (old, new) in [
            ("TOP text\n\n**bold**", "```TOP text\n\n**bold**"),
            ("TOP text\n\n**bold**", "TOP text\n```\n\n**bold**"),
            ("[id] text\n\n**bold**", "[id]: dest\n\n**bold**"),
        ] {
            let retained = RetainedPresentation::new(&classify(&snapshot(2, old)));
            assert!(retained.remap(&snapshot(3, new)).is_none(), "{new}");
        }
        let retained = RetainedPresentation::new(&classify(&snapshot(2, "TOP text")));
        assert!(retained
            .remap(&Snapshot::new("other", 3, "TOP text!"))
            .is_none());
        assert!(retained.remap(&snapshot(1, "TOP text!")).is_none());
        assert!(retained.remap(&snapshot(2, "TOP text!")).is_none());
    }
}
