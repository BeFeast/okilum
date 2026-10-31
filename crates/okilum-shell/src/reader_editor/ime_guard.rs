//! IME guard (#1637). While an input method composes, and for a short settle
//! after it commits, Enter, Space and Tab run no editor automation: no list
//! continuation, no completion accept, no wrapping. Preedit text runs no
//! transform rule. Text an input method commits is ordinary typing, so a `/`
//! or `[[` it commits still opens its menu.
//!
//! The editor input records the commit on the executor clock and keeps its
//! own Enter (indent carry, completion menu) and Tab (inline completion)
//! quiet for the settle (patch 0086). Every Okilum autoformat asks
//! [`Reader::ime_allows`] before it changes the text.
use super::*;

/// How long after an IME commit Enter, Space and Tab still belong to the
/// composition. Some input methods deliver the key that confirmed a
/// candidate after the commit; without the settle it also continued the
/// list, a doubled bullet (Tolaria #850, #1076).
pub(super) const IME_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// What would start an automatic edit or open a menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code, reason = "Space is constructed by #1638"))]
pub(crate) enum Trigger {
    /// List continuation, completion accept.
    Enter,
    /// Wrapping and inline autoformat.
    Space,
    /// List indent, completion accept.
    Tab,
    /// Text that arrived, typed or committed: transform rules and menu
    /// triggers such as `/` and `[[`. Held only while it is preedit.
    Text,
}

/// The policy, apart from the editor: `settling` covers the composition and
/// the settle after its commit.
pub(crate) fn allows(trigger: Trigger, composing: bool, settling: bool) -> bool {
    match trigger {
        Trigger::Text => !composing,
        Trigger::Enter | Trigger::Space | Trigger::Tab => !composing && !settling,
    }
}

impl Reader {
    /// Whether `trigger` may run editor automation in the note editor now.
    pub(crate) fn ime_allows(&self, trigger: Trigger, cx: &App) -> bool {
        let Some(editing) = &self.editing else {
            return false;
        };
        let input = editing.input.read(cx);
        allows(trigger, input.ime_composing(), input.ime_settling(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::time::Duration;

    #[test]
    fn keys_wait_for_the_settle_and_text_only_for_the_commit() {
        for trigger in [Trigger::Enter, Trigger::Space, Trigger::Tab] {
            assert!(allows(trigger, false, false), "{trigger:?}: plain typing");
            assert!(!allows(trigger, true, true), "{trigger:?}: composing");
            assert!(!allows(trigger, false, true), "{trigger:?}: settling");
        }
        assert!(allows(Trigger::Text, false, false));
        assert!(!allows(Trigger::Text, true, true), "preedit runs no rule");
        assert!(allows(Trigger::Text, false, true), "a commit is typing");
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
    ) -> (
        Entity<Reader>,
        Entity<EditorState>,
        &'a mut VisualTestContext,
        tempfile::TempDir,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), text).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        (reader, input, visual, directory)
    }

    fn caret(input: &Entity<EditorState>, visual: &mut VisualTestContext, at: usize) {
        visual.update(|_, cx| input.update(cx, |i, cx| i.set_selected_range(at..at, cx)));
        visual.run_until_parked();
    }

    fn compose(input: &Entity<EditorState>, visual: &mut VisualTestContext, text: &str) {
        visual.update(|window, cx| {
            input.update(cx, |i, cx| {
                i.replace_and_mark_text_in_range(None, text, None, window, cx)
            })
        });
        visual.run_until_parked();
    }

    fn commit(input: &Entity<EditorState>, visual: &mut VisualTestContext, text: &str) {
        visual.update(|window, cx| {
            input.update(cx, |i, cx| {
                let marked = i.marked_text_range(window, cx);
                assert!(marked.is_some(), "a composition to commit");
                i.replace_text_in_range(marked, text, window, cx)
            })
        });
        visual.run_until_parked();
    }

    fn allowed(reader: &Entity<Reader>, visual: &mut VisualTestContext, trigger: Trigger) -> bool {
        reader.read_with(visual, |r, cx| r.ime_allows(trigger, cx))
    }

    fn value(input: &Entity<EditorState>, visual: &mut VisualTestContext) -> String {
        input.read_with(visual, |i, _| i.value().to_string())
    }

    fn wait(visual: &mut VisualTestContext, millis: u64) {
        visual
            .executor()
            .advance_clock(Duration::from_millis(millis));
        visual.run_until_parked();
    }

    /// The Enter that confirmed a candidate inserts a plain newline: it does
    /// not continue the list (#1635). The same Enter after the settle does
    /// (positive control), so the probe can see a continuation.
    #[gpui::test]
    fn enter_that_commits_a_composition_continues_no_list(cx: &mut TestAppContext) {
        let (reader, input, visual, _directory) = open(cx, "  - a\n");
        let line_end = "  - a".len();

        // Positive control: no IME, Enter continues the item.
        assert!(allowed(&reader, visual, Trigger::Enter));
        caret(&input, visual, line_end);
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(value(&input, visual), "  - a\n  - \n", "Enter continues");
        input.update_in(visual, |i, window, cx| i.set_value("  - a\n", window, cx));
        visual.run_until_parked();

        // A composition at the end of the item: nothing may run.
        caret(&input, visual, line_end);
        compose(&input, visual, "か");
        for trigger in [Trigger::Enter, Trigger::Space, Trigger::Tab, Trigger::Text] {
            assert!(!allowed(&reader, visual, trigger), "{trigger:?} on preedit");
        }
        // The commit, and at once the Enter that confirmed it.
        commit(&input, visual, "漢");
        assert!(!allowed(&reader, visual, Trigger::Enter), "settling");
        assert!(allowed(&reader, visual, Trigger::Text), "commit is typing");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            "  - a漢\n\n",
            "a plain newline, no continuation"
        );

        // After the settle Enter continues the item again.
        wait(visual, IME_SETTLE.as_millis() as u64 + 100);
        assert!(allowed(&reader, visual, Trigger::Enter), "settle is over");
        caret(&input, visual, "  - a漢".len());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(value(&input, visual), "  - a漢\n  - \n\n");
    }

    /// Only the settle window holds keys: just inside it Enter still waits,
    /// just past it Enter is free. An unmark ends a composition like a commit.
    #[gpui::test]
    fn the_settle_lasts_500_ms_from_a_commit_or_an_unmark(cx: &mut TestAppContext) {
        let (reader, input, visual, _directory) = open(cx, "text\n");
        caret(&input, visual, 0);
        compose(&input, visual, "に");
        commit(&input, visual, "日");
        wait(visual, IME_SETTLE.as_millis() as u64 - 50);
        assert!(
            !allowed(&reader, visual, Trigger::Space),
            "inside the settle"
        );
        wait(visual, 100);
        assert!(allowed(&reader, visual, Trigger::Space), "past the settle");

        compose(&input, visual, "ほ");
        visual.update(|window, cx| input.update(cx, |i, cx| i.unmark_text(window, cx)));
        visual.run_until_parked();
        assert!(!allowed(&reader, visual, Trigger::Tab), "an unmark settles");
        wait(visual, IME_SETTLE.as_millis() as u64 + 50);
        assert!(allowed(&reader, visual, Trigger::Tab));

        // A cancelled composition commits nothing and starts no settle.
        compose(&input, visual, "や");
        compose(&input, visual, "");
        assert_eq!(value(&input, visual), "日ほtext\n");
        assert!(
            allowed(&reader, visual, Trigger::Enter),
            "cancel is no commit"
        );
    }

    /// A `/` or `[[` that an input method commits is typing: it may open its
    /// menu at once. As preedit it may not.
    #[gpui::test]
    fn menu_triggers_committed_by_an_ime_still_count(cx: &mut TestAppContext) {
        let (reader, input, visual, _directory) = open(cx, "\n");
        caret(&input, visual, 0);
        for trigger in ["/", "[["] {
            compose(&input, visual, trigger);
            assert!(
                !allowed(&reader, visual, Trigger::Text),
                "{trigger} as preedit"
            );
            commit(&input, visual, trigger);
            assert!(
                allowed(&reader, visual, Trigger::Text),
                "{trigger} committed"
            );
            assert!(
                !allowed(&reader, visual, Trigger::Enter),
                "{trigger}: settling"
            );
            wait(visual, IME_SETTLE.as_millis() as u64 + 50);
        }
        assert_eq!(value(&input, visual), "/[[\n");
    }
}
