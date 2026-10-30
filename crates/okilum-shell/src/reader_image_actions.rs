//! Image commands retain the clicked image and its owning note, never the
//! current file-manager selection or a basename guessed from a different note.
use super::*;
use gpui_component::{
    menu::{PopupMenu, PopupMenuItem},
    native_menu::NativeMenu,
};
use std::io::Read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Reveal,
    Open,
    Sidebar,
    CopyPath,
    CopyImage,
    Download,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    root: PathBuf,
    note: String,
    generation: u64,
    url: Option<String>,
    local: Option<PathBuf>,
    relative: Option<String>,
    placeholder: bool,
}

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = reader_image_actions, no_json, no_register)]
pub(super) struct ImageCommand {
    target: Target,
    command: Command,
}

fn reveal_label(os: platform::labels::Os) -> &'static str {
    match os {
        platform::labels::Os::Mac => "Show in Finder",
        platform::labels::Os::Windows => "Show in Explorer",
        platform::labels::Os::Linux => "Show in folder",
    }
}

impl Target {
    fn captured(reader: &Reader, url: Option<&str>) -> Self {
        let parsed = url.and_then(|url| url::Url::parse(url).ok());
        let local = parsed.as_ref().and_then(|url| url.to_file_path().ok());
        let relative = local
            .as_ref()
            .and_then(|path| path.strip_prefix(&reader.vault_root).ok())
            .map(okilum_core::vault::note_path);
        let remote = parsed
            .as_ref()
            .is_some_and(|url| matches!(url.scheme(), "https" | "http"));
        let placeholder = local
            .as_ref()
            .is_some_and(|path| reader.vault.cloud_placeholders.contains(path));
        Self {
            root: reader.vault_root.clone(),
            note: reader.current_rel.clone(),
            generation: reader.navigation.preparation_generation,
            url: (local.is_some() || remote).then(|| url.unwrap().to_owned()),
            local,
            relative,
            placeholder,
        }
    }
    fn matches(&self, reader: &Reader) -> bool {
        self.root == reader.vault_root
            && self.note == reader.current_rel
            && self.generation == reader.navigation.preparation_generation
    }
    fn entries(&self) -> Vec<(&'static str, Command, bool)> {
        let available = self.url.is_some();
        let mut entries = Vec::new();
        if self.local.is_some() || !available {
            entries.push((
                reveal_label(platform::labels::Os::CURRENT),
                Command::Reveal,
                !available,
            ));
        }
        entries.push((
            if self.local.is_none() && available {
                "Open in browser"
            } else {
                "Open in default app"
            },
            Command::Open,
            !available || self.placeholder,
        ));
        if self.local.is_some() || !available {
            entries.push(("Show in sidebar", Command::Sidebar, self.relative.is_none()));
            entries.push(("Copy path", Command::CopyPath, !available));
        }
        entries.push((
            "Copy image",
            Command::CopyImage,
            !available || self.placeholder,
        ));
        if self.placeholder {
            entries.push(("Download from iCloud", Command::Download, false));
        }
        entries
    }
    fn notice(&self) -> Option<&'static str> {
        if self.placeholder {
            Some("Download this image from iCloud before opening or copying it.")
        } else if self.url.is_none() {
            Some("Image file not found or its destination is ambiguous.")
        } else {
            None
        }
    }
}

/// Called only by the native Reader renderer; Brain's image authority does not
/// pass a Reader here. Ordinary Live Preview presses still reach the editor.
pub(super) fn decorate(
    reader: WeakEntity<Reader>,
    url: &str,
    element: AnyElement,
    cx: &App,
) -> AnyElement {
    let Some(entity) = reader.upgrade() else {
        return element;
    };
    let target = Target::captured(entity.read(cx), Some(url));
    let open = target.clone();
    let menu_reader = reader.clone();
    div()
        .id(SharedString::from(format!("image-actions-{url}")))
        .debug_selector(|| "image-action-surface".into())
        .child(element)
        .tooltip({
            let target = target.clone();
            move |window, cx| {
                gpui_component::tooltip::Tooltip::new(
                    target.notice().unwrap_or("Right-click for image actions"),
                )
                .build(window, cx)
            }
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            cx.stop_propagation();
            let target = target.clone();
            let _ = menu_reader.update(cx, |reader, cx| reader.image_menu(target, window, cx));
        })
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            let modified = if cfg!(target_os = "macos") {
                event.modifiers.platform
            } else {
                event.modifiers.control || event.modifiers.platform
            };
            if modified {
                cx.stop_propagation();
                let _ = reader.update(cx, |reader, cx| {
                    reader.image_command(
                        &ImageCommand {
                            target: open.clone(),
                            command: Command::Open,
                        },
                        window,
                        cx,
                    )
                });
            }
        })
        .into_any_element()
}

impl Reader {
    pub(super) fn open_source_image_at(
        &mut self,
        source: &str,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(image) = okilum_core::render::image_reference::references(
            source,
            &self.vault,
            &self.current_rel,
        )
        .into_iter()
        .find(|image| image.range.contains(&offset)) else {
            return false;
        };
        self.image_command(
            &ImageCommand {
                target: Target::captured(self, image.url.as_deref()),
                command: Command::Open,
            },
            window,
            cx,
        );
        true
    }

    fn image_menu(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        if !target.matches(self) {
            return;
        }
        let reader = cx.entity().downgrade();
        let popup = PopupMenu::build(window, cx, move |mut menu, _, _| {
            if let Some(notice) = target.notice() {
                menu = menu.label(notice);
            }
            for (label, command, disabled) in target.entries() {
                let target = target.clone();
                let reader = reader.clone();
                menu = menu.item(PopupMenuItem::new(label).disabled(disabled).on_click(
                    move |_, window, cx| {
                        let _ = reader.update(cx, |reader, cx| {
                            reader.image_command(
                                &ImageCommand {
                                    target: target.clone(),
                                    command,
                                },
                                window,
                                cx,
                            )
                        });
                    },
                ));
            }
            menu
        });
        cx.subscribe(&popup, |reader, _, _: &DismissEvent, cx| {
            reader.file_menu = None;
            cx.notify();
        })
        .detach();
        popup.focus_handle(cx).focus(window, cx);
        self.file_menu = Some((popup, window.mouse_position()));
        cx.notify();
    }

    pub(super) fn image_command(
        &mut self,
        action: &ImageCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !action.target.matches(self) {
            return;
        }
        let target = action.target.clone();
        let command = action.command;
        if target
            .entries()
            .iter()
            .any(|(_, candidate, disabled)| *candidate == command && *disabled)
        {
            reader_toast::transient(
                target
                    .notice()
                    .unwrap_or("This image action is unavailable."),
                window,
                cx,
            );
            return;
        }
        if command == Command::Sidebar {
            if let Some(relative) = &target.relative {
                self.reveal_in_tree(relative, window, cx);
            }
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let work = target.clone();
            let result = cx
                .background_executor()
                .spawn(async move { prepare(&work, command) })
                .await;
            let _ = this.update_in_own_window(cx, |reader, window, cx| {
                if !target.matches(reader) {
                    return;
                }
                match result {
                    Ok(Prepared::Path(path)) => match command {
                        Command::Reveal => reader_files::reveal(&path, window, cx),
                        Command::CopyPath => cx.write_to_clipboard(ClipboardItem::new_string(
                            okilum_core::vault::display_path(&path),
                        )),
                        _ => {
                            if let Ok(url) = url::Url::from_file_path(path) {
                                cx.open_url(url.as_str());
                            }
                        }
                    },
                    Ok(Prepared::Url(url)) => cx.open_url(&url),
                    Ok(Prepared::Image(bytes)) => cx.write_to_clipboard(ClipboardItem::new_image(
                        &gpui::Image::from_bytes(ImageFormat::Png, bytes),
                    )),
                    #[cfg(target_os = "macos")]
                    Ok(Prepared::Downloaded) => {
                        reader.queue_vault_mutation(
                            okilum_core::Changes {
                                changed: target.relative.clone().into_iter().collect(),
                                ..Default::default()
                            },
                            cx,
                        );
                        reader_toast::transient(
                            "Image downloaded. Its Open and Copy actions are now available.",
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        reader_toast::error(format!("Image action failed: {error:#}"), window, cx)
                    }
                }
            });
        })
        .detach();
    }
}

enum Prepared {
    Path(PathBuf),
    Url(String),
    Image(Vec<u8>),
    #[cfg(target_os = "macos")]
    Downloaded,
}

fn prepare(target: &Target, command: Command) -> anyhow::Result<Prepared> {
    let Some(path) = &target.local else {
        let url = target
            .url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Image file not found"))?;
        return if command == Command::Open {
            Ok(Prepared::Url(url.clone()))
        } else if command == Command::CopyImage {
            let bytes = note_paste::download::fetch_remote_image(url)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(Prepared::Image(clipboard_png(&bytes)?))
        } else {
            anyhow::bail!("This image is on the web")
        };
    };
    if command == Command::Download {
        #[cfg(target_os = "macos")]
        {
            reader_icloud::materialize(path, || Ok(()))?;
            return Ok(Prepared::Downloaded);
        }
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("iCloud downloads are available on macOS");
    }
    if command == Command::Reveal && target.placeholder && !path.try_exists()? {
        let mut name = std::ffi::OsString::from(".");
        name.push(
            path.file_name()
                .ok_or_else(|| anyhow::anyhow!("Missing image name"))?,
        );
        name.push(".icloud");
        let stub = path.with_file_name(name).canonicalize()?;
        if target.relative.is_some() {
            anyhow::ensure!(
                stub.starts_with(target.root.canonicalize()?),
                "The image moved outside this vault"
            );
        }
        return Ok(Prepared::Path(stub));
    }
    let path = if let Some(relative) = &target.relative {
        reader_files::checked_path(&target.root, relative)?
    } else {
        path.canonicalize()?
    };
    let metadata = std::fs::metadata(&path)?;
    anyhow::ensure!(metadata.is_file(), "Image file not found");
    if command == Command::CopyPath || command == Command::Reveal {
        return Ok(Prepared::Path(path));
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        anyhow::ensure!(
            metadata.st_flags() & 0x4000_0000 == 0,
            "Download this image from iCloud first"
        );
    }
    if command == Command::Open {
        return Ok(Prepared::Path(path));
    }
    let limit = note_paste::offer::MAX_IMAGE_BYTES;
    anyhow::ensure!(
        metadata.len() <= limit as u64,
        "Image exceeds the 64 MB clipboard limit"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= limit,
        "Image exceeds the 64 MB clipboard limit"
    );
    Ok(Prepared::Image(clipboard_png(&bytes)?))
}

fn clipboard_png(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    use image::ImageDecoder as _;
    let orientation = decoder.orientation()?;
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png)?;
    anyhow::ensure!(
        out.get_ref().len() <= note_paste::offer::MAX_IMAGE_BYTES,
        "Image exceeds the 64 MB clipboard limit"
    );
    Ok(out.into_inner())
}

#[derive(Clone, Default)]
pub(super) struct SourceFacts(Rc<std::cell::RefCell<SourceSnapshot>>);
#[derive(Default)]
struct SourceSnapshot {
    stamp: Option<gpui_component::input::projection::SourceStamp>,
    ranges: Vec<okilum_core::render::image_reference::ImageReference>,
    selected: Option<Target>,
    root: PathBuf,
    note: String,
    generation: u64,
}
impl SourceFacts {
    pub(super) fn refresh(
        &self,
        reader: &Reader,
        input: &gpui_component::input::EditorState,
        markdown: bool,
    ) {
        let mut snapshot = self.0.borrow_mut();
        if !markdown {
            snapshot.selected = None;
            return;
        }
        let stamp = input.source_stamp();
        if snapshot.stamp != Some(stamp)
            || snapshot.root != reader.vault_root
            || snapshot.note != reader.current_rel
            || snapshot.generation != reader.navigation.preparation_generation
        {
            snapshot.ranges = if input.text().len() <= okilum_core::source_classifier::MAX_BYTES {
                okilum_core::render::image_reference::references(
                    &input.text().to_string(),
                    &reader.vault,
                    &reader.current_rel,
                )
            } else {
                Vec::new()
            };
            snapshot.stamp = Some(stamp);
            snapshot.root = reader.vault_root.clone();
            snapshot.note = reader.current_rel.clone();
            snapshot.generation = reader.navigation.preparation_generation;
        }
        snapshot.selected = snapshot
            .ranges
            .iter()
            .find(|image| image.range.contains(&input.cursor()))
            .map(|image| Target::captured(reader, image.url.as_deref()));
    }
    pub(super) fn menu(&self, mut menu: NativeMenu) -> NativeMenu {
        let selected = self.0.borrow().selected.clone();
        if let Some(target) = selected {
            menu = menu.separator();
            if let Some(notice) = target.notice() {
                menu = menu.menu_with_disabled(
                    notice,
                    true,
                    Box::new(ImageCommand {
                        target: target.clone(),
                        command: Command::Open,
                    }),
                );
            }
            for (label, command, disabled) in target.entries() {
                menu = menu.menu_with_disabled(
                    label,
                    disabled,
                    Box::new(ImageCommand {
                        target: target.clone(),
                        command,
                    }),
                );
            }
        }
        menu
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn target(root: &Path, relative: &str) -> Target {
        let path = root.join(relative);
        Target {
            root: root.to_owned(),
            note: "note.md".into(),
            generation: 4,
            url: Some(url::Url::from_file_path(&path).unwrap().into()),
            local: Some(path),
            relative: Some(relative.into()),
            placeholder: false,
        }
    }
    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 3)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn image_menu_capabilities_distinguish_local_remote_missing_and_placeholder() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let local = target(&root, "attachments/Привет image.png");
        assert_eq!(local.entries().len(), 5);
        assert!(local.entries().iter().all(|(_, _, disabled)| !disabled));
        let remote = Target {
            url: Some("https://example.invalid/image.png".into()),
            local: None,
            relative: None,
            ..local.clone()
        };
        let entries = remote.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("Open in browser", Command::Open, false));
        assert_eq!(entries[1], ("Copy image", Command::CopyImage, false));
        let missing = Target {
            url: None,
            local: None,
            relative: None,
            ..local.clone()
        };
        assert_eq!(missing.entries().len(), 5);
        assert!(missing.entries().iter().all(|(_, _, disabled)| *disabled));
        assert!(missing.notice().unwrap().contains("not found"));
        let placeholder = Target {
            placeholder: true,
            ..local
        };
        let enabled = |command| {
            placeholder
                .entries()
                .iter()
                .any(|(_, c, disabled)| *c == command && !disabled)
        };
        assert!(enabled(Command::Reveal) && enabled(Command::Download));
        assert!(!enabled(Command::Open) && !enabled(Command::CopyImage));
        assert_eq!(reveal_label(platform::labels::Os::Mac), "Show in Finder");
        assert_eq!(
            reveal_label(platform::labels::Os::Windows),
            "Show in Explorer"
        );
        assert_eq!(reveal_label(platform::labels::Os::Linux), "Show in folder");
    }

    #[test]
    fn local_image_commands_keep_exact_path_and_copy_pixels_without_writing() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let relative = "Привет image.png";
        let path = root.join(relative);
        let bytes = png();
        std::fs::write(&path, &bytes).unwrap();
        let target = target(&root, relative);
        for command in [Command::Reveal, Command::Open, Command::CopyPath] {
            let Prepared::Path(actual) = prepare(&target, command).unwrap() else {
                panic!("expected local path")
            };
            assert_eq!(actual, path);
        }
        let Prepared::Image(copied) = prepare(&target, Command::CopyImage).unwrap() else {
            panic!("expected clipboard pixels")
        };
        let image = image::load_from_memory(&copied).unwrap();
        assert_eq!((image.width(), image.height()), (2, 3));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert!(clipboard_png(b"not an image").is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(prepare(&target, Command::Open).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn image_commands_recheck_containment_after_a_symlink_replacement() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let external = fixture.path().join("outside.png");
        std::fs::write(&external, png()).unwrap();
        let target = target(&root, "image.png");
        std::os::unix::fs::symlink(&external, root.join("image.png")).unwrap();
        assert!(prepare(&target, Command::CopyImage).is_err());
        assert!(prepare(&target, Command::Open).is_err());
        assert!(external.exists());
    }

    #[gpui::test]
    fn image_actions_copy_and_reveal_the_image_and_reject_a_different_note(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let state = fixture.path().join("state");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("image.png"), png()).unwrap();
        let source = "# Images\n\n![[image.png]]\n";
        std::fs::write(root.join("note.md"), source).unwrap();
        std::fs::write(root.join("other.md"), "# Other\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(state),
                        index_dir: Some(fixture.path().join("index")),
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
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        let captured = reader.read_with(visual, |reader, _| {
            Target::captured(
                reader,
                Some(
                    url::Url::from_file_path(root.join("image.png"))
                        .unwrap()
                        .as_str(),
                ),
            )
        });
        let bounds = visual.debug_bounds("image-action-surface").unwrap();
        visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(
                reader.file_menu.is_some(),
                "rendered image opens its context menu"
            )
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.image_command(
                &ImageCommand {
                    target: captured.clone(),
                    command: Command::CopyPath,
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                okilum_core::vault::display_path(&root.join("image.png"))
            )
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.image_command(
                &ImageCommand {
                    target: captured.clone(),
                    command: Command::CopyImage,
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert!(cx
                .read_from_clipboard()
                .unwrap()
                .entries()
                .iter()
                .any(|entry| matches!(entry, ClipboardEntry::Image(_))))
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| reader.source_input().unwrap());
        let offset = source.find("image.png").unwrap();
        input.update(visual, |input, cx| {
            input.set_selected_range(offset..offset, cx)
        });
        visual.run_until_parked();
        let facts = SourceFacts::default();
        reader.read_with(visual, |reader, cx| {
            facts.refresh(reader, input.read(cx), true)
        });
        let source_target = facts
            .0
            .borrow()
            .selected
            .clone()
            .expect("source caret has an image");
        assert_eq!(source_target.url, captured.url);
        // Constructing the native source menu while its editor is updating may
        // use only the observed snapshot, never another read of EditorState.
        input.update(visual, |_, _| {
            let _ = facts.menu(NativeMenu::new());
        });
        input.update(visual, |input, cx| input.set_selected_range(0..0, cx));
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            facts.refresh(reader, input.read(cx), true)
        });
        assert!(
            facts.0.borrow().selected.is_none(),
            "moving the caret outside the embed clears its image actions"
        );
        input.update(visual, |input, cx| {
            input.set_selected_range(offset..offset, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            facts.refresh(reader, input.read(cx), true)
        });
        assert_eq!(
            facts.0.borrow().selected.as_ref().unwrap().url,
            captured.url,
            "returning to the embed restores its captured image, without a text edit"
        );
        input.update(visual, |input, cx| input.set_selected_range(0..0, cx));
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_live_preview(window, cx)
        });
        visual.run_until_parked();
        let image_bounds = visual
            .debug_bounds("image-action-surface")
            .expect("Live Preview keeps image actions");
        visual.simulate_mouse_down(
            image_bounds.center(),
            MouseButton::Right,
            Modifiers::default(),
        );
        visual.simulate_mouse_up(
            image_bounds.center(),
            MouseButton::Right,
            Modifiers::default(),
        );
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(reader.file_menu.is_some()));
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let current = reader.read_with(visual, |reader, _| {
            Target::captured(reader, captured.url.as_deref())
        });
        reader.update_in(visual, |reader, window, cx| {
            // Edit/Read invalidates the old image menu even in the same note.
            assert!(!captured.matches(reader));
            let before = reader.tree.cursor.clone();
            reader.image_command(
                &ImageCommand {
                    target: captured.clone(),
                    command: Command::Sidebar,
                },
                window,
                cx,
            );
            assert_eq!(reader.tree.cursor, before);
            // A menu opened from the current rendered image captures a fresh
            // generation, just as decorate() does for a real right-click.
            assert!(current.matches(reader));
            reader.image_command(
                &ImageCommand {
                    target: current.clone(),
                    command: Command::Sidebar,
                },
                window,
                cx,
            );
            assert_eq!(reader.tree.cursor.as_deref(), Some("image.png"));
            reader.open_note("other.md", None, window, cx);
        });
        visual.run_until_parked();
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        reader.update_in(visual, |reader, window, cx| {
            reader.image_command(
                &ImageCommand {
                    target: current,
                    command: Command::CopyPath,
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "sentinel"
            )
        });
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            source
        );
    }
}
