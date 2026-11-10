//! Focus and typewriter modes of the note editor (#1643). Focus dims every
//! block but the caret's and the list items it is nested in; typewriter keeps
//! the caret line at the middle of the view. Both are app-wide toggles (View
//! menu, F8 and F9) and work the same in Source and Live Preview.
//!
//! The editor asks for the focused lines only when the caret or the text
//! changed, and the answer never reads more than [`SCAN`] lines around the
//! caret, so neither mode adds work that grows with the note: a 1 MiB note
//! costs what a short one does.
use super::*;
use gpui_base::input::FocusMode;
use gpui_component::input::RopeExt as _;
use gpui_component::Rope;
use std::ops::Range;

/// The most lines read above or below the caret to find its block, its code
/// fence and its parent items. A block longer than this is cut there.
const SCAN: usize = 1_000;
/// How much of the surface covers a dimmed line: the text shows at 40 %.
const WASH: f32 = 0.6;
/// The longest line prefix read to classify a line.
const HEAD: usize = 256;

/// What a line starts with, read from at most [`HEAD`] characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Blank,
    /// An opening or closing code fence: its character and run length.
    Fence(char, usize),
    Heading,
    /// A list item, with the column of its marker.
    Item(usize),
    /// Any other text, with its indentation in columns.
    Text(usize),
}

fn kind(text: &Rope, row: usize) -> Kind {
    // Classification runs for up to SCAN lines on each caret move. Keep the
    // bounded UTF-8 prefix on the stack rather than allocating once per line.
    let mut bytes = [0; HEAD * 4];
    let mut len = 0;
    let mut chars = 0;
    for ch in text.slice_line(row).chars().take(HEAD) {
        len += ch.encode_utf8(&mut bytes[len..]).len();
        chars += 1;
    }
    let head = std::str::from_utf8(&bytes[..len]).expect("encoded UTF-8 prefix");
    let body = head.trim_start_matches([' ', '\t']);
    let indent = head[..head.len() - body.len()].chars().fold(0, |col, ch| {
        if ch == '\t' {
            col + 4 - col % 4
        } else {
            col + 1
        }
    });
    let body = body.trim_end_matches(['\r', '\n']);
    if body.trim().is_empty() {
        // A line longer than the prefix with only spaces in it is still text.
        return if chars < HEAD {
            Kind::Blank
        } else {
            Kind::Text(indent)
        };
    }
    let run = |ch: char| body.chars().take_while(|&c| c == ch).count();
    if indent < 4 {
        if let Some(ch) = ['`', '~'].into_iter().find(|&ch| run(ch) >= 3) {
            return Kind::Fence(ch, run(ch));
        }
        let hashes = run('#');
        if (1..=6).contains(&hashes)
            && body[hashes..]
                .chars()
                .next()
                .is_none_or(|c| c == ' ' || c == '\t')
        {
            return Kind::Heading;
        }
    }
    let spaced = |rest: &str| rest.chars().next().is_none_or(|c| c == ' ' || c == '\t');
    let bullet = body.strip_prefix(['-', '*', '+']).is_some_and(spaced);
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    let ordered =
        (1..=9).contains(&digits) && body[digits..].strip_prefix(['.', ')']).is_some_and(spaced);
    if bullet || ordered {
        Kind::Item(indent)
    } else {
        Kind::Text(indent)
    }
}

/// The code fence the line `row` is in, as the rows from its opening to its
/// closing fence. Fences are counted from at most [`SCAN`] lines above: when
/// the note is longer than that, the count starts outside any fence.
fn fence_around(text: &Rope, row: usize, rows: usize) -> Option<Range<usize>> {
    let mut open: Option<(usize, char, usize)> = None;
    for at in row.saturating_sub(SCAN)..row {
        match (kind(text, at), open) {
            (Kind::Fence(ch, len), Some((_, open_ch, open_len)))
                if ch == open_ch && len >= open_len =>
            {
                open = None
            }
            (Kind::Fence(ch, len), None) => open = Some((at, ch, len)),
            _ => {}
        }
    }
    let (start, ch, len) = match (open, kind(text, row)) {
        (Some((start, ch, len)), _) => (start, ch, len),
        (None, Kind::Fence(ch, len)) => (row, ch, len),
        (None, _) => return None,
    };
    let last = rows.min(row.max(start) + SCAN);
    let end = (row.max(start + 1)..last)
        .find(|&at| matches!(kind(text, at), Kind::Fence(c, l) if c == ch && l >= len))
        .map_or(last, |at| at + 1);
    Some(start..end)
}

/// The buffer lines focus mode keeps bright with the caret at `cursor`: the
/// caret's block (a paragraph, a heading, a code fence, or a list item with
/// its continuation lines) and the line of every item it is nested in.
pub(crate) fn focus_lines(text: &Rope, cursor: usize) -> Vec<Range<usize>> {
    let rows = text.lines_len().max(1);
    let row = text
        .offset_to_point(cursor.min(text.len()))
        .row
        .min(rows - 1);
    if let Some(fence) = fence_around(text, row, rows) {
        return vec![fence];
    }
    let here = kind(text, row);
    if matches!(here, Kind::Blank | Kind::Heading) {
        let line = row..row + 1;
        return vec![line];
    }
    // A paragraph line belongs to the item above it when nothing but
    // paragraph lines lie between them.
    let mut start = row;
    if !matches!(here, Kind::Item(_)) {
        for at in (row.saturating_sub(SCAN)..row).rev() {
            match kind(text, at) {
                Kind::Text(_) => start = at,
                Kind::Item(_) => {
                    start = at;
                    break;
                }
                _ => break,
            }
        }
    }
    let item = match kind(text, start) {
        Kind::Item(column) => Some(column),
        _ => None,
    };
    let mut end = row + 1;
    while end < rows.min(row + SCAN) && matches!(kind(text, end), Kind::Text(_)) {
        end += 1;
    }
    let block = start..end;
    let mut lines = vec![block];
    // The items it is nested in: the nearest item above with a smaller
    // column, then the one above that, up to the list's margin. A paragraph
    // at the margin ends the list.
    let Some(mut column) = item else {
        return lines;
    };
    for at in (start.saturating_sub(SCAN)..start).rev() {
        if column == 0 {
            break;
        }
        match kind(text, at) {
            Kind::Item(parent) if parent < column => {
                lines.push(at..at + 1);
                column = parent;
            }
            Kind::Text(0) | Kind::Heading | Kind::Fence(..) => break,
            _ => {}
        }
    }
    lines.reverse();
    lines
}

impl Reader {
    /// Turn the editor's focus and typewriter modes on or off to match the
    /// settings. Called on every render; it changes the editor only when a
    /// setting or the theme changed.
    pub(super) fn apply_writing_modes(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = &self.editing else {
            return;
        };
        let typewriter = reader_ui_state::typewriter_mode(cx);
        let wash = reader_ui_state::focus_mode(cx).then(|| cx.theme().background.opacity(WASH));
        let bottom = reader_toast::bottom_space(window, cx);
        editing.input.update(cx, |input, cx| {
            // Use measured leading rather than accumulating an estimated row-height error.
            let line_height = f32::from(input.line_height().unwrap_or(px(20.))).max(1.);
            let mut rows = (f32::from(bottom) / line_height).ceil() as usize;
            // Room below the last line to bring it to the middle of the view.
            if typewriter {
                let half = f32::from(input.input_bounds().size.height) / 2.;
                rows = rows.max((half / line_height).ceil() as usize);
            }
            input.set_scroll_beyond_last_line(Some(rows), window, cx);
            if input.is_typewriter() != typewriter {
                input.set_typewriter(typewriter, cx);
            }
            if input.focus_wash() != wash {
                input.set_focus_mode(wash.map(|wash| FocusMode::new(wash, focus_lines)), cx);
            }
        });
    }
}

/// One app-wide, remembered toggle; every editor follows it.
pub(crate) fn toggle_focus_mode(cx: &mut App) {
    reader_ui_state::set_focus_mode(!reader_ui_state::focus_mode(cx), cx);
    refresh_menus(cx);
}

/// One app-wide, remembered toggle; every editor follows it.
pub(crate) fn toggle_typewriter_mode(cx: &mut App) {
    reader_ui_state::set_typewriter_mode(!reader_ui_state::typewriter_mode(cx), cx);
    refresh_menus(cx);
}

fn refresh_menus(cx: &mut App) {
    #[cfg(all(unix, feature = "brain"))]
    crate::brain::app_quit::set_menus(cx);
    #[cfg(not(all(unix, feature = "brain")))]
    crate::reader_app_menu::set_menus(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};

    fn lines(text: &str, at: &str) -> Vec<Range<usize>> {
        let cursor = text.find(at).expect("marker in text");
        focus_lines(&Rope::from(text), cursor)
    }

    #[test]
    fn a_paragraph_is_its_lines_between_blank_lines() {
        let text = "Intro line\n\nfirst line\nsecond CARET\nthird\n\nOutro\n";
        assert_eq!(lines(text, "CARET"), vec![2..5]);
        assert_eq!(lines(text, "Outro"), vec![6..7]);
    }

    #[test]
    fn a_heading_is_its_own_block() {
        let text = "# Title\nBody under it\n";
        assert_eq!(lines(text, "Title"), vec![0..1]);
        assert_eq!(lines(text, "Body"), vec![1..2]);
    }

    #[test]
    fn a_code_fence_is_one_block_with_blank_lines_inside() {
        let text = "Before\n\n```rust\nfn a() {}\n\nfn CARET() {}\n```\n\nAfter\n";
        assert_eq!(lines(text, "CARET"), vec![2..7]);
        // On the opening fence itself.
        assert_eq!(lines(text, "```rust"), vec![2..7]);
        // After the closing fence the count is even again.
        assert_eq!(lines(text, "After"), vec![8..9]);
    }

    #[test]
    fn a_list_item_keeps_its_parents_but_not_its_siblings() {
        let text = "- one\n- two\n  - two.a\n  - two.b\n    - CARET\n      more of it\n  - two.c\n- three\n";
        assert_eq!(lines(text, "CARET"), vec![1..2, 3..4, 4..6]);
        // Its continuation line belongs to it too.
        assert_eq!(lines(text, "more of it"), vec![1..2, 3..4, 4..6]);
        // A top-level item has no parents.
        assert_eq!(lines(text, "three"), vec![7..8]);
    }

    #[test]
    fn a_paragraph_at_the_margin_ends_the_list_above() {
        let text = "- item\n\nParagraph\n\n  - CARET\n";
        assert_eq!(lines(text, "CARET"), vec![4..5]);
    }

    #[test]
    fn a_blank_line_keeps_only_itself_and_the_end_of_the_note_is_safe() {
        let text = "Para\n\n\nNext";
        assert_eq!(focus_lines(&Rope::from(text), 5), vec![1..2]);
        assert_eq!(focus_lines(&Rope::from(text), text.len()), vec![3..4]);
        assert_eq!(focus_lines(&Rope::from(""), 0), vec![0..1]);
    }

    /// A 1 MiB note of sections: headings, paragraphs, nested lists and code.
    fn large_note() -> String {
        // Long prose keeps this realistic large note within the classifier's
        // independent node budget; size alone must not silently imply raw mode.
        let section = |i: usize| {
            format!(
                "## Section {i}\n\nA paragraph of plain words that wraps in a narrow view, \
                 number {i}, with **bold** and `code` in it. {}\nIts second line.\n\n\
                 - item {i}\n  - nested {i}\n    - deeper {i}\n\n\
                 ```rust\nfn f{i}() {{}}\n\nfn g{i}() {{}}\n```\n\n",
                "words ".repeat(1400)
            )
        };
        let mut note = String::from("# Large note\n\n");
        let mut i = 0;
        // Leave 64 bytes for the edit/Enter/IME sequence under Live's 1 MiB
        // admission limit. The fixture stays projected throughout the edits.
        let bytes = (1 << 20) - 64;
        while note.len() + section(i).len() <= bytes {
            note.push_str(&section(i));
            i += 1;
        }
        note.push_str(&"z".repeat(bytes - note.len()));
        assert_eq!(note.len(), bytes);
        note
    }

    fn fixture<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
    ) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
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
                        session_directory: Some(state.clone()),
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
        visual.simulate_resize(size(px(1000.), px(600.)));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        (reader, visual, directory)
    }

    /// Draw one frame, as a slow machine would between a key and the next
    /// task, and say how far the caret's middle is from the view's middle,
    /// in rows; `None` when the caret is not in the view.
    fn off_centre(input: &Entity<EditorState>, visual: &mut VisualTestContext) -> Option<f32> {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        input.read_with(visual, |input, _| {
            let cursor = input.cursor();
            let caret = input.range_to_bounds(&(cursor..cursor))?;
            let view = input.input_bounds();
            let line = f32::from(input.cursor_layout()?.1);
            Some((f32::from(caret.center().y) - f32::from(view.center().y)) / line)
        })
    }

    fn assert_centred(input: &Entity<EditorState>, visual: &mut VisualTestContext, what: &str) {
        let off = off_centre(input, visual);
        assert!(
            off.is_some_and(|off| off.abs() <= 0.6),
            "{what}: the caret is {off:?} rows from the middle of the view"
        );
    }

    /// Real Reader + native projected layout acceptance (#1643). Every immediate
    /// edit and settled publication is measured in Source and Live Preview.
    #[gpui::test]
    fn typewriter_keeps_the_caret_line_centred_in_a_1_mib_note(cx: &mut TestAppContext) {
        writing_geometry(cx, large_note(), true);
    }

    #[gpui::test]
    fn typewriter_keeps_source_frames_centred_in_a_1_mib_note(cx: &mut TestAppContext) {
        writing_geometry(cx, large_note(), false);
    }

    #[gpui::test]
    fn typewriter_keeps_actual_projected_frames_centred(cx: &mut TestAppContext) {
        let large = large_note();
        let end = large[..48 * 1024].rfind("## Section").unwrap();
        writing_geometry(cx, large[..end].to_owned(), true);
    }

    fn writing_geometry(cx: &mut TestAppContext, note: String, live: bool) {
        let (reader, visual, _directory) = fixture(cx, &note);
        reader.update_in(visual, |r, window, cx| {
            r.editing.as_ref().unwrap().input.update(cx, |input, _| {
                input.enable_layout_diagnostics();
            });
            if r.editing.as_ref().unwrap().live_preview.enabled != live {
                r.toggle_live_preview(window, cx);
            }
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        // Mid-note, at the start of a paragraph line.
        let middle = note[note.len() / 2..].find("A paragraph").unwrap() + note.len() / 2;
        input.update(visual, |input, cx| {
            input.set_selected_range(middle..middle, cx)
        });
        off_centre(&input, visual);
        visual.run_until_parked();
        // Positive control: without the mode the caret is not centred.
        let before = off_centre(&input, visual);
        assert!(
            before.is_none_or(|off| off.abs() > 2.),
            "live={live}: control, the caret is {before:?} rows off without the mode"
        );
        visual.update(|_, cx| reader_ui_state::set_typewriter_mode(true, cx));
        assert!(visual.update(|_, cx| reader_ui_state::typewriter_mode(cx)));
        visual.run_until_parked();
        assert!(input.read_with(visual, |input, _| input.is_typewriter()));
        if live {
            reader.read_with(visual, |r, _| {
                let concealed = r.editing.as_ref().unwrap().live_preview.concealed_text();
                assert!(
                    concealed.is_some_and(|display| display.len() < note.len()),
                    "the fixture must conceal Markdown, not merely enable Live"
                );
            });
            assert!(input.read_with(visual, |input, _| input
                .layout_diagnostics()
                .unwrap()
                .projection_composes
                > 0));
        }
        input.read_with(visual, |input, _| {
            input.take_layout_frames();
        });
        visual.update(|_, cx| reader_ui_state::set_focus_mode(true, cx));
        visual.run_until_parked();
        assert!(!input.read_with(visual, |input, _| input.focus_lines().is_empty()));
        // Enabling either mode is presentation-only.
        assert_eq!(
            input.read_with(visual, |input, _| input.value().to_string()),
            note
        );
        assert_centred(&input, visual, &format!("live={live}: turned on"));

        for (step, key) in ["x", "y", "z", "enter", "enter", "w"].iter().enumerate() {
            visual.update(|window, cx| {
                window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), cx);
            });
            assert_centred(&input, visual, &format!("live={live}: key {step} `{key}`"));
            visual.run_until_parked();
            assert_centred(&input, visual, &format!("live={live}: key {step} settled"));
        }
        assert_ne!(
            input.read_with(visual, |input, _| input.value().len()),
            note.len()
        );
        for step in 0..6 {
            visual.dispatch_action(gpui_component::input::Undo);
            assert_centred(&input, visual, &format!("live={live}: Undo {step}"));
            visual.run_until_parked();
            assert_centred(&input, visual, &format!("live={live}: Undo {step} settled"));
        }
        assert_eq!(
            input.read_with(visual, |input, _| input.value().to_string()),
            note,
            "live={live}: every edit undone without source mutation"
        );

        // IME uses the marked range's end as the caret and keeps exact Undo.
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| {
                let at = i.text().byte_to_utf16_idx(i.cursor());
                i.replace_and_mark_text_in_range(Some(at..at), "中", Some(1..1), window, cx);
                assert!(i.marked_text_range(window, cx).is_some());
            });
        });
        assert_centred(&input, visual, "IME marked caret");
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| i.unmark_text(window, cx));
        });
        visual.run_until_parked();
        assert_centred(&input, visual, "IME committed");
        visual.dispatch_action(gpui_component::input::Undo);
        visual.run_until_parked();
        assert_centred(&input, visual, "IME Undo");
        assert_eq!(input.read_with(visual, |i, _| i.value().to_string()), note);

        // Rewrapping changes the caret's visual row without a selection
        // change. Typewriter must use the newly published row metrics.
        visual.simulate_resize(size(px(720.), px(640.)));
        visual.run_until_parked();
        assert_centred(&input, visual, "rewrapped without moving the caret");

        visual.simulate_resize(size(px(720.), px(700.)));
        visual.run_until_parked();
        assert_centred(
            &input,
            visual,
            "viewport height changed without moving the caret",
        );

        // Focus must follow the canonical block after a distant caret move,
        // rather than treating projected display rows as source rows.
        for needle in ["- deeper", "fn g", "A paragraph"] {
            let at = note[middle..].find(needle).unwrap() + middle;
            input.update(visual, |i, cx| i.set_selected_range(at..at, cx));
            visual.run_until_parked();
            assert_centred(&input, visual, needle);
            input.read_with(visual, |i, _| {
                let expected = focus_lines(i.text(), i.cursor());
                assert_eq!(i.focus_lines(), expected);
                assert!(
                    !expected.is_empty() && expected.iter().map(|r| r.len()).sum::<usize>() < 10
                );
                assert!(i.focus_wash().is_some());
            });
        }

        // End-of-note typing and Undo must retain geometry too.
        input.update(visual, |input, cx| {
            input.set_selected_range(note.len()..note.len(), cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse("x").unwrap(), cx);
        });
        assert_centred(&input, visual, "end of note typed");
        visual.run_until_parked();
        assert_centred(&input, visual, "end of note typed and published");
        visual.dispatch_action(gpui_component::input::Undo);
        assert_centred(&input, visual, "end of note Undo");
        visual.run_until_parked();
        assert_centred(&input, visual, "end of note Undo and published");
        visual.simulate_resize(size(px(800.), px(700.)));
        visual.run_until_parked();
        assert_centred(&input, visual, "end of note rewrapped");
        input.read_with(visual, |i, _| {
            let row = i.text().offset_to_point(i.cursor()).row;
            assert_eq!(
                i.focus_lines(),
                std::slice::from_ref(&(row..row + 1)),
                "only the final paragraph stays bright"
            );
        });
        assert_eq!(
            input.read_with(visual, |input, _| input.value().to_string()),
            note
        );

        if live {
            let frames = input.read_with(visual, |input, _| input.take_layout_frames());
            assert!(
                !frames.is_empty(),
                "positive control: observed painted frames"
            );
            assert!(
                frames.iter().all(|frame| !frame.raw),
                "raw fallback is not projected Live"
            );
            assert!(
                frames.iter().any(|f| !f.edit && f.anchor == "typewriter"),
                "presentation publications use typewriter with no source edit"
            );
            assert!(
                input.read_with(visual, |i, _| i
                    .layout_diagnostics()
                    .unwrap()
                    .provider_applies
                    > 1),
                "the real Reader published replacement classification"
            );
            diagnostic_error!(
                "WRITING_PROJECTED bytes={} frames={} projection_composes={} max_prepaint_ms={:.3}",
                note.len(),
                frames.len(),
                input.read_with(visual, |input, _| input
                    .layout_diagnostics()
                    .unwrap()
                    .projection_composes),
                frames.iter().map(|f| f.prepaint_ms).fold(0.0_f32, f32::max)
            );
        }

        // Scroll toward the start: at EOF, scrolling farther down would
        // hit the extent and could not serve as an off-centre control.
        // Scrolling by hand is free until the caret moves again.
        let bounds = input.read_with(visual, |input, _| input.input_bounds());
        visual.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(200.))),
            ..Default::default()
        });
        visual.run_until_parked();
        let scrolled = off_centre(&input, visual);
        assert!(
            scrolled.is_none_or(|off| off.abs() > 2.),
            "live={live}: the wheel scrolled, caret {scrolled:?} rows off"
        );
        // Idle frames, projection publications and a rewrap must not steal
        // the viewport back from a user who has scrolled away.
        if live {
            let provider = reader.read_with(visual, |r, _| {
                r.editing
                    .as_ref()
                    .unwrap()
                    .live_preview
                    .accepted_provider()
                    .unwrap()
            });
            input.update(visual, |i, cx| {
                i.set_projection_provider(Some(provider), cx)
            });
            visual.run_until_parked();
        }
        visual.simulate_resize(size(px(760.), px(620.)));
        visual.run_until_parked();
        let paused = off_centre(&input, visual);
        assert!(
            paused.is_none_or(|off| off.abs() > 2.),
            "manual scroll survives publication/rewrap: {paused:?}"
        );
        visual.update(|window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse("left").unwrap(), cx);
        });
        assert_centred(&input, visual, "caret navigation after manual scroll");
        assert_eq!(
            input.read_with(visual, |i, _| i.value().to_string()),
            note,
            "caret navigation resumes following without a source edit"
        );
        visual.update(|window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse("v").unwrap(), cx);
        });
        assert_centred(
            &input,
            visual,
            &format!("live={live}: typed after a scroll"),
        );
        visual.update(|_, cx| {
            reader_ui_state::set_typewriter_mode(false, cx);
            reader_ui_state::set_focus_mode(false, cx);
        });
        visual.run_until_parked();
    }

    /// The last line of a note comes to the middle too: typewriter adds
    /// room below it.
    #[gpui::test]
    fn typewriter_centres_the_last_line(cx: &mut TestAppContext) {
        let note: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let (reader, visual, _directory) = fixture(cx, &note);
        visual.update(|_, cx| reader_ui_state::set_typewriter_mode(true, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        input.update(visual, |input, cx| {
            input.set_selected_range(note.len()..note.len(), cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse("x").unwrap(), cx);
        });
        visual.run_until_parked();
        assert_centred(&input, visual, "the last line");
    }

    /// Focus mode keeps the caret's block and its parent items, follows the
    /// caret, and is off (nothing kept, nothing dimmed) until turned on.
    #[gpui::test]
    fn focus_mode_follows_the_caret_block(cx: &mut TestAppContext) {
        let note =
            "# Title\n\nFirst paragraph\nstill first\n\n- one\n  - two\n    - three\n\nLast\n";
        let (reader, visual, _directory) = fixture(cx, note);
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        let at = |needle: &str| note.find(needle).unwrap();
        input.update(visual, |input, cx| {
            input.set_selected_range(at("three")..at("three"), cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            input.read_with(visual, |input, _| input.focus_lines().is_empty()),
            "off by default"
        );
        visual.update(|_, cx| reader_ui_state::set_focus_mode(true, cx));
        visual.run_until_parked();
        let kept = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            input.read_with(visual, |input, _| input.focus_lines().to_vec())
        };
        assert_eq!(kept(visual), vec![5..6, 6..7, 7..8]);
        input.update(visual, |input, cx| {
            input.set_selected_range(at("still")..at("still"), cx)
        });
        visual.run_until_parked();
        assert_eq!(kept(visual), vec![2..4]);
        visual.update(|_, cx| reader_ui_state::set_focus_mode(false, cx));
        visual.run_until_parked();
        assert!(kept(visual).is_empty(), "off again");
        assert!(input.read_with(visual, |input, _| input.focus_wash().is_none()));
    }

    /// The lines read do not depend on the note's length: a caret in the
    /// middle of a 1 MiB note of one long paragraph is cut at SCAN lines.
    #[test]
    fn the_block_is_found_within_a_bounded_window_in_a_1_mib_note() {
        let line = "word ".repeat(15) + "\n";
        let text = line.repeat((1 << 20) / line.len() + 1);
        assert!(text.len() >= 1 << 20);
        let rope = Rope::from(text.as_str());
        let middle = text.len() / 2;
        let row = rope.offset_to_point(middle).row;
        let found = focus_lines(&rope, middle);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains(&row), "{found:?} row {row}");
        assert!(found[0].len() <= 2 * SCAN + 1, "{found:?}");
    }
}
