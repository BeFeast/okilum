use okilum_core::document_links::{HeadingEntry, HeadingInventory};

/// Comrak remains the sole authority for container boundaries and references.
/// The existing cache handles ordinary notes and ambiguous container revisions.
#[derive(Default)]
pub(super) struct DraftCache {
    ordinary: super::incremental::Cache,
    containers: Option<ContainerReferences>,
}

struct ContainerReferences {
    source: String,
    sections: Vec<ReferenceSection>,
    environment: String,
}

struct ReferenceSection {
    source: String,
    heading: Option<HeadingEntry>,
}

const REFERENCE_BOUNDARY: &str = "# Okilum outline reference boundary\n\n";

impl DraftCache {
    fn update(&mut self, source: &str) -> Vec<HeadingEntry> {
        self.update_with(source, &mut |text| HeadingInventory::new(text).entries)
    }

    fn update_with(
        &mut self,
        source: &str,
        parse: &mut impl FnMut(&str) -> Vec<HeadingEntry>,
    ) -> Vec<HeadingEntry> {
        // The existing extractor is only an admission hint: candidate rows it
        // omits may live in containers (or fenced examples). Canonical parsing
        // below is the authority. Keep the established flat per-label cache
        // when the existing extractor already covers every candidate row.
        let container_candidates = self.containers.is_some()
            || (source.contains('[')
                && source.matches("]:").count()
                    > okilum_core::render::reference_definitions(source)
                        .matches("]:")
                        .count());
        if container_candidates
            && source.contains('[')
            && !source.contains("[[")
            && !source.contains("[^")
            && !source.starts_with('\u{feff}')
            && !source.starts_with("---\n")
            && !source.starts_with("---\r\n")
        {
            let next = match self.containers.take() {
                Some(old) => old.revise(source, parse),
                None => ContainerReferences::new(source, parse),
            };
            if let Some(next) = next {
                let entries = next.entries();
                self.containers = Some(next);
                return entries;
            }
        }
        self.containers = None;
        self.ordinary.update(source)
    }
}

impl ContainerReferences {
    fn new(source: &str, parse: &mut impl FnMut(&str) -> Vec<HeadingEntry>) -> Option<Self> {
        let entries = parse(source);
        let sections = reference_sections(source, entries)?;
        let environment = reference_environment(sections.iter().map(|s| s.source.as_str()))?;
        // Cold admission compares every reference-capable root heading with
        // the canonical whole-document result. A partial environment or an
        // uncertain container reset is rejected instead of retained.
        reference_fragment(&environment, "", parse)?;
        for section in &sections {
            if section.header_references() {
                let parsed = reference_fragment(&environment, section.header(), parse)?;
                if parsed.len() != 1 || !same_heading(&parsed[0], section.heading.as_ref()?) {
                    return None;
                }
            }
        }
        Some(Self {
            source: source.into(),
            sections,
            environment,
        })
    }

    fn entries(&self) -> Vec<HeadingEntry> {
        let mut offset = 0;
        let mut entries = Vec::new();
        for section in &self.sections {
            if let Some(mut heading) = section.heading.clone() {
                heading.target.offset += offset;
                entries.push(heading);
            }
            offset += section.source.len();
        }
        entries
    }

    fn revise(
        self,
        source: &str,
        parse: &mut impl FnMut(&str) -> Vec<HeadingEntry>,
    ) -> Option<Self> {
        if self.source == source {
            return Some(self);
        }
        let prefix = self
            .source
            .bytes()
            .zip(source.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.source.as_bytes()[prefix..]
            .iter()
            .rev()
            .zip(source.as_bytes()[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_end = self.source.len() - suffix;
        let new_end = source.len() - suffix;
        // Reparse from a previously certified root whose entire header is
        // unchanged. Prefix equality alone may stop inside a UTF-8 codepoint;
        // selected section/header boundaries always come from canonical offsets.
        let mut offset = 0;
        let mut start_index = 0;
        let mut start = 0;
        let mut end_index = self.sections.len();
        let mut end = source.len();
        for (index, section) in self.sections.iter().enumerate() {
            if offset + section.header().len() <= prefix && offset <= prefix {
                start_index = index;
                start = offset;
            }
            if offset >= old_end && offset > start && end_index == self.sections.len() {
                end_index = index;
                end = new_end + (offset - old_end);
            }
            offset += section.source.len();
        }
        if end_index <= start_index
            || !source.is_char_boundary(start)
            || !source.is_char_boundary(end)
        {
            return None;
        }
        let dirty = source.get(start..end)?;
        let environment = reference_environment(
            self.sections[..start_index]
                .iter()
                .map(|s| s.source.as_str())
                .chain(std::iter::once(dirty))
                .chain(self.sections[end_index..].iter().map(|s| s.source.as_str())),
        )?;
        // Include the next unchanged root header as a positive control. If a
        // changed fence/list/HTML context swallows or nests it, invalidate the
        // remainder through the existing full/context-aware parser path.
        let lookahead = self.sections.get(end_index).map_or("", |s| s.header());
        let fragment = format!("{dirty}{lookahead}");
        let parsed = reference_fragment(&environment, &fragment, parse)?;
        if !lookahead.is_empty()
            && !parsed.iter().any(|h| {
                h.supported_container && !h.target.setext && h.target.offset == dirty.len()
            })
        {
            return None;
        }
        let dirty_entries = parsed
            .into_iter()
            .filter(|h| h.target.offset < dirty.len())
            .collect();
        let mut middle = reference_sections(dirty, dirty_entries)?;
        let mut sections = self.sections;
        let suffix_sections = sections.split_off(end_index);
        sections.truncate(start_index);
        sections.append(&mut middle);
        sections.extend(suffix_sections);
        if environment != self.environment {
            // Reference-capable headings depend on the ordered definition
            // containers. Reparse those headers; ordinary headings and the
            // shifted authored suffix retain their exact cached coordinates.
            for section in &mut sections {
                if section.header_references() {
                    let mut parsed = reference_fragment(&environment, section.header(), parse)?;
                    if parsed.len() != 1
                        || parsed[0].target.offset != 0
                        || !parsed[0].supported_container
                        || parsed[0].target.setext
                    {
                        return None;
                    }
                    section.heading = parsed.pop();
                }
            }
        }
        Some(Self {
            source: source.into(),
            sections,
            environment,
        })
    }
}

impl ReferenceSection {
    fn header(&self) -> &str {
        if self.heading.is_none() {
            return "";
        }
        self.source.split_inclusive('\n').next().unwrap_or("")
    }

    fn header_references(&self) -> bool {
        self.header().contains('[')
    }
}

fn reference_sections(source: &str, entries: Vec<HeadingEntry>) -> Option<Vec<ReferenceSection>> {
    let entries = entries
        .into_iter()
        .filter(|h| h.supported_container)
        .collect::<Vec<_>>();
    if entries.iter().any(|h| h.target.setext) {
        return None;
    }
    let mut result = Vec::new();
    let mut start = 0;
    let mut heading = None;
    for mut entry in entries {
        let end = entry.target.offset;
        if end > start {
            result.push(ReferenceSection {
                source: source.get(start..end)?.into(),
                heading,
            });
        }
        start = end;
        entry.target.offset = 0;
        heading = Some(entry);
    }
    if start < source.len() || heading.is_some() {
        result.push(ReferenceSection {
            source: source.get(start..)?.into(),
            heading,
        });
    }
    Some(result)
}

fn reference_environment<'a>(sections: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut environment = String::new();
    for source in sections {
        // Only a candidate filter. Comrak interprets the entire original
        // container, including quote/list indentation, titles and fenced fakes.
        // No reference label, URL or container grammar is reconstructed here.
        if source.contains("]:") {
            if !source.ends_with("\n\n") && !source.ends_with("\r\n\r\n") {
                return None;
            }
            environment.push_str(REFERENCE_BOUNDARY);
            environment.push_str(source);
            environment.push_str("\n\n");
        }
    }
    Some(environment)
}

fn reference_fragment(
    environment: &str,
    source: &str,
    parse: &mut impl FnMut(&str) -> Vec<HeadingEntry>,
) -> Option<Vec<HeadingEntry>> {
    let prefix = environment.len() + REFERENCE_BOUNDARY.len();
    let input = format!("{environment}{REFERENCE_BOUNDARY}{source}");
    let parsed = parse(&input);
    if !parsed
        .iter()
        .any(|h| h.supported_container && !h.target.setext && h.target.offset == environment.len())
    {
        return None;
    }
    Some(
        parsed
            .into_iter()
            .filter(|h| h.target.offset >= prefix)
            .map(|mut h| {
                h.target.offset -= prefix;
                h
            })
            .collect(),
    )
}

fn same_heading(a: &HeadingEntry, b: &HeadingEntry) -> bool {
    a.text == b.text
        && a.level == b.level
        && a.supported_container == b.supported_container
        && a.target.offset == b.target.offset
        && a.target.setext == b.target.setext
}

/// The outline and its click coordinates share one exact editor snapshot.
#[derive(Default)]
pub(crate) struct DraftOutline {
    pub(super) cache: Option<DraftCache>,
    pub(super) source: String,
    pub(super) entries: Vec<HeadingEntry>,
    pub(super) lines: Vec<usize>,
    pub(super) pending: bool,
    pub(super) worker: Option<gpui::Task<()>>,
}

impl DraftOutline {
    #[cfg(test)]
    pub(super) fn new(source: &str) -> Self {
        Self::with_cache(source, &mut DraftCache::default())
    }

    pub(super) fn with_cache(source: &str, cache: &mut DraftCache) -> Self {
        let body = okilum_core::render::without_frontmatter(source);
        let prefix = source.len() - body.len();
        let entries: Vec<_> = cache
            .update(body)
            .into_iter()
            .filter(|heading| heading.supported_container)
            .enumerate()
            .map(|(index, mut heading)| {
                // Draft clicks use authored lines, never Reader block indexes.
                heading.target.block = index;
                heading.target.offset += prefix;
                heading
            })
            .collect();
        let mut previous = 0;
        let mut line = 0;
        let lines = entries
            .iter()
            .map(|heading| {
                line += source[previous..heading.target.offset]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count();
                previous = heading.target.offset;
                line
            })
            .collect();
        Self {
            cache: None,
            source: source.into(),
            entries,
            lines,
            pending: false,
            worker: None,
        }
    }

    pub(super) fn line_for(
        &self,
        current: &str,
        heading: &HeadingEntry,
    ) -> Result<u32, &'static str> {
        if self.pending || self.source != current {
            return Err(super::STALE_HEADING);
        }
        let index = self
            .entries
            .iter()
            .position(|entry| entry == heading)
            .ok_or(super::STALE_HEADING)?;
        u32::try_from(self.lines[index]).map_err(|_| super::STALE_HEADING)
    }

    pub(crate) fn current(&self, line: usize) -> Option<usize> {
        if self.pending {
            return None;
        }
        self.lines
            .partition_point(|heading| *heading <= line)
            .checked_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_core::document_links::HeadingTarget;

    #[test]
    fn incremental_revisions_match_canonical_inventory_and_authored_lines() {
        let mut cache = DraftCache::default();
        let initial = "---\r\ntitle: Synthetic\r\n---\r\n# Start\r\n\r\nBody Ω\r\n\r\nTarget\r\n------\r\n\r\n# Same\r\n\r\n# Same\r\n";
        let renamed = initial.replace("Target", "Renamed 🎯");
        let inserted = renamed.replace("Body Ω", "Body Ω\r\n\r\n## Inserted");
        let removed = inserted.replacen("# Same\r\n\r\n", "", 1);
        let merged = removed.replace("\r\n\r\nRenamed", "\r\nRenamed");
        let fenced = format!("```\n{merged}\n```\n# Outside\n");
        let reference = "# [Label][id]\n\n[id]: /target\n";
        for source in [
            initial, &renamed, &inserted, &removed, &merged, &fenced, reference, initial, "",
        ] {
            let snapshot = DraftOutline::with_cache(source, &mut cache);
            let body = okilum_core::render::without_frontmatter(source);
            let prefix = source.len() - body.len();
            let expected = okilum_core::document_links::HeadingInventory::new(body)
                .entries
                .into_iter()
                .filter(|h| h.supported_container)
                .enumerate()
                .map(|(index, mut h)| {
                    h.target.block = index;
                    h.target.offset += prefix;
                    h
                })
                .collect::<Vec<_>>();
            assert_eq!(snapshot.entries, expected, "revision: {source}");
            for heading in &snapshot.entries {
                let line = source[..heading.target.offset]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count();
                assert_eq!(snapshot.line_for(source, heading), Ok(line as u32));
            }
        }
    }

    fn draft_parity(cache: &mut DraftCache, source: &str) -> DraftOutline {
        let outline = DraftOutline::with_cache(source, cache);
        let body = okilum_core::render::without_frontmatter(source);
        let prefix = source.len() - body.len();
        let expected = HeadingInventory::new(body)
            .entries
            .into_iter()
            .filter(|h| h.supported_container)
            .enumerate()
            .map(|(index, mut h)| {
                h.target.block = index;
                h.target.offset += prefix;
                h
            })
            .collect::<Vec<_>>();
        assert_eq!(outline.entries, expected, "source: {source}");
        for heading in &outline.entries {
            let line = source[..heading.target.offset]
                .bytes()
                .filter(|b| *b == b'\n')
                .count();
            assert_eq!(outline.line_for(source, heading), Ok(line as u32));
        }
        outline
    }

    #[test]
    fn container_definitions_invalidate_reference_headings_and_reuse_unrelated_sections() {
        for definition in [
            "> [ref]: /target\r\n>\r\n",
            "- [ref]: /target\r\n",
            "- Parent\r\n  - [ref]: /target\r\n",
        ] {
            let source = format!(
                "---\r\ntitle: Ω\r\n---\r\n# Ω [Alias][ref]\r\n\r\n{}# Definitions\r\n\r\n{definition}\r\n# Tail 🎯 [ref]\r\n\r\n",
                (0..300)
                    .map(|i| format!("# Stable {i}\r\n\r\nBody\r\n\r\n"))
                    .collect::<String>()
            );
            let mut cache = DraftCache::default();
            let before = draft_parity(&mut cache, &source);
            assert_eq!(
                before.entries[0].text, "Ω Alias",
                "positive control: container definition resolves"
            );
            assert!(cache.containers.is_some(), "container cache admitted");
            let removed = source.replace("[ref]: /target", "Literal definition removed");
            let body = okilum_core::render::without_frontmatter(&removed);
            let mut calls = Vec::new();
            let entries = cache.update_with(body, &mut |text| {
                calls.push(text.to_owned());
                HeadingInventory::new(text).entries
            });
            let mut expected = HeadingInventory::new(body)
                .entries
                .into_iter()
                .filter(|h| h.supported_container)
                .collect::<Vec<_>>();
            let mut actual = entries
                .into_iter()
                .filter(|h| h.supported_container)
                .collect::<Vec<_>>();
            for entries in [&mut expected, &mut actual] {
                for (index, h) in entries.iter_mut().enumerate() {
                    h.target.block = index;
                }
            }
            assert_eq!(actual, expected);
            assert!(calls.iter().any(|s| s.contains("# Ω [Alias][ref]")));
            assert!(calls.iter().any(|s| s.contains("# Tail 🎯 [ref]")));
            assert!(
                !calls.iter().any(|s| s.contains("# Stable")),
                "unrelated sections must not reach parser"
            );
            assert!(calls.iter().map(String::len).sum::<usize>() < 4096);
            let after = draft_parity(&mut cache, &removed);
            assert!(after.entries[0].text.contains("[Alias][ref]"));
            assert!(after.line_for(&source, &before.entries[0]).is_err());
            draft_parity(&mut cache, &source);
        }
    }

    #[test]
    fn container_environment_keeps_canonical_first_wins_and_unicode_labels() {
        let source = "# Ω [Alias][κλειδί]\n\n# First\n\n> [κλειδί]: /first \"First title\"\n\n# Second\n\n- [κλειδί]: /second\n\n# Fenced fake\n\n```\n[κλειδί]: /fake\n```\n\n# Tail [κλειδί]\n\n";
        let mut cache = DraftCache::default();
        draft_parity(&mut cache, source);
        let environment = &cache
            .containers
            .as_ref()
            .expect("canonical container admission")
            .environment;
        assert!(environment.find("/first").unwrap() < environment.find("/second").unwrap());
        assert!(environment.find("/second").unwrap() < environment.find("/fake").unwrap());
        for revised in [
            source.replace("> [κλειδί]: /first \"First title\"", "> Removed"),
            source.replace("- [κλειδί]: /second", "- Removed"),
            source.replace("κλειδί", "ΚΛΕΙΔΊ"),
            source.replace("# Ω", "# Ψ"),
            source.replace("# Tail", "# 🎯 Tail"),
            source
                .replace("> [κλειδί]: /first \"First title\"", "> Removed")
                .replace("- [κλειδί]: /second", "- Removed"),
        ] {
            draft_parity(&mut cache, &revised);
        }
        draft_parity(&mut cache, source);
    }

    #[test]
    fn changed_container_context_and_unsupported_forms_fail_back_safely() {
        let seed = "# [Alias][id]\n\n# Definitions\n\n> [id]: /target\n\n# Tail\n\n";
        for insert in [
            "```\n",
            "~~~\n",
            "<script>\n",
            "- Item\n  ",
            "> Quote\n",
            "# New Ω\n\n",
        ] {
            let mut cache = DraftCache::default();
            draft_parity(&mut cache, seed);
            let changed = seed.replace("> [id]: /target", &format!("{insert}> [id]: /target"));
            draft_parity(&mut cache, &changed);
            draft_parity(&mut cache, seed);
        }
        for source in [
            "# [^footnote]\n\n[^footnote]: Text\n",
            "# [[Wiki]]\n\n> [id]: /target\n",
            "Heading [id]\n------------\n\n> [id]: /target\n",
            "# [Alias][id]\n\n> [id]: /target",
        ] {
            let mut cache = DraftCache::default();
            draft_parity(&mut cache, source);
            assert!(
                cache.containers.is_none(),
                "unsupported ambiguity is not retained"
            );
        }
    }

    #[test]
    fn container_reference_edits_at_each_line_boundary_match_current_draft_parser() {
        let seed = "# Ω [Alias][id]\r\n\r\n# Definitions\r\n\r\n- Parent\r\n  > [id]: /target\r\n\r\n# Next [id]\r\n\r\n```\r\n[id]: /fake\r\n```\r\n\r\n# Tail 🎯\r\n\r\n";
        let boundaries = std::iter::once(0).chain(seed.match_indices('\n').map(|(i, _)| i + 1));
        for boundary in boundaries {
            for insert in [
                "```\r\n",
                "~~~\r\n",
                "<script>\r\n",
                "> ",
                "  ",
                "# New [id]\r\n\r\n",
            ] {
                let mut cache = DraftCache::default();
                draft_parity(&mut cache, seed);
                let changed = format!("{}{insert}{}", &seed[..boundary], &seed[boundary..]);
                draft_parity(&mut cache, &changed);
                draft_parity(&mut cache, seed);
            }
        }
        let mut cache = DraftCache::default();
        for source in [
            seed.to_owned(),
            seed.replace("Ω", "Ψ"),
            seed.replace("🎯", "🎪"),
            "# Rebound [id]\n\n> [id]: /other\n\n".into(),
        ] {
            draft_parity(&mut cache, &source);
        }
        let mut flat = DraftCache::default();
        draft_parity(&mut flat, "# [Alias][id]\n\n[id]: /flat\n\n");
        assert!(
            flat.containers.is_none(),
            "existing flat per-label cache remains the route"
        );
    }

    fn snapshot() -> DraftOutline {
        DraftOutline {
            source: "# Same\n\n# Same\n".into(),
            entries: [0, 8]
                .into_iter()
                .enumerate()
                .map(|(block, offset)| HeadingEntry {
                    text: "Same".into(),
                    level: 1,
                    supported_container: true,
                    target: HeadingTarget {
                        block,
                        offset,
                        setext: false,
                    },
                })
                .collect(),
            lines: vec![0, 2],
            ..Default::default()
        }
    }

    #[test]
    fn duplicate_click_uses_exact_snapshot_and_stale_work_cannot_land() {
        let mut outline = snapshot();
        let second = outline.entries[1].clone();
        assert_eq!(
            outline.line_for(&outline.source, &second),
            Ok(2),
            "positive control: second duplicate"
        );
        assert!(outline.line_for("# Renamed\n\n# Same\n", &second).is_err());
        assert_eq!(outline.current(2), Some(1));
        outline.pending = true;
        assert!(outline.line_for(&outline.source, &second).is_err());
        assert_eq!(
            outline.current(2),
            None,
            "pending revision has no current section"
        );
    }

    #[test]
    fn removed_or_renamed_heading_cannot_retain_an_invalid_selection() {
        let mut outline = snapshot();
        let removed = outline.entries.remove(1);
        outline.lines.remove(1);
        outline.source = "# Same\n".into();
        assert_eq!(outline.current(2), Some(0));
        assert!(outline.line_for(&outline.source, &removed).is_err());
        let old = outline.entries[0].clone();
        outline.entries[0].text = "Renamed".into();
        outline.source = "# Renamed\n".into();
        assert_eq!(outline.current(0), Some(0));
        assert!(outline.line_for(&outline.source, &old).is_err());
        assert_eq!(
            outline.line_for(&outline.source, &outline.entries[0]),
            Ok(0)
        );
        outline.entries.clear();
        outline.lines.clear();
        outline.source.clear();
        assert_eq!(outline.current(99), None);
    }
}
