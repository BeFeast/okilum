//! Parser-certified root ATX headings are context checkpoints. Reparse from an
//! unchanged checkpoint before the edit, then probe an unchanged suffix heading.
//! Reuse is permitted only when the canonical parser sees that heading at root
//! again. An open fence/list/HTML container therefore expands the dirty suffix.
use okilum_core::document_links::HeadingEntry;

#[derive(Default)]
pub(super) struct Context {
    snapshot: Option<Snapshot>,
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
        // Reference definitions have document-wide effects on heading labels.
        // Until the canonical API exposes their dependency graph, do not reuse
        // any segment of a snapshot containing reference-capable syntax.
        if source.contains(['[', ']']) {
            self.snapshot = None;
            return parse(source);
        }
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
