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
    Table(Entity<reader_delimited::TablePreview>),
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
    generation: u64,
    extracting: bool,
    scroll: UniformListScrollHandle,
    _filter: Subscription,
}

impl ArchivePreview {
    pub(crate) fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter entries…"));
        let subscription = cx.subscribe(&filter, |this, _, _: &InputEvent, cx| {
            this.refresh_rows(cx);
            cx.notify();
        });
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
            generation: 0,
            extracting: false,
            scroll: UniformListScrollHandle::new(),
            _filter: subscription,
        }
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
            .child(
                div()
                    .id(("archive-mark", index))
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
                    })),
            )
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
            EntryView::Table(view) => view.clone().into_any_element(),
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
    } else if reader_delimited::eligible(name) {
        EntryView::Table(cx.new(|cx| reader_delimited::TablePreview::new(path, cx)))
    } else if reader_code_file::eligible(name) {
        EntryView::Code(
            cx.new(|cx| reader_code_file::CodePreview::new(root, name.into(), path, window, cx)),
        )
    } else {
        // Markdown, logs and anything else readable are shown as text; binary files say so.
        EntryView::Text(
            cx.new(|cx| reader_plain_text::PlainTextPreview::new(root, name.into(), path, cx)),
        )
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = brand::palette(cx);
        let view = v_flex().w_full().flex_1().min_h_0().gap_2();
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
        let summary = format!(
            "{} files, {} folders · {} unpacked, {} packed",
            listing.files,
            listing.directories,
            size(listing.size, Kind::File),
            size(listing.packed, Kind::File),
        );
        let marked = self.marked.len();
        let toolbar = h_flex()
            .px_6()
            .gap_2()
            .items_center()
            .child(div().text_color(palette.text_muted).child(summary))
            .child(div().flex_1())
            .child(
                Button::new("archive-extract-selected")
                    .debug_selector(|| "archive-extract-selected".into())
                    .ghost()
                    .small()
                    .disabled(marked == 0 || self.extracting)
                    .label(if marked == 0 {
                        "Extract selected…".to_string()
                    } else {
                        format!("Extract {marked} selected…")
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.extract(true, window, cx))),
            )
            .child(
                Button::new("archive-extract-all")
                    .debug_selector(|| "archive-extract-all".into())
                    .small()
                    .disabled(self.extracting)
                    .label(if self.extracting {
                        "Extracting…"
                    } else {
                        "Extract all…"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.extract(false, window, cx))),
            );
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
            .mx_6()
            .h(px(ROW_HEIGHT * 12.))
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
        view.child(toolbar)
            .child(div().px_6().child(Input::new(&self.filter)))
            .child(table)
            .when_some(self.render_entry(cx), |view, entry| {
                view.child(div().w_full().flex_1().min_h_0().child(entry))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn only_zip_archives_are_eligible() {
        for path in ["a.zip", "dir/B.ZIP"] {
            assert!(eligible(path), "{path}");
        }
        for path in ["a.zip.md", "a.tar.gz", "zip", "a.pdf"] {
            assert!(!eligible(path), "{path}");
        }
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
