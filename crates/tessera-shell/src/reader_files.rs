//! Explicit attachment previews and file actions; no external app opens on selection.
use super::*;
use crate::platform::labels::Os;
use gpui_component::menu::{PopupMenu, PopupMenuItem};

#[derive(Clone, Copy)]
pub(crate) enum FileAction {
    Reveal,
    Absolute,
    Relative,
    Wiki,
    Open,
    QuickLook,
}

pub(crate) fn checked_path(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = root.join(rel).canonicalize()?;
    anyhow::ensure!(path.starts_with(&root), "The file is outside this vault");
    Ok(path)
}

pub(crate) fn run(action: FileAction, root: &Path, rel: &str, window: &mut Window, cx: &mut App) {
    let result = (|| -> anyhow::Result<()> {
        let path = checked_path(root, rel)?;
        let copied = match action {
            FileAction::Reveal => {
                reveal(&path, window, cx);
                None
            }
            FileAction::Open => {
                cx.open_url(
                    url::Url::from_file_path(&path)
                        .map_err(|_| anyhow::anyhow!("Invalid file path"))?
                        .as_str(),
                );
                None
            }
            FileAction::QuickLook => {
                #[cfg(target_os = "macos")]
                {
                    // qlmanage delegates to the native Quick Look preview service.
                    // The path is a separate argument, never shell text.
                    let mut child = std::process::Command::new("/usr/bin/qlmanage")
                        .arg("-p")
                        .arg(&path)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()?;
                    cx.background_executor()
                        .spawn(async move {
                            let _ = child.wait();
                        })
                        .detach();
                }
                #[cfg(not(target_os = "macos"))]
                anyhow::bail!("Quick Look is available on macOS");
                #[cfg(target_os = "macos")]
                {
                    None
                }
            }
            FileAction::Absolute => Some(path.to_string_lossy().into_owned()),
            FileAction::Relative => Some(rel.to_owned()),
            FileAction::Wiki => Some(format!("[[{rel}|{}]]", Vault::title_of(rel))),
        };
        if let Some(text) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            reader_toast::transient("Copied", window, cx);
        }
        Ok(())
    })();
    if let Err(error) = result {
        reader_toast::error(format!("File action failed: {error}"), window, cx);
    }
}

/// Shows the item in the platform file manager; a failure of every route is
/// reported rather than left as a silent no-op.
pub(crate) fn reveal(path: &Path, window: &mut Window, cx: &mut App) {
    let task = crate::platform::reveal::reveal_path(path, cx);
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = task.await {
                let _ = cx.update(|window, cx| {
                    reader_toast::error(format!("File action failed: {error:#}"), window, cx)
                });
            }
        })
        .detach();
}

pub(crate) fn menu(mut menu: PopupMenu, root: PathBuf, rel: String) -> PopupMenu {
    let actions = [
        (Os::CURRENT.reveal(), FileAction::Reveal),
        ("Copy absolute path", FileAction::Absolute),
        ("Copy vault path", FileAction::Relative),
        ("Copy wikilink", FileAction::Wiki),
        ("Open with default app", FileAction::Open),
    ];
    for (label, action) in actions {
        let root = root.clone();
        let rel = rel.clone();
        // The bound Reader action only shows its key; the click acts on this row.
        let item = match action {
            FileAction::Reveal => PopupMenuItem::new(label).action(Box::new(super::RevealFile)),
            FileAction::Relative => {
                PopupMenuItem::new(label).action(Box::new(super::CopyVaultPath))
            }
            _ => PopupMenuItem::new(label),
        };
        menu = menu.item(item.on_click(move |_, window, cx| run(action, &root, &rel, window, cx)));
    }
    #[cfg(target_os = "macos")]
    {
        menu = menu.item(
            PopupMenuItem::new("Quick Look")
                .action(Box::new(super::QuickLookFile))
                .on_click(move |_, window, cx| run(FileAction::QuickLook, &root, &rel, window, cx)),
        );
    }
    menu
}

pub(crate) struct FilePreview {
    pub rel: String,
    pub path: PathBuf,
    pub details: String,
    pub image: bool,
    /// The inline reader for a PDF (#477); replaces the file card.
    pub pdf: Option<Entity<reader_pdf::PdfViewer>>,
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub thumbnail: Option<Entity<reader_thumbnail::Thumbnail>>,
    /// The log view, for a log opened in the quick viewer (#602).
    pub log: Option<Entity<reader_log::LogView>>,
}
impl FilePreview {
    pub fn load(root: &Path, rel: &str) -> anyhow::Result<Self> {
        let path = checked_path(root, rel)?;
        let meta = std::fs::metadata(&path)?;
        anyhow::ensure!(meta.is_file(), "Choose a file to preview");
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let modified = meta
            .modified()
            .ok()
            .map(|t| {
                let t: time::OffsetDateTime = t.into();
                format!(
                    "{} {} {} {:02}:{:02} UTC",
                    t.day(),
                    t.month(),
                    t.year(),
                    t.hour(),
                    t.minute()
                )
            })
            .unwrap_or_else(|| "Unknown modified date".into());
        Ok(Self {
            #[cfg(any(target_os = "macos", all(test, unix)))]
            thumbnail: None,
            pdf: None,
            log: None,
            rel: rel.into(),
            path,
            details: format!("{} · {} bytes · {modified}", ext.to_uppercase(), meta.len()),
            image: ["svg", "png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&ext.as_str())
                || (cfg!(target_os = "macos") && ["heic", "heif"].contains(&ext.as_str())),
        })
    }
}

impl Reader {
    /// An outside-vault link offers an explicit action without loading the file
    /// into the vault or launching anything as a side effect of link activation.
    pub(crate) fn outside_file_menu(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(raw) = url.strip_prefix("tessera://outside-file/") else {
            return;
        };
        let path = PathBuf::from(tessera_core::document_links::decode(raw));
        if !path.is_absolute() || !path.is_file() {
            return;
        }
        let label = format!(
            "Reveal {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let copy = path.to_string_lossy().into_owned();
        let popup = PopupMenu::build(window, cx, move |menu, _, _| {
            let path = path.clone();
            let copy = copy.clone();
            menu.item(
                PopupMenuItem::new(label.clone())
                    .on_click(move |_, window, cx| reveal(&path, window, cx)),
            )
            .item(
                PopupMenuItem::new("Copy absolute path").on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    reader_toast::transient("Copied", window, cx);
                }),
            )
        });
        cx.subscribe(&popup, |this, _, _: &DismissEvent, cx| {
            this.file_menu = None;
            cx.notify();
        })
        .detach();
        popup.focus_handle(cx).focus(window, cx);
        self.file_menu = Some((popup, window.mouse_position()));
        cx.notify();
    }

    pub(crate) fn file_link_menu(
        &mut self,
        url: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.prepared_links.get(url).is_some_and(|state| {
            state.status != tessera_core::document_links::prepared::LinkStatus::Resolved
        }) {
            return;
        }
        let action_url = self
            .prepared_links
            .get(url)
            .and_then(|s| s.action_url.as_deref())
            .unwrap_or(url);
        let rel = action_url
            .strip_prefix("tessera://attachment/")
            .map(tessera_core::document_links::decode)
            .or_else(|| {
                action_url
                    .strip_prefix(WIKI_SCHEME)
                    .map(|s| split_open_url(s).0)
            });
        let Some(rel) = rel else {
            return;
        };
        let root = self.vault_root.clone();
        let popup = PopupMenu::build(window, cx, move |m, _, _| {
            menu(m, root.clone(), rel.clone())
        });
        cx.subscribe(&popup, |this, _, _: &DismissEvent, cx| {
            this.file_menu = None;
            cx.notify();
        })
        .detach();
        popup.focus_handle(cx).focus(window, cx);
        self.file_menu = Some((popup, position));
        cx.notify();
    }
    pub(crate) fn file_action(
        &self,
        action: FileAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rel = if self.tree_focus.is_focused(window) {
            self.tree.cursor.as_deref().unwrap_or(self.selected_file())
        } else {
            self.selected_file()
        };
        run(action, &self.vault_root, rel, window, cx);
    }

    pub(crate) fn selected_file(&self) -> &str {
        self.file_preview
            .as_ref()
            .map(|p| p.rel.as_str())
            .unwrap_or(&self.current_rel)
    }
    pub(crate) fn preview_file(&mut self, rel: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_source(cx) {
            return;
        }
        match FilePreview::load(&self.vault_root, rel) {
            Ok(preview) => {
                self.cancel_pending_landing();
                self.document_preparation_generation =
                    self.document_preparation_generation.wrapping_add(1);
                self.table_overlay = None;
                if let Some(index) = self.history_nav {
                    self.history_ix = index;
                } else if self.history.get(self.history_ix).map(String::as_str) != Some(rel) {
                    if !self.history.is_empty() {
                        self.history.truncate(self.history_ix + 1);
                        self.history_positions.truncate(self.history_ix + 1);
                    }
                    self.history.push(rel.into());
                    self.history_positions.push(ListOffset {
                        item_ix: 0,
                        offset_in_item: px(0.),
                    });
                    self.history_ix = self.history.len() - 1;
                }
                self.document_header_hidden = px(0.);
                let mut preview = preview;
                if reader_pdf::is_pdf(rel) {
                    let path = preview.path.clone();
                    let viewer = cx.new(|cx| reader_pdf::PdfViewer::new(path, window, cx));
                    cx.subscribe(&viewer, |_, _, _: &reader_pdf::AvailabilityChanged, cx| {
                        cx.notify()
                    })
                    .detach();
                    preview.pdf = Some(viewer);
                }
                #[cfg(target_os = "macos")]
                if preview.pdf.is_none() && reader_thumbnail::eligible(rel) {
                    preview.thumbnail = Some(cx.new(|cx| {
                        reader_thumbnail::Thumbnail::new(self.vault_root.clone(), rel.into(), cx)
                    }));
                }
                // The quick viewer shows sibling logs as logs; vault
                // attachments keep the file card until #602's vault slice.
                if self.single_file && tessera_core::log::is_log_path(&preview.path) {
                    let (rel, path) = (rel.to_owned(), preview.path.clone());
                    let view = cx.new(|cx| reader_log::LogView::indexing(rel, path, cx));
                    view.read(cx).focus_handle().clone().focus(window, cx);
                    preview.log = Some(view);
                }
                let log = preview.log.is_some();
                self.file_preview = Some(preview);
                self.find_open = false;
                self.link_notice = None;
                self.editing = None;
                window.set_window_title(&format!("Tessera — {}", self.selected_title()));
                if log {
                    self.record_usable_document(cx);
                } else {
                    self.focus_handle.focus(window, cx);
                }
            }
            Err(error) => reader_toast::error(format!("Cannot preview file: {error}"), window, cx),
        }
        cx.notify();
    }
    pub(crate) fn render_file_preview(
        &self,
        preview: &FilePreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(log) = &preview.log {
            return self.render_log_preview(log, cx);
        }
        if tessera_core::excalidraw::is_drawing(&preview.rel) {
            return div()
                .id("reader-drawing-preview")
                .size_full()
                .overflow_y_scroll()
                .key_context("ReaderFile")
                .track_focus(&self.focus_handle)
                .child(self.render_document_header(cx))
                .child(reader_drawing::render(
                    &self.vault_root,
                    &preview.path,
                    None,
                    window,
                    cx,
                ))
                .into_any_element();
        }
        if let Some(pdf) = &preview.pdf {
            return v_flex()
                .id("reader-pdf-preview")
                .key_context("ReaderFile ReaderPdf")
                .track_focus(&self.focus_handle)
                .on_action(
                    cx.listener(|this, _: &PdfZoomIn, _, cx| {
                        this.pdf_zoom(reader_pdf::Zoom::In, cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &PdfZoomOut, _, cx| {
                    this.pdf_zoom(reader_pdf::Zoom::Out, cx)
                }))
                .on_action(cx.listener(|this, _: &PdfZoomFit, _, cx| {
                    this.pdf_zoom(reader_pdf::Zoom::Fit, cx)
                }))
                .size_full()
                .child(self.render_document_header(cx))
                .child(div().flex_1().min_h_0().w_full().child(pdf.clone()))
                .into_any_element();
        }
        #[cfg(any(target_os = "macos", all(test, unix)))]
        if let Some(thumbnail) = &preview.thumbnail {
            return v_flex()
                .id("reader-file-preview")
                .key_context("ReaderFile")
                .track_focus(&self.focus_handle)
                .size_full()
                .overflow_y_scroll()
                .gap_3()
                .child(self.render_document_header(cx))
                .child(
                    v_flex()
                        .flex_none()
                        .px_6()
                        .pb_6()
                        .gap_3()
                        .child(thumbnail.clone())
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(preview.details.clone()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Quick Look preview · Space to open"),
                        ),
                )
                .into_any_element();
        }
        let mut view = v_flex()
            .flex_none()
            .px_6()
            .pb_6()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(Icon::new(IconName::File).size(px(48.)))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                preview
                                    .path
                                    .extension()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("FILE")
                                    .to_uppercase(),
                            ),
                    ),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(preview.details.clone()),
            );
        if preview.image {
            view = view.child(super::reader_image::ReaderImage::new(image_source(
                preview.path.clone(),
            )));
        }
        v_flex()
            .id("reader-file-preview")
            .key_context("ReaderFile")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .gap_3()
            .child(self.render_document_header(cx))
            .child(view)
            .into_any_element()
    }
}

/// HEIC uses Apple's image decoder off the UI thread; the temporary PNG is
/// derived data and never lives beside the user's files.
pub(crate) fn image_source(path: PathBuf) -> ImageSource {
    #[cfg(target_os = "macos")]
    if path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif"))
    {
        return ImageSource::Custom(Arc::new(move |window, cx| {
            window.use_asset::<HeicImage>(&path, cx)
        }));
    }
    ImageSource::from(path)
}

#[cfg(target_os = "macos")]
struct HeicImage;
#[cfg(target_os = "macos")]
impl Asset for HeicImage {
    type Source = PathBuf;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        path: PathBuf,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        cx.background_executor().spawn(async move {
            let result = (|| -> anyhow::Result<Arc<RenderImage>> {
                let directory = tempfile::tempdir()?;
                let png = directory.path().join("preview.png");
                let status = std::process::Command::new("/usr/bin/sips")
                    .args(["-s", "format", "png"])
                    .arg(path)
                    .arg("--out")
                    .arg(&png)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()?;
                anyhow::ensure!(status.success(), "Cannot decode HEIC image");
                Image::from_bytes(ImageFormat::Png, std::fs::read(png)?).to_image_data(renderer)
            })();
            result.map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("tessera-files-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(path.join("vault")).unwrap();
            std::fs::write(path.join("vault/start.md"), "# Original note").unwrap();
            std::fs::write(path.join("vault/diagram.svg"), r#"<svg xmlns="http://www.w3.org/2000/svg" width="2600" height="862"><rect width="2600" height="862" fill="red"/></svg>"#).unwrap();
            std::fs::write(path.join("vault/report.pdf"), b"%PDF-fixture").unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn metadata_and_paths_do_not_modify_files() {
        let fixture = Fixture::new();
        let root = fixture.0.join("vault");
        let svg = FilePreview::load(&root, "diagram.svg").unwrap();
        assert!(svg.image);
        assert!(svg.details.contains("SVG"));
        let pdf = FilePreview::load(&root, "report.pdf").unwrap();
        assert!(!pdf.image);
        assert!(pdf.details.contains("12 bytes"));
        assert!(FilePreview::load(&root, "missing.svg").is_err());
        assert!(checked_path(&root, "../").is_err());
        assert_eq!(
            std::fs::read(root.join("report.pdf")).unwrap(),
            b"%PDF-fixture"
        );
    }

    #[gpui::test]
    fn file_selection_stays_in_reader_and_copy_targets_the_file(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let root = fixture.0.join("vault");
        // A PDF selection starts the viewer's real worker thread.
        cx.executor().allow_parking();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_global(reader_history::TestSessionDirectory(
                fixture.0.join("state"),
            ));
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        index_dir: Some(fixture.0.join("index")),
                        panel_settings_override: Some(fixture.0.join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        let drawing_source =
            include_str!("../../tessera-core/tests/fixtures/excalidraw/elements.excalidraw.md");
        for path in [
            "Схема.excalidraw.md",
            "Board.excalidraw",
            "Board.EXCALIDRAW.MD",
        ] {
            std::fs::write(root.join(path), drawing_source).unwrap();
        }
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("diagram.svg", window, cx);
            assert_eq!(reader.selected_file(), "diagram.svg");
            assert!(reader.file_preview.as_ref().unwrap().image);
            reader.toggle_source(window, cx);
            assert!(reader.editing.is_none());
            reader.file_action(FileAction::Relative, window, cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "diagram.svg"
            );
            reader.file_action(FileAction::Wiki, window, cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "[[diagram.svg|diagram]]"
            );
            reader.preview_file("report.pdf", window, cx);
            assert!(!reader.file_preview.as_ref().unwrap().image);
            assert!(reader.pdf_viewer().is_some(), "a PDF opens inline");
            assert_eq!(reader.history.last().unwrap(), "report.pdf");
            for (path, name) in [
                ("Схема.excalidraw.md", "Схема"),
                ("Board.excalidraw", "Board"),
                ("Board.EXCALIDRAW.MD", "Board"),
            ] {
                reader.preview_file(path, window, cx);
                assert_eq!(reader.selected_title(), name);
                assert_eq!(reader.note_label(path), name);
                assert_eq!(reader.selected_file(), path);
                assert_eq!(reader.history.last().unwrap(), path);
                reader.file_action(FileAction::Relative, window, cx);
                assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), path);
                assert_eq!(
                    std::fs::read_to_string(root.join(path)).unwrap(),
                    drawing_source
                );
            }
        });
        // The title assertions above deliberately select drawings. Restore the
        // PDF before testing its real keyboard focus and zoom handlers.
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("report.pdf", window, cx);
            assert!(reader.pdf_viewer().is_some(), "restore PDF for zoom checks");
        });
        // Zoom keys reach the viewer through the Reader's PDF key context.
        visual.run_until_parked();
        let zoom = |visual: &mut VisualTestContext| {
            reader.read_with(visual, |reader, cx| {
                reader.pdf_viewer().unwrap().read(cx).zoom_step()
            })
        };
        let fit = zoom(visual);
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-="
        } else {
            "ctrl-="
        });
        assert_eq!(zoom(visual), fit + 1);
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-0"
        } else {
            "ctrl-0"
        });
        assert_eq!(zoom(visual), fit);
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("diagram.svg", window, cx);
            assert!(reader.pdf_viewer().is_none());
        });
        assert_eq!(visual.opened_url(), None);
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            "# Original note"
        );
    }
}
