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
        menu = menu.item(
            PopupMenuItem::new(label)
                .on_click(move |_, window, cx| run(action, &root, &rel, window, cx)),
        );
    }
    #[cfg(target_os = "macos")]
    {
        menu = menu.item(
            PopupMenuItem::new("Quick Look    Space")
                .on_click(move |_, window, cx| run(FileAction::QuickLook, &root, &rel, window, cx)),
        );
    }
    menu
}

/// What the file card says about a file: type, human size, local date.
#[derive(Clone)]
pub(crate) struct FileCard {
    pub kind: String,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}
impl FileCard {
    pub fn details(&self) -> String {
        let mut parts = vec![self.kind.clone(), human_size(self.size)];
        if let Some(modified) = self.modified {
            let local = chrono::DateTime::<chrono::Local>::from(modified);
            parts.push(format!("Modified {}", human_date(&local)));
        }
        parts.join(" · ")
    }
    pub fn render_details(&self, cx: &App) -> AnyElement {
        div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.details())
            .into_any_element()
    }
    /// The fallback for a file Tessera cannot draw: no raw bytes, no UTC.
    pub fn render(&self, cx: &App) -> AnyElement {
        v_flex()
            .id("file-preview-unavailable")
            .py_6()
            .gap_2()
            .child(
                Icon::new(IconName::File)
                    .size(px(48.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(self.render_details(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Os::CURRENT.file_preview_hint()),
            )
            .into_any_element()
    }
}

/// Decimal units, as Finder and most file managers show them.
pub(crate) fn human_size(bytes: u64) -> String {
    if bytes == 1 {
        return "1 byte".into();
    }
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    for unit in ["KB", "MB", "GB", "TB"] {
        value /= 1000.;
        // Round first so 999,950 bytes reads "1 MB", not "1000 KB".
        let rounded = if value < 100. {
            (value * 10.).round() / 10.
        } else {
            value.round()
        };
        if rounded < 1000. || unit == "TB" {
            return if rounded.fract() == 0. {
                format!("{rounded:.0} {unit}")
            } else {
                format!("{rounded:.1} {unit}")
            };
        }
    }
    unreachable!()
}

/// A date in the caller's zone, with no zone name: "7 Oct 2026, 14:05".
pub(crate) fn human_date<Tz: chrono::TimeZone>(time: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    time.format("%-d %b %Y, %H:%M").to_string()
}

pub(crate) struct FilePreview {
    pub rel: String,
    pub path: PathBuf,
    pub card: FileCard,
    pub image: bool,
    pub thumbnail: Option<Entity<reader_thumbnail::Thumbnail>>,
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
        Ok(Self {
            thumbnail: None,
            rel: rel.into(),
            path,
            card: FileCard {
                kind: if ext.is_empty() {
                    "File".into()
                } else {
                    ext.to_uppercase()
                },
                size: meta.len(),
                modified: meta.modified().ok(),
            },
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
                if reader_thumbnail::eligible(rel) {
                    let card = preview.card.clone();
                    preview.thumbnail = Some(cx.new(|cx| {
                        reader_thumbnail::Thumbnail::new(
                            self.vault_root.clone(),
                            rel.into(),
                            card,
                            cx,
                        )
                    }));
                }
                self.file_preview = Some(preview);
                self.find_open = false;
                self.link_notice = None;
                self.editing = None;
                window.set_window_title(&format!("Tessera — {rel}"));
                self.focus_handle.focus(window, cx);
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
        let body = if let Some(thumbnail) = &preview.thumbnail {
            thumbnail.clone().into_any_element()
        } else if preview.image {
            v_flex()
                .gap_3()
                .child(super::reader_image::ReaderImage::new(image_source(
                    preview.path.clone(),
                )))
                .child(preview.card.render_details(cx))
                .into_any_element()
        } else {
            preview.card.render(cx)
        };
        v_flex()
            .id("reader-file-preview")
            .key_context("ReaderFile")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .gap_3()
            .child(self.render_document_header(cx))
            .child(v_flex().flex_none().px_6().pb_6().child(body))
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
            image::RgbImage::from_pixel(8, 6, image::Rgb([40, 100, 190]))
                .save(path.join("vault/photo.png"))
                .unwrap();
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
        assert!(svg.card.details().starts_with("SVG · "));
        let pdf = FilePreview::load(&root, "report.pdf").unwrap();
        assert!(!pdf.image);
        let details = pdf.card.details();
        assert!(
            details.starts_with("PDF · 12 bytes · Modified "),
            "{details}"
        );
        assert!(!details.contains("UTC"), "{details}");
        assert!(FilePreview::load(&root, "missing.svg").is_err());
        assert!(checked_path(&root, "../").is_err());
        assert_eq!(
            std::fs::read(root.join("report.pdf")).unwrap(),
            b"%PDF-fixture"
        );
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(human_size(0), "0 bytes");
        assert_eq!(human_size(1), "1 byte");
        assert_eq!(human_size(999), "999 bytes");
        assert_eq!(human_size(1000), "1 KB");
        assert_eq!(human_size(1_536), "1.5 KB");
        assert_eq!(human_size(999_950), "1 MB");
        assert_eq!(human_size(1_900_000), "1.9 MB");
        assert_eq!(human_size(1_949_999), "1.9 MB");
        assert_eq!(human_size(12_345_678), "12.3 MB");
        assert_eq!(human_size(123_456_789), "123 MB");
        assert_eq!(human_size(5_000_000_000), "5 GB");
        assert_eq!(human_size(u64::MAX), "18446744 TB");
    }

    #[test]
    fn dates_are_local_and_carry_no_zone_name() {
        use chrono::TimeZone;
        let zone = chrono::FixedOffset::east_opt(3 * 3600).unwrap();
        let time = zone.with_ymd_and_hms(2026, 10, 7, 9, 5, 0).unwrap();
        assert_eq!(human_date(&time), "7 Oct 2026, 09:05");
        // The same instant reads in the viewer's zone, not in UTC.
        let utc = time.with_timezone(&chrono::Utc);
        assert_eq!(human_date(&utc), "7 Oct 2026, 06:05");
        let card = FileCard {
            kind: "JPG".into(),
            size: 1_900_000,
            modified: Some(time.into()),
        };
        let local = chrono::DateTime::<chrono::Local>::from(std::time::SystemTime::from(time));
        assert_eq!(
            card.details(),
            format!("JPG · 1.9 MB · Modified {}", human_date(&local))
        );
        assert!(!card.details().contains("UTC"));
        let unknown = FileCard {
            modified: None,
            ..card
        };
        assert_eq!(unknown.details(), "JPG · 1.9 MB");
    }

    #[gpui::test]
    fn file_selection_stays_in_reader_and_copy_targets_the_file(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let root = fixture.0.join("vault");
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
            assert!(reader.file_preview.as_ref().unwrap().thumbnail.is_none());
            reader.preview_file("photo.png", window, cx);
            // Raster images decode in-process on every platform.
            assert!(reader.file_preview.as_ref().unwrap().thumbnail.is_some());
            reader.preview_file("report.pdf", window, cx);
            assert!(!reader.file_preview.as_ref().unwrap().image);
            assert_eq!(reader.history.last().unwrap(), "report.pdf");
        });
        assert_eq!(visual.opened_url(), None);
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            "# Original note"
        );
    }
}
