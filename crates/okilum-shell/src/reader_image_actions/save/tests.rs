use super::*;
use ::core::prelude::v1::test;

const NOTE: &str = "# Images\r\n\r\nBefore **selection**.\r\n\r\n![[image.png]]\r\n";
struct Fixture<'a> {
    reader: Entity<Reader>,
    visual: &'a mut VisualTestContext,
    root: PathBuf,
    _dir: tempfile::TempDir,
}
fn open(cx: &mut TestAppContext) -> Fixture<'_> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("note.md"), NOTE).unwrap();
    std::fs::write(root.join("image.png"), original_bytes()).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
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
                    session_directory: Some(dir.path().join("state")),
                    index_dir: Some(dir.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(960.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    Fixture {
        reader: entity.unwrap(),
        visual,
        root,
        _dir: dir,
    }
}
fn original_bytes() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 3)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    // The exact source includes trailing bytes that Copy image's PNG encoder
    // would discard. Save As must not invoke that encoder.
    let mut bytes = out.into_inner();
    bytes.extend_from_slice(b"\0original trailer\xff\r\n");
    bytes
}
impl Fixture<'_> {
    fn target(&self) -> Target {
        self.reader.read_with(self.visual, |reader, cx| {
            let mut target = Target::captured(
                reader,
                Some(
                    url::Url::from_file_path(self.root.join("image.png"))
                        .unwrap()
                        .as_str(),
                ),
            );
            target.accepted_source = reader.content.read(cx).document_source();
            assert!(
                target.accepted_source.is_some(),
                "accepted Read source exists"
            );
            target
        })
    }
    fn menu(&mut self) {
        let bounds = self
            .visual
            .debug_bounds("image-action-surface")
            .expect("real rendered image mounted");
        self.visual
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
        self.visual
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
        self.visual.run_until_parked();
        self.visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(self
            .reader
            .read_with(self.visual, |reader, _| reader.file_menu.is_some()));
    }
    fn pick(&mut self, label: &'static str) {
        let bounds = self.visual.debug_bounds(label).expect(label);
        self.visual
            .simulate_click(bounds.center(), Modifiers::default());
        self.visual.run_until_parked();
    }
    fn save(&mut self) {
        let target = self.target();
        self.reader.update_in(self.visual, |reader, window, cx| {
            reader.image_command(
                &ImageCommand {
                    target,
                    command: Command::SaveAs,
                },
                window,
                cx,
            )
        });
        self.visual.run_until_parked();
        assert!(
            self.visual.did_prompt_for_new_path(),
            "real Save As picker opened"
        );
    }
    fn changed_render(&mut self) {
        self.reader.update(self.visual, |reader, cx| {
            reader.content.update(cx, |content, cx| {
                content.set_text_with_source(
                    "A later accepted revision",
                    Some("A later accepted revision".into()),
                    cx,
                );
            })
        });
        self.visual.run_until_parked();
    }
}

#[gpui::test]
fn image_popup_copies_resolved_address_and_saves_original_bytes_with_selection_group(
    cx: &mut TestAppContext,
) {
    let mut f = open(cx);
    // Selection formats compose with the one object popup, without replacing
    // image commands. Programmatic select_all avoids mouse-selection assumptions.
    f.reader.update(f.visual, |reader, cx| {
        reader
            .content
            .update(cx, |content, cx| content.select_all(cx))
    });
    f.visual.run_until_parked();
    f.menu();
    assert!(f
        .visual
        .debug_bounds("read-menu-Copy as Markdown")
        .is_some());
    assert!(f
        .visual
        .debug_bounds("read-menu-Copy as Rich Text")
        .is_some());
    f.pick("image-menu-Copy image address");
    f.visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            url::Url::from_file_path(f.root.join("image.png"))
                .unwrap()
                .as_str()
        )
    });
    f.menu();
    f.pick("image-menu-Save image as…");
    assert!(f.visual.did_prompt_for_new_path());
    let out = f.root.join("saved.png");
    let chosen = out.clone();
    f.visual.simulate_new_path_selection(move |_| Some(chosen));
    f.visual.run_until_parked();
    assert_eq!(std::fs::read(&out).unwrap(), original_bytes());
    assert_eq!(
        std::fs::read(f.root.join("image.png")).unwrap(),
        original_bytes()
    );
    assert_eq!(
        std::fs::read(f.root.join("note.md")).unwrap(),
        NOTE.as_bytes()
    );
}

#[gpui::test]
fn image_save_cancel_same_file_and_destination_conflict_preserve_bytes(cx: &mut TestAppContext) {
    let mut f = open(cx);
    f.save();
    f.visual.simulate_new_path_selection(|_| None);
    f.visual.run_until_parked();
    let out = f.root.join("saved.png");
    assert!(!out.exists());
    f.save();
    let source = f.root.join("image.png");
    f.visual.simulate_new_path_selection(move |_| Some(source));
    f.visual.run_until_parked();
    assert_eq!(
        std::fs::read(f.root.join("image.png")).unwrap(),
        original_bytes()
    );
    std::fs::write(&out, b"existing destination").unwrap();
    f.save();
    let chosen = out.clone();
    f.visual.simulate_new_path_selection(move |_| Some(chosen));
    f.visual.run_until_parked();
    assert_eq!(std::fs::read(&out).unwrap(), b"existing destination");
    assert_eq!(
        std::fs::read(f.root.join("note.md")).unwrap(),
        NOTE.as_bytes()
    );
}

#[gpui::test]
fn stale_picker_document_or_image_cannot_publish_copy(cx: &mut TestAppContext) {
    let mut f = open(cx);
    let out = f.root.join("copy.png");
    f.save();
    // Same note/navigation generation, but a different accepted render.
    f.changed_render();
    let chosen = out.clone();
    f.visual.simulate_new_path_selection(move |_| Some(chosen));
    f.visual.run_until_parked();
    assert!(!out.exists());
    // Restore the actual accepted note for a positive, then changed-image case.
    f.reader.update(f.visual, |reader, cx| {
        reader.content.update(cx, |content, cx| {
            content.set_text_with_source(NOTE, Some(NOTE.into()), cx)
        })
    });
    f.visual.run_until_parked();
    f.save();
    let chosen = out.clone();
    f.visual.simulate_new_path_selection(move |_| Some(chosen));
    f.visual.run_until_parked();
    assert_eq!(
        std::fs::read(&out).unwrap(),
        original_bytes(),
        "healthy control"
    );
    std::fs::remove_file(&out).unwrap();
    f.save();
    let image = f.root.join("image.png");
    std::fs::remove_file(&image).unwrap();
    std::fs::write(&image, b"different image generation").unwrap();
    let chosen = out.clone();
    f.visual.simulate_new_path_selection(move |_| Some(chosen));
    f.visual.run_until_parked();
    assert!(!out.exists());
    assert_eq!(std::fs::read(image).unwrap(), b"different image generation");
}

// The positive staging probe names the Unix descriptor-relative temporary.
#[cfg(unix)]
#[gpui::test]
fn stale_prepared_completion_is_discarded_before_publication(cx: &mut TestAppContext) {
    let mut f = open(cx);
    let out = f.root.join("copy.png");
    f.save();
    let (release, held) = futures::channel::oneshot::channel();
    HOLD.with(|hold| *hold.borrow_mut() = Some(held));
    let chosen = out.clone();
    f.visual.simulate_new_path_selection(move |_| Some(chosen));
    f.visual.run_until_parked();
    assert!(
        !out.exists(),
        "publication is held after successful preparation"
    );
    assert!(
        std::fs::read_dir(&f.root)
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".okilum-create-")),
        "positive control: staged bytes really exist"
    );
    f.changed_render();
    release.send(()).unwrap();
    f.visual.run_until_parked();
    assert!(!out.exists());
    assert!(
        !std::fs::read_dir(&f.root)
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".okilum-create-")),
        "cancelled staging cleaned up"
    );
}

#[test]
fn remote_identity_capture_and_address_policy_do_not_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let remote = Target {
        root: dir.path().into(),
        note: "note.md".into(),
        generation: 1,
        url: Some("https://example.invalid/img.png?exact=1#fragment".into()),
        local: None,
        relative: None,
        placeholder: false,
        accepted_source: Some("![image](url)".into()),
    };
    let Original::Remote(address) = Original::capture(&remote).unwrap() else {
        panic!("URL-only identity")
    };
    assert_eq!(address, remote.url.as_deref().unwrap());
    assert!(Original::capture(&Target {
        url: Some("data:image/png;base64,AA".into()),
        ..remote.clone()
    })
    .is_err());
    assert!(Original::capture(&Target {
        placeholder: true,
        ..remote
    })
    .is_err());
}
