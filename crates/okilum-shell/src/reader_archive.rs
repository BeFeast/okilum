//! ZIP archives in the Reader (#996): the entries as a sortable, filterable table, an
//! in-place preview of one entry with the existing viewers, and extraction to a folder.
//!
//! Only the previewed entry is written, into a private temporary folder that lives as
//! long as this view; the archive itself is never extracted for a preview. All limits
//! and refusals (zip-slip, zip bombs, encrypted entries) come from `okilum_core::archive`.
use super::*;
use okilum_core::archive::{self, Kind, Limits, Listing};
use std::collections::BTreeSet;
use std::ops::Range;

const ROW_HEIGHT: f32 = 26.;
const NAME_MIN: f32 = 240.;
const NUMBER_WIDTH: f32 = 96.;
const DATE_WIDTH: f32 = 140.;
const MARK_WIDTH: f32 = 28.;
/// With an entry open the list starts at 40% and the preview below it at 60%;
/// the handle between them moves and is remembered (#1149).
const ENTRY_SHARE: f32 = 0.6;
/// The list keeps its header and three rows; the preview keeps a line.
const LIST_MIN_ROWS: f32 = 4.;
const ENTRY_MIN_HEIGHT: f32 = 48.;

pub(crate) fn eligible(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Name,
    Size,
    Packed,
    Modified,
}

enum EntryView {
    Loading,
    Text(Entity<reader_plain_text::PlainTextPreview>),
    Code(Entity<reader_code_file::CodePreview>),
    Pdf(Entity<reader_pdf::PdfViewer>),
    Image(PathBuf),
    Message(SharedString),
}

pub(crate) struct ArchivePreview {
    path: PathBuf,
    listing: Option<Arc<Listing>>,
    error: Option<SharedString>,
    filter: Entity<InputState>,
    sort: (Column, bool),
    /// Entry indices in display order, after the filter and sort.
    rows: Vec<usize>,
    marked: BTreeSet<usize>,
    focused: Option<usize>,
    entry: Option<EntryView>,
    /// Holds only the previewed entry; removed when the view drops.
    scratch: Option<tempfile::TempDir>,
    /// The previous preview's folder inside `scratch`, removed when another entry opens.
    previous: Option<PathBuf>,
    generation: u64,
    extracting: bool,
    scroll: UniformListScrollHandle,
    entry_split: reader_split::Split,
    list_focus: FocusHandle,
    entry_focus: FocusHandle,
    entry_width: f32,
    _filter: Subscription,
}

impl ArchivePreview {
    pub(crate) fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find in entries"));
        let subscription = cx.subscribe_in(
            &filter,
            window,
            |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => this.refresh_rows(cx),
                    InputEvent::PressEnter { .. } => this.list_focus.focus(window, cx),
                    _ => {}
                }
                cx.notify();
            },
        );
        let source = path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { archive::list(&source, Limits::default()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(listing) => this.listing = Some(Arc::new(listing)),
                    Err(error) => this.error = Some(describe(&error).into()),
                }
                this.refresh_rows(cx);
                cx.notify();
            });
        })
        .detach();
        Self {
            path,
            listing: None,
            error: None,
            filter,
            sort: (Column::Name, true),
            rows: Vec::new(),
            marked: BTreeSet::new(),
            focused: None,
            entry: None,
            scratch: None,
            previous: None,
            generation: 0,
            extracting: false,
            scroll: UniformListScrollHandle::new(),
            entry_split: reader_split::Split::new(
                "zip-entry",
                reader_split::Initial::Share(ENTRY_SHARE),
                ENTRY_MIN_HEIGHT,
                ROW_HEIGHT * LIST_MIN_ROWS,
            ),
            list_focus: cx.focus_handle(),
            entry_focus: cx.focus_handle(),
            entry_width: 0.,
            _filter: subscription,
        }
    }

    fn status_line(&self) -> reader_status_line::StatusLine {
        let text = self
            .listing
            .as_ref()
            .map(|listing| {
                format!(
                    "{} files, {} folders · {} unpacked, {} packed",
                    listing.files,
                    listing.directories,
                    size(listing.size, Kind::File),
                    size(listing.packed, Kind::File)
                )
            })
            .unwrap_or_default();
        reader_status_line::StatusLine {
            text: text.into(),
            ..Default::default()
        }
    }

    fn entry_bar(&self, window: &Window, cx: &App) -> AnyElement {
        let Some(listing) = &self.listing else {
            return div().into_any_element();
        };
        let Some(index) = self.focused else {
            return div().into_any_element();
        };
        let crumb = format!(
            "{} › {}",
            self.path.file_name().unwrap_or_default().to_string_lossy(),
            listing.entries[index].name
        );
        if let Some(EntryView::Code(code)) = &self.entry {
            let crumb = div()
                .debug_selector(|| "archive-entry-crumb".into())
                .flex_1()
                .min_w(px(80.))
                .truncate()
                .child(crumb)
                .into_any_element();
            return div()
                .w_full()
                .h(px(48.))
                .flex_none()
                .debug_selector(|| "archive-entry-bar".into())
                .border_b_1()
                .border_color(brand::palette(cx).border_subtle)
                .child(reader_viewer_chrome::render_compact_header_with_leading(
                    &code.read(cx).compact_chrome(code.clone(), cx),
                    if self.entry_width > 0. {
                        self.entry_width
                    } else {
                        window.viewport_size().width.as_f32().min(600.)
                    },
                    Some(crumb),
                    window,
                    cx,
                ))
                .into_any_element();
        }
        h_flex()
            .flex_none()
            .h(px(48.))
            .px_3()
            .gap_1()
            .min_w_0()
            .debug_selector(|| "archive-entry-bar".into())
            .border_b_1()
            .border_color(brand::palette(cx).border_subtle)
            .child(
                div()
                    .debug_selector(|| "archive-entry-crumb".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(crumb),
            )
            .when(matches!(&self.entry, Some(EntryView::Text(_))), |row| {
                if let Some(EntryView::Text(text)) = &self.entry {
                    row.child(text.read(cx).open_button(cx))
                } else {
                    row
                }
            })
            .children(match &self.entry {
                Some(EntryView::Pdf(pdf)) => reader_pdf::chrome(pdf, cx)
                    .actions
                    .into_iter()
                    .map(|a| a.button(true))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .into_any_element()
    }

    fn focus_find(&self, window: &mut Window, cx: &mut App) {
        if self.entry_focus.contains_focused(window, cx) {
            match &self.entry {
                Some(EntryView::Code(code)) => code.update(cx, |code, cx| {
                    code.open_paged_find(window, cx);
                }),
                _ => {}
            }
        } else {
            self.filter.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    #[cfg(test)]
    pub(crate) fn code_entry_search_snapshot(&self, cx: &App) -> Option<(String, String)> {
        let Some(EntryView::Code(code)) = &self.entry else {
            return None;
        };
        let state = code.read(cx);
        let input = state.input()?;
        Some((
            state
                .chrome(code.clone(), cx)
                .find?
                .input
                .read(cx)
                .value()
                .to_string(),
            input.read(cx).text().to_string(),
        ))
    }

    fn refresh_rows(&mut self, cx: &App) {
        let Some(listing) = self.listing.clone() else {
            self.rows.clear();
            return;
        };
        let needle = self.filter.read(cx).value().to_lowercase();
        let mut rows: Vec<usize> = listing
            .entries
            .iter()
            .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
            .map(|e| e.index)
            .collect();
        let (column, ascending) = self.sort;
        rows.sort_by(|&a, &b| {
            let (a, b) = (&listing.entries[a], &listing.entries[b]);
            let order = match column {
                Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Column::Size => a.size.cmp(&b.size),
                Column::Packed => a.packed.cmp(&b.packed),
                Column::Modified => a.modified.cmp(&b.modified),
            };
            if ascending {
                order
            } else {
                order.reverse()
            }
        });
        self.rows = rows;
    }

    fn sort_by(&mut self, column: Column, cx: &mut Context<Self>) {
        self.sort = if self.sort.0 == column {
            (column, !self.sort.1)
        } else {
            (column, true)
        };
        self.refresh_rows(cx);
        cx.notify();
    }

    /// Preview one entry: read it from the archive, write only it to the scratch folder,
    /// then hand that file to the viewer for its type.
    fn open_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(listing) = self.listing.clone() else {
            return;
        };
        let entry = listing.entries[index].clone();
        self.focused = Some(index);
        self.generation = self.generation.wrapping_add(1);
        if entry.kind == Kind::Directory {
            self.entry = None;
            cx.notify();
            return;
        }
        if self.scratch.is_none() {
            match tempfile::Builder::new().prefix("okilum-archive-").tempdir() {
                Ok(dir) => self.scratch = Some(dir),
                Err(error) => {
                    self.entry = Some(EntryView::Message(
                        format!("Cannot prepare a preview: {error}").into(),
                    ));
                    cx.notify();
                    return;
                }
            }
        }
        let generation = self.generation;
        let folder = self
            .scratch
            .as_ref()
            .unwrap()
            .path()
            .join(generation.to_string());
        let file_name = Path::new(&entry.name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "entry".into());
        // Keep only the entry being shown: drop the one written for the previous preview.
        if let Some(previous) = self.previous.replace(folder.clone()) {
            let _ = std::fs::remove_dir_all(previous);
        }
        let target = folder.join(&file_name);
        let source = self.path.clone();
        self.entry = Some(EntryView::Loading);
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let data = archive::read_entry(&source, index, Limits::default())?;
            std::fs::create_dir_all(&folder)?;
            std::fs::write(&target, data)?;
            anyhow::Ok(target)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.entry = Some(match result {
                    Ok(path) => viewer(path, &file_name, window, cx),
                    Err(error) => EntryView::Message(describe(&error).into()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn extract(&mut self, selected: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.extracting || self.listing.is_none() {
            return;
        }
        let indices: Vec<usize> = if selected {
            self.marked.iter().copied().collect()
        } else {
            Vec::new()
        };
        if selected && indices.is_empty() {
            return;
        }
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Extract here".into()),
        });
        let source = self.path.clone();
        self.extracting = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let folder = match picker.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(folder) = folder else {
                let _ = this.update(cx, |this, cx| {
                    this.extracting = false;
                    cx.notify();
                });
                return;
            };
            let result = cx
                .background_executor()
                .spawn(
                    async move { archive::extract(&source, &indices, &folder, Limits::default()) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.extracting = false;
                match result {
                    Ok(done) => {
                        let mut message = format!(
                            "Extracted {} {}",
                            done.files.len(),
                            if done.files.len() == 1 {
                                "file"
                            } else {
                                "files"
                            }
                        );
                        if !done.skipped.is_empty() {
                            message.push_str(&format!(
                                "; skipped {} (encrypted, links or unsupported)",
                                done.skipped.len()
                            ));
                        }
                        reader_toast::transient(message, window, cx);
                    }
                    Err(error) => reader_toast::error(
                        format!("Nothing was extracted: {}", describe(&error)),
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn header_cell(
        &self,
        column: Column,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let arrow = match self.sort {
            (c, true) if c == column => " ↑",
            (c, false) if c == column => " ↓",
            _ => "",
        };
        div()
            .id(SharedString::from(format!("archive-sort-{label}")))
            .cursor_pointer()
            .px_2()
            .child(format!("{label}{arrow}"))
            .on_click(cx.listener(move |this, _, _, cx| this.sort_by(column, cx)))
    }

    fn row(&self, position: usize, cx: &mut Context<Self>) -> AnyElement {
        let listing = self.listing.as_ref().unwrap();
        let index = self.rows[position];
        let entry = &listing.entries[index];
        let palette = brand::palette(cx);
        let marked = self.marked.contains(&index);
        let focused = self.focused == Some(index);
        let muted = palette.text_muted;
        let name = match entry.kind {
            Kind::Directory => entry.name.clone(),
            Kind::Symlink => format!("{}  (link)", entry.name),
            Kind::File if entry.encrypted => format!("🔒 {}", entry.name),
            Kind::File if entry.path.is_none() => format!("⚠ {}", entry.name),
            Kind::File => entry.name.clone(),
        };
        h_flex()
            .id(("archive-row", index))
            .debug_selector(move || format!("archive-row-{index}"))
            .h(px(ROW_HEIGHT))
            .w_full()
            .items_center()
            .border_b_1()
            .border_color(palette.border_subtle)
            .when(focused, |row| row.bg(palette.selected))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.open_entry(index, window, cx)))
            .child(if entry.kind == Kind::Directory {
                // Folders come with their files; only files are marked for extraction.
                div().flex_none().w(px(MARK_WIDTH)).into_any_element()
            } else {
                div()
                    .id(("archive-mark", index))
                    .debug_selector(move || format!("archive-mark-{index}"))
                    .flex_none()
                    .w(px(MARK_WIDTH))
                    .text_center()
                    .child(if marked { "☑" } else { "☐" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.marked.remove(&index) {
                            this.marked.insert(index);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(NAME_MIN))
                    .px_2()
                    .truncate()
                    .when(
                        entry.encrypted || !entry.supported || entry.path.is_none(),
                        |d| d.text_color(muted),
                    )
                    .child(name),
            )
            .child(number(size(entry.size, entry.kind), NUMBER_WIDTH, muted))
            .child(number(size(entry.packed, entry.kind), NUMBER_WIDTH, muted))
            .child(number(
                entry.modified.map(when).unwrap_or_default(),
                DATE_WIDTH,
                muted,
            ))
            .into_any_element()
    }

    /// The focused entry's name points outside the archive folder (`../`, absolute).
    /// Its preview is a copy under a safe name; extraction refuses it (#1081).
    fn unsafe_note(&self, listing: &Listing) -> Option<&'static str> {
        let entry = &listing.entries[self.focused?];
        (entry.kind == Kind::File && entry.path.is_none()).then_some(
            "⚠ This name points outside the archive folder. The preview is a safe copy; extracting it is refused.",
        )
    }

    fn render_entry(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let muted = cx.theme().muted_foreground;
        Some(match self.entry.as_ref()? {
            EntryView::Loading => div()
                .px_6()
                .text_color(muted)
                .child("Opening entry…")
                .into_any_element(),
            EntryView::Message(message) => div()
                .px_6()
                .text_color(muted)
                .child(message.clone())
                .into_any_element(),
            EntryView::Text(view) => view.clone().into_any_element(),
            EntryView::Code(view) => view.clone().into_any_element(),
            EntryView::Pdf(view) => view.clone().into_any_element(),
            EntryView::Image(path) => div()
                .px_6()
                .child(
                    reader_image::ReaderImage::new(reader_files::image_source(path.clone()))
                        .with_error_message("This image couldn’t be read."),
                )
                .into_any_element(),
        })
    }
}

pub(crate) fn chrome(
    viewer: &Entity<ArchivePreview>,
    cx: &App,
) -> reader_viewer_chrome::ViewerChrome {
    use reader_viewer_chrome::{ViewerAction, ViewerChrome};
    let state = viewer.read(cx);
    let close = viewer.clone();
    let focus = viewer.clone();
    let target = reader_find_field::Target {
        input: state.filter.clone(),
        scope: "Find in entries".into(),
        count: None,
        match_case: None,
        step: None,
        steps_visible: false,
        large_file: false,
        focus_override: Some(std::rc::Rc::new(move |window, cx| {
            focus.update(cx, |this, cx| this.focus_find(window, cx))
        })),
        close: std::rc::Rc::new(move |window, cx| {
            close.update(cx, |this, cx| {
                this.filter
                    .update(cx, |filter, cx| filter.set_value("", window, cx));
                this.refresh_rows(cx);
                this.list_focus.focus(window, cx);
                cx.notify();
            })
        }),
    };
    let mut actions = Vec::new();
    for selected in [true, false] {
        let target = viewer.clone();
        let marked = state.marked.len();
        let label = if selected {
            if marked == 0 {
                "Extract selected…".to_string()
            } else {
                format!("Extract {marked} selected…")
            }
        } else if state.extracting {
            "Extracting…".to_string()
        } else {
            "Extract all…".to_string()
        };
        let mut action = ViewerAction::new(
            if selected {
                "archive-extract-selected"
            } else {
                "archive-extract-all"
            },
            Icon::default().path(brand::READER_DOWNLOAD_ICON),
            label,
            std::rc::Rc::new(move |window, cx| {
                target.update(cx, |this, cx| this.extract(selected, window, cx))
            }),
        );
        action = if selected {
            action.with_action(ArchiveExtractSelected, cx)
        } else {
            action.with_action(ArchiveExtractAll, cx)
        };
        action.enabled = state.listing.is_some() && !state.extracting && (!selected || marked > 0);
        action.disabled_reason = (!action.enabled).then(|| {
            if state.extracting {
                "Extraction in progress"
            } else if selected && marked == 0 {
                "Select entries to extract"
            } else {
                "Reading archive…"
            }
            .into()
        });
        action.labelled = true;
        action.label_by_default = true;
        action.never_dropped = !selected;
        action.filled = !selected;
        action.collapse_priority = if selected { 0 } else { 55 };
        actions.push(action);
    }
    ViewerChrome {
        find: Some(target),
        actions,
        status: Some(state.status_line()),
        ..Default::default()
    }
}

/// The viewer for an entry written to a scratch file; `name` decides the type.
fn viewer(path: PathBuf, name: &str, window: &mut Window, cx: &mut App) -> EntryView {
    let root = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"].contains(&ext.as_str()) {
        EntryView::Image(path)
    } else if reader_pdf::is_pdf(name) {
        EntryView::Pdf(cx.new(|cx| reader_pdf::PdfViewer::new(path, window, cx)))
    } else if reader_code_file::eligible(name) {
        EntryView::Code(cx.new(|cx| {
            reader_code_file::CodePreview::new(root, name.into(), path, window, cx).without_header()
        }))
    } else {
        // Markdown, logs and anything else readable are shown as text; binary files say so.
        EntryView::Text(cx.new(|cx| {
            reader_plain_text::PlainTextPreview::new(root, name.into(), path, cx).without_header()
        }))
    }
}

fn describe(error: &anyhow::Error) -> String {
    match error.downcast_ref::<archive::Refused>() {
        Some(refused) => refused.to_string(),
        None => format!("{error:#}"),
    }
}

fn number(text: String, width: f32, color: Hsla) -> Div {
    div()
        .flex_none()
        .w(px(width))
        .px_2()
        .text_align(TextAlign::Right)
        .text_color(color)
        .child(text)
}

fn size(bytes: u64, kind: Kind) -> String {
    if kind == Kind::Directory {
        return String::new();
    }
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b if b >= 1_000 => format!("{:.1} KB", b as f64 / 1e3),
        b => format!("{b} B"),
    }
}

/// ZIP stores the writer's local time without a zone, so it is shown as written.
fn when(t: archive::Modified) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute
    )
}

impl Render for ArchivePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = brand::palette(cx);
        let view = v_flex()
            .debug_selector(|| "archive-view".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2();
        if let Some(error) = &self.error {
            return view.child(
                div()
                    .px_6()
                    .text_color(cx.theme().muted_foreground)
                    .child(error.clone()),
            );
        }
        let Some(listing) = self.listing.clone() else {
            return view.child(
                div()
                    .px_6()
                    .text_color(cx.theme().muted_foreground)
                    .child("Reading archive…"),
            );
        };
        let header = h_flex()
            .h(px(ROW_HEIGHT))
            .w_full()
            .items_center()
            .bg(palette.surface)
            .font_weight(FontWeight::SEMIBOLD)
            .border_b_1()
            .border_color(palette.border_subtle)
            .child(div().flex_none().w(px(MARK_WIDTH)))
            .child(
                self.header_cell(Column::Name, "Name", cx)
                    .flex_1()
                    .min_w(px(NAME_MIN)),
            )
            .child(
                self.header_cell(Column::Size, "Size", cx)
                    .flex_none()
                    .w(px(NUMBER_WIDTH))
                    .text_align(TextAlign::Right),
            )
            .child(
                self.header_cell(Column::Packed, "Packed", cx)
                    .flex_none()
                    .w(px(NUMBER_WIDTH))
                    .text_align(TextAlign::Right),
            )
            .child(
                self.header_cell(Column::Modified, "Modified", cx)
                    .flex_none()
                    .w(px(DATE_WIDTH))
                    .text_align(TextAlign::Right),
            );
        let count = self.rows.len();
        let table = v_flex()
            .debug_selector(|| "archive-table".into())
            .track_focus(&self.list_focus)
            .mx_6()
            .border_1()
            .border_color(palette.border_subtle)
            .child(header)
            .child(
                div().flex_1().min_h_0().w_full().child(
                    uniform_list(
                        "archive-rows",
                        count,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            range.map(|row| this.row(row, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                ),
            );
        let note = self
            .unsafe_note(&listing)
            .map(|note| div().px_6().text_color(palette.text_muted).child(note));
        let Some(entry) = self.render_entry(cx) else {
            // No entry open: at most 40% of the pane (#1149).
            return view
                .child(table.h(px(ROW_HEIGHT * 12.)).max_h(relative(0.4)))
                .children(note)
                .child(div().flex_1());
        };
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(table.flex_1().min_h_0())
            .children(note);
        // The slot takes the pane under the handle, like the PDF preview's;
        // the inner full-height column lets the text, code and table viewers'
        // `flex_1` fill it (#1079) instead of collapsing to a line (#1149).
        let target = cx.entity().downgrade();
        let measured = self.entry_width;
        let slot = div()
            .relative()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = bounds.size.width.as_f32();
                        if width > 0. && (width - measured).abs() > 0.5 {
                            cx.defer(move |cx| {
                                let _ = target.update(cx, |this, cx| {
                                    this.entry_width = width;
                                    cx.notify();
                                });
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .debug_selector(|| "archive-entry".into())
            .track_focus(&self.entry_focus)
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                v_flex()
                    .size_full()
                    .child(self.entry_bar(window, cx))
                    .child(entry),
            );
        view.child(reader_split::vertical(
            &self.entry_split,
            |view: &mut Self| &mut view.entry_split,
            list,
            slot,
            cx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn code2809_zip_entry_has_one_measured_compact_bar_and_view_menu(cx: &mut TestAppContext) {
        use reader_viewer_chrome::tests::{fixture, measured_bounds};
        cx.executor().allow_parking();
        let (reader, visual, temp) = fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/header.zip")).unwrap(),
        );
        zip.start_file("data.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, b"{\"a\":{\"b\":1}}").unwrap();
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("header.zip", window, cx)
        });
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        archive.update_in(visual, |archive, window, cx| {
            archive.open_entry(0, window, cx)
        });
        visual.run_until_parked();
        let started = std::time::Instant::now();
        while archive.read_with(visual, |archive, cx| !matches!(&archive.entry, Some(EntryView::Code(code)) if code.read(cx).document_snapshot().is_some())) {
            visual.run_until_parked(); assert!(started.elapsed() < Duration::from_secs(30)); std::thread::sleep(Duration::from_millis(10));
        }
        for width in [1100., 700., 460.] {
            visual.simulate_resize(gpui::size(px(width), px(800.)));
            visual.run_until_parked();
            let bar = measured_bounds(visual, "archive-entry-bar").unwrap();
            assert_eq!(bar.size.height, px(48.));
            for id in ["archive-entry-crumb", "code-view-menu", "code-compact-more"] {
                let item = visual
                    .debug_bounds(id)
                    .unwrap_or_else(|| panic!("required {id}"));
                assert!(
                    item.left() >= bar.left()
                        && item.right() <= bar.right()
                        && item.top() >= bar.top()
                        && item.bottom() <= bar.bottom(),
                    "{id} outside the only entry bar: {item:?} vs {bar:?}"
                );
            }
            let find = visual
                .debug_bounds("viewer-find")
                .or_else(|| visual.debug_bounds("viewer-find-slot"))
                .expect("required compact Find slot");
            assert!(find.left() >= bar.left() && find.right() <= bar.right());
            if bar.size.width < px(600.) {
                let icon = visual
                    .debug_bounds("viewer-find")
                    .expect("narrow entry Find is an icon");
                let switch = visual.debug_bounds("code-view-menu").unwrap();
                visual.simulate_click(icon.center(), Modifiers::default());
                visual.run_until_parked();
                let bar = measured_bounds(visual, "archive-entry-bar").unwrap();
                let field = visual
                    .debug_bounds("viewer-find-field")
                    .expect("the same Find field opened over compact row");
                assert!(field.left() >= bar.left() && field.right() <= bar.right());
                assert!(
                    field.size.width >= px(240.),
                    "compact overlay leaves room for query and steps"
                );
                assert!(
                    visual.debug_bounds("viewer-find-query").unwrap().size.width >= px(64.),
                    "typed query stays visible beside Find controls"
                );
                assert_eq!(visual.debug_bounds("code-view-menu").unwrap(), switch);
                visual.simulate_keystrokes("escape");
                visual.run_until_parked();
            }
        }
        let button = measured_bounds(visual, "code-view-menu").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(!gpui_base::GlobalState::native_overlays(window, cx).is_empty());
        });
        visual.simulate_keystrokes("down down down enter");
        visual.run_until_parked();
        archive.read_with(visual, |archive, cx| {
            let Some(EntryView::Code(code)) = &archive.entry else {
                panic!("code entry");
            };
            assert_eq!(
                code.read(cx).document_snapshot().unwrap().text.as_ref(),
                "{\"a\":{\"b\":1}}",
                "Raw menu action selected the unformatted snapshot"
            );
        });
    }

    #[gpui::test]
    fn code_entry_click_retains_keyboard_selection_and_copy(cx: &mut TestAppContext) {
        entry_click_copy(cx, "source.rs");
    }

    #[gpui::test]
    fn table_entry_click_retains_keyboard_cell_copy(cx: &mut TestAppContext) {
        entry_click_copy(cx, "data.csv");
    }

    fn entry_click_copy(cx: &mut TestAppContext, name: &str) {
        use reader_viewer_chrome::tests::{fixture, measured_bounds};
        cx.executor().allow_parking();
        let (reader, visual, temp) = fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/focus.zip")).unwrap(),
        );
        for (name, text) in [
            ("source.rs", "alpha beta gamma\n"),
            ("data.csv", "id,value\nfirst,1\nsecond,2\n"),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("focus.zip", window, cx)
        });
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        let index = archive.read_with(visual, |archive, _| {
            archive
                .listing
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .position(|entry| entry.name == name)
                .unwrap()
        });
        archive.update_in(visual, |archive, window, cx| {
            archive.open_entry(index, window, cx)
        });
        visual.run_until_parked();
        let at = if name == "source.rs" {
            let input = archive.read_with(visual, |archive, cx| {
                match archive.entry.as_ref().unwrap() {
                    EntryView::Code(code) => code.read(cx).input().unwrap(),
                    _ => panic!("code entry"),
                }
            });
            let bounds = input.read_with(visual, |input, _| input.text_bounds().unwrap());
            bounds.origin + point(px(8.), px(8.))
        } else {
            measured_bounds(visual, "delimited-cell-1-0")
                .unwrap()
                .center()
        };
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        visual.simulate_click(at, Modifiers::default());
        visual.run_until_parked();
        if name == "source.rs" {
            visual.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            });
        }
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(if name == "source.rs" {
                    "alpha beta gamma\n"
                } else {
                    "first"
                }),
                "clicking {name} retains the child's keyboard Copy handler"
            )
        });
    }

    #[gpui::test]
    fn entry_compact_bar_fits_460_pixels_and_partial_status_stays_inside_the_pane(
        cx: &mut TestAppContext,
    ) {
        use reader_viewer_chrome::tests::{fixture, measured_bounds};
        let (reader, visual, temp) = fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/release-0.9.4.zip")).unwrap(),
        );
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("package.json", options).unwrap();
        std::io::Write::write_all(&mut zip, b"{\"name\":\"entry-fixture\"}\n").unwrap();
        zip.start_file("gateway.log", options).unwrap();
        std::io::Write::write_all(&mut zip, &b"INFO ready\n".repeat(7000)).unwrap();
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("release-0.9.4.zip", window, cx);
            reader.maximize_note(window, cx);
        });
        visual.simulate_resize(gpui::size(px(460.), px(800.)));
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        for name in ["package.json", "gateway.log"] {
            let index = archive.read_with(visual, |archive, _| {
                archive
                    .listing
                    .as_ref()
                    .unwrap()
                    .entries
                    .iter()
                    .position(|entry| entry.name == name)
                    .unwrap()
            });
            archive.update_in(visual, |archive, window, cx| {
                archive.open_entry(index, window, cx)
            });
            visual.run_until_parked();
            let pane = measured_bounds(visual, "archive-entry").unwrap();
            let bar = measured_bounds(visual, "archive-entry-bar").unwrap();
            let crumb = measured_bounds(visual, "archive-entry-crumb").unwrap();
            assert_eq!(bar.size, gpui::size(px(460.), px(48.)));
            assert_eq!(bar.top(), pane.top());
            assert!(crumb.size.width >= px(120.));
            assert!(crumb.left() >= bar.left() && crumb.right() <= bar.right());
            if name == "gateway.log" {
                let open = measured_bounds(visual, "plain-file-open").unwrap();
                assert!(open.left() >= bar.left() && open.right() <= bar.right());
                let status = measured_bounds(visual, "plain-file-status-line").unwrap();
                let archive_status = measured_bounds(visual, "file-status-line").unwrap();
                assert_eq!(status.size.height, px(28.));
                assert!(status.top() >= bar.bottom() && status.bottom() <= pane.bottom());
                assert!(status.bottom() <= archive_status.top());
            }
        }
    }

    #[gpui::test]
    fn a_selected_entry_fills_the_space_under_the_list(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n").unwrap();
        let mut zip =
            zip::ZipWriter::new(std::fs::File::create(root.join("redesign.zip")).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for i in 0..74 {
            zip.start_file(format!("src/components/file{i:02}.tsx"), options)
                .unwrap();
            std::io::Write::write_all(&mut zip, b"export const C = () => null;\n").unwrap();
        }
        zip.start_file("src/types/empty-state.d.ts", options)
            .unwrap();
        for i in 0..40 {
            std::io::Write::write_all(
                &mut zip,
                format!("export interface EmptyStateProps{i} {{\n  title: string;\n}}\n")
                    .as_bytes(),
            )
            .unwrap();
        }
        zip.finish().unwrap();
        let state = fixture.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        session_directory: Some(state.clone()),
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
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let open = |visual: &mut VisualTestContext| {
            reader.update_in(visual, |r, window, cx| {
                r.preview_file("redesign.zip", window, cx)
            });
            visual.run_until_parked();
            let archive = reader.read_with(visual, |r, _| {
                r.file_preview
                    .as_ref()
                    .unwrap()
                    .archive
                    .clone()
                    .expect("archive view")
            });
            let index = archive.read_with(visual, |a, _| {
                a.listing
                    .as_ref()
                    .expect("listing")
                    .entries
                    .iter()
                    .position(|e| e.name.ends_with(".d.ts"))
                    .unwrap()
            });
            archive.update_in(visual, |a, window, cx| a.open_entry(index, window, cx));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        open(visual);
        let slot = visual.debug_bounds("archive-entry").expect("entry slot");
        let code = visual
            .debug_bounds("reader-code-file")
            .expect("code preview");
        // 15 lines at the 13 px code size need well over 250 px.
        assert!(
            f32::from(code.size.height) > 300.,
            "the code preview collapsed: {code:?} in {slot:?}"
        );
        let bar = visual
            .debug_bounds("archive-entry-bar")
            .expect("entry compact bar");
        assert_eq!(bar.size.height, px(48.));
        assert!(
            (code.top() - bar.bottom()).abs() <= px(1.),
            "code starts directly below its compact bar"
        );
        assert!(
            (code.bottom() - slot.bottom()).abs() <= px(1.),
            "code still fills the remaining pane"
        );

        // The list starts at 40% of the split, the preview at 60%.
        let pane = visual.debug_bounds("zip-entry-pane").expect("entry pane");
        let table = visual.debug_bounds("archive-table").expect("entry table");
        let share = f32::from(pane.size.height) / f32::from(pane.size.height + table.size.height);
        assert!(
            (0.55..0.65).contains(&share),
            "initial preview share {share}"
        );
        // A real drag of the handle moves the boundary, and it is remembered
        // for the next archive view and the next launch (#1149).
        let handle = visual.debug_bounds("zip-entry-handle").expect("handle");
        let target = handle.center() + point(px(0.), px(150.));
        visual.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            handle.center() + point(px(0.), px(20.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let dragged = visual.debug_bounds("zip-entry-pane").unwrap().size.height;
        let expected = pane.size.height - px(150.);
        assert!(
            (dragged - expected).abs() < px(8.),
            "the preview follows the handle: {dragged:?}, expected {expected:?}"
        );
        reader.update_in(visual, |r, window, cx| {
            r.open_note("note.md", None, window, cx)
        });
        visual.run_until_parked();
        open(visual);
        let reopened = visual.debug_bounds("zip-entry-pane").unwrap().size.height;
        assert!(
            (reopened - dragged).abs() < px(1.),
            "the next archive view keeps it: {reopened:?}"
        );
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join("reader-ui.json")).unwrap()).unwrap();
        let saved = saved["split_sizes"]["zip-entry"].as_f64().unwrap() as f32;
        assert!(
            (saved - f32::from(dragged)).abs() < 1.,
            "saved for the next launch: {saved}"
        );
        // Back to the initial share for the small-window check.
        archive_split_reset(&reader, visual);

        // A small window: the list starts at 40%, the preview keeps room.
        visual.simulate_resize(gpui::size(px(1000.), px(560.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let view = visual.debug_bounds("archive-view").expect("archive view");
        let table = visual.debug_bounds("archive-table").expect("entry table");
        let code = visual
            .debug_bounds("reader-code-file")
            .expect("code preview");
        assert!(
            f32::from(table.size.height) <= f32::from(view.size.height) * 0.4 + 1.,
            "the table is capped: {table:?} in {view:?}"
        );
        assert!(
            f32::from(table.size.height) < ROW_HEIGHT * 12.,
            "positive control: the cap, not the 12-row height, applies here: {table:?}"
        );
        assert!(
            f32::from(code.size.height) > 80.,
            "the preview keeps room: {code:?}"
        );
    }

    fn archive_split_reset(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
        let archive = reader.read_with(visual, |r, _| {
            r.file_preview.as_ref().unwrap().archive.clone().unwrap()
        });
        archive.update(visual, |a, cx| {
            a.entry_split = reader_split::Split::new(
                "zip-entry-reset",
                reader_split::Initial::Share(ENTRY_SHARE),
                ENTRY_MIN_HEIGHT,
                ROW_HEIGHT * LIST_MIN_ROWS,
            );
            cx.notify();
        });
    }

    #[test]
    fn only_zip_archives_are_eligible() {
        for path in ["a.zip", "dir/B.ZIP"] {
            assert!(eligible(path), "{path}");
        }
        for path in ["a.zip.md", "a.tar.gz", "zip", "a.pdf"] {
            assert!(!eligible(path), "{path}");
        }
    }

    #[gpui::test]
    fn header_find_filters_entries_and_enter_and_escape_return_to_the_entry_list(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/archive.zip")).unwrap(),
        );
        for name in ["package.json", "README.md"] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, b"{}").unwrap();
        }
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("archive.zip", window, cx)
        });
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        assert_eq!(
            archive.read_with(visual, |archive, _| archive.rows.len()),
            2
        );
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.run_until_parked();
        visual.simulate_input("package");
        visual.run_until_parked();
        archive.read_with(visual, |archive, cx| {
            assert_eq!(archive.filter.read(cx).value().to_string(), "package");
            assert_eq!(
                archive.rows.len(),
                1,
                "header query actually narrows archive entries"
            );
            assert_eq!(
                archive.listing.as_ref().unwrap().entries[archive.rows[0]].name,
                "package.json"
            );
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(archive.read(cx).list_focus.is_focused(window));
            assert_eq!(
                archive.read(cx).filter.read(cx).value().to_string(),
                "package"
            );
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.update(|window, cx| {
            let archive = archive.read(cx);
            assert_eq!(archive.rows.len(), 2, "one Escape restores every entry");
            assert!(archive.filter.read(cx).value().is_empty());
            assert!(archive.list_focus.is_focused(window));
        });
    }

    #[test]
    fn sizes_and_dos_times_are_human_readable() {
        assert_eq!(size(999, Kind::File), "999 B");
        assert_eq!(size(1_500, Kind::File), "1.5 KB");
        assert_eq!(size(2_500_000, Kind::File), "2.5 MB");
        assert_eq!(size(10, Kind::Directory), "");
        let t = archive::Modified {
            year: 2024,
            month: 10,
            day: 9,
            hour: 7,
            minute: 5,
            second: 0,
        };
        assert_eq!(when(t), "2024-10-09 07:05");
    }
}
