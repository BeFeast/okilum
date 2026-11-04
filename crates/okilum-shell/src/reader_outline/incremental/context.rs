//! Parser-certified root ATX headings are context checkpoints. Reparse from an
//! unchanged checkpoint before the edit, then probe an unchanged suffix heading.
//! Reuse is permitted only when the canonical parser sees that heading at root
//! again. An open fence/list/HTML container therefore expands the dirty suffix.
use okilum_core::document_links::HeadingEntry;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Context {
    snapshot: Option<Snapshot>,
    references: Option<ReferenceSnapshot>,
}

struct Snapshot {
    source: String,
    entries: Vec<HeadingEntry>,
}

impl Context {
    pub(super) fn update(
        &mut self,
        source: &str,
        parse: &mut impl FnMut(&str) -> Vec<HeadingEntry>,
    ) -> Vec<HeadingEntry> {
        if source.contains(['[', ']']) {
            self.snapshot = None;
            let old = self.references.take();
            if let Some((entries, snapshot)) = reference_revision(source, old, parse) {
                self.references = Some(snapshot);
                return entries;
            }
            return parse(source);
        }
        self.references = None;
        let entries = match self.snapshot.take() {
            None => parse(source),
            Some(old) if old.source == source => old.entries,
            Some(old) => revise(&old, source, parse),
        };
        self.snapshot = Some(Snapshot {
            source: source.into(),
            entries: entries.clone(),
        });
        entries
    }
}

// This is a dependency scanner, not a Markdown parser: it accepts only flat,
// independently bounded blocks and simple ASCII reference identifiers. Every
// heading is still produced by the canonical parser with the complete definition
// environment. Containers, escapes, Unicode labels, footnotes and unknown forms
// retain the whole-document fallback rather than guessing normalization/context.
struct ReferenceSnapshot {
    blocks: Vec<ReferenceBlock>,
    definitions: BTreeMap<String, String>,
}

struct ReferenceBlock {
    source: String,
    dependencies: BTreeSet<String>,
    entries: Vec<HeadingEntry>,
}

fn reference_label(label: &str) -> Option<String> {
    if !label
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_ \t".contains(&b))
    {
        return None;
    }
    let label = label
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    (!label.is_empty()).then_some(label)
}

fn reference_revision(
    source: &str,
    old: Option<ReferenceSnapshot>,
    parse: &mut impl FnMut(&str) -> Vec<HeadingEntry>,
) -> Option<(Vec<HeadingEntry>, ReferenceSnapshot)> {
    if source.starts_with("---\n")
        || source.starts_with("---\r\n")
        || source.starts_with('\u{feff}')
    {
        return None;
    }
    let mut definitions = BTreeMap::new();
    let mut environment = String::new();
    let mut masked = String::new();
    let mut block_dependencies = Vec::new();
    let mut dependencies = BTreeSet::new();
    let mut has_definition = false;
    let mut has_content = false;
    for line in source.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        if text.bytes().all(|b| b == b' ' || b == b'\t') {
            if has_definition && has_content {
                return None;
            }
            block_dependencies.push(std::mem::take(&mut dependencies));
            has_definition = false;
            has_content = false;
            masked.push('\n');
            continue;
        }
        if let Some((label, destination)) = text.strip_prefix('[').and_then(|s| s.split_once("]:"))
        {
            let label = reference_label(label)?;
            let destination = destination.strip_prefix(' ')?;
            let destination = destination
                .strip_prefix('<')
                .and_then(|d| d.strip_suffix('>'))
                .unwrap_or(destination);
            if destination.is_empty()
                || destination
                    .chars()
                    .any(|c| c.is_whitespace() || "[]<>\\\"'()".contains(c))
            {
                return None;
            }
            definitions.entry(label).or_insert_with(|| text.to_owned());
            environment.push_str(text);
            environment.push('\n');
            has_definition = true;
            masked.push_str("Definition\n");
            continue;
        }
        has_content = true;
        let mut remaining = text;
        while let Some(open) = remaining.find('[') {
            let plain = &remaining[..open];
            if plain.contains(']') {
                return None;
            }
            masked.push_str(plain);
            let rest = &remaining[open + 1..];
            let close = rest.find(']')?;
            let label = &rest[..close];
            if !label.is_empty() {
                dependencies.insert(reference_label(label)?);
            }
            masked.push_str("Reference");
            remaining = &rest[close + 1..];
        }
        if remaining.contains(']') {
            return None;
        }
        masked.push_str(remaining);
        masked.push('\n');
    }
    if has_definition && has_content {
        return None;
    }
    // Reuse the existing conservative block gate; replacing bracket tokens
    // cannot authorize indentation, lists, fences, HTML, escapes or extensions.
    super::independent_blocks(&masked)?;
    // A final nonblank block has not yet contributed its dependency set.
    if has_content || has_definition {
        block_dependencies.push(dependencies);
    }
    let mut parts = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        offset += line.len();
        if line
            .trim_end_matches(['\n', '\r'])
            .bytes()
            .all(|b| b == b' ' || b == b'\t')
        {
            parts.push(&source[start..offset]);
            start = offset;
        }
    }
    if start < source.len() {
        parts.push(&source[start..]);
    }
    if parts.len() != block_dependencies.len() {
        return None;
    }
    let cold = old.is_none();
    let old = old.unwrap_or(ReferenceSnapshot {
        blocks: Vec::new(),
        definitions: BTreeMap::new(),
    });
    let changed = definitions
        .keys()
        .chain(old.definitions.keys())
        .filter(|key| definitions.get(*key) != old.definitions.get(*key))
        .cloned()
        .collect::<BTreeSet<_>>();
    let prefix = parts
        .iter()
        .zip(&old.blocks)
        .take_while(|(part, block)| **part == block.source)
        .count();
    let suffix = parts[prefix..]
        .iter()
        .rev()
        .zip(old.blocks[prefix..].iter().rev())
        .take_while(|(part, block)| **part == block.source)
        .count();
    let old_len = old.blocks.len();
    let mut previous = old.blocks.into_iter().map(Some).collect::<Vec<_>>();
    environment.push('\n');
    let mut blocks = Vec::new();
    let mut entries = Vec::new();
    let mut offset = 0;
    for (index, part) in parts.iter().enumerate() {
        let cached = if index < prefix {
            previous[index].take()
        } else if index >= parts.len() - suffix {
            previous[old_len - (parts.len() - index)].take()
        } else {
            None
        };
        let block = match cached {
            Some(block) if block.dependencies.is_disjoint(&changed) => block,
            _ => {
                // Prepend definitions so first-definition-wins is identical
                // even when this block contains a later duplicate definition.
                let input = format!("{environment}{part}");
                let mut parsed = parse(&input);
                for heading in &mut parsed {
                    heading.target.offset = heading.target.offset.checked_sub(environment.len())?;
                }
                ReferenceBlock {
                    source: (*part).into(),
                    dependencies: block_dependencies[index].clone(),
                    entries: parsed,
                }
            }
        };
        entries.extend(block.entries.iter().cloned().map(|mut heading| {
            heading.target.offset += offset;
            heading
        }));
        offset += part.len();
        blocks.push(block);
    }
    // A cold cache verifies the scanner's accepted layout against the whole
    // canonical document. Unknown parser behavior fails closed, never cached.
    if cold {
        let expected = parse(source);
        if entries.len() != expected.len()
            || entries.iter().zip(&expected).any(|(a, b)| {
                a.text != b.text
                    || a.level != b.level
                    || a.supported_container != b.supported_container
                    || a.target.offset != b.target.offset
                    || a.target.setext != b.target.setext
            })
        {
            return None;
        }
    }
    Some((
        entries,
        ReferenceSnapshot {
            blocks,
            definitions,
        },
    ))
}

fn header_end(source: &str, offset: usize) -> usize {
    source[offset..]
        .find('\n')
        .map_or(source.len(), |n| offset + n + 1)
}

fn checkpoint(entry: &HeadingEntry) -> bool {
    entry.supported_container && !entry.target.setext
}

fn revise(
    old: &Snapshot,
    source: &str,
    parse: &mut impl FnMut(&str) -> Vec<HeadingEntry>,
) -> Vec<HeadingEntry> {
    let (prefix, suffix) = unchanged(&old.source, source);
    // The whole checkpoint header must be unchanged, including its newline.
    // Starting at that certified root preserves all preceding container state.
    let start = old
        .entries
        .iter()
        .filter(|h| checkpoint(h) && header_end(&old.source, h.target.offset) <= prefix)
        .map(|h| h.target.offset)
        .next_back()
        .unwrap_or(0);
    let old_suffix = old.source.len() - suffix;
    let new_suffix = source.len() - suffix;
    let mut minimum_probe = start;
    let mut probes = 0;
    for boundary in old
        .entries
        .iter()
        .filter(|h| checkpoint(h) && h.target.offset >= old_suffix)
    {
        let end = new_suffix + (boundary.target.offset - old_suffix);
        if end <= start || end < minimum_probe {
            continue;
        }
        if probes == 8 {
            break;
        }
        probes += 1;
        let probe_end = header_end(source, end);
        let parsed = parse(&source[start..probe_end]);
        let certified = parsed.iter().any(|heading| {
            checkpoint(heading)
                && heading.target.offset == end - start
                && heading.text == boundary.text
                && heading.level == boundary.level
        });
        if certified {
            let mut result = old
                .entries
                .iter()
                .filter(|h| h.target.offset < start)
                .cloned()
                .collect::<Vec<_>>();
            result.extend(
                parsed
                    .into_iter()
                    .filter(|h| h.target.offset < end - start)
                    .map(|mut h| {
                        h.target.offset += start;
                        h
                    }),
            );
            result.extend(
                old.entries
                    .iter()
                    .filter(|h| h.target.offset >= boundary.target.offset)
                    .cloned()
                    .map(|mut h| {
                        h.target.offset = new_suffix + (h.target.offset - old_suffix);
                        h
                    }),
            );
            return result;
        }
        // Geometric byte growth and a hard probe cap bound failed resync work.
        minimum_probe = start.saturating_add((probe_end - start).saturating_mul(2));
    }
    let mut result = old
        .entries
        .iter()
        .filter(|h| h.target.offset < start)
        .cloned()
        .collect::<Vec<_>>();
    result.extend(parse(&source[start..]).into_iter().map(|mut h| {
        h.target.offset += start;
        h
    }));
    result
}

/// Byte diff, rounded outwards to exact UTF-8 boundaries in both snapshots.
fn unchanged(old: &str, new: &str) -> (usize, usize) {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    (prefix, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_core::document_links::HeadingInventory;

    fn normalized(mut entries: Vec<HeadingEntry>) -> Vec<HeadingEntry> {
        // Reader block ordinals are irrelevant to authored-line draft clicks.
        for (index, heading) in entries.iter_mut().enumerate() {
            heading.target.block = index;
        }
        entries
    }

    fn parity(context: &mut Context, source: &str) -> usize {
        let mut bytes = 0;
        let entries = context.update(source, &mut |part| {
            bytes += part.len();
            HeadingInventory::new(part).entries
        });
        assert_eq!(
            normalized(entries),
            normalized(HeadingInventory::new(source).entries),
            "source: {source}"
        );
        bytes
    }

    #[test]
    fn fences_lists_and_html_resynchronize_only_at_parser_certified_root() {
        let original = "# Before Ω\r\n\r\n- Item\r\n  - Nested\r\n\r\n  ## Nested heading\r\n\r\n# Middle 🎯\r\n\r\n```rust\r\n# Code heading\r\n```\r\n\r\n  ## Indented root\r\n\r\n# End\r\n\r\nTail\r\n";
        let mut context = Context::default();
        assert_eq!(
            parity(&mut context, original),
            original.len(),
            "cold parser positive control"
        );
        let revisions = [
            original.replace("Nested", "Nested renamed λ"),
            original.replace("  ## Nested heading", "## Formerly nested"),
            original.replace("```rust", "````rust"),
            original.replace("```rust", "~~~rust"),
            original.replacen("```\r\n", "", 1),
            original.replace("# Middle 🎯", "  # Middle 🎯"),
            original.replace("- Item", "- Item\r\n\r\n  ```\r\n"),
            original.replace("# Middle 🎯", "<script>\r\n# Middle 🎯"),
            original.replace("- Item", "- Item\r\n\r\n  [id]: /target"),
            original.to_owned(),
            "# Rebound\n\n1. Item\n\n   ## Nested\n\n# End\n".into(),
            "".into(),
        ];
        for source in revisions {
            parity(&mut context, &source);
        }
    }

    #[test]
    fn dirty_container_edit_reuses_large_suffix_but_open_fence_invalidates_it() {
        let original = (0..1000)
            .map(|i| {
                format!("# Section {i}\n\n- Item {i}\n  - Nested\n\n```rust\n# Hidden\n```\n\n")
            })
            .collect::<String>();
        let mut context = Context::default();
        assert_eq!(parity(&mut context, &original), original.len());
        let edited = original.replace("- Item 500\n", "- Item 500 Ω\n  - Added\n");
        let bytes = parity(&mut context, &edited);
        assert!(bytes > 0 && bytes < 512, "bounded parser work: {bytes}");
        // An unterminated fence must swallow every previously cached heading
        // after the edit. No unchanged suffix checkpoint can certify root.
        let open = format!(
            "{}\n````\n{}",
            &edited[..edited.find("# Section 501").unwrap()],
            &edited[edited.find("# Section 501").unwrap()..]
        );
        let bytes = parity(&mut context, &open);
        assert!(
            bytes >= open.len() - open.find("# Section 500").unwrap(),
            "positive control: context-changing edit expands through EOF"
        );
        parity(&mut context, &edited);
        assert_eq!(parity(&mut context, &edited), 0);
    }

    #[test]
    fn reference_definitions_invalidate_only_dependent_blocks_with_exact_offsets() {
        let source = format!(
            "# Ω [Alias][target]\r\n\r\n# [other]\r\n\r\n{}[target]: /first\r\n\r\n[other]: /other\r\n\r\n# Tail 🎯\r\n\r\n# Suffix [target]\r\n",
            (0..500)
                .map(|i| format!("# Stable {i}\r\n\r\nBody\r\n\r\n"))
                .collect::<String>()
        );
        let mut context = Context::default();
        parity(&mut context, &source);
        assert!(
            context.references.is_some(),
            "positive control: dependency cache enrolled"
        );
        let removed = source.replace("[target]: /first\r\n\r\n", "");
        let mut calls = Vec::new();
        let entries = context.update(&removed, &mut |text| {
            calls.push(text.to_owned());
            HeadingInventory::new(text).entries
        });
        assert_eq!(
            normalized(entries),
            normalized(HeadingInventory::new(&removed).entries)
        );
        assert!(
            calls
                .iter()
                .any(|part| part.contains("# Ω [Alias][target]")),
            "dependent prefix heading reparses"
        );
        assert!(
            calls.iter().any(|part| part.contains("# Suffix [target]")),
            "dependent suffix heading reparses"
        );
        assert!(
            !calls.iter().any(|part| part.contains("# [other]")
                || part.contains("# Stable")
                || part.contains("# Tail")),
            "unaffected prefix and shifted suffix reuse"
        );
        assert!(calls.iter().map(String::len).sum::<usize>() < 512);
        for changed in [
            removed.replace("[other]: /other", "[TARGET]: /new\r\n\r\n[other]: /other"),
            source.replace("[target]: /first", "[target]: /new"),
            source.replace("# Ω [Alias][target]", "# Ω [target][]"),
            source.replace("# Ω [Alias][target]", "# Ω [target]"),
            source.replace("[target]: /first", "[Target]: /first\r\n[target]: /ignored"),
            source
                .replace("[target]: /first", "[Target]: /first\r\n[target]: /ignored")
                .replace("[Target]: /first\r\n", ""),
        ] {
            parity(&mut context, &changed);
        }
    }

    #[test]
    fn ascii_label_normalization_duplicates_and_setext_match_canonical_parser() {
        let source = "# Ω [Alias][A  B]\n\nShortcut [a b]\n----------------\n\n# [A B][]\n\n[a b]: /first\n[A B]: /ignored\n\n# Tail 🎯\n";
        let mut context = Context::default();
        parity(&mut context, source);
        assert!(context.references.is_some());
        for revised in [
            source.replace("[a b]: /first\n", ""),
            source.replace("[a b]: /first\n[A B]: /ignored\n", ""),
            source.replace("[a b]: /first", "[A\tB]: /new"),
            source.replace("# Ω [Alias][A  B]", "# Ψ [Alias][missing]"),
            source.replace("[a b]: /first", "[missing]: /new\n[a b]: /first"),
        ] {
            parity(&mut context, &revised);
        }
    }

    #[test]
    fn unknown_reference_syntax_falls_back_and_discards_dependency_state() {
        let known = "# Ω [Alias][id]\n\n[id]: /target\n\n# End\n";
        for unknown in [
            known.replace("[id]: /target", "[id]: /target \"Title\""),
            known.replace("[id]: /target", "[id]:\n  /target"),
            known.replace("[id]", "[κλειδί]"),
            known.replace("# End", "- Item\n  ## [Alias][id]"),
            known.replace("# End", "```\n# [Alias][id]\n```"),
            known.replace("# End", "# [[Wiki]]"),
            known.replace("# End", "# [^footnote]"),
            format!("---\n{known}---\n# Outside\n"),
        ] {
            let mut context = Context::default();
            parity(&mut context, known);
            let mut calls = Vec::new();
            let entries = context.update(&unknown, &mut |text| {
                calls.push(text.to_owned());
                HeadingInventory::new(text).entries
            });
            assert_eq!(
                calls,
                std::slice::from_ref(&unknown),
                "unknown syntax is one full canonical parse"
            );
            assert!(context.references.is_none());
            assert_eq!(
                normalized(entries),
                normalized(HeadingInventory::new(&unknown).entries)
            );
            parity(&mut context, known);
        }
    }

    #[test]
    fn every_line_boundary_container_edit_matches_full_parser() {
        let seeds = [
            "# Start Ω\n\n- Item\n  - Nested\n\n  ## Child\n\n# Next\n\n```rust\n# Hidden\n```\n\n# End 🎯\n",
            "# Start\r\n\r\n1. Item\r\n\r\n   ```\r\n   ## Hidden\r\n   ```\r\n\r\n  ## Root\r\n\r\n# End\r\n",
        ];
        let edits = [
            "```\n",
            "````\n",
            "~~~\n",
            "- Item\n",
            "  - Nested\n",
            "    ",
            "  ",
            "> Quote\n",
            "<div>\n",
            "</div>\n",
            "# New Ω\n",
            "\n",
            "[id]: /target\n",
        ];
        for seed in seeds {
            let offsets = std::iter::once(0)
                .chain(seed.match_indices('\n').map(|(i, _)| i + 1))
                .collect::<Vec<_>>();
            for offset in offsets {
                for edit in edits {
                    let mut context = Context::default();
                    parity(&mut context, seed);
                    let changed = format!("{}{}{}", &seed[..offset], edit, &seed[offset..]);
                    parity(&mut context, &changed);
                    parity(&mut context, seed);
                }
            }
        }
    }

    #[test]
    fn utf8_dirty_ranges_round_outward_and_do_not_split_authored_offsets() {
        for (before, after) in [
            ("Ω", "Ψ"),
            ("🎯", "🎪"),
            ("aéz", "aêz"),
            ("é", "é🎯"),
            ("", "Ω"),
        ] {
            let (prefix, suffix) = unchanged(before, after);
            assert!(before.is_char_boundary(prefix) && after.is_char_boundary(prefix));
            assert!(before.is_char_boundary(before.len() - suffix));
            assert!(after.is_char_boundary(after.len() - suffix));
        }
        let mut context = Context::default();
        for source in [
            "# Ω\n\n- Item\n\n# 🎯\n",
            "# Ψ\n\n- Item λ\n\n# 🎪\n",
            "# Ψ\n\n- Item\n  ## Νέο\n\n# 🎪\n",
        ] {
            parity(&mut context, source);
        }
    }
}
