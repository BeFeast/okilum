//! Hover details for a rendered local image (#1622). The tooltip shows the
//! captured image's vault-relative path at once; its file size and pixel
//! dimensions are read off the UI thread from that exact path, from the image
//! header only, and kept only while that tooltip lives. A result is accepted
//! only for the same note/generation and the same file revision it was read
//! from. Remote, missing, ambiguous, outside-vault and iCloud placeholder
//! images are never read, fetched or downloaded on hover.
use super::*;
use gpui_component::ActiveTheme as _;
use std::time::SystemTime;

/// The most bytes read to find an image's dimensions. Headers sit at the
/// start of the file; a format that keeps them further in reports its
/// dimensions as unavailable instead of being read whole.
const HEADER_LIMIT: u64 = 1024 * 1024;
/// A file replaced while its header was read is read again, at most this often.
const ATTEMPTS: usize = 3;

pub(super) const HINT: &str = "Right-click for image actions";
const LOADING: &str = "Reading size…";
const UNAVAILABLE: &str = "Size and dimensions unavailable";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Facts {
    pub(super) bytes: u64,
    /// `None` when the header is truncated, unreadable or unsupported; never
    /// reported as zero.
    pub(super) pixels: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Loading,
    Ready(Facts),
    Unavailable,
}

/// Identifies one revision of a file at one path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Revision {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

impl Revision {
    fn of(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: (metadata.dev(), metadata.ino()),
        }
    }
}

fn stat(root: &Path, relative: &str) -> anyhow::Result<(PathBuf, Revision)> {
    let path = reader_files::checked_path(root, relative)?;
    let metadata = std::fs::metadata(&path)?;
    anyhow::ensure!(metadata.is_file(), "Image file not found");
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        // Opening a dataless iCloud file would download it.
        anyhow::ensure!(
            metadata.st_flags() & 0x4000_0000 == 0,
            "Download this image from iCloud first"
        );
    }
    Ok((path, Revision::of(&metadata)))
}

fn read_once(root: &Path, relative: &str) -> anyhow::Result<(Revision, Facts)> {
    let (path, revision) = stat(root, relative)?;
    let header = std::io::BufReader::new(std::fs::File::open(path)?.take(HEADER_LIMIT));
    let pixels = image::ImageReader::new(header)
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .filter(|(width, height)| *width > 0 && *height > 0);
    Ok((
        revision.clone(),
        Facts {
            bytes: revision.len,
            pixels,
        },
    ))
}

/// Runs on the background executor. `hold` lets tests pause between reading a
/// revision and confirming it is still current.
async fn lookup(
    root: PathBuf,
    relative: String,
    mut hold: Option<futures::channel::oneshot::Receiver<()>>,
) -> State {
    for _ in 0..ATTEMPTS {
        let Ok((revision, facts)) = read_once(&root, &relative) else {
            return State::Unavailable;
        };
        if let Some(hold) = hold.take() {
            let _ = hold.await;
        }
        match stat(&root, &relative) {
            Ok((_, current)) if current == revision => return State::Ready(facts),
            Ok(_) => continue,
            Err(_) => return State::Unavailable,
        }
    }
    State::Unavailable
}

fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b if b >= 1_000 => format!("{:.1} KB", b as f64 / 1e3),
        1 => "1 byte".into(),
        b => format!("{b} bytes"),
    }
}

/// One hovered image. It lives exactly as long as its tooltip.
pub(super) struct Hover {
    pub(super) target: Target,
    /// `None` when this image is never read on hover.
    pub(super) state: Option<State>,
    _task: Option<Task<()>>,
}

impl Hover {
    fn new(target: Target, reader: WeakEntity<Reader>, cx: &mut Context<Self>) -> Self {
        let readable = target.url.is_some() && target.local.is_some() && !target.placeholder;
        let task = target
            .relative
            .clone()
            .filter(|_| readable)
            .map(|relative| {
                let root = target.root.clone();
                let captured = target.clone();
                #[cfg(test)]
                let hold = tests::HOLD.with(|hold| hold.borrow_mut().take());
                #[cfg(not(test))]
                let hold = None;
                cx.spawn(async move |this, cx| {
                    let state = cx
                        .background_executor()
                        .spawn(lookup(root, relative, hold))
                        .await;
                    let current = reader
                        .read_with(cx, |reader, _| captured.matches(reader))
                        .unwrap_or(false);
                    let _ = this.update(cx, |hover, cx| {
                        // A result read for another note, vault or generation
                        // is dropped; only its honest absence remains.
                        hover.state = Some(if current { state } else { State::Unavailable });
                        cx.notify();
                    });
                })
            });
        Self {
            state: task.as_ref().map(|_| State::Loading),
            target,
            _task: task,
        }
    }

    /// Tooltip lines, the first one primary and the rest secondary.
    pub(super) fn lines(&self) -> Vec<String> {
        let target = &self.target;
        let Some(relative) = target.relative.as_ref().filter(|_| target.url.is_some()) else {
            return vec![target.notice().unwrap_or(HINT).to_owned()];
        };
        if let Some(notice) = target.notice() {
            return vec![relative.clone(), notice.to_owned()];
        }
        let details = match self.state {
            Some(State::Loading) => LOADING.to_owned(),
            Some(State::Ready(Facts {
                bytes,
                pixels: Some((width, height)),
            })) => format!("{} · {width} × {height} px", size_label(bytes)),
            Some(State::Ready(Facts {
                bytes,
                pixels: None,
            })) => format!("{} · dimensions unavailable", size_label(bytes)),
            Some(State::Unavailable) | None => UNAVAILABLE.to_owned(),
        };
        vec![relative.clone(), details, HINT.to_owned()]
    }
}

/// Builds the tooltip for a rendered image. Called by GPUI once per shown
/// tooltip, never per frame.
pub(super) fn tooltip(
    reader: WeakEntity<Reader>,
    target: Target,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let hover = cx.new(|cx| Hover::new(target, reader, cx));
    #[cfg(test)]
    tests::LAST.with(|last| *last.borrow_mut() = Some(hover.clone()));
    gpui_component::tooltip::Tooltip::element(move |_, cx| {
        let muted = cx.theme().muted_foreground;
        let mut lines = hover.read(cx).lines().into_iter();
        let mut content = div()
            .debug_selector(|| "image-tooltip".into())
            .flex()
            .flex_col();
        if let Some(first) = lines.next() {
            content = content.child(first);
        }
        for line in lines {
            content = content.child(div().text_color(muted).child(line));
        }
        content
    })
    .build(window, cx)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    thread_local! {
        pub(in super::super) static HOLD:
            std::cell::RefCell<Option<futures::channel::oneshot::Receiver<()>>> =
            const { std::cell::RefCell::new(None) };
        pub(in super::super) static LAST: std::cell::RefCell<Option<Entity<Hover>>> =
            const { std::cell::RefCell::new(None) };
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, height)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    fn hold() -> futures::channel::oneshot::Sender<()> {
        let (release, hold) = futures::channel::oneshot::channel();
        HOLD.with(|slot| *slot.borrow_mut() = Some(hold));
        release
    }

    /// Hovers the one rendered image of the current note through GPUI's own
    /// tooltip path and returns the hover state its tooltip was built with.
    fn show(visual: &mut VisualTestContext) -> Entity<Hover> {
        LAST.with(|last| last.borrow_mut().take());
        visual.simulate_mouse_move(point(px(2.), px(2.)), None, Modifiers::default());
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(2));
        visual.run_until_parked();
        let bounds = visual
            .debug_bounds("image-action-surface")
            .expect("the note renders its image");
        visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(600));
        visual.run_until_parked();
        LAST.with(|last| last.borrow_mut().take())
            .expect("hovering the rendered image builds its tooltip")
    }

    fn open(reader: &Entity<Reader>, visual: &mut VisualTestContext, note: &str) {
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note(note, None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert_eq!(reader.current_rel, note));
    }

    fn ready(hover: &Entity<Hover>, visual: &mut VisualTestContext) -> Facts {
        match hover.read_with(visual, |hover, _| hover.state) {
            Some(State::Ready(facts)) => facts,
            other => panic!("expected image details, got {other:?}"),
        }
    }

    #[gpui::test]
    fn hovering_a_rendered_image_reads_its_exact_details_off_the_ui_thread(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        let root = root.canonicalize().unwrap();
        // Lowercase: before path reconciliation, Vault::resolve_asset_path
        // lowercases the target but compares it ASCII-case-insensitively, so
        // an uppercase non-ASCII asset name does not render yet (pre-existing).
        let unicode = "attachments/привет image.png";
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("root.png", png(2, 3)),
            (unicode, png(5, 7)),
            ("attachments/photo.jpg", jpeg(4, 6)),
            ("a/same.png", png(8, 9)),
            ("b/same.png", png(10, 11)),
            ("trunc.png", png(3, 3)[..16].to_vec()),
            ("root.md", b"# Root\n\n![[root.png]]\n".to_vec()),
            (
                "unicode.md",
                format!("# U\n\n![[{unicode}]]\n").into_bytes(),
            ),
            ("jpeg.md", b"# J\n\n![[attachments/photo.jpg]]\n".to_vec()),
            ("a.md", b"# A\n\n![[a/same.png]]\n".to_vec()),
            ("b.md", b"# B\n\n![[b/same.png]]\n".to_vec()),
            ("ambiguous.md", b"# Ambiguous\n\n![[same.png]]\n".to_vec()),
            ("missing.md", b"# Missing\n\n![[missing.png]]\n".to_vec()),
            ("trunc.md", b"# Truncated\n\n![[trunc.png]]\n".to_vec()),
        ];
        for (relative, bytes) in &files {
            std::fs::write(root.join(relative), bytes).unwrap();
        }
        let len = |relative: &str| std::fs::metadata(root.join(relative)).unwrap().len();
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
                        note: Some("root.md".into()),
                        session_directory: Some(fixture.path().join("state")),
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
        let observe = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| {
                let reader = reader.read(cx);
                (
                    reader.current_rel.clone(),
                    reader.tree.cursor.clone(),
                    reader.navigation.preparation_generation,
                    window.focused(cx),
                )
            })
        };

        // The path is shown while the held read is pending; the details
        // replace the loading line once the background read completes.
        let before = observe(visual);
        let release = hold();
        let hover = show(visual);
        assert!(
            visual.debug_bounds("image-tooltip").is_some(),
            "the hover tooltip is on screen"
        );
        hover.read_with(visual, |hover, _| {
            assert_eq!(hover.state, Some(State::Loading));
            assert_eq!(hover.lines(), ["root.png", LOADING, HINT]);
            assert_eq!(hover.target.relative.as_deref(), Some("root.png"));
            assert_eq!(hover.target.local.as_deref(), Some(&*root.join("root.png")));
        });
        release.send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(
            ready(&hover, visual),
            Facts {
                bytes: len("root.png"),
                pixels: Some((2, 3)),
            },
            "positive control: the local image's header was actually read"
        );
        hover.read_with(visual, |hover, _| {
            assert_eq!(
                hover.lines()[1],
                format!("{} · 2 × 3 px", size_label(len("root.png")))
            )
        });
        assert_eq!(observe(visual), before, "hover leaves navigation and focus");
        // The hovered target is the one Copy path still copies.
        let target = hover.read_with(visual, |hover, _| hover.target.clone());
        reader.update_in(visual, |reader, window, cx| {
            reader.image_command(
                &ImageCommand {
                    target,
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
                okilum_core::vault::display_path(&root.join("root.png"))
            )
        });

        // A new revision at the same path, written after the held read, is
        // what the tooltip reports; the stale header is not.
        let release = hold();
        let hover = show(visual);
        std::fs::write(root.join("root.png"), png(12, 13)).unwrap();
        release.send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(
            ready(&hover, visual),
            Facts {
                bytes: len("root.png"),
                pixels: Some((12, 13)),
            }
        );
        assert_ne!(len("root.png"), files[0].1.len() as u64);
        let hover = show(visual);
        assert_eq!(ready(&hover, visual).pixels, Some((12, 13)));

        open(&reader, visual, "unicode.md");
        let hover = show(visual);
        assert_eq!(
            ready(&hover, visual),
            Facts {
                bytes: len(unicode),
                pixels: Some((5, 7)),
            }
        );
        hover.read_with(visual, |hover, _| assert_eq!(hover.lines()[0], unicode));

        open(&reader, visual, "jpeg.md");
        let hover = show(visual);
        assert_eq!(ready(&hover, visual).pixels, Some((4, 6)));
        hover.read_with(visual, |hover, _| {
            assert_eq!(hover.lines()[0], "attachments/photo.jpg")
        });

        // Two files share a basename; each tooltip reports its own file, and
        // a read held across a note switch is rejected when it completes.
        open(&reader, visual, "a.md");
        let hover = show(visual);
        assert_eq!(ready(&hover, visual).pixels, Some((8, 9)));
        hover.read_with(visual, |hover, _| {
            assert_eq!(hover.lines()[0], "a/same.png")
        });
        let release = hold();
        let held = show(visual);
        assert_eq!(
            held.read_with(visual, |hover, _| hover.state),
            Some(State::Loading)
        );
        open(&reader, visual, "b.md");
        release.send(()).unwrap();
        visual.run_until_parked();
        held.read_with(visual, |hover, _| {
            assert_eq!(hover.state, Some(State::Unavailable));
            assert_eq!(hover.lines(), ["a/same.png", UNAVAILABLE, HINT]);
        });
        let hover = show(visual);
        assert_eq!(
            ready(&hover, visual),
            Facts {
                bytes: len("b/same.png"),
                pixels: Some((10, 11)),
            }
        );
        hover.read_with(visual, |hover, _| {
            assert_eq!(hover.lines()[0], "b/same.png")
        });

        // Unresolved images keep their notice and are never read.
        for note in ["ambiguous.md", "missing.md"] {
            open(&reader, visual, note);
            let hover = show(visual);
            visual.run_until_parked();
            hover.read_with(visual, |hover, _| {
                assert_eq!(hover.state, None, "{note}");
                assert_eq!(
                    hover.lines(),
                    ["Image file not found, outside this vault, or ambiguous."]
                );
            });
        }

        // A truncated header keeps the path and size, never zero dimensions.
        open(&reader, visual, "trunc.md");
        let hover = show(visual);
        assert_eq!(
            ready(&hover, visual),
            Facts {
                bytes: 16,
                pixels: None,
            }
        );
        hover.read_with(visual, |hover, _| {
            assert_eq!(
                hover.lines(),
                ["trunc.png", "16 bytes · dimensions unavailable", HINT]
            )
        });
        std::fs::remove_file(root.join("trunc.png")).unwrap();
        let hover = show(visual);
        hover.read_with(visual, |hover, _| {
            assert_eq!(hover.state, Some(State::Unavailable));
            assert_eq!(hover.lines(), ["trunc.png", UNAVAILABLE, HINT]);
        });

        // Remote and iCloud placeholder images are not read, fetched or
        // revealed by a hover; the same local file without the placeholder
        // flag is read through the same tooltip builder.
        let local = reader.read_with(visual, |reader, _| {
            Target::captured(
                reader,
                Some(
                    url::Url::from_file_path(root.join("root.png"))
                        .unwrap()
                        .as_str(),
                ),
            )
        });
        let remote = reader.read_with(visual, |reader, _| {
            Target::captured(reader, Some("https://example.invalid/remote.png"))
        });
        let placeholder = Target {
            placeholder: true,
            ..local.clone()
        };
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        let before = observe(visual);
        let build = |target: Target, visual: &mut VisualTestContext| {
            visual.update(|window, cx| {
                let _ = tooltip(reader.downgrade(), target, window, cx);
            });
            let hover = LAST.with(|last| last.borrow_mut().take()).unwrap();
            visual.run_until_parked();
            hover
        };
        let hover = build(remote, visual);
        hover.read_with(visual, |hover, _| {
            assert_eq!(hover.state, None);
            assert_eq!(hover.lines(), [HINT]);
        });
        let hover = build(placeholder, visual);
        hover.read_with(visual, |hover, _| {
            assert_eq!(hover.state, None);
            assert_eq!(
                hover.lines(),
                [
                    "root.png",
                    "Download this image from iCloud before opening or copying it."
                ]
            );
        });
        let hover = build(local, visual);
        assert_eq!(ready(&hover, visual).pixels, Some((12, 13)));
        assert_eq!(observe(visual), before);
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "sentinel"
            )
        });

        // Rendered Live Preview uses the same tooltip and leaves the source
        // text, selection and focus alone.
        open(&reader, visual, "root.md");
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_live_preview(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| reader.source_input().unwrap());
        let editor = |visual: &mut VisualTestContext| {
            input.read_with(visual, |input, _| {
                (input.text().to_string(), input.selected_range())
            })
        };
        let (text, selection) = editor(visual);
        let before = observe(visual);
        let hover = show(visual);
        assert_eq!(ready(&hover, visual).pixels, Some((12, 13)));
        assert_eq!(editor(visual), (text, selection));
        assert_eq!(observe(visual), before);
        for (relative, bytes) in &files {
            if relative.ends_with(".md") {
                assert_eq!(&std::fs::read(root.join(relative)).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn size_labels_use_decimal_units() {
        assert_eq!(size_label(1), "1 byte");
        assert_eq!(size_label(68), "68 bytes");
        assert_eq!(size_label(1_250), "1.2 KB");
        assert_eq!(size_label(3_400_000), "3.4 MB");
    }
}
