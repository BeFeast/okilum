//! Selection-owned native notifications, separate from Markdown indexing.
use super::*;
use chrono::{DateTime, FixedOffset, Local};
use notify::{Event, EventKind, RecursiveMode, Watcher};

fn affects_file(event: &Event, path: &Path) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        && (event.need_rescan() || event.paths.iter().any(|p| p == path))
}

pub(super) fn metadata_label(extension: &str, metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .map(|t| DateTime::<Local>::from(t).fixed_offset());
    details(
        extension,
        metadata.len(),
        modified,
        Local::now().fixed_offset(),
    )
}

fn details(
    extension: &str,
    bytes: u64,
    modified: Option<DateTime<FixedOffset>>,
    now: DateTime<FixedOffset>,
) -> String {
    let size = match bytes {
        1 => "1 byte".into(),
        b if b < 1_000 => format!("{b} bytes"),
        b => {
            let mut value = b as f64;
            let mut size = String::new();
            for unit in ["KB", "MB", "GB", "TB"] {
                value /= 1e3;
                // Round before choosing the unit: 999,950 bytes is "1.0 MB", not "1000.0 KB".
                let rounded = (value * 10.).round() / 10.;
                if rounded < 1e3 || unit == "TB" {
                    size = format!("{rounded:.1} {unit}");
                    break;
                }
            }
            size
        }
    };
    let mut label = format!("{} · {size}", extension.to_uppercase());
    if let Some(modified) = modified {
        let days = (now.date_naive() - modified.date_naive()).num_days();
        let date = match days {
            0 => format!("today {}", modified.format("%H:%M")),
            1 => format!("yesterday {}", modified.format("%H:%M")),
            _ => modified.format("%-d %b %Y %H:%M").to_string(),
        };
        label.push_str(" · ");
        label.push_str(&date);
    }
    label
}

impl Reader {
    pub(super) fn observe_preview_file(
        &self,
        preview: &mut FilePreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !preview.image && !reader_pdf::is_pdf(&preview.rel) {
            return;
        }
        if preview.image {
            preview.image_cache = Some(RetainAllImageCache::new(cx));
        }
        // Watch the containing directory: watching the file itself loses the
        // subscription when an editor or Syncthing replaces its inode.
        let selected = self.vault_root.join(&preview.rel);
        let (send, receive) = async_channel::bounded(1);
        let watch = (|| -> notify::Result<_> {
            // macOS temporary/vault roots can contain a /var symlink; native
            // notifications use the resolved parent spelling.
            let parent = selected
                .parent()
                .unwrap()
                .canonicalize()
                .map_err(notify::Error::io)?;
            let watched = parent.join(selected.file_name().unwrap());
            let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
                if event
                    .as_ref()
                    .map_or(true, |event| affects_file(event, &watched))
                {
                    let _ = send.try_send(());
                }
            })?;
            watcher.watch(&parent, RecursiveMode::NonRecursive)?;
            Ok(watcher)
        })();
        let watcher = match watch {
            Ok(watcher) => watcher,
            Err(error) => {
                reader_toast::error(
                    format!("Automatic preview updates unavailable: {error}"),
                    window,
                    cx,
                );
                return;
            }
        };
        let root = self.vault_root.clone();
        let rel = preview.rel.clone();
        let identity = preview.live_identity.clone();
        preview._live = Some(cx.spawn_in(window, async move |this, cx| {
            // Dropping the selection cancels this task and releases the OS watch.
            let _watcher = watcher;
            while receive.recv().await.is_ok() {
                loop {
                    cx.background_executor()
                        .timer(tessera_core::watch::QUIET_WINDOW)
                        .await;
                    if receive.try_recv().is_err() {
                        break;
                    }
                }
                let (root, rel) = (root.clone(), rel.clone());
                let fresh = cx
                    .background_executor()
                    .spawn(async move { FilePreview::load(&root, &rel) })
                    .await;
                if this
                    .update_in(cx, |this, window, cx| {
                        if this
                            .file_preview
                            .as_ref()
                            .is_some_and(|p| Arc::ptr_eq(&p.live_identity, &identity))
                        {
                            this.refresh_preview_file(fresh, window, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn refresh_preview_file(
        &mut self,
        fresh: anyhow::Result<FilePreview>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.file_preview.as_mut() else {
            return;
        };
        if let Some(cache) = &preview.image_cache {
            cache.update(cx, |cache, cx| cache.clear(window, cx));
        }
        #[cfg(target_os = "macos")]
        cx.remove_asset::<HeicImage>(&preview.path);
        match fresh {
            Ok(fresh) => {
                preview.path = fresh.path;
                preview.details = fresh.details;
                preview.unavailable = None;
                if let Some(pdf) = &preview.pdf {
                    pdf.update(cx, |pdf, cx| pdf.reload(preview.path.clone(), cx));
                }
                #[cfg(target_os = "macos")]
                if preview.thumbnail.is_some() {
                    preview.thumbnail = Some(cx.new(|cx| {
                        reader_thumbnail::Thumbnail::new(
                            self.vault_root.clone(),
                            preview.rel.clone(),
                            cx,
                        )
                    }));
                }
            }
            Err(_) => {
                // Do not retain a stale page/image or load a redirected target.
                if let Some(pdf) = &preview.pdf {
                    pdf.update(cx, |pdf, cx| pdf.source_unavailable(window, cx));
                }
                preview.unavailable = Some("This file couldn’t be read. The preview will update when it becomes available.".into());
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn notifications_ignore_reads_and_other_files_but_keep_both_rename_endpoints() {
        let path = PathBuf::from("folder/picture.svg");
        let other = PathBuf::from("folder/other.svg");
        assert!(!affects_file(
            &Event::new(EventKind::Access(notify::event::AccessKind::Read)).add_path(path.clone()),
            &path
        ));
        assert!(!affects_file(
            &Event::new(EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any
            )))
            .add_path(other.clone()),
            &path
        ));
        let rename = Event::new(EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )))
        .add_path(other)
        .add_path(path.clone());
        assert!(affects_file(&rename, &path));
        assert!(affects_file(
            &Event::new(EventKind::Remove(notify::event::RemoveKind::File)).add_path(path.clone()),
            &path
        ));
    }
    #[test]
    fn metadata_uses_human_size_and_local_calendar_days() {
        let now = DateTime::parse_from_rfc3339("2026-10-08T09:41:00+03:00").unwrap();
        assert_eq!(
            details("png", 1_200_000, Some(now), now),
            "PNG · 1.2 MB · today 09:41"
        );
        let yesterday = DateTime::parse_from_rfc3339("2026-10-07T23:59:00+03:00").unwrap();
        assert_eq!(
            details("svg", 1200, Some(yesterday), now),
            "SVG · 1.2 KB · yesterday 23:59"
        );
        assert_eq!(details("pdf", 12, None, now), "PDF · 12 bytes");
        assert_eq!(details("jpg", 1_956_930, None, now), "JPG · 2.0 MB");
        assert_eq!(details("jpg", 999_950, None, now), "JPG · 1.0 MB");
        assert_eq!(details("bin", 1_000_000_000_000, None, now), "BIN · 1.0 TB");
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn reader_window<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        cx.executor().allow_parking();
        cx.update(gpui_component::init);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.to_path_buf()),
                        defer_loading: true,
                        session_directory: Some(root.join("state")),
                        panel_settings_override: Some(root.join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        (reader.unwrap(), visual)
    }
    #[track_caller]
    fn settle(
        visual: &mut VisualTestContext,
        reader: &Entity<Reader>,
        done: impl Fn(&Reader, &App, &std::collections::HashMap<&str, Bounds<Pixels>>) -> bool,
    ) {
        let start = std::time::Instant::now();
        loop {
            // OS notifications run on a real thread; the debounce timer uses
            // GPUI's independently controlled test clock.
            visual.executor().advance_clock(Duration::from_millis(50));
            visual.run_until_parked();
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            let bounds: std::collections::HashMap<_, _> = [
                "reader-block-image",
                "reader-image-unreadable",
                "reader-file-unavailable",
            ]
            .into_iter()
            .filter_map(|id| visual.debug_bounds(id).map(|bounds| (id, bounds)))
            .collect();
            if reader.read_with(visual, |reader, cx| done(reader, cx, &bounds)) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "native preview update did not arrive; bounds={bounds:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn replace(root: &Path, name: &str, bytes: &[u8]) {
        let temp = root.join("incoming.tmp");
        std::fs::write(&temp, bytes).unwrap();
        // std::fs::rename cannot overwrite on Windows; the gap also models Sync.
        std::fs::remove_file(root.join(name)).unwrap();
        std::fs::rename(temp, root.join(name)).unwrap();
    }

    #[gpui::test]
    fn native_pdf_notifications_reload_corruption_replacement_and_restore(cx: &mut TestAppContext) {
        fn center_pixel(image: &RenderImage) -> &[u8] {
            let size = image.size(0);
            let offset = ((size.height.0 / 2 * size.width.0 + size.width.0 / 2) * 4) as usize;
            &image.as_bytes(0).unwrap()[offset..offset + 4]
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pdf");
        let one = crate::pdf_engine::fixture::pdf(&[(300., 300., [1., 0., 0.])]);
        std::fs::write(&path, &one).unwrap();
        let (reader, visual) = reader_window(cx, dir.path());
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("doc.pdf", window, cx)
        });
        settle(visual, &reader, |r, cx, _| {
            let pdf = r.pdf_viewer().unwrap().read(cx);
            pdf.page_count() == Some(1) && pdf.cached_page_image(0).is_some()
        });
        let viewer = reader.read_with(visual, |r, _| r.pdf_viewer().unwrap().clone());
        let old_image = viewer.read_with(visual, |v, _| v.cached_page_image(0).unwrap());
        assert_eq!(center_pixel(&old_image), [0, 0, 255, 255]);
        visual.update(|window, _| {
            assert!(
                window.has_image_atlas_entry(&old_image),
                "painted PDF positive control"
            );
        });
        reader.update(visual, |r, _| {
            assert!(
                r.watcher.is_none(),
                "works without a recursive vault watcher"
            );
            assert!(
                r.file_preview.as_ref().unwrap()._live.is_some(),
                "native subscription positive control"
            );
        });
        // The replacement has the same page count, byte size and mtime. Frame
        // polling cannot discover it through the old metadata revision check;
        // native notification must evict the red pixels and paint blue pixels.
        let blue = crate::pdf_engine::fixture::pdf(&[(300., 300., [0., 0., 1.])]);
        assert_eq!(blue.len(), one.len());
        let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        let replacement = dir.path().join("same-count.tmp");
        std::fs::write(&replacement, &blue).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp);
        settle(visual, &reader, |r, cx, _| {
            let pdf = r.pdf_viewer().unwrap().read(cx);
            pdf.page_count() == Some(1)
                && pdf.cached_page_image(0).is_some_and(|image| {
                    image.id != old_image.id && center_pixel(&image) == [255, 0, 0, 255]
                })
        });
        visual.update(|window, _| {
            assert!(
                !window.has_image_atlas_entry(&old_image),
                "replaced PDF pixels leave atlas"
            );
        });
        std::fs::write(&path, b"corrupt incoming PDF").unwrap();
        settle(visual, &reader, |r, cx, _| {
            !r.pdf_viewer().unwrap().read(cx).is_available()
        });
        let two = crate::pdf_engine::fixture::pdf(&[
            (300., 300., [0., 1., 0.]),
            (300., 300., [0., 0., 1.]),
        ]);
        replace(dir.path(), "doc.pdf", &two);
        settle(visual, &reader, |r, cx, _| {
            r.pdf_viewer().unwrap().read(cx).page_count() == Some(2)
        });
        let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        let fresh = dir.path().join("incoming.tmp");
        std::fs::write(&fresh, vec![b'x'; two.len()]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&fresh, &path).unwrap();
        settle(visual, &reader, |r, cx, _| {
            !r.pdf_viewer().unwrap().read(cx).is_available()
        });
        std::fs::remove_file(&path).unwrap();
        settle(visual, &reader, |r, _, _| {
            r.file_preview.as_ref().unwrap().unavailable.is_some()
        });
        std::fs::write(&path, &one).unwrap();
        settle(visual, &reader, |r, cx, _| {
            r.file_preview.as_ref().unwrap().unavailable.is_none()
                && r.pdf_viewer().unwrap().read(cx).page_count() == Some(1)
        });
        reader.read_with(visual, |r, _| {
            assert_eq!(r.selected_file(), "doc.pdf");
            assert_eq!(r.navigation.history, ["doc.pdf"]);
            assert_eq!(
                *r.pdf_viewer().unwrap(),
                viewer,
                "refresh preserves the selected viewer"
            );
        });
        assert_eq!(std::fs::read(path).unwrap(), one);
    }

    fn svg(width: usize, height: usize, color: &str) -> Vec<u8> {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"><rect width="{width}" height="{height}" fill="{color}"/></svg>"#).into_bytes()
    }
    #[gpui::test]
    fn native_image_notifications_evict_cached_pixels_and_recover(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.svg");
        let first = svg(300, 150, "red");
        std::fs::write(&path, &first).unwrap();
        let (reader, visual) = reader_window(cx, dir.path());
        reader.update_in(visual, |r, window, cx| {
            r.preview_file("image.svg", window, cx)
        });
        settle(visual, &reader, |_, _, v| {
            v.get("reader-block-image")
                .is_some_and(|b| (b.size.height - px(150.)).abs() < px(1.))
        });
        let cache = reader.read_with(visual, |r, _| {
            r.file_preview
                .as_ref()
                .unwrap()
                .image_cache
                .clone()
                .unwrap()
        });
        assert_eq!(
            cache.read_with(visual, |c, _| c.len()),
            1,
            "decoded-image positive control"
        );
        std::fs::write(&path, b"not an image").unwrap();
        settle(visual, &reader, |_, _, v| {
            v.get("reader-image-unreadable").is_some()
        });
        let last = svg(400, 400, "blue");
        replace(dir.path(), "image.svg", &last);
        settle(visual, &reader, |_, _, v| {
            v.get("reader-block-image")
                .is_some_and(|b| (b.size.height - px(400.)).abs() < px(1.))
        });
        std::fs::remove_file(&path).unwrap();
        settle(visual, &reader, |r, _, _| {
            r.file_preview.as_ref().unwrap().unavailable.is_some()
        });
        std::fs::write(&path, &first).unwrap();
        settle(visual, &reader, |r, _, v| {
            r.file_preview.as_ref().unwrap().unavailable.is_none()
                && v.get("reader-block-image")
                    .is_some_and(|b| (b.size.height - px(150.)).abs() < px(1.))
        });
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let outside_path = outside.path().join("outside.svg");
            std::fs::write(&outside_path, &last).unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&outside_path, &path).unwrap();
            settle(visual, &reader, |r, _, _| {
                r.file_preview.as_ref().unwrap().unavailable.is_some()
            });
            assert_eq!(std::fs::read(&outside_path).unwrap(), last);
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, &first).unwrap();
            settle(visual, &reader, |r, _, v| {
                r.file_preview.as_ref().unwrap().unavailable.is_none()
                    && v.get("reader-block-image")
                        .is_some_and(|b| (b.size.height - px(150.)).abs() < px(1.))
            });
        }
        let old_identity = reader.read_with(visual, |r, _| {
            Arc::downgrade(&r.file_preview.as_ref().unwrap().live_identity)
        });
        std::fs::write(dir.path().join("other.svg"), &last).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.preview_file("other.svg", window, cx)
        });
        std::fs::write(&path, b"old selection changed").unwrap();
        settle(visual, &reader, |r, _, v| {
            r.selected_file() == "other.svg"
                && v.get("reader-block-image")
                    .is_some_and(|b| (b.size.height - px(400.)).abs() < px(1.))
        });
        assert!(
            old_identity.upgrade().is_none(),
            "old selection and observer task released"
        );
        assert_eq!(std::fs::read(dir.path().join("other.svg")).unwrap(), last);
    }
}
