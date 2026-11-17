//! A drop onto the rendered note (#1615): what was dropped goes in at the
//! start of the block under the pointer, as its own paragraph before it.
//! Platform-neutral and pure.
//!
//! The drop is routed like a paste onto an empty line between the previous
//! block and the dropped-on one ([`BlockSlot::routing_source`]). The editor
//! then receives one edit, `before + inserted + after` at
//! [`BlockSlot::at`], which leaves exactly the routing source with the
//! inserted text at [`BlockSlot::route_offset`]; background results (copied
//! files, a saved image) find that empty line again by their anchor.

/// Where a drop onto a rendered block goes in the note's source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSlot {
    /// Where the edit goes: the start of the block's first line, or the end
    /// of the note.
    pub at: usize,
    /// Newlines that end the previous block's text before the dropped text,
    /// so it is not a continuation of that paragraph.
    pub before: &'static str,
    /// Newlines that separate the dropped text from the block after it.
    pub after: &'static str,
}

impl BlockSlot {
    /// The slot at the start of the line holding `offset`, or at the end of
    /// the note (`offset >= source.len()`).
    pub fn new(source: &str, offset: usize) -> Self {
        let offset = floor_boundary(source, offset.min(source.len()));
        let at = if offset == source.len() {
            offset
        } else {
            source[..offset].rfind('\n').map_or(0, |i| i + 1)
        };
        let head = &source[..at];
        let before = if head.is_empty() || head.ends_with("\n\n") || head.ends_with("\n\r\n") {
            ""
        } else if head.ends_with('\n') {
            // A line right above: a blank line first, or the drop joins it.
            let previous = head[..head.len() - 1]
                .rsplit('\n')
                .next()
                .unwrap_or_default();
            if previous.trim().is_empty() {
                ""
            } else {
                "\n"
            }
        } else {
            // The end of a note without a final newline.
            "\n\n"
        };
        let after = if source[at..].trim().is_empty() {
            "\n"
        } else {
            "\n\n"
        };
        Self { at, before, after }
    }

    /// The source a drop is routed against: the slot's empty line in place.
    pub fn routing_source(&self, source: &str) -> String {
        let mut out = String::with_capacity(source.len() + 4);
        out.push_str(&source[..self.at]);
        out.push_str(self.before);
        out.push_str(self.after);
        out.push_str(&source[self.at..]);
        out
    }

    /// Where the routed text goes in [`Self::routing_source`].
    pub fn route_offset(&self) -> usize {
        self.at + self.before.len()
    }

    /// The editor's single edit at [`Self::at`] for `inserted` (empty when
    /// only a background result will fill the slot).
    pub fn edit(&self, inserted: &str) -> String {
        format!("{}{inserted}{}", self.before, self.after)
    }
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The note after a drop of `inserted` into the slot at `offset`.
    fn dropped(source: &str, offset: usize, inserted: &str) -> String {
        let slot = BlockSlot::new(source, offset);
        let mut after = source.to_string();
        after.insert_str(slot.at, &slot.edit(inserted));
        // The one edit leaves the routing source with the text in its slot,
        // so anchors taken on the routing source find the slot again.
        let mut routed = slot.routing_source(source);
        routed.insert_str(slot.route_offset(), inserted);
        assert_eq!(after, routed);
        after
    }

    #[test]
    fn a_drop_on_a_block_goes_before_it_as_its_own_paragraph() {
        let source = "# Title\n\nFirst paragraph.\n\n## Next\n\nBody.\n";
        let at = source.find("## Next").unwrap();
        assert_eq!(
            dropped(source, at, "![[clip.png]]"),
            "# Title\n\nFirst paragraph.\n\n![[clip.png]]\n\n## Next\n\nBody.\n"
        );
        // Anywhere on the block's first line: the start of that line.
        assert_eq!(BlockSlot::new(source, at + 4).at, at);
    }

    #[test]
    fn the_first_block_and_frontmatter() {
        assert_eq!(dropped("Intro.\n", 0, "x"), "x\n\nIntro.\n");
        let source = "---\ntags: [a]\n---\n# Title\n";
        let at = source.find("# Title").unwrap();
        // Right under the closing fence: a blank line keeps the
        // frontmatter's last line apart.
        assert_eq!(
            dropped(source, at, "x"),
            "---\ntags: [a]\n---\n\nx\n\n# Title\n"
        );
    }

    #[test]
    fn a_block_right_under_a_paragraph_line() {
        // A heading may follow a paragraph line directly; the drop must not
        // become that paragraph's last line.
        let source = "Paragraph line\n# Heading\n";
        let at = source.find("# Heading").unwrap();
        assert_eq!(
            dropped(source, at, "x"),
            "Paragraph line\n\nx\n\n# Heading\n"
        );
    }

    #[test]
    fn the_end_of_the_note() {
        assert_eq!(dropped("Text.\n", 6, "x"), "Text.\n\nx\n");
        assert_eq!(dropped("Text.", 5, "x"), "Text.\n\nx\n");
        assert_eq!(dropped("Text.\n\n", 7, "x"), "Text.\n\nx\n");
        assert_eq!(dropped("", 0, "x"), "x\n");
    }

    #[test]
    fn crlf_notes_and_an_empty_slot() {
        let source = "One.\r\n\r\nTwo.\r\n";
        let at = source.find("Two").unwrap();
        assert_eq!(dropped(source, at, "x"), "One.\r\n\r\nx\n\nTwo.\r\n");
        // A background result fills the slot later: the slot alone first.
        let slot = BlockSlot::new("A.\n\nB.\n", 4);
        assert_eq!(slot.edit(""), "\n\n");
        assert_eq!(slot.routing_source("A.\n\nB.\n"), "A.\n\n\n\nB.\n");
        assert_eq!(slot.route_offset(), 4);
    }

    #[test]
    fn offsets_are_clamped_to_characters() {
        let source = "é\n\nB\n";
        let slot = BlockSlot::new(source, 1);
        assert_eq!(slot.at, 0);
        let slot = BlockSlot::new(source, 99);
        assert_eq!(slot.at, source.len());
    }
}
