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

    /// S3a's bounded first slice: structural edits inside one already accepted
    /// top-level block. Unknown/global syntax remains a conservative fallback.
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
        let dirty = self.plan.regions().iter().find_map(|r| match r {
            Region::Conceal { block, .. } if block.start <= start && end <= block.end => {
                Some(block.clone())
            }
            _ => None,
        })?;
        let shift = |offset: usize| offset.checked_add(new_end)?.checked_sub(end);
        let updated = dirty.start..shift(dirty.end)?;
        let fragment = new.get(updated.clone())?;
        // Bound parser input before entering Comrak, not after a long parse.
        // Fences/HTML/definitions can change nonlocal parsing. Indented blocks
        // can attach to surrounding containers; do not infer their context.
        if dirty.len() > LOCAL_BYTES
            || fragment.len() > LOCAL_BYTES
            || fragment.contains('\0')
            || fragment.lines().any(|line| {
                let plain = line.trim_start();
                line.len() - plain.len() >= 4
                    || line.starts_with('\t')
                    || plain.starts_with(['`', '~', '<', '[', '>'])
            })
        {
            return None;
        }
        if updated.start == 0 && fragment.trim_start_matches('\u{feff}').starts_with("---") {
            return None;
        }
        let local = Snapshot::new(current.document(), current.generation(), fragment);
        let classified = super::classify(&local);
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
        for region in self.plan.regions() {
            if region.block() == &dirty {
                regions.extend(classified.plan.regions().iter().map(|r| match r {
                    Region::Source(r) => Region::Source(offset(r)),
                    Region::Conceal { block, markers } => Region::Conceal {
                        block: offset(block),
                        markers: markers.iter().map(offset).collect(),
                    },
                }));
            } else {
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
        styles.extend(classified.styles.iter().map(|s| StyleSpan {
            range: offset(&s.range),
            style: s.style,
        }));
        styles.sort_by_key(|s| s.range.start);
        let mut marker_scopes: Vec<_> = self
            .marker_scopes
            .iter()
            .filter_map(|(m, s)| Some((map(m)?, map(s)?)))
            .collect();
        marker_scopes.extend(
            classified
                .marker_scopes
                .iter()
                .map(|(m, s)| (offset(m), offset(s))),
        );
        marker_scopes.sort_by_key(|(m, _)| m.start);
        let plan = Plan::new(current, regions);
        let base = crate::source_projection::project(current, &plan, &Active::default()).ok()?;
        Some(Self {
            snapshot: current.clone(),
            base: Ok(Arc::new(base)),
            plan,
            styles,
            marker_scopes,
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
        let plan = Plan::new(current, regions);
        // Build and validate once per revision, then reuse during reveal/IME.
        let base = crate::source_projection::project(current, &plan, &Active::default()).ok()?;
        let result = Self {
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
    fn structural_local_parse_refuses_global_context_and_oversized_input() {
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
        let new = format!("plain\n{}\n\n**AFTER**", "x".repeat(4097));
        assert!(RetainedPresentation::new(&classify(&old))
            .remap(&snapshot(2, &new))
            .is_none());
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
            ("TOP text\n\n**bold**", "TOP text\n**bold**"),
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
