//! Keyboard shortcut sheet and shortcut hints (#647).
//!
//! The keys shown here are read from the live keymap, never written twice:
//! [`catalog`] only names and groups actions, and every key text comes from the
//! bindings `bind_keys` installed. Tooltips use [`hint`] for the same reason,
//! so a rebound key changes the sheet, the tooltip and the menu together.
use super::*;
use platform::labels::Os;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Navigation,
    Notes,
    Search,
    View,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::Navigation => "Navigation",
            Group::Notes => "Notes",
            Group::Search => "Search",
            Group::View => "View",
        }
    }
}

struct Entry {
    action: &'static str,
    label: &'static str,
    group: Group,
}

fn entry<A: Action>(label: &'static str, group: Group) -> Entry {
    Entry {
        action: A::name_for_type(),
        label,
        group,
    }
}

/// Every Reader action that has a key binding, in sheet order. A binding whose
/// action is missing here fails `every_bound_reader_action_is_listed`.
fn catalog() -> Vec<Entry> {
    use Group::*;
    vec![
        entry::<HistoryBack>("Back", Navigation),
        entry::<HistoryForward>("Forward", Navigation),
        entry::<reader_log::SelectPreviousRecord>("Previous log record", Navigation),
        entry::<reader_log::SelectNextRecord>("Next log record", Navigation),
        entry::<ListNext>("Next note in list", Navigation),
        entry::<ListPrev>("Previous note in list", Navigation),
        entry::<ReaderScrollDown>("Scroll down", Navigation),
        entry::<ReaderScrollUp>("Scroll up", Navigation),
        entry::<ReaderPageDown>("Page down", Navigation),
        entry::<ReaderPageUp>("Page up", Navigation),
        entry::<ScrollTop>("Scroll to top", Navigation),
        entry::<ScrollBottom>("Scroll to bottom", Navigation),
        entry::<TreeDown>("Next row in folders", Navigation),
        entry::<TreeUp>("Previous row in folders", Navigation),
        entry::<TreeRight>("Expand folder", Navigation),
        entry::<TreeLeft>("Collapse folder", Navigation),
        entry::<TreeOpen>("Open selected row", Navigation),
        entry::<TreePreview>("Preview selected file", Navigation),
        entry::<TreeExpandSubtree>("Expand folder and subfolders", Navigation),
        entry::<TreeCollapseSubtree>("Collapse folder and subfolders", Navigation),
        entry::<HistoryVersionNext>("Next version in history", Navigation),
        entry::<HistoryVersionPrevious>("Previous version in history", Navigation),
        entry::<NewNote>("New note", Notes),
        entry::<SaveSource>("Save", Notes),
        entry::<CloseNote>("Close note", Notes),
        entry::<RenameTreeNote>("Rename selected item", Notes),
        entry::<RenameNote>("Rename note", Notes),
        entry::<reader_move_picker::FolderAccept>("Choose destination folder", Notes),
        entry::<reader_move_picker::FolderNext>("Next destination folder", Notes),
        entry::<reader_move_picker::FolderPrevious>("Previous destination folder", Notes),
        entry::<reader_move_picker::CloseFolderPicker>("Close destination picker", Notes),
        entry::<DeleteNote>("Move to Trash", Notes),
        entry::<UndoTrash>("Undo Move to Trash", Notes),
        entry::<RevealFile>(Os::CURRENT.reveal(), Notes),
        entry::<CopyVaultPath>("Copy vault path", Notes),
        entry::<reader_log::CopyRawLine>("Copy raw log line", Notes),
        entry::<reader_delimited::CopyCells>("Copy selected table cells", Notes),
        entry::<QuickLookFile>("Quick Look", Notes),
        entry::<reader_open::OpenFile>("Open file", Notes),
        entry::<reader_open::OpenFolder>("Open folder", Notes),
        entry::<QuickOpen>("Quick open", Search),
        entry::<FullTextSearch>("Search note contents", Search),
        entry::<FindInNote>("Find in note", Search),
        entry::<reader_log::FocusLogFilter>("Filter log", Search),
        entry::<RecentOlder>("Previous recent note", Navigation),
        entry::<RecentNewer>("Next recent note", Navigation),
        entry::<PaletteNext>("Next result", Search),
        entry::<PalettePrevious>("Previous result", Search),
        entry::<PdfZoomIn>("Zoom in PDF", View),
        entry::<PdfZoomOut>("Zoom out PDF", View),
        entry::<PdfZoomFit>("Fit PDF to width", View),
        entry::<ToggleSource>("Edit / Read", View),
        entry::<OpenLivePreview>("Live Preview", View),
        entry::<CollapseSidebarSections>("Folders only", View),
        entry::<ExpandSidebarSections>("Expand sidebar sections", View),
        entry::<ToggleNotes>("Notes panel", View),
        entry::<ToggleBacklinks>("On this page panel", View),
        entry::<ToggleHiddenFiles>("Show hidden files", View),
        entry::<ToggleShortcutSheet>("Keyboard shortcuts", View),
        entry::<reader_open::NewWindow>("New window", View),
        entry::<reader_settings::OpenSettings>("Settings", View),
        entry::<Dismiss>("Close or clear", View),
    ]
}

/// Display text of every distinct key bound to `action`, highest precedence
/// first (the one GPUI and the toolkit menus pick). On macOS a `ctrl` alias of
/// a `cmd` binding exists for muscle memory and is not shown.
fn keys_for(keymap: &gpui::Keymap, action: &str, os: Os) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for binding in keymap.bindings().rev() {
        if binding.action().name() != action {
            continue;
        }
        let text = binding
            .keystrokes()
            .iter()
            .map(|key| os.shortcut(&key.unparse()))
            .collect::<Vec<_>>()
            .join(" ");
        if !keys.contains(&text) {
            keys.push(text);
        }
    }
    if os == Os::Mac {
        let all = keys.clone();
        keys.retain(|key| mac_native(key).is_none_or(|cmd| !all.contains(&cmd)));
    }
    keys
}

/// The ⌘ form of a macOS ⌃ shortcut without ⌘: "⌃⇧." → "⇧⌘.".
fn mac_native(key: &str) -> Option<String> {
    const GLYPHS: [char; 4] = ['⌃', '⌥', '⇧', '⌘'];
    let modifiers: String = key.chars().take_while(|c| GLYPHS.contains(c)).collect();
    if !modifiers.contains('⌃') || modifiers.contains('⌘') {
        return None;
    }
    let rest = &key[modifiers.len()..];
    Some(format!("{}⌘{rest}", modifiers.replace('⌃', "")))
}

/// The key shown next to `action` in a tooltip or a hint, if it is bound.
pub(crate) fn shortcut(action: &dyn Action, cx: &App) -> Option<String> {
    keys_for(&cx.key_bindings().borrow(), action.name(), Os::CURRENT)
        .into_iter()
        .next()
}

/// `label` followed by the bound key of `action`, e.g. "Back ⌥←" — the same
/// shape as [`platform::labels::with_shortcut`] for keys the keymap cannot
/// describe (keys an input handles itself).
pub(crate) fn hint(label: &str, action: &dyn Action, cx: &App) -> SharedString {
    match shortcut(action, cx) {
        Some(key) => format!("{label} {key}").into(),
        None => SharedString::from(label.to_owned()),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub group: Group,
    pub label: &'static str,
    pub keys: Vec<String>,
}

/// Sheet rows for the current keymap. Actions with no binding on this
/// platform are left out rather than shown without a key.
pub(crate) fn rows(keymap: &gpui::Keymap, os: Os) -> Vec<Row> {
    let mut rows: Vec<Row> = catalog()
        .into_iter()
        .filter_map(|entry| {
            let keys = keys_for(keymap, entry.action, os);
            (!keys.is_empty()).then_some(Row {
                group: entry.group,
                label: entry.label,
                keys,
            })
        })
        .collect();
    // Stable: catalog order within each group.
    rows.sort_by_key(|row| row.group);
    rows
}

/// Case-insensitive match on the label, the group or the key text, so both
/// "trash" and "⌘N" (or "ctrl+n") find a row.
fn matches(row: &Row, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || row.label.to_lowercase().contains(&query)
        || row.group.title().to_lowercase().contains(&query)
        || row
            .keys
            .iter()
            .any(|key| key.to_lowercase().contains(&query))
}

pub(crate) struct Sheet {
    pub open: bool,
    pub input: Entity<InputState>,
    _subscription: Subscription,
}

impl Sheet {
    pub fn new(window: &mut Window, cx: &mut Context<Reader>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search shortcuts…"));
        let subscription = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            open: false,
            input,
            _subscription: subscription,
        }
    }
}

impl Reader {
    pub(super) fn toggle_shortcut_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shortcut_sheet.open {
            self.close_shortcut_sheet(window, cx);
            return;
        }
        if self.quick_open.open {
            self.close_quick_open(window, cx);
        }
        self.clear_hover(cx);
        self.shortcut_sheet.open = true;
        self.shortcut_sheet.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn close_shortcut_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shortcut_sheet.open = false;
        self.restore_document_focus(window, cx);
        cx.notify();
    }

    /// Escape: a non-empty filter is cleared first, then the sheet closes (#483).
    pub(super) fn dismiss_shortcut_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shortcut_sheet.input.read(cx).value().is_empty() {
            self.close_shortcut_sheet(window, cx);
        } else {
            self.shortcut_sheet.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
            cx.notify();
        }
    }

    pub(super) fn render_shortcut_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = brand::palette(cx);
        let tokens = brand::reader_palette(cx);
        let query = self.shortcut_sheet.input.read(cx).value().to_string();
        let rows: Vec<Row> = rows(&cx.key_bindings().borrow(), Os::CURRENT)
            .into_iter()
            .filter(|row| matches(row, &query))
            .collect();
        let mut list = v_flex()
            .id("shortcut-sheet-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .pb_2();
        let mut group = None;
        for (ix, row) in rows.iter().enumerate() {
            if group != Some(row.group) {
                group = Some(row.group);
                list = list.child(
                    div()
                        .px_2()
                        .pt_3()
                        .pb_1()
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(palette.text_muted)
                        .child(row.group.title()),
                );
            }
            list = list.child(
                h_flex()
                    .id(("shortcut-sheet-row", ix))
                    .h(px(30.))
                    .flex_none()
                    .px_2()
                    .gap_3()
                    .rounded(px(6.))
                    .hover(|s| s.bg(tokens.hover))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(row.label),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .children(row.keys.iter().take(2).map(|key| {
                                div()
                                    .h(px(20.))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(4.))
                                    .bg(tokens.hover)
                                    .text_size(px(12.))
                                    .text_color(palette.text_muted)
                                    .child(key.clone())
                            })),
                    ),
            );
        }
        if rows.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_3()
                    .text_sm()
                    .text_color(palette.text_muted)
                    .child("No matching shortcuts"),
            );
        }
        div()
            .id("shortcut-sheet-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .justify_center()
            .pt(px(56.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_shortcut_sheet(window, cx)),
            )
            .child(
                v_flex()
                    .id("shortcut-sheet")
                    .debug_selector(|| "shortcut-sheet".into())
                    .key_context("ShortcutSheet")
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .w(px(520.))
                    .max_w_full()
                    .h(px(456.))
                    .rounded(px(12.))
                    .bg(palette.surface)
                    .text_color(palette.text)
                    .shadow_lg()
                    .child(
                        div()
                            .p_2()
                            .child(Input::new(&self.shortcut_sheet.input).cleanable(true)),
                    )
                    .child(list),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::collections::BTreeSet;

    /// The binders a Reader window runs, without the toolkit's own inputs.
    fn reader_keymap(cx: &mut TestAppContext) -> gpui::Keymap {
        cx.update(|cx| {
            bind_keys(cx);
            reader_open::install(cx);
            reader_settings::install(cx);
            let keymap = cx.key_bindings();
            let bindings = keymap.borrow().bindings().cloned().collect();
            gpui::Keymap::new(bindings)
        })
    }

    #[gpui::test]
    fn every_bound_reader_action_is_listed(cx: &mut TestAppContext) {
        // Bound only in the separate settings window, which has no sheet.
        const NOT_IN_READER: [&str; 1] = ["okilum_settings::CloseSettings"];
        let keymap = reader_keymap(cx);
        assert!(
            keymap.bindings().len() > 40,
            "positive control: the Reader keymap was installed"
        );
        let listed: BTreeSet<&str> = catalog().iter().map(|entry| entry.action).collect();
        let shown: BTreeSet<&str> = rows(&keymap, Os::CURRENT)
            .iter()
            .map(|row| row.label)
            .collect();
        for binding in keymap.bindings() {
            let name = binding.action().name();
            if NOT_IN_READER.contains(&name) {
                continue;
            }
            assert!(
                listed.contains(name),
                "{name} is bound but missing from the shortcut sheet catalog"
            );
            let label = catalog()
                .into_iter()
                .find(|entry| entry.action == name)
                .unwrap()
                .label;
            assert!(shown.contains(label), "{name} is bound but not shown");
        }
    }

    #[gpui::test]
    fn listed_actions_are_bound_and_unique(cx: &mut TestAppContext) {
        let keymap = reader_keymap(cx);
        // Quick Look exists only on macOS; Windows lacks the Unix-only editing
        // actions. Every other listed action must have a key, or the catalog
        // has gone stale.
        let platform_only = [QuickLookFile::name_for_type()];
        let mut seen = BTreeSet::new();
        for entry in catalog() {
            assert!(seen.insert(entry.action), "{} listed twice", entry.action);
            let bound = !keys_for(&keymap, entry.action, Os::CURRENT).is_empty();
            if cfg!(target_os = "macos") || (cfg!(unix) && !platform_only.contains(&entry.action)) {
                assert!(bound, "{} is listed but not bound", entry.action);
            }
        }
    }

    #[gpui::test]
    fn rows_are_grouped_and_use_platform_labels(cx: &mut TestAppContext) {
        let keymap = reader_keymap(cx);
        let rows = rows(&keymap, Os::CURRENT);
        let groups: Vec<Group> = rows.iter().map(|row| row.group).collect();
        let mut sorted = groups.clone();
        sorted.sort();
        assert_eq!(groups, sorted, "rows stay grouped");
        for group in [Group::Navigation, Group::Notes, Group::Search, Group::View] {
            assert!(groups.contains(&group), "{group:?} has rows");
        }
        let row = |label: &str| rows.iter().find(|row| row.label == label).unwrap();
        assert_eq!(row("New note").keys, [Os::CURRENT.shortcut("secondary-n")]);
        assert_eq!(
            row("Keyboard shortcuts").keys,
            [Os::CURRENT.shortcut("secondary-/")]
        );
        assert_eq!(row("Close or clear").keys, ["Esc"]);
        assert_eq!(
            row("Choose destination folder").keys,
            [Os::CURRENT.shortcut("enter")]
        );
        assert_eq!(row("Next destination folder").keys, ["↓"]);
        assert_eq!(row("Previous destination folder").keys, ["↑"]);
        assert_eq!(row("Close destination picker").keys, ["Esc"]);
        // The two hidden-files bindings X11 needs (#395) read as one key.
        assert_eq!(row("Show hidden files").keys.len(), 1);
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(row("Quick open").keys, ["Ctrl+K"]);
            assert_eq!(row("Back").keys, ["Alt+←", "Ctrl+["]);
            for row in &rows {
                for key in &row.keys {
                    assert!(!key.contains('⌘'), "{} shows {key}", row.label);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            // ⌃K is a muscle-memory alias of ⌘K and is not shown.
            assert_eq!(row("Quick open").keys, ["⌘K"]);
            assert_eq!(row("Show hidden files").keys, ["⇧⌘."]);
        }
    }

    #[gpui::test]
    fn hint_uses_the_highest_precedence_binding(cx: &mut TestAppContext) {
        let _ = reader_keymap(cx);
        cx.update(|cx| {
            // Menus render the binding GPUI ranks highest; hints must agree.
            for entry in catalog() {
                let keymap = cx.key_bindings();
                let last = keymap
                    .borrow()
                    .bindings()
                    .rfind(|binding| binding.action().name() == entry.action)
                    .map(|binding| Os::CURRENT.shortcut(&binding.keystrokes()[0].unparse()));
                let first = keys_for(&keymap.borrow(), entry.action, Os::CURRENT)
                    .into_iter()
                    .next();
                assert_eq!(first, last, "{}", entry.label);
            }
            assert_eq!(
                hint("New note", &NewNote, cx),
                SharedString::from(format!("New note {}", Os::CURRENT.shortcut("secondary-n")))
            );
            assert_eq!(hint("Next", &FindNext, cx), SharedString::from("Next"));
        });
    }

    #[test]
    fn macos_hides_ctrl_aliases_of_cmd_shortcuts() {
        // Parsed on any host: `cmd` is the platform modifier everywhere.
        let keymap = gpui::Keymap::new(vec![
            KeyBinding::new("ctrl-shift-.", ToggleHiddenFiles, None),
            KeyBinding::new("cmd->", ToggleHiddenFiles, None),
            KeyBinding::new("cmd-k", QuickOpen, None),
            KeyBinding::new("ctrl-k", QuickOpen, None),
            KeyBinding::new("ctrl-down", ListNext, None),
            KeyBinding::new("j", ListNext, None),
        ]);
        let name = |action: &dyn Action| action.name();
        let mac = |action| keys_for(&keymap, name(action), Os::Mac);
        assert_eq!(mac(&ToggleHiddenFiles), ["⇧⌘."]);
        assert_eq!(mac(&QuickOpen), ["⌘K"]);
        // No ⌘ form exists, so ⌃↓ is the real shortcut and stays.
        assert_eq!(mac(&ListNext), ["J", "⌃↓"]);
        assert_eq!(
            keys_for(&keymap, name(&QuickOpen), Os::Linux),
            ["Ctrl+K", "Super+K"]
        );
        assert_eq!(mac_native("⌃⇧."), Some("⇧⌘.".into()));
        assert_eq!(mac_native("⌥⌘\\\\"), None);
        assert_eq!(mac_native("J"), None);
    }

    #[test]
    fn filter_matches_label_group_and_key() {
        let row = Row {
            group: Group::Notes,
            label: "Move to Trash",
            keys: vec!["Ctrl+Backspace".into()],
        };
        assert!(matches(&row, ""));
        assert!(matches(&row, "  trash "));
        assert!(matches(&row, "NOTES"));
        assert!(matches(&row, "ctrl+back"));
        assert!(!matches(&row, "search"));
    }

    #[gpui::test]
    fn shortcut_opens_filters_and_escape_closes_the_sheet(cx: &mut TestAppContext) {
        let fixture =
            std::env::temp_dir().join(format!("okilum-shortcuts-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("One.md"), "# One\n\nBody\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_global(reader_history::TestSessionDirectory(fixture.join("state")));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("One.md".into()),
                        index_dir: Some(fixture.join("index")),
                        panel_settings_override: Some(fixture.join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |this, window, cx| {
            assert_eq!(this.current_rel, "One.md", "loaded-note positive control");
            this.focus_handle.focus(window, cx);
        });
        assert!(visual.debug_bounds("shortcut-sheet").is_none());
        let toggle = if cfg!(target_os = "macos") {
            "cmd-/"
        } else {
            "ctrl-/"
        };
        visual.simulate_keystrokes(toggle);
        visual.run_until_parked();
        assert!(visual.debug_bounds("shortcut-sheet").is_some());
        reader.update_in(visual, |this, window, cx| {
            assert!(this.shortcut_sheet.open);
            assert!(this
                .shortcut_sheet
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
        });
        visual.simulate_input("trash");
        visual.run_until_parked();
        reader.read_with(visual, |this, cx| {
            assert_eq!(this.shortcut_sheet.input.read(cx).value(), "trash");
        });
        // Escape clears a non-empty filter first, then closes (#483).
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |this, cx| {
            assert!(this.shortcut_sheet.open);
            assert!(this.shortcut_sheet.input.read(cx).value().is_empty());
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |this, _| assert!(!this.shortcut_sheet.open));
        assert!(visual.debug_bounds("shortcut-sheet").is_none());
        // The same chord closes the sheet from inside its filter field.
        visual.simulate_keystrokes(toggle);
        visual.run_until_parked();
        visual.simulate_keystrokes(toggle);
        visual.run_until_parked();
        reader.read_with(visual, |this, _| assert!(!this.shortcut_sheet.open));
        assert_eq!(
            std::fs::read_to_string(root.join("One.md")).unwrap(),
            "# One\n\nBody\n"
        );
        let _ = std::fs::remove_dir_all(&fixture);
    }
}
