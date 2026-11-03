//! Exact note context for Live Preview blocks. Publishing a new input snapshot
//! must not scan the entire note for link definitions (#1621). Only a rendered
//! block that could contain a reference link needs them (#1067).
use gpui::SharedString;
use okilum_core::source_projection::revision::Text;
use std::{
    cell::OnceCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

type Footnotes = std::collections::HashMap<(usize, usize), String>;
pub(super) type WorkerCache = Arc<Mutex<Option<(u64, Arc<Footnotes>)>>>;

#[derive(Clone, Default)]
pub(super) struct BlockNote {
    text: Option<Text>,
    // All blocks/clones of this immutable revision share one extraction. A new
    // snapshot gets its own cell, including edits that only change definitions.
    definitions: Rc<OnceCell<SharedString>>,
    footnotes: Option<Arc<Footnotes>>,
}

impl BlockNote {
    #[cfg(test)]
    pub(super) fn new(text: SharedString) -> Self {
        Self::initial(text.to_string().into())
    }
    pub(super) fn initial(text: Text) -> Self {
        Self {
            text: Some(text),
            definitions: Default::default(),
            footnotes: None,
        }
    }

    pub(super) fn pending() -> Self {
        // Pending context is explicitly unresolved. Never resolve a new revision
        // using an old definition, or flatten it while typing/painting.
        Self::default()
    }
    pub(super) fn prepare(
        text: Text,
        blocks: Vec<std::ops::Range<usize>>,
    ) -> (Text, SharedString, Arc<Footnotes>) {
        let definitions = okilum_core::render::reference_definitions(&text).into();
        let footnotes = blocks
            .into_iter()
            .filter_map(|range| {
                okilum_core::obsidian::block_footnotes(&text, range.clone())
                    .map(|source| ((range.start, range.end), source))
            })
            .collect();
        (text, definitions, Arc::new(footnotes))
    }
    pub(super) fn from_prepared(
        (text, definitions, footnotes): (Text, SharedString, Arc<Footnotes>),
    ) -> Self {
        Self {
            text: Some(text),
            definitions: Rc::new(OnceCell::from(definitions)),
            footnotes: Some(footnotes),
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
    fn prepared_revision_resolves_references_and_footnotes_without_foreground_source_work() {
        use okilum_core::source_projection::revision::{reset_work, take_work};
        let source = "Text[^first].\r\n\r\n| Link |\r\n| --- |\r\n| [label][id] and note[^second] |\r\n\r\n[id]: https://example.com/current\r\n[^first]: First footnote.\r\n[^second]: Second footnote.\r\n";
        let start = source.find("| Link").unwrap();
        let end = source.find("\r\n\r\n[id]").unwrap();
        let range = start..end;
        let revision = Text::from_rope(gpui_component::Rope::from_str(source));
        let prepared =
            BlockNote::from_prepared(BlockNote::prepare(revision.clone(), vec![range.clone()]));
        reset_work();
        let block = prepared
            .block_footnotes(&source[range.clone()], range.clone())
            .unwrap();
        let resolved = prepared.with_definitions(block);
        let work = take_work();
        assert_eq!(work.flattened_bytes, 0);
        assert_eq!(work.scanned_bytes, 0);
        assert!(resolved.contains("https://example.com/current"));
        assert!(resolved.contains("²"));
        assert!(!resolved.contains("[^second]"));
        // A newer unresolved revision must never borrow the previous target.
        let pending = BlockNote::pending();
        assert_eq!(
            pending.with_definitions("[label][id]".into()),
            "[label][id]"
        );
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
