//! The selected document must not wait for a vault/index rebuild (#2486).
use super::*;

#[derive(Clone, PartialEq, Eq)]
struct Selection {
    root: PathBuf,
    rel: String,
    navigation: u64,
    watcher: u64,
    input: Option<EntityId>,
}
impl Selection {
    fn current(reader: &Reader) -> Option<Self> {
        if !reader.document_ready()
            || reader.current_rel.is_empty()
            || reader.file_preview.is_some()
            || reader
                .loading
                .as_ref()
                .is_some_and(|load| load.active && !load.published)
        {
            return None;
        }
        Some(Self {
            root: reader.vault_root.clone(),
            rel: reader.current_rel.clone(),
            navigation: reader.navigation.generation,
            watcher: reader.watcher_generation,
            input: reader.editing.as_ref().map(|e| e.input.entity_id()),
        })
    }
}

/// One shallow parent subscription survives atomic replacement of the note's
/// inode. Reads, sibling writes and index activity never refresh this document.
struct Watch {
    selection: Selection,
    _watcher: notify::RecommendedWatcher,
    events: std::sync::mpsc::Receiver<()>,
    first: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
}
impl Watch {
    fn new(selection: Selection) -> anyhow::Result<Self> {
        use notify::Watcher;
        let path = Path::new(&selection.rel);
        anyhow::ensure!(
            path.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Invalid selected path"
        );
        let root = selection.root.canonicalize()?;
        anyhow::ensure!(root == selection.root, "Opened root was redirected");
        let selected = root.join(path);
        let parent = selected.parent().unwrap();
        anyhow::ensure!(
            parent.canonicalize()? == parent,
            "Selected folder was redirected"
        );
        let watched = selected.clone();
        let (send, events) = std::sync::mpsc::sync_channel(1);
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.as_ref().map_or(true, |e| affects_file(e, &watched)) {
                    let _ = send.try_send(());
                }
            })?;
        watcher.watch(parent, notify::RecursiveMode::NonRecursive)?;
        let now = std::time::Instant::now();
        // Registration precedes the first reread, closing the selection/watch gap.
        Ok(Self {
            selection,
            _watcher: watcher,
            events,
            first: Some(now),
            last: Some(now),
        })
    }
    fn changed(&mut self) -> bool {
        let now = std::time::Instant::now();
        if self.events.try_recv().is_ok() {
            self.first.get_or_insert(now);
            self.last = Some(now);
        }
        if self
            .last
            .is_some_and(|t| t.elapsed() >= okilum_core::watch::QUIET_WINDOW)
            || self
                .first
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.first = None;
            self.last = None;
            true
        } else {
            false
        }
    }
}
fn affects_file(event: &notify::Event, path: &Path) -> bool {
    !matches!(event.kind, notify::EventKind::Access(_))
        && (event.need_rescan() || event.paths.iter().any(|p| p == path))
}

impl Reader {
    pub(crate) fn has_open_note_watch(&self) -> bool {
        self.open_note_watch
            .as_ref()
            .is_some_and(|(root, rel, navigation)| {
                *root == self.vault_root
                    && *rel == self.current_rel
                    && *navigation == self.navigation.generation
            })
    }

    pub(crate) fn watch_open_note(&mut self, cx: &mut Context<Self>) {
        let handle = self.reader_window;
        cx.spawn(async move |this, cx| {
            let mut watch: Option<Watch> = None;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let Ok(Ok(selection)) = handle.update(cx, |_, _, cx| {
                    this.update(cx, |reader, _| {
                        let selected = Selection::current(reader);
                        if selected.is_none() {
                            reader.open_note_watch = None;
                        }
                        selected
                    })
                }) else {
                    break;
                };
                let Some(selection) = selection else {
                    watch = None;
                    continue;
                };
                let target = selection.clone();
                let (next, changed, error) = cx
                    .background_executor()
                    .spawn(async move {
                        let mut error = None;
                        if watch.as_ref().is_none_or(|w| w.selection != target) {
                            watch = match Watch::new(target) {
                                Ok(watch) => Some(watch),
                                Err(cause) => {
                                    error = Some(format!(
                                        "Automatic note updates unavailable: {cause}"
                                    ));
                                    None
                                }
                            };
                        }
                        let changed = watch.as_mut().is_some_and(|w| w.changed());
                        (watch, changed, error)
                    })
                    .await;
                watch = next;
                let active = watch.is_some();
                if changed || error.is_some() || active {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = this.update(cx, |reader, cx| {
                            if Selection::current(reader).as_ref() == Some(&selection) {
                                if active && !reader.has_open_note_watch() {
                                    reader.open_note_watch = Some((
                                        selection.root.clone(),
                                        selection.rel.clone(),
                                        selection.navigation,
                                    ));
                                } else if !active {
                                    reader.open_note_watch = None;
                                }
                                if let Some(error) = error {
                                    if reader.link_notice.as_deref() != Some(error.as_str())
                                        && reader.link_notice.is_none()
                                    {
                                        reader.link_notice = Some(error.into());
                                        cx.notify();
                                    }
                                } else if changed {
                                    if reader.link_notice.as_deref().is_some_and(|n| {
                                        n.starts_with("Automatic note updates unavailable:")
                                    }) {
                                        reader.link_notice = None;
                                    }
                                    reader.reconcile_published_document(window, cx);
                                }
                            }
                        });
                    });
                }
            }
        })
        .detach();
    }
}
#[cfg(test)]
mod tests {
    use super::super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::EditorState;
    use std::io::Write;

    fn open<'a>(
        cx: &'a mut TestAppContext,
        dir: &Path,
        profile: u8,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let root = dir.join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n\nbase line\n").unwrap();
        std::fs::write(root.join("sibling.md"), "sibling").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: (profile != 2).then(|| root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(dir.join("index")),
                        session_directory: Some(dir.join("state")),
                        panel_settings_override: Some(dir.join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        visual.run_until_parked();
        let reader = reader.unwrap();
        reader.update(visual, |r, _| {
            if profile == 1 {
                assert!(r.share_session().is_some());
            }
            assert_eq!(r.single_file, profile == 2);
        });
        (reader, visual)
    }

    fn settle(
        visual: &mut VisualTestContext,
        reader: &Entity<Reader>,
        label: &str,
        done: impl Fn(&Reader, &App) -> bool,
    ) {
        let start = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(25));
            visual.executor().advance_clock(Duration::from_millis(100));
            visual.run_until_parked();
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
            if reader.read_with(visual, |r, cx| done(r, cx)) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "open-note refresh exceeded five seconds: {label}"
            );
        }
    }

    fn replace(root: &Path, text: &str) {
        let mut incoming = tempfile::Builder::new()
            .prefix(".syncthing.note.md.")
            .suffix(".tmp")
            .tempfile_in(root)
            .unwrap();
        incoming.write_all(text.as_bytes()).unwrap();
        incoming.as_file().sync_all().unwrap();
        std::fs::rename(incoming.path(), root.join("note.md")).unwrap();
    }

    fn input(r: &Reader) -> Entity<EditorState> {
        r.editing.as_ref().unwrap().input.clone()
    }

    #[test]
    fn issue2486_selected_notifications_ignore_reads_and_siblings() {
        use notify::{
            event::{AccessKind, ModifyKind, RenameMode},
            Event, EventKind,
        };
        let path = Path::new("folder/note.md");
        assert!(!super::affects_file(
            &Event::new(EventKind::Access(AccessKind::Read)).add_path(path.into()),
            path
        ));
        assert!(!super::affects_file(
            &Event::new(EventKind::Modify(ModifyKind::Any)).add_path("folder/other.md".into()),
            path
        ));
        assert!(super::affects_file(
            &Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path("folder/incoming.tmp".into())
                .add_path(path.into()),
            path
        ));
    }

    #[gpui::test]
    fn issue2486_reading_rename_over_and_in_place_append_control(cx: &mut TestAppContext) {
        for profile in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("vault");
            let (reader, visual) = open(cx, dir.path(), profile);
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(root.join("note.md"))
                .unwrap();
            writeln!(file, "append positive control").unwrap();
            drop(file);
            settle(
                visual,
                &reader,
                "in-place append positive control",
                |r, _| r.note_source.contains("append positive control"),
            );
            replace(&root, "# Note\n\nremote line\n");
            settle(visual, &reader, "reading rename-over", |r, _| {
                r.note_source.contains("remote line")
            });
            assert_eq!(
                std::fs::read_to_string(root.join("note.md")).unwrap(),
                "# Note\n\nremote line\n"
            );
            visual.update(|window, _| window.remove_window());
            visual.run_until_parked();
        }
    }

    #[gpui::test]
    fn issue2486_clean_editor_rename_over_keeps_cursor_and_scroll(cx: &mut TestAppContext) {
        for profile in 0..3 {
            for live in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let root = dir.path().join("vault");
                let (reader, visual) = open(cx, dir.path(), profile);
                let original = format!(
                    "# Note\n\n{}",
                    (0..120).map(|i| format!("line {i}\n")).collect::<String>()
                );
                replace(&root, &original);
                settle(visual, &reader, "long note positive control", |r, _| {
                    r.note_source.contains("line 119")
                });
                reader.update_in(visual, |r, window, cx| {
                    r.toggle_source(window, cx);
                    r.set_live_preview(live, window, cx);
                });
                visual.run_until_parked();
                let editor = reader.read_with(visual, |r, _| input(r));
                let caret = original.find("line 40").unwrap();
                editor.update(visual, |i, cx| {
                    i.enable_layout_diagnostics();
                    i.set_selected_range(caret + 4..caret, cx);
                    i.scroll_line_to_top(35, cx);
                });
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let (scroll, top, caret_y) = editor.read_with(visual, |i, _| {
                    (
                        i.scroll_offset(),
                        i.top_visible_line(),
                        i.cursor_layout().unwrap().0.origin.y,
                    )
                });
                assert!(
                    scroll.y < px(0.),
                    "positive control: editor viewport is scrolled"
                );
                replace(&root, &format!("{original}remote line\n"));
                settle(visual, &reader, "clean editor rename-over", |r, cx| {
                    input(r).read(cx).value().contains("remote line")
                });
                editor.read_with(visual, |i, _| {
                    let frames = i.take_layout_frames();
                    let painted = frames.last().expect("positive control: editor actually painted");
                    assert_eq!(painted.generation, i.source_stamp().generation, "geometry belongs to the incoming revision");
                    assert_eq!(
                        i.cursor(),
                        caret,
                        "clean refresh retains the cursor and selection direction"
                    );
                    assert_eq!(i.selected_range(), caret..caret + 4);
                    // Live Preview can remeasure rows above the viewport. A
                    // corrected pixel offset must still retain the same source
                    // row and caret on screen, which is the visible scroll state.
                    assert_eq!(i.top_visible_line(), top, "profile={profile} live={live}: source row stays visible; before={scroll:?} after={:?}", i.scroll_offset());
                    let after_y = i.cursor_layout().unwrap().0.origin.y;
                    assert!((after_y - caret_y).abs() <= px(1.), "profile={profile} live={live}: viewport anchor moved: before={caret_y:?} after={after_y:?}; scroll before={scroll:?} after={:?}", i.scroll_offset());
                });
                reader.read_with(visual, |r, _| {
                    assert!(!r.editing.as_ref().unwrap().conflict_detected)
                });
                replace(&root, &format!("{original}second remote line\n"));
                settle(visual, &reader, "second clean replacement", |r, cx| {
                    input(r).read(cx).value().contains("second remote line")
                });
                reader.read_with(visual, |r, _| {
                    assert!(
                        !r.editing.as_ref().unwrap().conflict_detected,
                        "a native clean reset is still clean"
                    )
                });
                visual.update(|window, _| window.remove_window());
                visual.run_until_parked();
            }
        }
    }

    #[gpui::test]
    fn issue2486_dirty_editor_notice_and_save_keep_both_without_lost_lines(
        cx: &mut TestAppContext,
    ) {
        for profile in 0..3 {
            for live in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let root = dir.path().join("vault");
                let (reader, visual) = open(cx, dir.path(), profile);
                reader.update_in(visual, |r, window, cx| {
                    r.toggle_source(window, cx);
                    r.set_live_preview(live, window, cx);
                });
                visual.run_until_parked();
                let editor = reader.read_with(visual, |r, _| input(r));
                editor.update_in(visual, |i, window, cx| {
                    i.replace_text_in_range(Some(0..0), "local unsaved line\n", window, cx)
                });
                let remote = "# Note\n\nbase line\nremote incoming line\n";
                replace(&root, remote);
                settle(visual, &reader, "dirty editor conflict notice", |r, _| {
                    r.editing.as_ref().unwrap().conflict_detected
                        && r.link_notice
                            .as_deref()
                            .is_some_and(|n| n.starts_with(CONFLICT_NOTICE))
                });
                let local = editor.read_with(visual, |i, _| i.value().to_string());
                assert!(
                    local.contains("local unsaved line"),
                    "local input positive control"
                );
                assert!(
                    !local.contains("remote incoming line"),
                    "incoming source never replaces a dirty buffer"
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("note.md")).unwrap(),
                    remote
                );
                reader.update(visual, |r, cx| {
                    assert!(!r.save_source(cx), "canonical save remains conflicted")
                });
                let copy = reader.read_with(visual, |r, _| {
                    r.editing
                        .as_ref()
                        .unwrap()
                        .conflict_copy
                        .as_ref()
                        .unwrap()
                        .1
                        .clone()
                });
                assert_eq!(std::fs::read_to_string(root.join(&copy)).unwrap(), local);
                assert_eq!(
                    std::fs::read_to_string(root.join("note.md")).unwrap(),
                    remote,
                    "no lost remote line on Save"
                );
                reader.update(visual, |r, cx| {
                    r.save_source(cx);
                });
                reader.read_with(visual, |r, _| {
                    assert_eq!(
                        r.editing
                            .as_ref()
                            .unwrap()
                            .conflict_copy
                            .as_ref()
                            .unwrap()
                            .1,
                        copy,
                        "same saved revision reuses its copy"
                    )
                });
                editor.update_in(visual, |i, window, cx| {
                    i.replace_text_in_range(Some(0..0), "local after notice\n", window, cx)
                });
                reader.update(visual, |r, cx| {
                    r.save_source(cx);
                });
                let newer = reader.read_with(visual, |r, _| {
                    r.editing
                        .as_ref()
                        .unwrap()
                        .conflict_copy
                        .as_ref()
                        .unwrap()
                        .1
                        .clone()
                });
                assert_ne!(newer, copy);
                let kept = std::fs::read_to_string(root.join(newer)).unwrap();
                assert!(
                    kept.contains("local after notice") && kept.contains("local unsaved line"),
                    "no lost local line on subsequent Save"
                );
                assert_eq!(
                    std::fs::read_to_string(root.join(&copy)).unwrap(),
                    local,
                    "older conflict copy stays intact"
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("note.md")).unwrap(),
                    remote
                );
                visual.update(|window, _| window.remove_window());
                visual.run_until_parked();
            }
        }
    }

    #[gpui::test]
    fn issue2486_selected_document_does_not_wait_for_indexing(cx: &mut TestAppContext) {
        for full in [false, true] {
            for edit in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let root = dir.path().join("vault");
                let (reader, visual) = open(cx, dir.path(), 0);
                if edit {
                    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
                    visual.run_until_parked();
                }
                let (send, hold) = async_channel::bounded(1);
                reader.update_in(visual, |r, window, cx| {
                    if full {
                        let mut opts = r.loading.as_ref().unwrap().opts.clone();
                        opts.validation_hold = Some(hold);
                        opts.force_source_read = true;
                        r.start_preparation(opts, Some(okilum_core::Changes { rescan: true, ..Default::default() }), window, cx);
                        assert!(r.loading.as_ref().unwrap().active && r.loading.as_ref().unwrap().published,
                            "positive control: full scan is held with its displayed document retained");
                    } else {
                        r.incremental_hold = Some(hold);
                        std::fs::write(root.join("sibling.md"), "changed sibling").unwrap();
                        r.apply_vault_changes(okilum_core::Changes { changed: ["sibling.md".into()].into(), ..Default::default() }, window, cx);
                        assert!(r.incremental_active, "positive control: incremental index worker is held");
                    }
                });
                visual.run_until_parked();
                let remote = "# Note\n\nremote during indexing\n";
                replace(&root, remote);
                let visible = |r: &Reader, cx: &App| {
                    if edit {
                        input(r).read(cx).value().contains("remote during indexing")
                    } else {
                        r.note_source.contains("remote during indexing")
                    }
                };
                settle(
                    visual,
                    &reader,
                    "selected note while full/incremental index work is held",
                    visible,
                );
                reader.read_with(visual, |r, _| {
                    assert!(
                        r.has_open_note_watch(),
                        "positive control: independent selected source owner"
                    );
                    if full {
                        assert!(r.loading.as_ref().unwrap().active);
                    } else {
                        assert!(r.incremental_active);
                    }
                });
                // A late snapshot captured before the Sync delivery must not
                // roll the displayed note back after its source owner saw it.
                reader.update_in(visual, |r, window, cx| {
                    r.reconcile_inventory_document(
                        &[("note.md".into(), "# Note\n\nolder inventory bytes\n".into())].into(),
                        window,
                        cx,
                    );
                });
                visual.run_until_parked();
                reader.read_with(visual, |r, cx| {
                    assert!(
                        visible(r, cx),
                        "late inventory cannot supersede observed canonical source"
                    )
                });
                assert_eq!(
                    std::fs::read_to_string(root.join("note.md")).unwrap(),
                    remote
                );
                send.try_send(()).unwrap();
                visual.run_until_parked();
                reader.read_with(visual, |r, cx| {
                    assert!(
                        visible(r, cx),
                        "released index work retains the incoming source"
                    )
                });
                visual.update(|window, _| window.remove_window());
                visual.run_until_parked();
            }
        }
    }
}
