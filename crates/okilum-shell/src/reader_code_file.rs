//! Source-code files in the Reader (#998). tree-sitter highlights them in a
//! read-only editor, the engine Markdown code blocks already use; edit mode
//! opens the same file in a writable editor with the same language.
use super::*;
use gpui_component::button::ButtonGroup;
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState, WrappingIndent};
use std::time::Instant;

pub(crate) mod document;
#[cfg(feature = "data-view-native-harness")]
pub(crate) mod native_harness;
mod structure;
mod toolbar;
use document::Document;

/// Bounded text loading; explicit JSON editing shares the source-tool budget.
pub(crate) const MAX_BYTES: u64 = 64 * 1024 * 1024;
const EDIT_BYTES: usize = 4 * 1024 * 1024;

actions!(reader_code_file, [CopyNode]);

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
        "json" | "jsonc" | "okilog" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" | "xsd" | "xsl" => "xml",
        "txt" => "text",
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

/// Preview eligibility does not grant a new writable file format (#2669).
pub(crate) fn editable_format(rel: &str) -> bool {
    eligible(rel) && language(rel) != Some("xml")
}

enum State {
    Loading,
    Content(Entity<EditorState>),
    Message(&'static str),
}

pub(crate) struct CodePreview {
    root: PathBuf,
    rel: String,
    state: State,
    source_bytes: Option<usize>,
    /// A `.json` file's raw text and its display-only pretty form (#1131).
    json: Option<JsonText>,
    table: Option<Entity<reader_delimited::TablePreview>>,
    show_table: bool,
    document: Option<Arc<Document>>,
    page: usize,
    structure: structure::Structure,
    structure_view: bool,
    requested_tree: Option<bool>,
    paged: bool,
    document_revision: u64,
    find: Entity<InputState>,
    matches: Vec<std::ops::Range<usize>>,
    match_index: usize,
    search_revision: u64,
    searching: bool,
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

type Pretty = Result<String, okilum_core::json_view::Error>;

struct JsonText {
    raw: String,
    /// None until formatted: large files wait for Pretty.
    pretty: Option<Pretty>,
    showing_pretty: bool,
    formatting: bool,
    requested_pretty: bool,
}

/// `.json` only: JSONC allows comments and is never reformatted.
fn is_json(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

enum Loaded {
    Text(String, Option<Pretty>),
    TooLarge,
    Unsupported,
}

impl CodePreview {
    #[cfg(feature = "data-view-native-harness")]
    pub(crate) fn native_table(&self) -> Option<Entity<reader_delimited::TablePreview>> {
        self.table.clone()
    }

    #[cfg(feature = "data-view-native-harness")]
    pub(crate) fn native_source_ready(&self) -> bool {
        self.document.is_some()
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
        let json = is_json(&rel);
        let table_path = path.clone();
        let task = cx.background_executor().spawn(async move {
            if std::fs::metadata(&path)?.len() > MAX_BYTES {
                return anyhow::Ok(Loaded::TooLarge);
            }
            Ok(
                match String::from_utf8({
                    use std::io::Read as _;
                    let mut bytes = Vec::new();
                    std::fs::File::open(&path)?
                        .take(MAX_BYTES + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes.len() as u64 > MAX_BYTES {
                        return Ok(Loaded::TooLarge);
                    }
                    bytes
                }) {
                    Ok(text) => {
                        // Formatting runs here, off the UI thread, within a budget.
                        let pretty = (json && text.len() <= PRETTY_BYTES)
                            .then(|| okilum_core::json_view::pretty(&text));
                        Loaded::Text(text, pretty)
                    }
                    Err(_) => Loaded::Unsupported,
                },
            )
        });
        crate::source_presentation::code_languages::install();
        let language = language(&rel).unwrap_or("text");
        // Each selection owns its own entity; a late read cannot reach a newer one.
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.state = match result {
                    Ok(Loaded::Text(text, pretty)) => {
                        this.source_bytes = Some(text.len());
                        if json
                            && text
                                .trim_start_matches('\u{feff}')
                                .trim_start()
                                .starts_with('[')
                        {
                            let table = cx.new(|cx| {
                                reader_delimited::TablePreview::new(table_path.clone(), cx)
                            });
                            cx.observe(&table, |_, _, cx| cx.notify()).detach();
                            this.table = Some(table);
                        }
                        let shown = if json {
                            let showing_pretty =
                                !reader_ui_state::json_raw(cx) && matches!(pretty, Some(Ok(_)));
                            let json = this.json.insert(JsonText {
                                raw: text,
                                pretty,
                                showing_pretty,
                                formatting: false,
                                requested_pretty: showing_pretty,
                            });
                            json.shown().to_owned()
                        } else {
                            text
                        };
                        let range = document::first_page(&shown);
                        let apply_started = Instant::now();
                        let input = cx.new(|cx| editor(language, &shown[range], window, cx));
                        if shown.len() > EDIT_BYTES {
                            diagnostic_error!(
                                "DATA_VIEW_2669 bytes={} editor_apply_ms={}",
                                shown.len(),
                                apply_started.elapsed().as_millis()
                            );
                        }
                        this.prepare_document(shown, window, cx);
                        timing(&input, started, cx);
                        State::Content(input)
                    }
                    Ok(Loaded::TooLarge) => {
                        State::Message("This file exceeds the 64 MiB preview limit.")
                    }
                    Ok(Loaded::Unsupported) => {
                        State::Message("This file can’t be previewed as text.")
                    }
                    Err(_) => State::Message("This file couldn’t be read."),
                };
                cx.notify();
            });
        })
        .detach();
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find in whole file…"));
        let find_subscription = cx.subscribe_in(&find, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.search(window, cx);
            }
        });
        Self {
            root,
            rel,
            state: State::Loading,
            source_bytes: None,
            json: None,
            table: None,
            show_table: true,
            document: None,
            page: 0,
            structure: structure::Structure::default(),
            structure_view: false,
            requested_tree: None,
            paged: false,
            document_revision: 0,
            find,
            matches: Vec::new(),
            match_index: 0,
            search_revision: 0,
            searching: false,
            focus: cx.focus_handle(),
            _find_subscription: find_subscription,
        }
    }

    /// Pretty or Raw for this JSON file; Raw is remembered for the next one.
    fn show_json(&mut self, pretty: bool, window: &mut Window, cx: &mut Context<Self>) {
        reader_ui_state::set_json_raw(!pretty, cx);
        let Some(json) = &mut self.json else {
            return;
        };
        json.requested_pretty = pretty;
        if pretty && json.pretty.is_none() {
            // A large file: format on a worker, then show it.
            if json.formatting {
                return;
            }
            json.formatting = true;
            let raw = json.raw.clone();
            let task = cx
                .background_executor()
                .spawn(async move { okilum_core::json_view::pretty(&raw) });
            cx.spawn_in(window, async move |this, cx| {
                let pretty = task.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if let Some(json) = &mut this.json {
                        json.formatting = false;
                        json.pretty = Some(pretty);
                    }
                    // Raw chosen while formatting wins: keep the result, don't show it.
                    let pretty = this.json.as_ref().is_some_and(|json| json.requested_pretty);
                    this.show_json(pretty, window, cx);
                });
            })
            .detach();
            cx.notify();
            return;
        }
        let showing_pretty = pretty && matches!(json.pretty, Some(Ok(_)));
        if json.showing_pretty == showing_pretty {
            cx.notify();
            return;
        }
        json.showing_pretty = showing_pretty;
        let shown = json.shown().to_owned();
        let range = document::first_page(&shown);
        if let State::Content(input) = &self.state {
            input.update(cx, |input, cx| {
                input.set_value(shown[range].to_owned(), window, cx);
                input.prepare_highlighting(window, cx);
            });
        }
        self.prepare_document(shown, window, cx);
        cx.notify();
    }

    fn prepare_document(&mut self, shown: String, window: &mut Window, cx: &mut Context<Self>) {
        self.page = 0;
        self.paged = document::first_page(&shown).end < shown.len();
        self.document = None;
        self.structure_view = false;
        self.matches.clear();
        self.document_revision += 1;
        self.search_revision += 1;
        self.searching = false;
        let revision = self.document_revision;
        let language = language(&self.rel).unwrap_or("text");
        let json = self.json.is_some();
        let task = cx
            .background_executor()
            .spawn(async move { Arc::new(Document::for_file(shown, language, json)) });
        cx.spawn_in(window, async move |this, cx| {
            let document = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.document_revision != revision {
                    return;
                }
                this.structure.reset(&document);
                this.structure_view = this.requested_tree.unwrap_or(document.pages.len() > 1)
                    && !document.structure.is_empty();
                this.document = Some(document);
                this.search(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn show_page(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = &self.document else {
            return;
        };
        self.structure_view = false;
        self.requested_tree = Some(false);
        self.page = page.min(doc.pages.len() - 1);
        let text = doc.text[doc.pages[self.page].clone()].to_owned();
        if let State::Content(input) = &self.state {
            input.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.prepare_highlighting(window, cx);
            });
        }
        cx.notify();
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_revision += 1;
        let revision = self.search_revision;
        let Some(doc) = self
            .document
            .clone()
            .filter(|doc| doc.json_tree || doc.pages.len() > 1)
        else {
            return;
        };
        let query = self.find.read(cx).value().to_string();
        self.matches.clear();
        self.structure.match_node = None;
        self.searching = !query.is_empty();
        let task = cx
            .background_executor()
            .spawn(async move { doc.find(&query) });
        cx.spawn_in(window, async move |this, cx| {
            let matches = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.search_revision != revision {
                    return;
                }
                this.searching = false;
                this.matches = matches;
                this.match_index = 0;
                this.show_match(window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn show_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.matches.get(self.match_index).cloned() else {
            return;
        };
        let Some(doc) = &self.document else {
            return;
        };
        if self.structure_view {
            self.structure.reveal(&range, doc);
            cx.notify();
            return;
        }
        let page = doc.page_at(range.start);
        self.show_page(page, window, cx);
        let doc = self.document.as_ref().unwrap();
        let page_range = &doc.pages[self.page];
        if let State::Content(input) = &self.state {
            input.update(cx, |state, cx| {
                state.set_selected_range(
                    range.start - page_range.start
                        ..range.end.min(page_range.end) - page_range.start,
                    cx,
                );
            });
        }
    }

    fn copy_node(&self, cx: &mut App) {
        let (Some(doc), State::Content(input)) = (&self.document, &self.state) else {
            return;
        };
        let offset = doc.pages[self.page].start + input.read(cx).cursor();
        let range = if self.structure_view {
            self.structure
                .selected
                .and_then(|i| doc.structure.get(i))
                .map(|row| row.range.clone())
        } else {
            doc.node_range(offset)
        };
        if let Some(range) = range {
            cx.write_to_clipboard(ClipboardItem::new_string(doc.text[range].to_owned()));
        }
    }

    /// Find targets the visible table or the whole-file paged presentation.
    pub(crate) fn open_paged_find(&self, window: &mut Window, cx: &mut App) -> bool {
        if self.show_table {
            if let Some(table) = self.table.as_ref().filter(|table| table.read(cx).ready()) {
                return table.update(cx, |table, cx| table.focus_filter(window, cx));
            }
        }
        if self.structure_view || self.paged {
            self.find.update(cx, |input, cx| input.focus(window, cx));
            true
        } else {
            false
        }
    }

    /// The Pretty / Raw switch and why a file stays raw.
    fn json_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let json = self.json.as_ref()?;
        let note = match &json.pretty {
            _ if json.formatting => Some("Formatting…".to_owned()),
            Some(Err(okilum_core::json_view::Error::Invalid(invalid))) => Some(format!(
                "Not valid JSON — showing raw (line {}, column {})",
                invalid.line, invalid.column
            )),
            Some(Err(okilum_core::json_view::Error::Limit { reason, limit })) => Some(format!(
                "Pretty unavailable: {reason} limit {limit}; showing raw"
            )),
            None => Some("Large file — showing raw".to_owned()),
            Some(Ok(_)) => None,
        };
        let entity = cx.entity().downgrade();
        let raw_entity = entity.clone();
        Some(
            h_flex()
                .gap_1()
                .pb_2()
                .items_center()
                // The dashboard filter chips' style, so the shown mode reads as selected.
                .child(
                    ButtonGroup::new("json-mode")
                        .child(
                            Button::new("json-pretty")
                                .small()
                                .ghost()
                                .label(if json.pretty.is_none() && !json.showing_pretty {
                                    "Pretty-print anyway"
                                } else {
                                    "Pretty"
                                })
                                .selected(json.showing_pretty)
                                // Invalid is a diagnostic, not an action gate.
                                // The strict formatter refuses safely and keeps Raw.
                                .disabled(json.formatting)
                                .debug_selector(|| "json-pretty".into())
                                .on_click(move |_, window, cx| {
                                    let _ = entity
                                        .update(cx, |this, cx| this.show_json(true, window, cx));
                                }),
                        )
                        .child(
                            Button::new("json-raw")
                                .small()
                                .ghost()
                                .label("Raw")
                                .selected(!json.showing_pretty)
                                .debug_selector(|| "json-raw".into())
                                .on_click(move |_, window, cx| {
                                    let _ = raw_entity
                                        .update(cx, |this, cx| this.show_json(false, window, cx));
                                }),
                        ),
                )
                .children(note.map(|note| {
                    div()
                        .pl_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .debug_selector(|| "json-note".into())
                        .child(note)
                }))
                .into_any_element(),
        )
    }

    /// JSON tools admit the same input sizes as the bounded viewer. Keep the
    /// existing TXT editor and other code-format admission rules independent.
    pub(crate) fn editable(&self) -> bool {
        if Path::new(&self.rel)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        {
            // TXT already had an independent FileEditor before this preview.
            // Its own canonical read/recovery checks remain authoritative.
            return true;
        }
        let edit_bytes = if Path::new(&self.rel)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            okilum_core::code_operations::MAX_BYTES
        } else {
            EDIT_BYTES
        };
        matches!(self.state, State::Content(_))
            && editable_format(&self.rel)
            && self.source_bytes.is_some_and(|bytes| bytes <= edit_bytes)
    }

    /// The file is still being read; whether it is editable is not known yet.
    pub(crate) fn loading(&self) -> bool {
        matches!(self.state, State::Loading)
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

impl JsonText {
    fn shown(&self) -> &str {
        match &self.pretty {
            Some(Ok(pretty)) if self.showing_pretty => pretty,
            _ => &self.raw,
        }
    }
}

impl Render for CodePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "data-view-native-harness")]
        native_trace("preview.render.begin");
        let table_ready = self
            .table
            .as_ref()
            .is_some_and(|table| table.read(cx).ready());
        let modes = table_ready.then(|| {
            h_flex()
                .px_4()
                .py_1()
                .gap_2()
                .children([true, false].map(|table| {
                    Button::new(if table { "json-table" } else { "json-source" })
                        .debug_selector(move || {
                            if table {
                                "json-table".into()
                            } else {
                                "json-source".into()
                            }
                        })
                        .small()
                        .ghost()
                        .label(if table { "Table" } else { "Source" })
                        .selected(self.show_table == table)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.show_table = table;
                            cx.notify();
                        }))
                }))
                .into_any_element()
        });
        if table_ready && self.show_table {
            return v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .children(modes)
                .child(self.table.as_ref().unwrap().clone())
                .into_any_element();
        }
        let view = v_flex()
            .children(modes)
            .id("reader-code-file")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CopyNode, _, cx| this.copy_node(cx)))
            .on_action(cx.listener(|this, _: &FindInNote, window, cx| {
                if !this.open_paged_find(window, cx) {
                    if let Some(input) = this.input() {
                        input.update(cx, |state, cx| {
                            state.open_search(false, cx);
                            state.focus(window, cx);
                        });
                    }
                }
            }))
            .debug_selector(|| "reader-code-file".into())
            .w_full()
            .max_w(px(reader_ui_state::note_column_width(cx)))
            .mx_auto()
            .flex_1()
            .min_h_0()
            .px(px(READER_SIDE_PADDING))
            .pb_6();
        let result = match &self.state {
            State::Loading => view.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Opening file…"),
            ),
            State::Message(message) => {
                let root = self.root.clone();
                let rel = self.rel.clone();
                view.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(*message),
                        )
                        .child(
                            reader_icon_button(
                                "code-file-open",
                                IconName::ExternalLink,
                                "Open with default app",
                                cx,
                            )
                            .on_click(move |_, window, cx| {
                                reader_files::run(
                                    reader_files::FileAction::Open,
                                    &root,
                                    &rel,
                                    window,
                                    cx,
                                );
                            }),
                        ),
                )
            }
            State::Content(input) => view
                .child(self.toolbar(cx))
                .children(self.json_bar(cx))
                .when(self.structure_view, |view| {
                    view.child(self.structure_element(cx))
                })
                .when(!self.structure_view, |view| {
                    view.child(
                        // The element applies its own flag to the state on every render.
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
                            .size_full(),
                    )
                }),
        };
        #[cfg(feature = "data-view-native-harness")]
        native_trace("preview.render.end");
        result.into_any_element()
    }
}

impl Reader {
    /// The read-only code view's editor, when a code file is shown.
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
            visual.debug_bounds("table-duckdb").is_some(),
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
        let source_button = visual.debug_bounds("json-source").unwrap();
        visual.simulate_click(source_button.center(), Modifiers::default());
        visual.run_until_parked();
        preview.read_with(visual, |p, cx| {
            assert!(!p.show_table);
            assert!(p.input().is_some_and(|input| !input.read(cx).is_editable()));
        });
        assert!(visual.debug_bounds("json-pretty").is_some());
        assert!(visual.debug_bounds("table-duckdb").is_none());
        let button = visual.debug_bounds("json-table").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("table-duckdb").is_some());
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
    use ::core::prelude::v1::test;

    #[test]
    fn languages_follow_names_and_extensions_only() {
        for (rel, expected) in [
            ("work/lan-forward.py", Some("python")),
            ("src/main.rs", Some("rust")),
            ("cfg/app.JSON", Some("json")),
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
            ("data.csv", None),
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
