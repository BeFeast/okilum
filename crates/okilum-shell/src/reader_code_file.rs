//! Source-code files in the Reader (#998). tree-sitter highlights them in a
//! read-only editor, the engine Markdown code blocks already use; edit mode
//! opens the same file in a writable editor with the same language.
use super::*;
use gpui_component::input::{Editor, EditorState, InputEvent, InputState, WrappingIndent};
use std::time::Instant;

pub(crate) mod contents;
pub(crate) mod document;
mod find;
#[cfg(test)]
mod find_review_tests;
#[cfg(test)]
mod find_tests;
mod loading;
mod presentation;
pub(crate) use presentation::PresentationState;
#[cfg(test)]
mod fold_all_scroll_tests;
#[cfg(test)]
mod loading_tests;
#[cfg(feature = "data-view-native-harness")]
pub(crate) mod native_harness;
#[cfg(test)]
mod review_tests;
mod structure;
mod toolbar;
#[cfg(feature = "data-view-native-harness")]
mod toolbar_native;
mod view;
#[cfg(test)]
mod view_tests;
use document::Document;
use view::copy_text_node;
pub(crate) use view::{bind_keys, Availability, View};

/// Bounded preview loading; UI editing has its own smaller admission cap.
pub(crate) const MAX_BYTES: u64 = 64 * 1024 * 1024;
const EDIT_BYTES: usize = 4 * 1024 * 1024;

actions!(
    reader_code_file,
    [
        CopyNode,
        CopySelection,
        ShowTable,
        ShowTree,
        ShowFormatted,
        ShowRaw,
        FoldAll,
        UnfoldAll,
        PreviousPart,
        NextPart,
        GoToError,
        ShowNodeRaw,
        TreeUp,
        TreeDown,
        TreeLeft,
        TreeRight
    ]
);

/// Key context of code views and code editors; ⌥Z / Alt+Z binds only here.
pub(crate) const KEY_CONTEXT: &str = "ReaderCode";

/// The tree-sitter grammar for a source file, from its name or extension.
/// `text` means a known code file without a compiled grammar. Content is
/// never sniffed: a wrong guess would colour prose as code.
pub(crate) fn language(rel: &str) -> Option<&'static str> {
    let path = Path::new(rel);
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "makefile" | "gnumakefile" => return Some("make"),
        "cmakelists.txt" => return Some("cmake"),
        "dockerfile" | "containerfile" | "justfile" | ".env" => return Some("text"),
        _ => {}
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "py" | "pyw" | "pyi" => "python",
        "rs" => "rust",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "sh" | "bash" | "zsh" => "bash",
        "json" | "jsonc" | "okilog" | "har" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" | "xsd" | "xsl" => "xml",
        "txt" | "csv" | "tsv" => "text",
        "sql" => "sql",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "scala" => "scala",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "proto" => "proto",
        "graphql" | "gql" => "graphql",
        "diff" | "patch" => "diff",
        "cmake" => "cmake",
        "mk" => "make",
        "ini" | "cfg" | "conf" | "env" | "fish" | "ps1" | "bat" | "dockerfile" => "text",
        _ => return None,
    })
}

pub(crate) fn eligible(rel: &str) -> bool {
    language(rel).is_some()
}

/// Every previewable code/data format edits through the source editor; XML
/// joined YAML and TOML in #2887. HAR captures stay read-only (#2836).
pub(crate) fn editable_format(rel: &str) -> bool {
    eligible(rel) && !crate::reader_har::eligible(rel)
}

/// The editor saves UTF-8. An XML file whose declaration names another
/// encoding (ISO-8859-1, Windows-1251, UTF-16, …) would get UTF-8 bytes under
/// that declaration, so it stays read-only with this reason (#2887). No
/// declaration, or `UTF-8`/`UTF8` in any case, is UTF-8 and editable.
pub(crate) fn xml_encoding_refusal(rel: &str, text: &str) -> Option<String> {
    if language(rel) != Some("xml") {
        return None;
    }
    let encoding = xml_declared_encoding(text)?;
    if encoding.eq_ignore_ascii_case("utf-8") || encoding.eq_ignore_ascii_case("utf8") {
        return None;
    }
    let shown: String = encoding
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    Some(format!(
        "Editing XML in {shown} isn't supported; the file stays as is"
    ))
}

/// The `encoding` pseudo-attribute of a leading `<?xml …?>` declaration.
fn xml_declared_encoding(text: &str) -> Option<&str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text.strip_prefix("<?xml")?;
    // `<?xml-stylesheet …?>` is a processing instruction, not the declaration.
    if !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
        return None;
    }
    let mut rest = &rest[..rest.find("?>")?];
    loop {
        rest = rest.trim_start();
        let name_end = rest.find(|c: char| c == '=' || c.is_ascii_whitespace())?;
        let name = &rest[..name_end];
        rest = rest[name_end..]
            .trim_start()
            .strip_prefix('=')?
            .trim_start();
        let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
        rest = &rest[1..];
        let value_end = rest.find(quote)?;
        if name == "encoding" {
            return Some(rest[..value_end].trim());
        }
        rest = &rest[value_end + 1..];
    }
}

enum State {
    Loading,
    Content(Entity<EditorState>),
    Message(String),
}

pub(crate) struct CodePreview {
    root: PathBuf,
    rel: String,
    path: PathBuf,
    raw_requested: bool,
    _table_subscription: Option<Subscription>,
    table_failure: Option<String>,
    opened_at: Instant,
    state: State,
    source_bytes: Option<usize>,
    storage_reason: Option<String>,
    /// A non-UTF-8 XML declaration keeps the file read-only (#2887).
    encoding_reason: Option<String>,
    /// A `.json` file's raw text and its display-only pretty form (#1131).
    json: Option<JsonText>,
    table: Option<Entity<reader_delimited::TablePreview>>,
    view: View,
    own_header: bool,
    raw_document: Option<Arc<Document>>,
    scrubber: Entity<gpui_component::slider::SliderState>,
    requested_page: Option<usize>,
    page_scheduled: bool,
    document: Option<Arc<Document>>,
    page: usize,
    structure: structure::Structure,
    paged: bool,
    document_revision: u64,
    applied_page: Option<(u64, usize)>,
    read_active: bool,
    restored_presentation: Option<PresentationState>,
    automatic_view: bool,
    provisional_interacted: std::rc::Rc<std::cell::Cell<bool>>,
    provisional_subscription: Option<Subscription>,
    editor_find: Option<Entity<reader_find_field::editor::EditorTarget>>,
    /// Contents jumps so far (#2814 9b): a repeated jump to the same caret
    /// is still a caret move.
    contents_jumps: u64,
    case_sensitive: std::rc::Rc<std::cell::Cell<bool>>,
    match_rows: std::collections::HashMap<usize, std::ops::Range<usize>>,
    match_pattern: Option<regex::Regex>,
    scrollbar_marks: Vec<usize>,
    node_copy_cache: std::cell::RefCell<Option<(NodeCopyKey, Option<std::ops::Range<usize>>)>>,
    #[cfg(test)]
    preparation_gate: Option<futures::channel::oneshot::Receiver<()>>,
    #[cfg(test)]
    format_gate: Option<futures::channel::oneshot::Receiver<()>>,
    #[cfg(test)]
    first_content_ms: Option<u128>,
    find: Entity<InputState>,
    matches: Vec<std::ops::Range<usize>>,
    match_index: usize,
    search_revision: u64,
    search_key: Option<(u64, String, bool)>,
    searching: bool,
    search_error: Option<SharedString>,
    search_task: Option<Task<()>>,
    search_cancel: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    search_scans: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    search_scans_started: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    search_installs: usize,
    focus: FocusHandle,
    _find_subscription: Subscription,
}

/// The native probe hosts the preview in the same bounded flex layout as Reader.
#[cfg(any(test, feature = "data-view-native-harness"))]
struct Surface(Entity<CodePreview>);
#[cfg(any(test, feature = "data-view-native-harness"))]
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().child(self.0.clone())
    }
}

/// Above this a JSON file opens raw; Pretty formats it on a worker on request.
const PRETTY_BYTES: usize = 8 * 1024 * 1024;

struct JsonText {
    raw: Arc<Document>,
    /// None until formatted: large files wait for Pretty.
    pretty: Option<Result<Arc<Document>, okilum_core::json_view::Error>>,
    formatting: bool,
    requested: View,
}

/// `.json` only: JSONC allows comments and is never reformatted.
fn is_json(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json") || ext.eq_ignore_ascii_case("har"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct NodeCopyKey {
    revision: u64,
    page: usize,
    cursor: usize,
}

enum Loaded {
    Text(String),
    TooLarge(u64),
    Unsupported,
}

/// Identifies a failure of the *document location*, not private recovery state.
#[derive(Debug)]
pub(crate) struct EditLocationError(std::io::Error);
impl std::fmt::Display for EditLocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "This folder cannot be written: {}", self.0)
    }
}
impl std::error::Error for EditLocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// An explicit Edit attempt checks the same directory needed by atomic Save.
/// Reads never probe storage. The empty sibling is removed immediately.
pub(crate) fn check_edit_location(path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("No parent directory"))?;
    match tempfile::Builder::new()
        .prefix(".okilum-edit-check-")
        .tempfile_in(parent)
    {
        Ok(probe) => {
            probe.close()?;
            Ok(())
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            Err(EditLocationError(error).into())
        }
        Err(error) => Err(error.into()),
    }
}

impl CodePreview {
    #[cfg(test)]
    pub(crate) fn source_bytes_for_layout_test(&mut self, bytes: usize) {
        self.source_bytes = Some(bytes);
    }

    #[cfg(test)]
    pub(crate) fn active_view(&self) -> View {
        self.view
    }

    #[cfg(test)]
    pub(crate) fn table_view(&self) -> Option<Entity<reader_delimited::TablePreview>> {
        self.table.clone()
    }

    #[cfg(feature = "data-view-native-harness")]
    pub(crate) fn native_table(&self) -> Option<Entity<reader_delimited::TablePreview>> {
        self.table.clone()
    }

    #[cfg(feature = "data-view-native-harness")]
    pub(crate) fn native_source_ready(&self) -> bool {
        self.document.is_some()
            || self.view == View::Table
                && self.source_bytes.is_some()
                && self.table_failure.is_none()
    }

    pub(crate) fn preserve_zoom_anchor(
        &mut self,
        pointer: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if let State::Content(input) = &self.state {
            input.update(cx, |state, _| state.preserve_zoom_anchor(pointer));
        }
    }

    pub(crate) fn new(
        root: PathBuf,
        rel: String,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let started = Instant::now();
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        let find_subscription =
            cx.subscribe_in(&find, window, |this, _, event, window, cx| match event {
                InputEvent::Change => {
                    this.provisional_interacted.set(true);
                    this.search(window, cx);
                }
                InputEvent::PressEnter { shift, .. } if this.whole_find() => {
                    this.step_match(if *shift { -1 } else { 1 }, window, cx)
                }
                _ => {}
            });
        let scrubber = cx.new(|_| {
            gpui_component::slider::SliderState::new()
                .min(0.)
                .max(1.)
                .step(0.001)
        });
        cx.subscribe_in(
            &scrubber,
            window,
            |this, _, event: &gpui_component::slider::SliderEvent, window, cx| {
                let (gpui_component::slider::SliderEvent::Change(value)
                | gpui_component::slider::SliderEvent::Release(value)) = event;
                let page = this.document.as_ref().map_or(0, |doc| {
                    view::fraction_to_part(value.start(), doc.pages.len())
                });
                this.request_page(page, window, cx);
            },
        )
        .detach();
        let delimited = reader_delimited::eligible(&rel);
        let initial = if delimited {
            reader_ui_state::code_view(
                if rel.to_ascii_lowercase().ends_with(".tsv") {
                    "tsv"
                } else {
                    "csv"
                },
                cx,
            )
            .filter(|v| matches!(v, View::Raw | View::Table))
            .unwrap_or(View::Table)
        } else {
            View::Raw
        };
        let mut preview = Self {
            root,
            rel,
            path: path.clone(),
            raw_requested: false,
            _table_subscription: None,
            table_failure: None,
            opened_at: started,
            state: State::Loading,
            source_bytes: None,
            storage_reason: None,
            encoding_reason: None,
            json: None,
            table: None,
            view: initial,
            own_header: true,
            raw_document: None,
            scrubber,
            requested_page: None,
            page_scheduled: false,
            document: None,
            page: 0,
            structure: structure::Structure::default(),
            paged: false,
            document_revision: 0,
            applied_page: None,
            read_active: true,
            restored_presentation: None,
            automatic_view: true,
            provisional_interacted: Default::default(),
            provisional_subscription: None,
            editor_find: None,
            contents_jumps: 0,
            case_sensitive: std::rc::Rc::new(std::cell::Cell::new(
                reader_ui_state::find_case_sensitive(cx),
            )),
            match_rows: Default::default(),
            match_pattern: None,
            scrollbar_marks: Vec::new(),
            node_copy_cache: Default::default(),
            #[cfg(test)]
            preparation_gate: None,
            #[cfg(test)]
            format_gate: None,
            #[cfg(test)]
            first_content_ms: None,
            find,
            matches: Vec::new(),
            match_index: 0,
            search_revision: 0,
            search_key: None,
            searching: false,
            search_error: None,
            search_task: None,
            search_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            search_scans: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            search_scans_started: Default::default(),
            #[cfg(test)]
            search_installs: 0,
            focus: cx.focus_handle(),
            _find_subscription: find_subscription,
        };
        if delimited {
            let table =
                cx.new(|cx| reader_delimited::TablePreview::new(path, window, cx).without_header());
            preview.attach_table(table, window, cx);
            preview.automatic_view = false;
            if initial == View::Raw {
                preview.load_text(window, cx);
            }
        } else {
            preview.load_text(window, cx);
        }
        preview
    }

    fn show_page(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = &self.document else {
            return;
        };
        let page = page.min(doc.pages.len() - 1);
        if self.applied_page == Some((self.document_revision, page)) {
            return;
        }
        self.page = page;
        self.applied_page = Some((self.document_revision, page));
        self.scrubber.update(cx, |slider, cx| {
            slider.set_value(
                self.page as f32 / (doc.pages.len().saturating_sub(1).max(1)) as f32,
                window,
                cx,
            )
        });
        let text = doc.text[doc.pages[self.page].clone()].to_owned();
        if let State::Content(input) = &self.state {
            input.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.prepare_highlighting(window, cx);
            });
        }
        if let Some(input) = self.input() {
            self.apply_diagnostics(&input, cx);
        }
        cx.notify();
    }

    fn copy_selection(&self, cx: &mut App) {
        if matches!(self.view, View::Tree | View::Table) {
            return;
        }
        if let Some(input) = self.input() {
            let input = input.read(cx);
            let range = input.selected_range();
            if !range.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    input.text().slice(range).to_string(),
                ));
            }
        }
    }

    fn node_copy_range(&self, cx: &App) -> Option<std::ops::Range<usize>> {
        let (Some(doc), State::Content(input)) = (&self.document, &self.state) else {
            return None;
        };
        if self.view == View::Table {
            return None;
        }
        if self.view == View::Tree {
            self.structure
                .selected
                .and_then(|i| doc.structure.get(i))
                .map(|row| row.range.clone())
        } else {
            let offset = doc.pages[self.page].start + input.read(cx).cursor();
            let key = NodeCopyKey {
                revision: self.document_revision,
                page: self.page,
                cursor: offset,
            };
            if let Some((cached_key, range)) = &*self.node_copy_cache.borrow() {
                if *cached_key == key {
                    return range.clone();
                }
            }
            let range = doc.node_range(offset);
            *self.node_copy_cache.borrow_mut() = Some((key, range.clone()));
            range
        }
    }
    fn copy_node(&self, window: &mut Window, cx: &mut App) {
        if let Some(range) = self.node_copy_range(cx) {
            copy_text_node(&self.document.as_ref().unwrap().text[range], window, cx);
        }
    }

    /// Route Find to the active view’s shared field.
    pub(crate) fn open_paged_find(&self, window: &mut Window, cx: &mut App) -> bool {
        if self.view == View::Table {
            return self
                .table
                .as_ref()
                .is_some_and(|table| table.update(cx, |table, cx| table.focus_filter(window, cx)));
        }
        if !matches!(self.state, State::Content(_)) {
            return false;
        }
        self.provisional_interacted.set(true);
        if self.view == View::Table {
            return self
                .table
                .as_ref()
                .filter(|table| table.read(cx).ready())
                .is_some_and(|table| table.update(cx, |t, cx| t.focus_filter(window, cx)));
        }
        self.find.update(cx, |input, cx| input.focus(window, cx));
        true
    }

    /// UI editing admission is independent of the 64 MiB source-tool budget.
    pub(crate) fn editable(&self) -> bool {
        (matches!(self.state, State::Content(_))
            || reader_delimited::eligible(&self.rel) && self.source_bytes.is_some())
            && self.storage_reason.is_none()
            && self.encoding_reason.is_none()
            && editable_format(&self.rel)
            // CSV/TSV edit through the existing source editor without the code
            // viewer's 4 MiB UI cap, exactly as before they shared this owner.
            && self
                .source_bytes
                .is_some_and(|bytes| reader_delimited::eligible(&self.rel) || bytes <= EDIT_BYTES)
    }

    /// The file is still being read; whether it is editable is not known yet.
    pub(crate) fn loading(&self) -> bool {
        matches!(self.state, State::Loading)
            && (!reader_delimited::eligible(&self.rel) || self.source_bytes.is_none())
    }

    /// Shared immutable whole-file parse for Contents (#2795). Node indices,
    /// parent links, byte ranges and labels belong to this exact snapshot;
    /// retaining the Arc never reparses or copies the source/tree.
    pub(crate) fn document_snapshot(&self) -> Option<Arc<Document>> {
        self.document.clone()
    }

    /// The right-panel outline must never parse a source page as a document.
    pub(crate) fn outline_input(&self) -> Option<Entity<EditorState>> {
        self.document_snapshot()
            .filter(|doc| doc.pages.len() == 1)?;
        self.input()
    }

    pub(crate) fn input(&self) -> Option<Entity<EditorState>> {
        match &self.state {
            State::Content(input) => Some(input.clone()),
            _ => None,
        }
    }
}

/// The read-only code view: line numbers and folding on, wrap from the global
/// preference. `toggle_source` configures the writable editor the same way.
fn editor(
    language: &'static str,
    text: &str,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> EditorState {
    let mut input = EditorState::new(window, cx)
        .language(language)
        .line_number(true)
        .folding(true)
        .searchable(true)
        .replaceable(false)
        .soft_wrap(text.len() < EDIT_BYTES && reader_ui_state::code_soft_wrap(cx))
        .wrapping_indent(WrappingIndent::None);
    input.set_value(text.to_owned(), window, cx);
    input.ensure_highlighter_factory(gpui_component::highlighter::input_highlighter_factory());
    input.prepare_highlighting(window, cx);
    input.set_readonly(true, cx);
    input
}

/// The context menu of text with no language server: Copy / Select All, plus
/// Cut / Paste when editable (#1010). The builder runs inside the input's own
/// update, so it must not read the input; Copy without a selection does nothing.
pub(crate) fn text_menu(
    menu: gpui_component::native_menu::NativeMenu,
    editable: bool,
) -> gpui_component::native_menu::NativeMenu {
    use gpui_component::input::{Copy, Cut, Paste, SelectAll};
    let menu = if editable {
        menu.menu("Cut", Box::new(Cut))
            .menu("Copy", Box::new(Copy))
            .menu("Paste", Box::new(Paste))
    } else {
        menu.menu("Copy", Box::new(Copy))
    };
    menu.separator().menu("Select All", Box::new(SelectAll))
}

/// Code text size: the editor's 13 px scaled with the reading text size.
pub(crate) fn text_size(cx: &App) -> Pixels {
    px(13. * reader_ui_state::font_size(cx) / BODY_FONT_SIZE)
}

/// Opt-in measurement for #998: time from selection to highlighted text.
fn timing(input: &Entity<EditorState>, started: Instant, cx: &mut Context<CodePreview>) {
    if std::env::var("OKILUM_CODE_TIMING").as_deref() != Ok("1") {
        return;
    }
    diagnostic_error!(
        "code_preview text_ready_ms={}",
        started.elapsed().as_millis()
    );
    let mut reported = false;
    cx.observe(input, move |_, input, cx| {
        if !reported && !input.read(cx).highlighting_pending() {
            reported = true;
            diagnostic_error!(
                "code_preview highlighted_ms={}",
                started.elapsed().as_millis()
            );
        }
    })
    .detach();
}

impl Render for CodePreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "data-view-native-harness")]
        native_trace("preview.render.begin");
        #[cfg(feature = "data-view-native-harness")]
        let _work = toolbar_native::trace::Work::start(12);
        #[cfg(feature = "data-view-native-harness")]
        native_trace("chrome.begin");
        let header = self.own_header.then(|| {
            let chrome = self.compact_chrome(cx.entity(), cx);
            canvas(
                move |bounds, window, cx| {
                    let mut header = reader_viewer_chrome::render_compact_header(
                        &chrome,
                        bounds.size.width.as_f32(),
                        window,
                        cx,
                    );
                    header.layout_as_root(bounds.size.into(), window, cx);
                    header.prepaint_at(bounds.origin, window, cx);
                    header
                },
                |_, mut header, window, cx| header.paint(window, cx),
            )
            .w_full()
            .h(px(48.))
            .flex_none()
            .into_any_element()
        });
        #[cfg(feature = "data-view-native-harness")]
        native_trace("chrome.end");
        let content = self.render_content(cx);
        let footer = self.bottom_line(cx);
        let view = v_flex()
            .id("reader-code-file")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .when(self.provisional_subscription.is_some(), |view| {
                let clicked = self.provisional_interacted.clone();
                let keyed = clicked.clone();
                view.capture_any_mouse_down(move |_, _, _| clicked.set(true))
                    .capture_key_down(move |_, _, _| keyed.set(true))
            })
            .debug_selector(|| "reader-code-file".into())
            .on_action(cx.listener(|this, _: &TreeUp, _, cx| this.tree_move(-1, cx)))
            .on_action(cx.listener(|this, _: &TreeDown, _, cx| this.tree_move(1, cx)))
            .on_action(cx.listener(|this, _: &TreeLeft, _, cx| this.tree_branch(false, cx)))
            .on_action(cx.listener(|this, _: &TreeRight, _, cx| this.tree_branch(true, cx)))
            .on_action(
                cx.listener(|this, _: &ShowNodeRaw, window, cx| this.show_selected_raw(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CopyNode, window, cx| this.copy_node(window, cx)))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy_selection(cx)))
            .on_action(cx.listener(|this, _: &GoToError, window, cx| this.go_to_error(window, cx)))
            .on_action(cx.listener(|this, _: &FindInNote, window, cx| this.open_find(window, cx)))
            .on_action(
                cx.listener(|this, _: &ShowTable, window, cx| {
                    this.set_view(View::Table, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ShowTree, window, cx| this.set_view(View::Tree, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowFormatted, window, cx| {
                this.set_view(View::Formatted, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ShowRaw, window, cx| this.set_view(View::Raw, window, cx)),
            )
            .on_action(cx.listener(|this, _: &FoldAll, _, cx| this.fold_all(true, cx)))
            .on_action(cx.listener(|this, _: &UnfoldAll, _, cx| this.fold_all(false, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousPart, window, cx| this.step_part(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NextPart, window, cx| this.step_part(true, window, cx)),
            )
            .w_full()
            .flex_1()
            .min_h_0()
            .children(header)
            .child(content)
            .child(footer);
        #[cfg(feature = "data-view-native-harness")]
        native_trace("preview.render.end");
        let view = view.into_any_element();
        #[cfg(feature = "data-view-native-harness")]
        let view = toolbar_native::trace::Trace::new(view, 0).into_any_element();
        view
    }
}

impl CodePreview {
    #[inline(never)]
    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.view == View::Table {
            if let Some(table) = &self.table {
                return table.clone().into_any_element();
            }
        }
        let view = v_flex()
            .w_full()
            .max_w(px(reader_ui_state::note_column_width(cx)))
            .mx_auto()
            .flex_1()
            .min_h_0()
            .px(px(READER_SIDE_PADDING));
        match &self.state {
            State::Loading => view.child(div().child("Opening file…")).into_any_element(),
            State::Message(message) => view.child(self.message(message, cx)).into_any_element(),
            State::Content(input) => view
                .child(if self.view == View::Tree {
                    self.structure_element(cx)
                } else {
                    Editor::new(input)
                        .readonly(true)
                        .context_menu(|menu, _, _| {
                            text_menu(menu, false)
                                .separator()
                                .menu("Copy node", Box::new(CopyNode))
                        })
                        .appearance(false)
                        .font_family(reader_fonts::code(cx))
                        .text_size(text_size(cx))
                        .size_full()
                        .into_any_element()
                })
                .into_any_element(),
        }
    }
}

impl Reader {
    #[inline(never)]
    pub(crate) fn code_action_listeners(&self, view: Div, cx: &mut Context<Self>) -> Div {
        if self.read_code_view().is_none() {
            return view;
        }
        view.on_action(cx.listener(|this, _: &ShowTable, window, cx| {
            this.change_code_view(View::Table, window, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowTree, window, cx| {
            this.change_code_view(View::Tree, window, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowFormatted, window, cx| {
            this.change_code_view(View::Formatted, window, cx)
        }))
        .on_action(
            cx.listener(|this, _: &ShowRaw, window, cx| {
                this.change_code_view(View::Raw, window, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &FoldAll, _, cx| this.fold_code_view(true, cx)))
        .on_action(cx.listener(|this, _: &UnfoldAll, _, cx| this.fold_code_view(false, cx)))
        .on_action(cx.listener(|this, _: &CopyNode, window, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.copy_node(window, cx));
            }
        }))
        .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.copy_selection(cx));
            }
        }))
        .on_action(cx.listener(|this, _: &GoToError, window, cx| {
            if this.editing.is_some() {
                this.file_editor_go_to_error(window, cx);
                return;
            }
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.go_to_error(window, cx));
            }
        }))
        .on_action(cx.listener(|this, _: &PreviousPart, window, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.step_part(false, window, cx));
            }
        }))
        .on_action(cx.listener(|this, _: &NextPart, window, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.step_part(true, window, cx));
            }
        }))
    }
    pub(super) fn resume_code_presentation(&self, cx: &mut App) {
        if let Some(code) = self
            .file_preview
            .as_ref()
            .and_then(|preview| preview.code.as_ref())
        {
            let resume_find = code.update(cx, |code, cx| {
                let suspended = !code.read_active;
                code.read_active = true;
                if suspended && code.view == View::Table {
                    if let Some(table) = &code.table {
                        table.update(cx, |table, cx| table.resume_work(cx));
                    }
                }
                if let Some(input) = code.input() {
                    code.apply_diagnostics(&input, cx);
                }
                if suspended && !code.whole_find() && code.view != View::Table {
                    if let Some(editor) = &code.editor_find {
                        editor.update(cx, |editor, cx| editor.resume(cx));
                    }
                }
                cx.notify();
                let query = code.find.read(cx).value();
                let current = code
                    .search_key
                    .as_ref()
                    .is_some_and(|(revision, old, case)| {
                        *revision == code.document_revision
                            && old.as_str() == query.as_ref()
                            && *case == code.case_sensitive.get()
                    });
                suspended && code.whole_find() && !query.is_empty() && !current
            });
            if resume_find {
                let code = code.clone();
                let window = self.reader_window;
                // Edit exits without a Window also use this path. Re-enter
                // the owning window only after its current lease is released.
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        code.update(cx, |code, cx| {
                            if code.read_active {
                                code.search(window, cx);
                            }
                        });
                    });
                });
            }
        }
    }
    fn read_code_view(&self) -> Option<Entity<CodePreview>> {
        if self.editing.is_some() {
            return None;
        }
        self.file_preview.as_ref()?.code.clone()
    }
    fn change_code_view(&self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(code) = self.read_code_view() {
            code.update(cx, |code, cx| code.set_view(view, window, cx));
        }
    }
    fn fold_code_view(&self, folded: bool, cx: &mut Context<Self>) {
        if let Some(code) = self.read_code_view() {
            code.update(cx, |code, cx| code.fold_all(folded, cx));
        }
    }
}

impl Reader {
    /// Native regression tests inspect the loaded read-only editor directly.
    #[cfg(test)]
    pub(crate) fn code_view_input(&self, cx: &App) -> Option<Entity<EditorState>> {
        self.file_preview.as_ref()?.code.as_ref()?.read(cx).input()
    }

    /// Code files open in the editor only within the size cap.
    pub(crate) fn code_file_editable(&self, cx: &App) -> bool {
        self.file_preview
            .as_ref()
            .and_then(|preview| preview.code.as_ref())
            .is_none_or(|code| code.read(cx).editable())
    }

    /// ⌥Z / Alt+Z (#998): remembered for every code file and window.
    pub(crate) fn toggle_code_soft_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wrap = !reader_ui_state::code_soft_wrap(cx);
        reader_ui_state::set_code_soft_wrap(wrap, cx);
        let Some(preview) = &self.file_preview else {
            cx.notify();
            return;
        };
        let shown = preview.code.as_ref().and_then(|code| code.read(cx).input());
        let edited = eligible(&preview.rel)
            .then(|| self.source_input())
            .flatten();
        for input in shown.into_iter().chain(edited) {
            input.update(cx, |input, cx| input.set_soft_wrap(wrap, window, cx));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml2887_only_a_non_utf8_declaration_refuses_editing() {
        let refused = |rel: &str, text: &str| xml_encoding_refusal(rel, text);
        for (text, encoding) in [
            (
                "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a/>",
                "ISO-8859-1",
            ),
            (
                "<?xml version='1.0' encoding='windows-1251'?>\n<a/>",
                "windows-1251",
            ),
            (
                "\u{feff}<?xml version=\"1.0\"\n  encoding = \"UTF-16\" ?><a/>",
                "UTF-16",
            ),
            (
                "<?xml version=\"1.0\" encoding=\"us-ascii\" standalone=\"yes\"?>",
                "us-ascii",
            ),
        ] {
            assert_eq!(
                refused("feed.xml", text).as_deref(),
                Some(
                    format!("Editing XML in {encoding} isn't supported; the file stays as is")
                        .as_str()
                ),
                "{text}"
            );
            assert!(refused("schema.xsd", text).is_some(), "XSD shares the gate");
            assert!(refused("notes.txt", text).is_none(), "only XML is gated");
        }
        for text in [
            "<a>no declaration</a>",
            "<?xml version=\"1.0\"?><a/>",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><a/>",
            "<?xml version=\"1.0\" encoding='utf-8'?><a/>",
            "<?xml version=\"1.0\" encoding=\"UTF8\"?><a/>",
            "\u{feff}<?xml version=\"1.0\" encoding=\"Utf-8\"?><a/>",
            "<?xml-stylesheet href=\"x.xsl\" encoding=\"latin1\"?><a/>",
            "<a><?xml version=\"1.0\" encoding=\"latin1\"?></a>",
        ] {
            assert_eq!(refused("feed.xml", text), None, "{text}");
        }
    }
    #[gpui::test]
    fn data_table_2735_json_defaults_to_table_and_keeps_source(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("rows.json");
        let source = r#"[{"name":"Alpha","n":10},{"name":"Two","n":2}]"#;
        std::fs::write(&path, source).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_delimited::bind_keys(cx);
        });
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                CodePreview::new(
                    fixture.path().into(),
                    "rows.json".into(),
                    path.clone(),
                    window,
                    cx,
                )
            });
            preview = Some(view.clone());
            Root::new(cx.new(|_| Surface(view)), window, cx)
        });
        visual.simulate_resize(size(px(800.), px(500.)));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("delimited-viewport").is_some(),
            "JSON array opens a table"
        );
        let preview = preview.unwrap();
        preview.update_in(visual, |p, window, cx| {
            assert!(
                p.open_paged_find(window, cx),
                "Find targets the visible JSON table"
            );
        });
        visual.simulate_input("Two");
        // Table Find debounces typing (60 ms) before the worker projection.
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(70));
        visual.run_until_parked();
        let first = visual.debug_bounds("delimited-cell-1-0").unwrap();
        visual.simulate_click(first.center(), Modifiers::default());
        visual.simulate_keystrokes("secondary-c");
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "Two",
                "Find filters the displayed table"
            )
        });
        preview.update_in(visual, |p, window, cx| {
            p.set_view(View::Formatted, window, cx)
        });
        visual.run_until_parked();
        preview.read_with(visual, |p, cx| {
            assert!(p.view != View::Table);
            assert!(p.input().is_some_and(|input| !input.read(cx).is_editable()));
        });
        assert!(visual.debug_bounds("code-view-menu").is_some());
        assert!(visual.debug_bounds("delimited-viewport").is_none());
        preview.update_in(visual, |p, window, cx| p.set_view(View::Table, window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("delimited-viewport").is_some());
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
    use ::core::prelude::v1::test;

    #[test]
    fn languages_follow_names_and_extensions_only() {
        for (rel, expected) in [
            ("work/lan-forward.py", Some("python")),
            ("src/main.rs", Some("rust")),
            ("cfg/app.JSON", Some("json")),
            ("capture/session.HAR", Some("json")),
            ("bin/run.sh", Some("bash")),
            ("ci.yml", Some("yaml")),
            ("Cargo.toml", Some("toml")),
            ("web/app.tsx", Some("tsx")),
            ("include/a.hpp", Some("cpp")),
            ("Makefile", Some("make")),
            ("build/CMakeLists.txt", Some("cmake")),
            ("Dockerfile", Some("text")),
            (".env", Some("text")),
            ("notes/Plan.md", None),
            ("data.csv", Some("text")),
            ("data.tsv", Some("text")),
            ("notes.txt", Some("text")),
            ("server.log", None),
            ("photo.png", None),
            ("README", None),
        ] {
            assert_eq!(language(rel), expected, "{rel}");
        }
    }

    #[test]
    fn every_grammar_name_is_compiled_in() {
        use gpui_component::highlighter::Language;
        for rel in [
            "a.py",
            "a.rs",
            "a.js",
            "a.ts",
            "a.tsx",
            "a.go",
            "a.sh",
            "a.json",
            "a.yaml",
            "a.toml",
            "a.sql",
            "a.c",
            "a.cpp",
            "a.cs",
            "a.swift",
            "a.kt",
            "a.java",
            "a.rb",
            "a.php",
            "a.lua",
            "a.html",
            "a.css",
            "a.scala",
            "a.zig",
            "a.ex",
            "a.proto",
            "a.graphql",
            "a.diff",
            "a.cmake",
            "a.mk",
        ] {
            let name = language(rel).unwrap();
            assert_ne!(Language::from_str(name), Language::Plain, "{rel} → {name}");
        }
    }
}

#[cfg(test)]
mod native_tests;

#[cfg(feature = "data-view-native-harness")]
fn native_trace(phase: &str) {
    if std::env::var_os("OKILUM_DATA_VIEW_NATIVE_FIXTURE").is_some() {
        diagnostic_error!("DATA_VIEW_2669_NATIVE_TRACE {phase}");
    }
}
