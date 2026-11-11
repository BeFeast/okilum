//! One authoritative presentation state; switches never mutate canonical bytes.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum View {
    Table,
    Tree,
    Formatted,
    #[default]
    Raw,
}
impl View {
    pub const ALL: [Self; 4] = [Self::Table, Self::Tree, Self::Formatted, Self::Raw];
    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "Table",
            Self::Tree => "Tree",
            Self::Formatted => "Formatted",
            Self::Raw => "Raw",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Table => "code-view-table",
            Self::Tree => "code-view-tree",
            Self::Formatted => "code-view-formatted",
            Self::Raw => "code-view-raw",
        }
    }
    pub fn icon(self) -> Icon {
        Icon::default().path(match self {
            Self::Table => brand::CODE_VIEW_TABLE_ICON,
            Self::Tree => brand::CODE_VIEW_TREE_ICON,
            Self::Formatted => "icons/list.svg",
            Self::Raw => "icons/code-xml.svg",
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Available,
    Disabled(String),
    Absent,
}
impl Availability {
    pub fn available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

pub(super) fn size(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.)
}
pub(super) fn fraction_to_part(fraction: f32, parts: usize) -> usize {
    (fraction.clamp(0., 1.) * parts.saturating_sub(1) as f32).round() as usize
}

impl CodePreview {
    pub(crate) fn without_header(mut self) -> Self {
        self.own_header = false;
        self
    }
    pub(crate) fn availability(&self, view: View) -> Availability {
        let Some(raw) = &self.raw_document else {
            return if view == View::Raw {
                Availability::Disabled(match &self.state {
                    State::Message(message) => message.clone(),
                    _ => "Opening file…".into(),
                })
            } else {
                Availability::Absent
            };
        };
        if view == View::Raw {
            return Availability::Available;
        }
        if view == View::Table {
            return if self.table.is_some() {
                Availability::Available
            } else {
                Availability::Absent
            };
        }
        if self.json.is_some() && raw.invalid.is_some() {
            return Availability::Disabled("Not available: the file is not valid JSON".into());
        }
        if view == View::Tree {
            return if !raw.structure.is_empty() {
                Availability::Available
            } else {
                Availability::Absent
            };
        }
        let Some(json) = &self.json else {
            return Availability::Absent;
        };
        if let Some(Err(error)) = &json.pretty {
            return Availability::Disabled(match error {
                okilum_core::json_view::Error::Limit { reason, limit } => format!(
                    "Not available: {reason} limit {}",
                    if *reason == "formatted output bytes" {
                        "64 MB".into()
                    } else {
                        limit.to_string()
                    }
                ),
                _ => "Not available: the file is not valid JSON".into(),
            });
        }
        Availability::Available
    }
    fn guarded(&self) -> bool {
        self.raw_document
            .as_ref()
            .is_some_and(|doc| doc.text.len() > PRETTY_BYTES || doc.pages.len() > 1)
    }
    fn type_key(&self) -> Option<&'static str> {
        match language(&self.rel)? {
            "json" if is_json(&self.rel) => Some(if self.table.is_some() {
                "json-array"
            } else {
                "json"
            }),
            "yaml" => Some("yaml"),
            "toml" => Some("toml"),
            "xml" => Some("xml"),
            _ => None,
        }
    }
    pub(super) fn first_view(&self, cx: &App) -> View {
        let Some(raw) = &self.raw_document else {
            return View::Raw;
        };
        if raw.text.len() > PRETTY_BYTES {
            return View::Raw;
        }
        if raw.pages.len() > 1 {
            return if self.availability(View::Tree).available() {
                View::Tree
            } else {
                View::Raw
            };
        }
        if let Some(view) = self
            .type_key()
            .and_then(|kind| reader_ui_state::code_view(kind, cx))
        {
            if self.availability(view).available() {
                return view;
            }
        }
        if self.table.is_some() {
            View::Table
        } else if self.json.is_some()
            && raw.invalid.is_none()
            && self.availability(View::Formatted).available()
        {
            View::Formatted
        } else {
            View::Raw
        }
    }
    pub(super) fn snapshot_for(&self, view: View) -> Option<Arc<Document>> {
        if view == View::Formatted {
            self.json.as_ref()?.pretty.as_ref()?.as_ref().ok().cloned()
        } else {
            self.raw_document.clone()
        }
    }
    pub(crate) fn set_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        if !self.availability(view).available() {
            return;
        }
        if let Some(json) = &mut self.json {
            json.requested = view;
        }
        if view == View::Formatted && self.snapshot_for(view).is_none() {
            let Some(json) = &mut self.json else {
                return;
            };
            if json.formatting {
                return;
            }
            json.formatting = true;
            let raw = json.raw.clone();
            let task = cx.background_executor().spawn(async move {
                okilum_core::json_view::pretty(&raw.text)
                    .map(|text| Arc::new(Document::for_file(text, "json", true)))
            });
            cx.spawn_in(window, async move |this, cx| {
                let result = task.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    let Some(json) = &mut this.json else {
                        return;
                    };
                    json.formatting = false;
                    json.pretty = Some(result);
                    if json.requested == View::Formatted {
                        this.set_view(View::Formatted, window, cx);
                    }
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
            return;
        }
        if !self.guarded() {
            if let Some(kind) = self.type_key() {
                reader_ui_state::set_code_view(kind, view, cx);
            }
        }
        if self.view == view {
            return;
        }
        self.view = view;
        if let Some(doc) = self.snapshot_for(view) {
            let changed = self
                .document
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &doc));
            self.paged = doc.pages.len() > 1;
            self.document = Some(doc);
            if changed {
                self.document_revision += 1;
                self.show_page(0, window, cx);
                self.search(window, cx);
            }
        }
        if self.view == View::Tree || self.paged {
            // The retained whole-file row owns Find in these presentations.
            // A floating toolkit strip from Formatted must not survive here.
            if let Some(input) = self.input() {
                input.update(cx, |input, cx| input.close_search(cx));
            }
        }
        if self.view == View::Tree {
            self.show_match(window, cx);
            self.focus.focus(window, cx);
        } else if let Some(input) = self.input() {
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }
    pub(super) fn open_find(&self, window: &mut Window, cx: &mut App) {
        if !self.open_paged_find(window, cx) {
            if let Some(input) = self.input() {
                input.update(cx, |state, cx| {
                    state.open_search(false, cx);
                    state.focus(window, cx);
                });
            }
        }
    }
    pub(super) fn fold_availability(&self) -> Availability {
        let Some(doc) = &self.document else {
            return Availability::Disabled("Opening file…".into());
        };
        if self.view == View::Table {
            return Availability::Absent;
        }
        if doc.invalid.is_some() || (language(&self.rel) != Some("text") && !doc.parses) {
            return Availability::Disabled("Nothing to fold: the file does not parse".into());
        }
        if self.view == View::Tree {
            return Availability::Available;
        }
        if doc.pages.len() > 1 {
            return Availability::Disabled(
                if self.availability(View::Tree).available() {
                    "Nothing to fold in Raw: this file is one line. Tree can fold it"
                } else {
                    "Nothing to fold: this file is one line"
                }
                .into(),
            );
        }
        if !doc.foldable {
            return Availability::Absent;
        }
        Availability::Available
    }
    pub(super) fn request_page(
        &mut self,
        page: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.requested_page = Some(page);
        if self.page_scheduled {
            return;
        }
        self.page_scheduled = true;
        cx.on_next_frame(window, |this, window, cx| {
            this.page_scheduled = false;
            if let Some(page) = this.requested_page.take() {
                this.show_page(page, window, cx);
            }
        });
    }
    pub(super) fn apply_diagnostics(&self, input: &Entity<EditorState>, cx: &mut App) {
        use gpui_base::input::{Diagnostic, DiagnosticSeverity, RopeExt};
        use gpui_component::input::Position;
        let Some(doc) = &self.document else {
            return;
        };
        let page = &doc.pages[self.page];
        let invalid = doc
            .invalid
            .as_ref()
            .filter(|error| page.contains(&error.offset));
        input.update(cx, |input, cx| {
            let text = input.text().clone();
            if let Some(diagnostics) = input.diagnostics_mut() {
                diagnostics.reset(&text);
                if let Some(error) = invalid {
                    let local = error.offset - page.start;
                    let start = text.offset_to_position(local);
                    diagnostics.push(Diagnostic {
                        range: start..Position {
                            character: start.character + 1,
                            ..start
                        },
                        severity: DiagnosticSeverity::Error,
                        message: error.message.clone().into(),
                        ..Default::default()
                    });
                }
            }
            cx.notify();
        });
    }
    pub(super) fn go_to_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(error) = self
            .raw_document
            .as_ref()
            .and_then(|doc| doc.invalid.clone())
        else {
            return;
        };
        self.set_view(View::Raw, window, cx);
        let doc = self.raw_document.as_ref().unwrap();
        let page = doc.page_at(error.offset);
        let start = doc.pages[page].start;
        self.show_page(page, window, cx);
        if let Some(input) = self.input() {
            input.update(cx, |input, cx| {
                input.set_selected_range(error.offset - start..error.offset - start, cx);
                input.focus(window, cx);
            });
        }
    }
}

pub(super) fn copy_text_node(text: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    let message = copy_node_notice(text);
    // WindowHandle::update can already hold Root (including native hosts).
    // Publish feedback after that lease ends; clipboard copying stays immediate.
    window.defer(cx, move |window, cx| {
        reader_toast::transient(message, window, cx);
    });
}

pub(crate) fn bind_keys(cx: &mut App) {
    for context in ["ReaderCode", "ReaderCode > Input"] {
        let modifier = if cfg!(target_os = "macos") {
            "ctrl"
        } else {
            "alt"
        };
        cx.bind_keys([
            KeyBinding::new(&format!("{modifier}-1"), ShowTable, Some(context)),
            KeyBinding::new(&format!("{modifier}-2"), ShowTree, Some(context)),
            KeyBinding::new(&format!("{modifier}-3"), ShowFormatted, Some(context)),
            KeyBinding::new(&format!("{modifier}-4"), ShowRaw, Some(context)),
            KeyBinding::new("ctrl-shift-c", CopyNode, Some(context)),
            KeyBinding::new("alt-pageup", PreviousPart, Some(context)),
            KeyBinding::new("alt-pagedown", NextPart, Some(context)),
        ]);
        let folds = if cfg!(target_os = "macos") {
            ["ctrl-{", "ctrl-{"]
        } else {
            ["ctrl-shift-[", "ctrl-{"]
        };
        let unfolds = if cfg!(target_os = "macos") {
            ["ctrl-}", "ctrl-}"]
        } else {
            ["ctrl-shift-]", "ctrl-}"]
        };
        for key in folds {
            cx.bind_keys([KeyBinding::new(key, FoldAll, Some(context))]);
        }
        for key in unfolds {
            cx.bind_keys([KeyBinding::new(key, UnfoldAll, Some(context))]);
        }
    }
}

pub(super) fn copy_node_notice(text: &str) -> String {
    format!("Copied node · {} lines", text.lines().count().max(1))
}
