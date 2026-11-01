//! Reuse independently parsed Markdown blocks. The fast path deliberately
//! accepts only context-free paragraphs/ATX/Setext; context-sensitive Markdown
//! falls back to the canonical whole-document parser, never a guessed boundary.
use okilum_core::document_links::{HeadingEntry, HeadingInventory};

#[derive(Default)]
pub(super) struct Cache {
    blocks: Vec<Block>,
}

struct Block {
    source: String,
    entries: Vec<HeadingEntry>,
}

impl Cache {
    pub(super) fn update(&mut self, source: &str) -> Vec<HeadingEntry> {
        self.update_with(source, |text| HeadingInventory::new(text).entries)
    }

    fn update_with(
        &mut self,
        source: &str,
        mut parse: impl FnMut(&str) -> Vec<HeadingEntry>,
    ) -> Vec<HeadingEntry> {
        let Some(parts) = independent_blocks(source) else {
            self.blocks.clear();
            return parse(source);
        };
        let old = std::mem::take(&mut self.blocks);
        let prefix = parts
            .iter()
            .zip(&old)
            .take_while(|(part, old)| **part == old.source)
            .count();
        let suffix = parts[prefix..]
            .iter()
            .rev()
            .zip(old[prefix..].iter().rev())
            .take_while(|(part, old)| **part == old.source)
            .count();
        let old_len = old.len();
        let mut previous = old.into_iter().map(Some).collect::<Vec<_>>();
        let mut entries = Vec::new();
        let mut offset = 0;
        for (index, part) in parts.iter().enumerate() {
            let block = if index < prefix {
                previous[index].take().unwrap()
            } else if index >= parts.len() - suffix {
                previous[old_len - (parts.len() - index)].take().unwrap()
            } else {
                Block {
                    source: (*part).into(),
                    entries: parse(part),
                }
            };
            entries.extend(block.entries.iter().cloned().map(|mut heading| {
                heading.target.offset += offset;
                heading
            }));
            offset += part.len();
            self.blocks.push(block);
        }
        entries
    }
}

/// Blank lines are barriers only in this conservative grammar. A fence, list,
/// quote, HTML, reference definition/use, indentation or extension delimiter
/// invalidates the fast path for the entire snapshot, including cached blocks.
fn independent_blocks(source: &str) -> Option<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        if text.bytes().all(|byte| byte == b' ' || byte == b'\t') {
            offset += line.len();
            blocks.push(&source[start..offset]);
            start = offset;
            continue;
        }
        if text.starts_with(char::is_whitespace)
            || text.contains(['[', ']', '<', '>', '`', '~', '|', '\\', '*', '_', '$', '!'])
        {
            return None;
        }
        let atx = text.trim_start_matches('#');
        let heading = atx.len() < text.len()
            && text.len() - atx.len() <= 6
            && (atx.is_empty() || atx.starts_with(' '));
        let setext =
            (text.len() >= 3 && text.bytes().all(|b| b == b'-')) || text.bytes().all(|b| b == b'=');
        // Numeric starts can introduce ordered lists; reject rather than
        // attempting to infer continuation/container state across blocks.
        let paragraph = text.chars().next().is_some_and(|c| c.is_alphabetic());
        if !heading && !setext && !paragraph {
            return None;
        }
        offset += line.len();
    }
    if start < source.len() {
        blocks.push(&source[start..]);
    }
    Some(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn large_note_parses_only_dirty_block_and_reuses_shifted_suffix() {
        let source = (0..4000)
            .map(|i| format!("# Heading {i}\n\nBody paragraph\n\n"))
            .collect::<String>();
        let mut cache = Cache::default();
        let mut bytes = 0;
        cache.update_with(&source, |text| {
            bytes += text.len();
            Vec::new()
        });
        assert_eq!(
            bytes,
            source.len(),
            "positive control: cold parser receives every block"
        );
        let changed = source.replace("# Heading 2000", "# Renamed Unicode Ω\nAdditional line");
        bytes = 0;
        cache.update_with(&changed, |text| {
            bytes += text.len();
            Vec::new()
        });
        assert!(
            bytes > 0 && bytes < 100,
            "dirty parsing must be bounded, got {bytes}"
        );
        bytes = 0;
        cache.update_with(&changed, |text| {
            bytes += text.len();
            Vec::new()
        });
        assert_eq!(bytes, 0, "identical snapshot requires no parser call");
    }

    #[test]
    fn shifted_suffix_keeps_authored_offsets_after_insert_and_remove() {
        use okilum_core::document_links::HeadingTarget;
        let mut cache = Cache::default();
        let mut parse = |text: &str| {
            vec![HeadingEntry {
                text: text.trim().into(),
                level: 1,
                supported_container: true,
                target: HeadingTarget {
                    block: 0,
                    offset: 0,
                    setext: false,
                },
            }]
        };
        let original = "# First\n\n# Last\n";
        assert_eq!(cache.update_with(original, &mut parse).len(), 2);
        let inserted = "# First\n\n# Middle Ω\n\n# Last\n";
        let entries = cache.update_with(inserted, &mut parse);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].text, "# Last");
        assert_eq!(entries[2].target.offset, inserted.find("# Last").unwrap());
        let entries = cache.update_with("# Last\n", &mut parse);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target.offset, 0);
    }

    #[test]
    fn syntax_context_changes_invalidate_cached_boundaries() {
        for syntax in [
            "```\n",
            "~~~\n",
            "<div>\n",
            "> Quote\n",
            "- Item\n",
            "-\n",
            "1. Item\n",
            "    Code\n",
            "[id]: /target\n",
            "# [Label][id]\n",
            "| table |\n",
            "\u{a0}\n",
        ] {
            let source = format!("# Before\n\n{syntax}# After\n\n");
            let mut cache = Cache::default();
            cache.update_with("# Before\n\n# After\n\n", |_| Vec::new());
            let mut calls = Vec::new();
            cache.update_with(&source, |text| {
                calls.push(text.to_owned());
                Vec::new()
            });
            assert_eq!(
                calls,
                [source],
                "context-sensitive syntax must parse whole snapshot"
            );
        }
    }
}
