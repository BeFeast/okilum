//! Where text finished in the background goes (#1611): the paste position,
//! found again after the user kept editing. Platform-neutral and pure.
//!
//! The editor has no persistent anchors, so a paste remembers its offset and
//! a little text on both sides. The offset wins while that text is still
//! around it; otherwise the surrounding text is searched for, and a unique
//! match is the moved position. No match: the caller uses the caret and says
//! so. A [`Region`] is the text a paste inserted, for results that replace
//! parts of it later (downloaded images).

use std::ops::Range;

/// Bytes of context kept on each side of the anchor.
const CONTEXT: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub offset: usize,
    before: String,
    after: String,
    /// `after` runs to the end of the text.
    to_end: bool,
    /// A block anchor whose `after` occurs more than once (the later of two
    /// identical blocks): `before` decides, and must precede the position.
    before_required: bool,
}

impl Anchor {
    /// The position `offset` in `source`.
    pub fn at(source: &str, offset: usize) -> Self {
        let offset = floor(source, offset.min(source.len()));
        let from = floor(source, offset.saturating_sub(CONTEXT));
        let to = floor(source, (offset + CONTEXT).min(source.len()));
        Self {
            offset,
            before: source[from..offset].to_string(),
            after: source[offset..to].to_string(),
            to_end: to == source.len(),
            before_required: false,
        }
    }

    /// The position `offset` before a block in `source`, for a drop onto
    /// that block (#1615). The text kept after it is the shortest start of
    /// the rest of the note that occurs only once in it (the whole rest when
    /// no start is unique), and at least the block's whole first line, not
    /// just [`CONTEXT`] bytes: two sections that
    /// share a longer opening (two `## Notes` with the same first lines) are
    /// still told apart by [`Self::locate_block`].
    pub fn at_block(source: &str, offset: usize) -> Self {
        let mut anchor = Self::at(source, offset);
        let rest = &source[anchor.offset..];
        let once = |len: usize| occurs_once(source, &rest[..len]);
        let repeated = !rest.is_empty() && !once(rest.len());
        let len = if rest.is_empty() || repeated {
            rest.len()
        } else {
            // A longer start of a unique start is unique too: search the
            // shortest one.
            let (mut low, mut high) = (1, rest.len());
            while low < high {
                let mid = ceil(rest, low + (high - low) / 2);
                if mid >= high {
                    break;
                }
                if once(mid) {
                    high = mid;
                } else {
                    low = ceil(rest, mid + 1);
                }
            }
            ceil(rest, high.min(rest.len()))
        };
        // At least the block's whole first line: a short unique start (`#`)
        // could survive in unrelated text after the block itself is gone.
        let len = len.max(rest.find('\n').map_or(rest.len(), |at| at + 1));
        anchor.after = rest[..len].to_string();
        anchor.to_end = anchor.offset + len == source.len();
        if repeated {
            // The rest of the note occurs more than once (the later of two
            // identical blocks): keep the shortest end of the text before it
            // that makes the pair unique. The whole head always does: the
            // pair is then the whole note.
            let head = &source[..anchor.offset];
            let unique =
                |from: usize| occurs_once(source, &format!("{}{}", &head[from..], anchor.after));
            let (mut low, mut high) = (0, head.len());
            while high - low > 1 {
                let mut mid = floor(head, low + (high - low) / 2);
                if mid <= low {
                    mid = ceil(head, low + 1);
                }
                if mid >= high {
                    break;
                }
                if unique(mid) {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            if high > low && unique(high) {
                low = high;
            }
            anchor.before = head[low..].to_string();
            anchor.before_required = true;
        }
        anchor
    }

    /// Whether text was typed right at the anchor since (#1611): what follows
    /// `at` (from [`Self::locate`]) in `current` is no longer what followed
    /// the anchor, and it starts with a visible character. A result inserted
    /// there then goes on its own line instead of being glued to it.
    pub fn typed_at(&self, current: &str, at: usize) -> bool {
        let Some(rest) = current.get(at..) else {
            return false;
        };
        let intact =
            rest.starts_with(&self.after) && (!self.to_end || rest.len() == self.after.len());
        !intact && rest.chars().next().is_some_and(|c| !c.is_whitespace())
    }

    /// The anchor's position in the current text, if it can still be told.
    pub fn locate(&self, current: &str) -> Option<usize> {
        let offset = self.offset;
        let stays = offset <= current.len()
            && current.is_char_boundary(offset)
            && offset >= self.before.len()
            && current[..offset].ends_with(&self.before)
            && current[offset..].starts_with(&self.after);
        if stays {
            return Some(offset);
        }
        let joined = format!("{}{}", self.before, self.after);
        if !joined.is_empty() {
            if let Some(at) = unique(current, &joined) {
                return Some(at + self.before.len());
            }
        }
        // One side was edited: the other side alone, if it is distinctive.
        if self.before.len() >= 8 {
            if let Some(at) = unique(current, &self.before) {
                return Some(at + self.before.len());
            }
        }
        if self.after.len() >= 8 {
            if let Some(at) = unique(current, &self.after) {
                return Some(at);
            }
        }
        None
    }

    /// For a drop before a block (#1615): the anchor's position only while
    /// the block that followed it is still there, that is, while all of the
    /// text captured after the anchor still follows it. When the block was
    /// deleted, [`Self::locate`] may still find the text before it and land
    /// before another block, even one with the same first line (two
    /// `## Notes`).
    pub fn locate_block(&self, current: &str) -> Option<usize> {
        // Where the block's own text (and, for a repeated block, the text
        // before it) still stands at the start of a top-level block (never
        // in the frontmatter, a code block or a paragraph). Searched
        // directly, so the block is found even when the text around the old
        // position was edited.
        let starts = okilum_core::deep_link::block_starts(current);
        let fits = |at: usize| {
            (at == current.len() || starts.binary_search(&at).is_ok())
                && current.is_char_boundary(at)
                && current[at..].starts_with(&self.after)
                && (!self.to_end || current.len() - at == self.after.len())
                && (!self.before_required || current[..at].ends_with(&self.before))
        };
        let candidates: Vec<usize> = if self.after.is_empty() {
            fits(current.len())
                .then_some(current.len())
                .into_iter()
                .collect()
        } else {
            current
                .match_indices(self.after.as_str())
                .map(|(at, _)| at)
                .filter(|at| fits(*at))
                .collect()
        };
        match candidates.as_slice() {
            [] => None,
            [only] => Some(*only),
            // The block's text now occurs more than once (typed again
            // elsewhere): only the paste locator's own answer, if it is one
            // of them.
            _ => self.locate(current).filter(|at| candidates.contains(at)),
        }
    }

    /// The position of `expected` text that was inserted at the anchor, if it
    /// is still there untouched.
    pub fn locate_text(&self, current: &str, expected: &str) -> Option<usize> {
        let at = self.locate(current)?;
        current[at..].starts_with(expected).then_some(at)
    }
}

/// The text a paste inserted, found again later (#1611): where a background
/// result (a downloaded image) may replace its placeholder. The start is an
/// [`Anchor`]; the end is the inserted length plus whatever the note grew by
/// since, so typing inside the pasted text cannot push a placeholder out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    start: Anchor,
    len: usize,
    doc_len: usize,
}

impl Region {
    /// The `len` bytes at `start` in `source`, the note right after the paste.
    pub fn at(source: &str, start: usize, len: usize) -> Self {
        let start = floor(source, start.min(source.len()));
        Self {
            start: Anchor::at(source, start),
            len: len.min(source.len() - start),
            doc_len: source.len(),
        }
    }

    /// The bytes the region may cover in `current`.
    pub fn locate(&self, current: &str) -> Option<Range<usize>> {
        let start = self.start.locate(current)?;
        let grown = current.len().saturating_sub(self.doc_len);
        let end = floor(current, (start + self.len + grown).min(current.len()));
        Some(start..end.max(start))
    }

    /// Every occurrence of `text` inside the region, front to back.
    pub fn occurrences(&self, current: &str, text: &str) -> Vec<Range<usize>> {
        let Some(span) = self.locate(current) else {
            return Vec::new();
        };
        if text.is_empty() {
            return Vec::new();
        }
        current[span.clone()]
            .match_indices(text)
            .map(|(at, _)| span.start + at..span.start + at + text.len())
            .collect()
    }

    /// The region again after `hits` (from [`Self::occurrences`] on
    /// `before`) were each replaced by `replacement`; `after` is the note now.
    pub fn after_replacing(
        &self,
        before: &str,
        hits: &[Range<usize>],
        replacement: &str,
        after: &str,
    ) -> Self {
        let Some(span) = self.locate(before) else {
            return self.clone();
        };
        let removed: usize = hits.iter().map(|hit| hit.len()).sum();
        let len = (span.len() + hits.len() * replacement.len()).saturating_sub(removed);
        Self::at(after, span.start, len)
    }
}

fn unique(haystack: &str, needle: &str) -> Option<usize> {
    let mut found = haystack.match_indices(needle).map(|(at, _)| at);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Whether `needle` occurs exactly once in `haystack`, overlaps counted.
fn occurs_once(haystack: &str, needle: &str) -> bool {
    let Some(first) = haystack.find(needle) else {
        return false;
    };
    let step = needle.chars().next().map_or(1, char::len_utf8);
    haystack
        .get(first + step..)
        .is_none_or(|later| !later.contains(needle))
}

fn ceil(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

fn floor(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_anchor_needs_its_block() {
        let source = "# Title\n\nFirst paragraph.\n\n## Next\n\nBody.\n";
        let at = source.find("## Next").unwrap();
        let anchor = Anchor::at(source, at);
        // Text typed elsewhere: the block is still there.
        let edited = source.replace("First paragraph.", "First paragraph, edited.");
        assert_eq!(anchor.locate_block(&edited), edited.find("## Next"));
        // The block deleted: the text before it is still found, the block is not.
        let deleted = source.replace("## Next\n\n", "");
        assert!(
            anchor.locate(&deleted).is_some(),
            "positive control: locate alone"
        );
        assert_eq!(anchor.locate_block(&deleted), None);
        // Two sections sharing more than the 48 bytes of context: the
        // block anchor keeps text until it is unique.
        let shared = "## Notes\n\nThe same long opening that both of these sections share, ";
        assert!(shared.len() > 48);
        let long = format!("# Title\n\nIntro.\n\n{shared}first.\n\n{shared}second.\n\nEnd.\n");
        let at = long.find("## Notes").unwrap();
        let block = Anchor::at_block(&long, at);
        let gone = long.replacen(&format!("{shared}first.\n\n"), "", 1);
        assert!(
            block.locate(&gone).is_some(),
            "positive control: locate alone"
        );
        assert_eq!(
            block.locate_block(&gone),
            None,
            "not before the other section"
        );
        assert!(
            Anchor::at(&long, at).locate_block(&gone).is_some(),
            "positive control: 48 bytes of context alone accept the other section"
        );
        // An edit past the unique start: still the same block.
        let edited = long.replacen("first.", "fIRST, edited.", 1);
        assert_eq!(block.locate_block(&edited), Some(at));
        // A short unique start (`##`): the block is found again after the
        // paragraph before it was edited, where the paste locator's
        // context is gone and the start alone is too short for it.
        let note = "# Title\n\nFirst paragraph.\n\n## N\n\nBody.\n";
        let next = Anchor::at_block(note, note.find("## N").unwrap());
        assert_eq!(
            next.after, "## N\n",
            "the block's whole first line, though `##` alone is unique"
        );
        let edited = note.replace("First paragraph.", "First paragraph, edited.");
        assert_eq!(
            next.locate(&edited),
            None,
            "positive control: locate alone fails"
        );
        assert_eq!(next.locate_block(&edited), edited.find("## N"));
        // The block's start typed again elsewhere: ambiguous, refused.
        let doubled = edited.replace("Body.", "## N\n\nBody.");
        assert_eq!(next.locate_block(&doubled), None);
        // The later of two identical blocks: the rest is not unique, the
        // text before it decides.
        let same = "## Notes\n\nSame.\n\n## Notes\n\nSame.\n";
        let second = same.rfind("## Notes").unwrap();
        let later = Anchor::at_block(same, second);
        // The later copy deleted with the blank line before it.
        let gone = &same[..second - 1];
        assert_eq!(gone, "## Notes\n\nSame.\n");
        assert_eq!(
            later.locate(gone),
            Some(0),
            "positive control: locate alone"
        );
        assert_eq!(
            later.locate_block(gone),
            None,
            "not before the surviving copy"
        );
        // The earlier copy edited: still the later block.
        let edited = same.replacen("Same.", "Same, edited.", 1);
        assert_eq!(later.locate_block(&edited), edited.rfind("## Notes"));
        // The same with a repeated preamble longer than the 48 bytes of context.
        let preamble = "A long shared preamble paragraph that repeats before each section.\n\n";
        assert!(preamble.len() > 48);
        let twice = format!("{preamble}## Notes\n\nSame.\n\n{preamble}## Notes\n\nSame.\n");
        let at = twice.rfind("## Notes").unwrap();
        let block = Anchor::at_block(&twice, at);
        let deleted =
            twice[..twice.len() - preamble.len() - "## Notes\n\nSame.\n".len()].to_string();
        assert_eq!(deleted, format!("{preamble}## Notes\n\nSame.\n\n"));
        assert!(
            block.locate(&deleted).is_some(),
            "positive control: locate alone"
        );
        assert_eq!(
            block.locate_block(&deleted),
            None,
            "not before the earlier copy"
        );
        // Unique from the first byte, or not at all.
        assert_eq!(Anchor::at_block("a\nb\n", 2).after, "b\n");
        assert_eq!(Anchor::at_block("x\nx\n", 2).after, "x\n");
        assert_eq!(Anchor::at_block("x\n", 2).after, "");
        // Two blocks with the same first line: deleting the target must not
        // place the drop before the other one.
        let twice = "# Title\n\nIntro.\n\n## Notes\n\nFirst notes.\n\n## Notes\n\nSecond notes.\n";
        let target = Anchor::at(twice, twice.find("## Notes").unwrap());
        let deleted = twice.replacen("## Notes\n\nFirst notes.\n\n", "", 1);
        assert!(
            target.locate(&deleted).is_some(),
            "positive control: locate alone"
        );
        assert_eq!(target.locate_block(&deleted), None);
        // Review of #3074 (83378): the Reader showed a title, the opened
        // draft has none, and its `#` survives only in a YAML comment. The
        // drop is refused, never placed inside the frontmatter.
        let shown = "---\ntags: [a]\n---\n# Title\n\nBody.\n";
        let opened = "---\ntags: [a] # label\n---\nBody.\n";
        let title = Anchor::at_block(shown, shown.find("# Title").unwrap());
        assert_eq!(title.after, "# Title\n");
        assert_eq!(title.locate_block(opened), None);
        // The whole first line, but only at the start of a top-level block:
        // not a comment in the frontmatter, not a line in a code block.
        let in_yaml = "---\nnote: x\n# Title\n---\nBody.\n";
        assert_eq!(title.locate_block(in_yaml), None);
        let in_code = "```\n# Title\n```\n\nBody.\n";
        assert_eq!(title.locate_block(in_code), None);
        // Positive controls: an unchanged title is still found after edits
        // to the frontmatter or the text around it.
        let retagged = "---\ntags: [b, c]\n---\n# Title\n\nBody, edited.\n";
        assert_eq!(title.locate_block(retagged), retagged.find("# Title"));
        assert_eq!(title.locate_block("# Title\n\nBody.\n"), Some(0));
        // The end of the note: no block to keep.
        let end = Anchor::at(source, source.len());
        assert_eq!(
            end.locate_block(&format!("Typed.\n{source}")),
            Some(source.len() + 7)
        );
    }

    #[test]
    fn unchanged_text_keeps_the_offset() {
        let source = "first line\nsecond line\n";
        let anchor = Anchor::at(source, 11);
        assert_eq!(anchor.locate(source), Some(11));
        assert_eq!(Anchor::at("", 0).locate(""), Some(0));
        assert_eq!(Anchor::at("", 0).locate("typed"), Some(0));
    }

    #[test]
    fn edits_elsewhere_move_the_anchor_with_its_text() {
        let source = "alpha beta gamma\n";
        let anchor = Anchor::at(source, 6);
        // Typed before the anchor.
        assert_eq!(anchor.locate("NEW alpha beta gamma\n"), Some(10));
        // Typed after it.
        assert_eq!(anchor.locate("alpha beta gamma\nmore"), Some(6));
    }

    #[test]
    fn a_gone_anchor_is_not_guessed() {
        let source = "abc";
        let anchor = Anchor::at(source, 1);
        assert_eq!(anchor.locate("xyz"), None);
        // Ambiguous: the same short text twice.
        assert_eq!(anchor.locate("zz abc abc"), None);
    }

    #[test]
    fn inserted_text_must_be_intact() {
        let source = "see https://example.com/a today";
        let anchor = Anchor::at(source, 4);
        assert_eq!(anchor.locate_text(source, "https://example.com/a"), Some(4));
        let edited = "see https://example.com/b today";
        assert_eq!(anchor.locate_text(edited, "https://example.com/a"), None);
        let moved = format!("intro\n{source}");
        assert_eq!(
            anchor.locate_text(&moved, "https://example.com/a"),
            Some(10)
        );
    }

    #[test]
    fn a_region_finds_its_placeholders_after_edits() {
        let image = "![a](https://x.org/a.png)";
        let pasted = format!("text {image} and {image}");
        let source = format!("{image} before\n{pasted}\nafter {image}");
        let start = source.find("text").unwrap();
        let region = Region::at(&source, start, pasted.len());
        // Only the pasted copies, not the ones around it.
        let hits = region.occurrences(&source, image);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.start >= start));
        // Typing inside the pasted text keeps both in the region.
        let edited = source.replacen("text ", "text typed here ", 1);
        assert_eq!(region.occurrences(&edited, image).len(), 2);
        // Replacing them: the region follows the new length.
        let mut replaced = edited.clone();
        let hits = region.occurrences(&edited, image);
        for hit in hits.iter().rev() {
            replaced.replace_range(hit.clone(), "![[a.png]]");
        }
        let next = region.after_replacing(&edited, &hits, "![[a.png]]", &replaced);
        assert!(next.occurrences(&replaced, image).is_empty());
        assert_eq!(next.occurrences(&replaced, "![[a.png]]").len(), 2);
    }

    #[test]
    fn multibyte_context_is_cut_on_char_boundaries() {
        let source = "שלום עולם ".repeat(10);
        let anchor = Anchor::at(&source, source.len() / 2);
        assert!(anchor.locate(&source).is_some());
    }

    #[test]
    fn text_typed_at_the_anchor_is_noticed() {
        let anchor = Anchor::at("", 0);
        assert!(anchor.typed_at("# Trip", 0));
        assert!(!anchor.typed_at("", 0));
        let anchor = Anchor::at("intro text before \nafter text here", 18);
        let typed = "intro text before X\nafter text here";
        let at = anchor.locate(typed).unwrap();
        assert_eq!(at, 18);
        assert!(anchor.typed_at(typed, at));
        let untouched = "intro text before \nafter text here";
        assert!(!anchor.typed_at(untouched, anchor.locate(untouched).unwrap()));
    }
}
