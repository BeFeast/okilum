use super::*;

/// Key context of the reader window. Every binding below is scoped to it so
/// the inputs keep their own (`Input`) bindings: gpui dispatches the deepest
/// matching binding first, and an input that does not consume a key (Escape,
/// Enter on a single-line input) propagates it up to these.
pub(super) const READER_CONTEXT: &str = "Reader";

/// Something drawn inside the Reader in the last frame holds focus: its
/// dispatch path runs through the Reader's context. Focus on a handle that
/// was not drawn dispatches from the window root, outside the Reader, and
/// so does a window with no focus.
pub(super) fn focus_drawn_in_reader(window: &Window) -> bool {
    window
        .context_stack()
        .iter()
        .any(|context| context.contains(READER_CONTEXT))
}

#[allow(unused_imports)]
pub(crate) use okilum_shell_kit::actions::{
    ArchiveExtractAll, ArchiveExtractSelected, CanvasFit, CanvasZoomIn, CanvasZoomOut,
    CopyDrawingDetails, CopyFileContent, CopyViewerImage, Dismiss, DismissManagerModel,
    ExpandDrawing, FindInNote, FindNext, FindPrev, OpenDrawingExternally, PdfZoomFit, PdfZoomIn,
    PdfZoomOut, ShowDrawingDetails, ToggleSoftWrap, OPEN_IN_TERMINAL_KEY,
    OPEN_IN_TERMINAL_KEY_WINDOWS,
};

actions!(
    okilum,
    [
        NewNote,
        OpenDailyNote,
        NewUntitled,
        ShowRecentFiles,
        CloseNote,
        NewFolder,
        RenameNote,
        RenameTreeNote,
        DeleteNote,
        UndoTrash,
        RedoTrash,
        RecoverLinkMoves,
        NoteSourceHistory,
        HistoryVersionNext,
        HistoryVersionPrevious,
        RecoverUnsavedNotes,
        ToggleSource,
        OpenLivePreview,
        ToggleTask,
        SaveSource,
        RevealFile,
        CopyVaultPath,
        QuickLookFile,
        TreePreview,
        ReaderScrollDown,
        ReaderScrollUp,
        ReaderPageDown,
        ReaderPageUp,
        QuickOpen,
        FullTextSearch,
        SwitchWindowMode,
        RecentOlder,
        RecentNewer,
        PaletteNext,
        PalettePrevious,
        HistoryBack,
        HistoryForward,
        ListNext,
        ListPrev,
        ScrollTop,
        ScrollBottom,
        ToggleNotes,
        ToggleSplit,
        FocusOtherPane,
        ToggleBacklinks,
        MaximizeNote,
        ToggleProject,
        ToggleHiddenFiles,
        CopyFilePath,
        CopyFileWikilink,
        OpenFileDefault,
        NewFromTemplate,
        TreeDown,
        TreeUp,
        TreeRight,
        TreeLeft,
        TreeOpen,
        TreeReturn,
        TreeExpandSubtree,
        TreeCollapseSubtree,
        CollapseFolders,
        FocusCurrentFolder,
        ToggleLeftMode,
        ToggleShortcutSheet,
        ToggleNoteWidth,
        ToggleFocusMode,
        ToggleTypewriterMode,
    ]
);

/// F8 / F9: focus and typewriter modes in the note editor (#1643), the keys
/// Typora gives them. Free in every Reader context.
const FOCUS_MODE_KEY: &str = "f8";
const TYPEWRITER_MODE_KEY: &str = "f9";

/// A default whose chord differs on Windows (Oleg, 2026-11-14). There Ctrl+Alt
/// is AltGr, which types characters on many layouts, with or without Shift,
/// and Alt+Shift switches the input language. Windows chords use neither:
/// Ctrl+Shift+key. macOS and Linux keep `elsewhere`.
const fn windows_key(elsewhere: &'static str, windows: &'static str) -> &'static str {
    if cfg!(windows) {
        windows
    } else {
        elsewhere
    }
}
pub(crate) const COPY_WITH_FORMATTING_KEY: &str = windows_key("secondary-alt-c", "ctrl-shift-h");
/// Copy as Jira (#3292): Ctrl+Shift+J / ⇧⌘J (Alt+J on Windows, where
/// Ctrl+Shift+J validates code files in ReaderFileEdit and Ctrl+Alt /
/// Alt+Shift are reserved).
pub(crate) const COPY_AS_JIRA_KEY: &str = windows_key("secondary-shift-j", "alt-j");
/// Paste as Markdown from Jira (#3292): Ctrl+Shift+K / ⇧⌘K (Alt+K on
/// Windows, where Ctrl+Shift+K pastes titled links and Ctrl+Shift+I is
/// gpui-component's inspector).
pub(crate) const PASTE_FROM_JIRA_KEY: &str = windows_key("secondary-shift-k", "alt-k");
const TOGGLE_PROJECT_KEY: &str = windows_key("secondary-alt-p", "ctrl-shift-p");
const OPEN_BLOCK_EXTERNALLY_KEY: &str = windows_key("secondary-alt-e", "ctrl-shift-d");
const PASTE_AS_TITLED_LINK_KEY: &str = windows_key("secondary-alt-v", "ctrl-shift-k");
pub(crate) const MAXIMIZE_NOTE_KEY: &str = windows_key("secondary-alt-f", "ctrl-shift-enter");
/// Not Ctrl+Shift+B on Windows: Microsoft Pinyin opens its emoji panel there.
/// Y is free in every Okilum context and in the common Windows IMEs.
/// The Notes sidebar (Oleg, 2026-11-17, #773): ⌘⇧\\ / Ctrl+Shift+\\, so that
/// ⌘\\ / Ctrl+\\ is free for split view. macOS folds Shift into punctuation:
/// Cmd+Shift+backslash arrives as cmd-|. Now / Tree, which used this chord,
/// keeps its menu item and toolbar button without a default shortcut.
pub(crate) const NOTES_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-|"
} else {
    "ctrl-shift-\\"
};
pub(crate) const BACKLINKS_KEY: &str = windows_key("ctrl-alt-\\", "ctrl-shift-y");
pub(crate) const PARAGRAPH_KEY: &str = windows_key("secondary-alt-0", "ctrl-shift-t");
pub(crate) const FORMAT_CODE_KEY: &str = windows_key("alt-shift-f", "ctrl-shift-g");
pub(crate) const VALIDATE_CODE_KEY: &str = windows_key("alt-shift-v", "ctrl-shift-j");
pub(crate) const REPAIR_CODE_KEY: &str = windows_key("alt-shift-r", "ctrl-shift-r");
/// Show in Finder/folder/Explorer. Not Win+Alt+R on Windows: the Xbox Game
/// Bar records the screen there and the key never reaches Okilum (#3110).
/// Not Ctrl+Shift+S either: that is Save All in Windows apps (#3133), and
/// Ctrl+Shift+E is Live Preview. U is free in every Okilum context.
pub(crate) const REVEAL_FILE_KEY_WINDOWS: &str = "ctrl-shift-u";
pub(crate) const REVEAL_FILE_KEY: &str = windows_key("alt-cmd-r", REVEAL_FILE_KEY_WINDOWS);

/// ⌥⌘W / Ctrl+Alt+W (Ctrl+Shift+W on Windows): normal (reading width) or
/// full note width. Free in every Reader context; W for width.
pub(super) const NOTE_WIDTH_KEY: &str = windows_key("secondary-alt-w", "ctrl-shift-w");

/// One app-wide, remembered toggle; every window follows it.
pub(crate) fn toggle_note_width(cx: &mut App) {
    reader_ui_state::set_full_note_width(!reader_ui_state::full_note_width(cx), cx);
    #[cfg(all(unix, feature = "brain"))]
    brain::app_quit::set_menus(cx);
    #[cfg(not(all(unix, feature = "brain")))]
    reader_app_menu::set_menus(cx);
}

/// Toolbar and menu label: what the toggle switches to.
pub(crate) fn note_width_label(cx: &App) -> &'static str {
    if reader_ui_state::full_note_width(cx) {
        "Normal note width"
    } else {
        "Full note width"
    }
}

pub(crate) fn view_menu(cx: &App) -> Menu {
    Menu {
        name: "View".into(),
        disabled: false,
        items: vec![
            MenuItem::action("Zoom In UI", ui_zoom::ZoomIn),
            MenuItem::action("Zoom Out UI", ui_zoom::ZoomOut),
            MenuItem::action("Actual UI Size", ui_zoom::Reset),
            MenuItem::separator(),
            MenuItem::action("Zoom In", reader_text_zoom::ZoomIn),
            MenuItem::action("Zoom Out", reader_text_zoom::ZoomOut),
            MenuItem::action("Actual Size", reader_text_zoom::Reset),
            MenuItem::separator(),
            MenuItem::action("Now / Tree", ToggleLeftMode),
            MenuItem::action("Split View", ToggleSplit),
            MenuItem::action("Focus Other Pane", FocusOtherPane),
            MenuItem::action("Reveal in Tree", FocusCurrentFolder),
            MenuItem::action("Maximize Note", MaximizeNote),
            MenuItem::action("Full Note Width", ToggleNoteWidth)
                .checked(reader_ui_state::full_note_width(cx)),
            MenuItem::separator(),
            MenuItem::action("Focus Mode", ToggleFocusMode)
                .checked(reader_ui_state::focus_mode(cx)),
            MenuItem::action("Typewriter Mode", ToggleTypewriterMode)
                .checked(reader_ui_state::typewriter_mode(cx)),
        ],
    }
}

/// Tree shortcuts (#410) work while the folder tree has focus. Arrow keys
/// keep their modifiers on macOS, so these strings are what a Mac reports;
/// the tests below check that against the macOS key shape.
const TREE_KEYS: &str = "ReaderTree && !Input";
pub(super) const TREE_EXPAND_SUBTREE_KEY: &str = "alt-right";
pub(super) const TREE_COLLAPSE_SUBTREE_KEY: &str = "alt-left";

/// ⌘⇧E / Ctrl+Shift+E (#916). Letters keep shift on macOS (only shifted
/// punctuation is rewritten, see HIDDEN_FILES_KEY_MAC), so one string fits.
pub(super) const LIVE_PREVIEW_KEY: &str = "secondary-shift-e";

/// X11/Wayland may report the press either way: `.` with shift, or `>`.
pub(super) const HIDDEN_FILES_KEYS: [&str; 2] = ["ctrl-shift-.", "ctrl->"];
/// macOS reports shifted punctuation as the shifted character with shift
/// cleared, so `cmd-shift-.` never matches; ⇧⌘. arrives as `cmd->` (#395).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) const HIDDEN_FILES_KEY_MAC: &str = "cmd->";

pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        reader_read_menu::COPY_MARKDOWN_KEY,
        reader_read_menu::CopyAsMarkdown,
        Some("ReaderRead && !Input && !ReaderTree"),
    )]);
    cx.bind_keys([
        KeyBinding::new(
            reader_read_menu::COPY_MARKDOWN_KEY,
            reader_read_menu::CopyAsMarkdown,
            Some("ReaderSource > Input"),
        ),
        KeyBinding::new(
            COPY_WITH_FORMATTING_KEY,
            reader_read_menu::CopyWithFormatting,
            Some("ReaderRead && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            COPY_AS_JIRA_KEY,
            reader_read_menu::CopyAsJira,
            Some("ReaderRead && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            COPY_AS_JIRA_KEY,
            reader_read_menu::CopyAsJira,
            Some("ReaderSource > Input"),
        ),
        // Prefer the standard chord in note menus; Shift+Insert remains bound.
        KeyBinding::new(
            "secondary-v",
            gpui_component::input::Paste,
            Some("ReaderSource > Input"),
        ),
    ]);
    project_terminal::bind_keys(cx);
    reader_text_zoom::bind_keys(cx);
    ui_zoom::init(cx);
    reader_editor::format::bind_keys(cx);
    reader_title_nav::bind_menu_keys(cx);
    quick_open::bind_palette_keys(cx);
    #[cfg(any(unix, windows))]
    reader_move_picker::bind_keys(cx);
    reader_log::bind_keys(cx);
    kubernetes_workspace::bind_keys(cx);
    reader_delimited::bind_keys(cx);
    reader_har::bind_keys(cx);
    reader_code_file::bind_keys(cx);
    for context in ["ReaderFileEdit", "ReaderFileEdit > Input"] {
        cx.bind_keys([
            KeyBinding::new(FORMAT_CODE_KEY, reader_editor::FormatCode, Some(context)),
            KeyBinding::new(
                VALIDATE_CODE_KEY,
                reader_editor::ValidateCode,
                Some(context),
            ),
            KeyBinding::new(REPAIR_CODE_KEY, reader_editor::RepairCode, Some(context)),
        ]);
    }
    for context in ["ReaderCode", "ReaderCode > Input"] {
        cx.bind_keys([KeyBinding::new(
            REPAIR_CODE_KEY,
            reader_editor::RepairCode,
            Some(context),
        )]);
    }
    // App-level, so the key, the View menu and the toolbar all reach it.
    cx.on_action(|_: &ToggleNoteWidth, cx| toggle_note_width(cx));
    cx.bind_keys([
        KeyBinding::new(NOTE_WIDTH_KEY, ToggleNoteWidth, Some(READER_CONTEXT)),
        KeyBinding::new(NOTE_WIDTH_KEY, ToggleNoteWidth, Some("Reader > Input")),
    ]);
    cx.on_action(|_: &ToggleFocusMode, cx| reader_editor::toggle_focus_mode(cx));
    cx.on_action(|_: &ToggleTypewriterMode, cx| reader_editor::toggle_typewriter_mode(cx));
    cx.bind_keys([
        KeyBinding::new(FOCUS_MODE_KEY, ToggleFocusMode, Some(READER_CONTEXT)),
        KeyBinding::new(FOCUS_MODE_KEY, ToggleFocusMode, Some("Reader > Input")),
        KeyBinding::new(
            TYPEWRITER_MODE_KEY,
            ToggleTypewriterMode,
            Some(READER_CONTEXT),
        ),
        KeyBinding::new(
            TYPEWRITER_MODE_KEY,
            ToggleTypewriterMode,
            Some("Reader > Input"),
        ),
    ]);
    cx.bind_keys([
        KeyBinding::new(TOGGLE_PROJECT_KEY, ToggleProject, Some(READER_CONTEXT)),
        KeyBinding::new(REVEAL_FILE_KEY, RevealFile, Some(READER_CONTEXT)),
        KeyBinding::new(
            open_in::OPEN_IN_EDITOR_KEY,
            open_in::OpenInDefaultEditor,
            Some(READER_CONTEXT),
        ),
        KeyBinding::new("alt-cmd-c", CopyVaultPath, Some(READER_CONTEXT)),
        // Quick Look is a macOS service; elsewhere Space would only report that.
        #[cfg(target_os = "macos")]
        KeyBinding::new("space", QuickLookFile, Some("ReaderFile && !Input")),
    ]);
    // ⌘/Ctrl + and − arrive as `=`/`+` and `-` depending on layout and shift.
    let pdf = Some("ReaderPdf");
    cx.bind_keys([
        KeyBinding::new("secondary-=", PdfZoomIn, pdf),
        KeyBinding::new("secondary-+", PdfZoomIn, pdf),
        KeyBinding::new("secondary-shift-=", PdfZoomIn, pdf),
        KeyBinding::new("secondary--", PdfZoomOut, pdf),
        KeyBinding::new("secondary-0", PdfZoomFit, pdf),
    ]);

    cx.bind_keys([
        KeyBinding::new(
            "down",
            ReaderScrollDown,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            "up",
            ReaderScrollUp,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            "pagedown",
            ReaderPageDown,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            "pageup",
            ReaderPageUp,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
    ]);

    cx.bind_keys([
        KeyBinding::new("down", HistoryVersionNext, Some("ReaderHistory")),
        KeyBinding::new("up", HistoryVersionPrevious, Some("ReaderHistory")),
        KeyBinding::new("down", HistoryVersionNext, Some("ReaderHistory > Input")),
        KeyBinding::new("up", HistoryVersionPrevious, Some("ReaderHistory > Input")),
    ]);
    let ctx = Some(READER_CONTEXT);
    // `j`/`k` are plain letters: with an input focused they must keep being
    // text, so those two are disabled anywhere under an `Input` context.
    let not_input = format!("{READER_CONTEXT} && !Input");
    // Where macOS has both a `ctrl` alias and a `cmd` binding, the `cmd` one
    // comes later: GPUI ranks later bindings higher, and menus, tooltips and
    // the shortcut sheet (#647) all show the highest-ranked key.
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-f", FullTextSearch, ctx),
        KeyBinding::new("ctrl-shift-f", FullTextSearch, Some("Reader > Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-f", FullTextSearch, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-f", FullTextSearch, Some("Reader > Input")),
        KeyBinding::new("ctrl-tab", RecentOlder, ctx),
        KeyBinding::new("ctrl-shift-tab", RecentNewer, ctx),
        KeyBinding::new("ctrl-tab", RecentOlder, Some("Reader > Input")),
        KeyBinding::new("ctrl-shift-tab", RecentNewer, Some("Reader > Input")),
        KeyBinding::new("secondary-/", ToggleShortcutSheet, ctx),
        KeyBinding::new("secondary-/", ToggleShortcutSheet, Some("Reader > Input")),
        KeyBinding::new("escape", Dismiss, Some("Reader > ShortcutSheet > Input")),
        KeyBinding::new("down", PaletteNext, Some("Reader > QuickOpen > Input")),
        KeyBinding::new("up", PalettePrevious, Some("Reader > QuickOpen > Input")),
        KeyBinding::new("escape", Dismiss, Some("Reader > QuickOpen > Input")),
        KeyBinding::new("escape", Dismiss, Some("ViewerFind > Input")),
        KeyBinding::new("escape", Dismiss, Some("InlineCreate > Input")),
        KeyBinding::new("escape", Dismiss, Some("LogSaving > Input")),
        #[cfg(any(unix, windows))]
        KeyBinding::new("escape", Dismiss, Some("InlineRename > Input")),
        #[cfg(any(unix, windows))]
        KeyBinding::new("f2", RenameTreeNote, Some("ReaderTree && !Input")),
        #[cfg(any(unix, windows))]
        KeyBinding::new("f2", RenameNote, Some("Reader && !ReaderTree && !Input")),
        #[cfg(any(unix, windows))]
        KeyBinding::new("f2", RenameNote, Some("ReaderSource > Input")),
        // #1642: F2 next to a rendered Live Preview block opens its source;
        // elsewhere it falls through to Rename above. Bound later, tried first.
        KeyBinding::new(
            "f2",
            reader_editor::OpenRenderedBlock,
            Some("ReaderSource > Input"),
        ),
        KeyBinding::new("secondary-n", NewNote, ctx),
        #[cfg(any(unix, windows))]
        KeyBinding::new("secondary-backspace", DeleteNote, Some("Reader && !Input")),
        #[cfg(any(unix, windows))]
        KeyBinding::new("secondary-z", UndoTrash, Some("Reader && !Input")),
        // Redo uses each platform's own Redo key, as the note editor does.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-z", RedoTrash, Some("Reader && !Input")),
        #[cfg(any(target_os = "linux", windows))]
        KeyBinding::new("ctrl-y", RedoTrash, Some("Reader && !Input")),
        #[cfg(any(target_os = "linux", windows))]
        KeyBinding::new("ctrl-shift-z", RedoTrash, Some("Reader && !Input")),
        KeyBinding::new("secondary-w", CloseNote, ctx),
        KeyBinding::new("secondary-w", CloseNote, Some("Reader > Input")),
        KeyBinding::new("secondary-[", HistoryBack, ctx),
        KeyBinding::new("secondary-]", HistoryForward, ctx),
        KeyBinding::new("secondary-e", ToggleSource, ctx),
        KeyBinding::new("secondary-s", SaveSource, ctx),
        KeyBinding::new("secondary-e", ToggleSource, Some("Reader > Input")),
        // #916: straight to Live Preview from Reader, Edit or Source.
        KeyBinding::new(LIVE_PREVIEW_KEY, OpenLivePreview, ctx),
        KeyBinding::new(LIVE_PREVIEW_KEY, OpenLivePreview, Some("Reader > Input")),
        // Obsidian's «Toggle checkbox status» key (#1051).
        KeyBinding::new("secondary-l", ToggleTask, Some("Reader > Input")),
        // #1993: the caret's code block in an external editor.
        KeyBinding::new(
            OPEN_BLOCK_EXTERNALLY_KEY,
            reader_editor::OpenBlockExternally,
            Some("ReaderSource > Input"),
        ),
        // #2340: the code block languages after ```, before anything is typed.
        // macOS usually takes Ctrl+Space for the input source, so Option+Esc
        // (Xcode's completion key) is bound there too.
        KeyBinding::new(
            "ctrl-space",
            reader_editor::ShowCodeLanguages,
            Some("ReaderSource > Input"),
        ),
        #[cfg(target_os = "macos")]
        KeyBinding::new(
            "alt-escape",
            reader_editor::ShowCodeLanguages,
            Some("ReaderSource > Input"),
        ),
        // #1611: Paste as Plain Text / as Titled Link, in the note editor.
        // Plain Text is macOS's own "Paste and Match Style" key, Option+Shift+Cmd+V:
        // Cmd+Shift+V is commonly a global clipboard-history hotkey (Raycast).
        #[cfg(target_os = "macos")]
        KeyBinding::new(
            "cmd-alt-shift-v",
            note_paste::PasteAsPlainText,
            Some("ReaderSource > Input"),
        ),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new(
            "ctrl-shift-v",
            note_paste::PasteAsPlainText,
            Some("ReaderSource > Input"),
        ),
        KeyBinding::new(
            PASTE_AS_TITLED_LINK_KEY,
            note_paste::PasteAsTitledLink,
            Some("ReaderSource > Input"),
        ),
        KeyBinding::new(
            PASTE_FROM_JIRA_KEY,
            note_paste::PasteFromJira,
            Some("ReaderSource > Input"),
        ),
        KeyBinding::new("secondary-s", SaveSource, Some("Reader > Input")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FindInNote, ctx),
        // Reader commands outrank toolkit editing aliases only inside Reader inputs.
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FindInNote, Some("Reader > Input")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FindInNote, Some(reader_code_file::KEY_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FindInNote, Some("ReaderCode > Input")),
        KeyBinding::new("ctrl-k", QuickOpen, Some("Reader > Input")),
        KeyBinding::new("escape", DismissManagerModel, Some("ManagerModels > Input")),
        KeyBinding::new("escape", DismissManagerModel, Some("ManagerModels")),
        KeyBinding::new("escape", Dismiss, ctx),
        KeyBinding::new("ctrl-k", QuickOpen, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FindInNote, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FindInNote, Some("Reader > Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FindInNote, Some(reader_code_file::KEY_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FindInNote, Some("ReaderCode > Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", QuickOpen, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", QuickOpen, Some("Reader > Input")),
        KeyBinding::new("alt-left", HistoryBack, ctx),
        KeyBinding::new("alt-right", HistoryForward, ctx),
        KeyBinding::new("ctrl-down", ListNext, ctx),
        KeyBinding::new("ctrl-up", ListPrev, ctx),
        KeyBinding::new("j", ListNext, Some(&not_input)),
        KeyBinding::new("k", ListPrev, Some(&not_input)),
        KeyBinding::new("secondary-shift-l", FocusCurrentFolder, ctx),
        KeyBinding::new(
            "secondary-shift-l",
            FocusCurrentFolder,
            Some("Reader > Input"),
        ),
        // Split view (Oleg, 2026-11-17, #773); the Notes toggle moved to NOTES_KEY.
        KeyBinding::new("secondary-\\", ToggleSplit, ctx),
        KeyBinding::new("secondary-\\", ToggleSplit, Some("Reader > Input")),
        KeyBinding::new(NOTES_KEY, ToggleNotes, ctx),
        // Layouts that report Shift+backslash as the bar itself.
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-|", ToggleNotes, ctx),
        KeyBinding::new(BACKLINKS_KEY, ToggleBacklinks, ctx),
        KeyBinding::new(MAXIMIZE_NOTE_KEY, MaximizeNote, ctx),
        KeyBinding::new(MAXIMIZE_NOTE_KEY, MaximizeNote, Some("Reader > Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-\\", ToggleBacklinks, ctx),
        KeyBinding::new("down", TreeDown, Some("ReaderTree && !Input")),
        KeyBinding::new("up", TreeUp, Some("ReaderTree && !Input")),
        KeyBinding::new("right", TreeRight, Some("ReaderTree && !Input")),
        KeyBinding::new("left", TreeLeft, Some("ReaderTree && !Input")),
        #[cfg(not(any(unix, windows)))]
        KeyBinding::new("enter", TreeOpen, Some("ReaderTree && !Input")),
        #[cfg(any(unix, windows))]
        KeyBinding::new("enter", TreeReturn, Some("ReaderTree && !Input")),
        KeyBinding::new("space", TreePreview, Some("ReaderTree && !Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-down", TreeOpen, Some(TREE_KEYS)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-down", TreeOpen, Some(TREE_KEYS)),
        KeyBinding::new(TREE_EXPAND_SUBTREE_KEY, TreeExpandSubtree, Some(TREE_KEYS)),
        KeyBinding::new(
            TREE_COLLAPSE_SUBTREE_KEY,
            TreeCollapseSubtree,
            Some(TREE_KEYS),
        ),
        KeyBinding::new(HIDDEN_FILES_KEYS[0], ToggleHiddenFiles, ctx),
        KeyBinding::new(HIDDEN_FILES_KEYS[1], ToggleHiddenFiles, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new(HIDDEN_FILES_KEY_MAC, ToggleHiddenFiles, ctx),
        KeyBinding::new("ctrl-home", ScrollTop, ctx),
        KeyBinding::new("ctrl-end", ScrollBottom, ctx),
        // #998: VS Code's ⌥Z / Alt+Z, only in code views and code editors:
        // elsewhere ⌥Z types a character (Ω, ż).
        KeyBinding::new("alt-z", ToggleSoftWrap, Some(reader_code_file::KEY_CONTEXT)),
    ]);
}
