//! Exact note context for Live Preview blocks. Publishing a new input snapshot
//! must not scan the entire note for link definitions (#1621). Only a rendered
//! block that could contain a reference link needs them (#1067).
use gpui::SharedString;
use okilum_core::source_projection::revision::Text;
use std::{cell::OnceCell, rc::Rc};

#[derive(Clone, Default)]
pub(super) struct BlockNote {
    text: Option<Text>,
    // All blocks/clones of this immutable revision share one extraction. A new
    // snapshot gets its own cell, including edits that only change definitions.
    definitions: Rc<OnceCell<SharedString>>,
    footnotes: Option<Rc<std::collections::HashMap<(usize, usize), String>>>,
}

impl BlockNote {
    pub(super) fn new(text: SharedString) -> Self {
        Self {
            text: Some(text.to_string().into()),
            definitions: Default::default(),
            footnotes: None,
        }
    }

    pub(super) fn from_snapshot(_text: Text) -> Self {
        // Pending context is explicitly unresolved. Never resolve a new revision
        // using an old definition, or flatten it while typing/painting.
        Self::default()
    }
    pub(super) fn prepare(
        text: Text,
        blocks: Vec<std::ops::Range<usize>>,
    ) -> (
        Text,
        String,
        std::collections::HashMap<(usize, usize), String>,
    ) {
        let definitions = okilum_core::render::reference_definitions(&text);
        let footnotes = blocks
            .into_iter()
            .filter_map(|range| {
                okilum_core::obsidian::block_footnotes(&text, range.clone())
                    .map(|source| ((range.start, range.end), source))
            })
            .collect();
        (text, definitions, footnotes)
    }
    pub(super) fn from_prepared(
        (text, definitions, footnotes): (
            Text,
            String,
            std::collections::HashMap<(usize, usize), String>,
        ),
    ) -> Self {
        Self {
            text: Some(text),
            definitions: Rc::new(OnceCell::from(definitions.into())),
            footnotes: Some(Rc::new(footnotes)),
        }
    }
    pub(super) fn block_footnotes(
        &self,
        source: &str,
        range: std::ops::Range<usize>,
    ) -> Option<String> {
        if !source.contains("[^") && !source.contains("^[") {
            return None;
        }
        if let Some(footnotes) = &self.footnotes {
            return footnotes.get(&(range.start, range.end)).cloned();
        }
        okilum_core::obsidian::block_footnotes(self.text.as_ref()?, range)
    }

    pub(super) fn with_definitions(&self, block: String) -> SharedString {
        // Full, collapsed and shortcut reference links/images all need '['.
        // Check only the rendered block, not the whole note. This deliberately
        // permits false positives (code/escaped brackets), never false negatives.
        if self.text.is_none() || !block.contains('[') {
            return block.into();
        }
        let definitions = self.definitions.get_or_init(|| {
            okilum_core::render::reference_definitions(self.text.as_ref().unwrap()).into()
        });
        if definitions.is_empty() {
            block.into()
        } else {
            format!("{block}\n\n{definitions}").into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(note: &BlockNote, block: &str) -> String {
        okilum_core::render::block_html(&note.with_definitions(block.into()))
    }

    #[test]
    fn typing_snapshots_and_reference_free_blocks_do_not_scan_the_note() {
        let long = "Ordinary prose.\n".repeat(20_000);
        for suffix in ["a", "ab", "abc"] {
            let note =
                BlockNote::new(format!("{long}{suffix}\n\n[id]: https://example.com\n").into());
            let block = "| Name | Value |\n| --- | --- |\n| Plain | 123 |";
            assert!(html(&note, block).contains("<table>"));
            assert!(note.definitions.get().is_none());
            assert_eq!(
                note.with_definitions("```rs\nlet x = 1;\n```".into()),
                "```rs\nlet x = 1;\n```"
            );
            assert!(note.definitions.get().is_none());
            // Positive control: this same snapshot must resolve a real link.
            assert!(html(&note, "[label][id]").contains("href=\"https://example.com\""));
            assert!(note.definitions.get().is_some());
        }
    }

    #[test]
    fn reference_links_and_images_resolve_and_share_only_their_exact_revision() {
        let old = BlockNote::new(
            "[id]: https://example.com/old\n\n[^a]: footnote\n\n```\n[fake]: wrong\n```\n".into(),
        );
        for block in ["[label][id]", "[id][]", "[id]", "![alt][id]"] {
            assert!(
                html(&old, block).contains("https://example.com/old"),
                "{block}"
            );
        }
        let clone = old.clone();
        assert!(Rc::ptr_eq(&old.definitions, &clone.definitions));
        assert!(!old.definitions.get().unwrap().contains("footnote"));
        assert!(!old.definitions.get().unwrap().contains("fake"));

        let edited = BlockNote::new("[id]: https://example.com/new\n".into());
        assert!(edited.definitions.get().is_none());
        assert!(
            html(&edited, "| Link |\n| --- |\n| [label][id] |").contains("https://example.com/new")
        );
        assert!(!html(&edited, "[id]").contains("https://example.com/old"));
        assert!(html(&clone, "[id]").contains("https://example.com/old"));
        // Removing a definition must not reuse an earlier revision's output.
        let removed = BlockNote::new("No definitions now.\n".into());
        assert!(!html(&removed, "[id]").contains("href="));
        assert!(removed.definitions.get().unwrap().is_empty());
    }
}
