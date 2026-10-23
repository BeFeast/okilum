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
        }
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
