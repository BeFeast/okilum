use okilum_core::document_links::{HeadingEntry, HeadingInventory};

/// The outline and its click coordinates share one exact editor snapshot.
#[derive(Default)]
pub(crate) struct DraftOutline {
    pub(super) source: String,
    pub(super) entries: Vec<HeadingEntry>,
    pub(super) lines: Vec<usize>,
    pub(super) pending: bool,
    pub(super) worker: Option<gpui::Task<()>>,
}

impl DraftOutline {
    pub(super) fn new(source: &str) -> Self {
        let body = okilum_core::render::without_frontmatter(source);
        let prefix = source.len() - body.len();
        let entries: Vec<_> = HeadingInventory::new(body)
            .entries
            .into_iter()
            .filter(|heading| heading.supported_container)
            .map(|mut heading| {
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
