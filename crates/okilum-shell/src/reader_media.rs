//! Read-only local media surfaces. Opening a document never launches another app.
use super::*;

pub(super) fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            ["mp4", "webm", "mov", "m4v", "mkv", "avi"]
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
}

/// File URLs use ordinary Windows paths; vault roots can retain a verbatim
/// prefix. Resolve both spellings before comparing their vault identities.
pub(super) fn relative_file(root: &Path, path: &Path) -> anyhow::Result<String> {
    let root = okilum_core::vault::canonical_root(root)?;
    let path = path.canonicalize()?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("The media file is outside this vault"))?;
    Ok(okilum_core::vault::note_path(relative))
}

pub(super) fn video_card(root: &Path, path: &Path, cx: &App) -> AnyElement {
    let relative = relative_file(root, path).ok();
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let details = std::fs::metadata(path)
        .map(|m| format!("Video · {} bytes", m.len()))
        .unwrap_or_else(|_| "This video couldn’t be read.".into());
    let mut card = v_flex()
        .id(SharedString::from(path.to_string_lossy().into_owned()))
        .gap_3()
        .p_4()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_md()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(details),
        );
    if let Some(rel) = relative {
        let root = root.to_path_buf();
        card = card.child(
            Button::new("video-open-system")
                .label("Open with default app")
                .debug_selector(|| "video-open-system".into())
                .on_click(move |_, window, cx| {
                    reader_files::run(reader_files::FileAction::Open, &root, &rel, window, cx);
                }),
        );
    }
    card.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn media_file_urls_share_the_canonical_vault_identity_and_reject_outside_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let file = root.join("clip #1.MOV");
        std::fs::write(&file, b"video").unwrap();
        let canonical = okilum_core::vault::canonical_root(&root).unwrap();
        let from_url = url::Url::from_file_path(canonical.join("clip #1.MOV"))
            .unwrap()
            .to_file_path()
            .unwrap();
        assert_eq!(relative_file(&canonical, &from_url).unwrap(), "clip #1.MOV");
        #[cfg(windows)]
        assert_ne!(
            canonical.components().next(),
            from_url.components().next(),
            "the control must exercise verbatim versus ordinary prefixes"
        );
        let outside = dir.path().join("outside.mp3");
        std::fs::write(&outside, b"audio").unwrap();
        assert!(relative_file(&root, &outside).is_err());
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            assert_eq!(relative_file(&alias, &from_url).unwrap(), "clip #1.MOV");
            std::os::unix::fs::symlink(&outside, root.join("escape.mp3")).unwrap();
            assert!(relative_file(&root, &root.join("escape.mp3")).is_err());
        }
    }

    struct Card(PathBuf);
    impl Render for Card {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            video_card(self.0.parent().unwrap(), &self.0, cx)
        }
    }

    #[gpui::test]
    fn video_card_only_opens_its_checked_file_after_an_explicit_click(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip #1.MOV");
        std::fs::write(&path, b"synthetic movie identity").unwrap();
        cx.update(crate::test_init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let card = cx.new(|_| Card(path.clone()));
            Root::new(card, window, cx)
        });
        visual.run_until_parked();
        assert!(visual.opened_url().is_none());
        let button = visual
            .debug_bounds("video-open-system")
            .expect("the actual button must render");
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        let expected = url::Url::from_file_path(path.canonicalize().unwrap()).unwrap();
        assert_eq!(visual.opened_url().as_deref(), Some(expected.as_str()));
        assert_eq!(std::fs::read(&path).unwrap(), b"synthetic movie identity");
        // Deletion between render and click must never open a stale identity.
        std::fs::remove_file(&path).unwrap();
        visual.update(|_, cx| cx.open_url("https://sentinel.invalid"));
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            visual.opened_url().as_deref(),
            Some("https://sentinel.invalid")
        );
    }

    #[test]
    fn video_extensions_do_not_capture_audio_or_images() {
        for file in [
            "clip.mp4",
            "clip.WEBM",
            "clip.MOV",
            "clip.m4v",
            "clip.mkv",
            "clip.avi",
        ] {
            assert!(is_video(Path::new(file)));
        }
        for file in ["voice.mp3", "voice.wav", "image.jpg", "clip.mp4.md", "mp4"] {
            assert!(!is_video(Path::new(file)));
        }
    }

    #[gpui::test]
    fn showcase_images_use_the_shipping_decoder_and_apply_exif(cx: &mut TestAppContext) {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/viewers");
        cx.update(|cx| {
            for (file, format) in [
                ("photo.jpg", ImageFormat::Jpeg),
                ("progressive.jpeg", ImageFormat::Jpeg),
                ("oriented.JPG", ImageFormat::Jpeg),
                ("bitmap.bmp", ImageFormat::Bmp),
            ] {
                let preview =
                    reader_files::FilePreview::load(&root, &format!("images/{file}")).unwrap();
                assert!(preview.image);
                let decoded = Image::from_bytes(format, std::fs::read(preview.path).unwrap())
                    .to_image_data(cx.svg_renderer())
                    .unwrap();
                assert_eq!(
                    decoded.size(0),
                    size(DevicePixels(240), DevicePixels(144)),
                    "{file}"
                );
                // GPUI uploads BGRA. EXIF must place red at top left and green at bottom right.
                let pixels = decoded.as_bytes(0).unwrap();
                let red = &pixels[(30 * 240 + 30) * 4..][..4];
                let green = &pixels[(110 * 240 + 180) * 4..][..4];
                assert!(red[2] > red[1] * 2, "{file}: {red:?}");
                assert!(green[1] > green[2], "{file}: {green:?}");
            }
        });
    }
}
