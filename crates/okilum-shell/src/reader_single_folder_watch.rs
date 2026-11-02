//! Selected-document refresh for the existing bounded single-folder watcher.
//! Inventory subscriptions and debounce remain in the ordinary Reader lifecycle.
use super::super::*;

#[derive(Clone)]
struct Selection {
    root: PathBuf,
    rel: String,
    watcher: u64,
    navigation: u64,
    refresh: u64,
}

impl Selection {
    fn capture(reader: &Reader) -> Self {
        Self {
            root: reader.vault_root.clone(),
            rel: reader.current_rel.clone(),
            watcher: reader.watcher_generation,
            navigation: reader.navigation.generation,
            refresh: reader.navigation.reconciliation_generation,
        }
    }

    fn is_current(&self, reader: &Reader) -> bool {
        reader.single_file
            && reader.vault_root == self.root
            && reader.current_rel == self.rel
            && reader.watcher_generation == self.watcher
            && reader.navigation.generation == self.navigation
            && reader.navigation.reconciliation_generation == self.refresh
    }
}

fn read_selected_source(root: &Path, rel: &str) -> anyhow::Result<String> {
    let path = super::checked_path(root, rel)?;
    Ok(std::fs::read_to_string(path)?)
}

impl Reader {
    pub(crate) fn refresh_single_folder_document(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.single_file || self.current_rel.is_empty() {
            return;
        }
        // Invalidate older reads immediately, before starting the next worker.
        self.navigation.reconciliation_generation =
            self.navigation.reconciliation_generation.wrapping_add(1);
        let selected = Selection::capture(self);
        cx.spawn_in(window, async move |this, cx| {
            let read = selected.clone();
            let source = cx
                .background_executor()
                .spawn(async move { read_selected_source(&read.root, &read.rel) })
                .await;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                this.publish_single_folder_document(selected, source.ok(), window, cx);
            });
        })
        .detach();
    }

    fn publish_single_folder_document(
        &mut self,
        selected: Selection,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !selected.is_current(self) {
            return;
        }
        let sources = source
            .map(|source| (selected.rel, source))
            .into_iter()
            .collect();
        // This owns dirty-editor conflicts and deleted-note retention; watcher
        // updates never replace the user's buffer or write canonical files.
        self.reconcile_inventory_document(&sources, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn source_reads_refuse_paths_escaping_the_open_root() {
        let fixture = fixture();
        let root = fixture.path().join("notes");
        let outside = fixture.path().join("outside.md");
        std::fs::write(&outside, "outside bytes").unwrap();
        assert_eq!(
            read_selected_source(&root, "start.md").unwrap(),
            "# Original\n"
        );
        assert!(read_selected_source(&root, "../outside.md").is_err());
        assert!(read_selected_source(&root, outside.to_str().unwrap()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("redirect.md")).unwrap();
            assert!(read_selected_source(&root, "redirect.md").is_err());
        }
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside bytes");
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        fixture: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(fixture.join("notes/start.md")),
                        session_directory: Some(fixture.join("state")),
                        panel_settings_override: Some(fixture.join("panels.json")),
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
        (reader.unwrap(), visual)
    }

    fn fixture() -> tempfile::TempDir {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("notes")).unwrap();
        std::fs::write(fixture.path().join("notes/start.md"), "# Original\n").unwrap();
        fixture
    }

    fn settle(
        visual: &mut VisualTestContext,
        reader: &Entity<Reader>,
        done: impl Fn(&Reader, &App) -> bool,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            // Exercise the installed timer, rather than calling poll_vault.
            std::thread::sleep(Duration::from_millis(25));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
            if reader.read_with(visual, |r, cx| done(r, cx)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "single-folder update timed out"
            );
        }
    }

    fn listed(r: &Reader, path: &str) -> bool {
        r.vault.entries.iter().any(|entry| entry.path == path)
    }

    #[gpui::test]
    fn external_changes_update_exact_document_and_siblings_without_enrollment(
        cx: &mut TestAppContext,
    ) {
        let fixture = fixture();
        let root = fixture.path().join("notes");
        let (reader, visual) = open(cx, fixture.path());
        reader.read_with(visual, |r, _| {
            assert!(
                r.single_file && r.watcher.is_some(),
                "native watcher positive control"
            );
            assert!(r.note_source.contains("Original"));
        });
        std::fs::create_dir(root.join("unexpanded")).unwrap();
        std::fs::write(root.join("unexpanded/hidden.md"), "# Hidden").unwrap();
        std::fs::write(root.join("sibling.md"), "# Sibling").unwrap();
        settle(visual, &reader, |r, _| listed(r, "sibling.md"));
        std::fs::rename(root.join("sibling.md"), root.join("renamed.md")).unwrap();
        std::fs::write(root.join("start.md"), "# External edit\n").unwrap();
        settle(visual, &reader, |r, _| {
            listed(r, "renamed.md")
                && !listed(r, "sibling.md")
                && r.note_source.contains("External edit")
        });
        // Rename is path identity: retain the selected deleted note, never
        // silently select the new name or a similarly named sibling.
        std::fs::rename(root.join("start.md"), root.join("moved.md")).unwrap();
        settle(visual, &reader, |r, _| {
            r.deleted_note.is_some() && listed(r, "moved.md")
        });
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "start.md");
            assert!(r.note_source.contains("External edit"));
        });
        std::fs::remove_file(root.join("renamed.md")).unwrap();
        settle(visual, &reader, |r, _| !listed(r, "renamed.md"));
        reader.read_with(visual, |r, _| {
            assert!(r.single_file && r.searcher.is_none() && r.index_dir.is_none());
            assert!(r.shared_session.is_none());
            assert!(!listed(r, "unexpanded/hidden.md"));
        });
        assert!(!root.join(".okilum").exists());
    }

    #[gpui::test]
    fn dirty_external_edit_and_delete_preserve_buffer_and_disk(cx: &mut TestAppContext) {
        let fixture = fixture();
        let path = fixture.path().join("notes/start.md");
        let (reader, visual) = open(cx, fixture.path());
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .set_value("# My unsaved draft\n", window, cx);
        });
        std::fs::write(&path, "# Other editor\n").unwrap();
        settle(visual, &reader, |r, _| {
            r.link_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("changed"))
        });
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.editing
                    .as_ref()
                    .unwrap()
                    .input()
                    .read(cx)
                    .value()
                    .as_ref(),
                "# My unsaved draft\n"
            );
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Other editor\n");
        std::fs::remove_file(&path).unwrap();
        settle(visual, &reader, |r, _| r.deleted_note.is_some());
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "start.md");
            assert_eq!(
                r.editing
                    .as_ref()
                    .unwrap()
                    .input()
                    .read(cx)
                    .value()
                    .as_ref(),
                "# My unsaved draft\n"
            );
        });
        assert!(
            !path.exists(),
            "watcher cannot implicitly restore a deleted source"
        );
    }

    #[gpui::test]
    fn stale_publications_cannot_replace_current_selection(cx: &mut TestAppContext) {
        let fixture = fixture();
        let (reader, visual) = open(cx, fixture.path());
        reader.update_in(visual, |r, window, cx| {
            let current = Selection::capture(r);
            assert!(current.is_current(r), "publication positive control");
            let mut stale = current.clone();
            stale.watcher = stale.watcher.wrapping_sub(1);
            r.publish_single_folder_document(stale, Some("# Stale watcher".into()), window, cx);
            let mut stale = current.clone();
            stale.root = fixture.path().join("old-root");
            r.publish_single_folder_document(stale, None, window, cx);
            let mut stale = current.clone();
            stale.rel = "other.md".into();
            r.publish_single_folder_document(stale, None, window, cx);
            let mut stale = current.clone();
            stale.navigation = stale.navigation.wrapping_sub(1);
            r.publish_single_folder_document(stale, None, window, cx);
            let mut stale = current.clone();
            stale.refresh = stale.refresh.wrapping_sub(1);
            r.publish_single_folder_document(stale, Some("# Older refresh".into()), window, cx);
            assert!(r.deleted_note.is_none());
            assert!(r.note_source.contains("Original"));
            r.publish_single_folder_document(
                current,
                Some("# Current refresh\n".into()),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.note_source.contains("Current refresh"))
        });
    }

    #[gpui::test]
    fn old_root_notifications_cannot_publish_after_switch(cx: &mut TestAppContext) {
        let fixture = fixture();
        let old_root = fixture.path().join("notes");
        let new_root = fixture.path().join("new-root");
        std::fs::create_dir(&new_root).unwrap();
        std::fs::write(new_root.join("start.md"), "# New root\n").unwrap();
        let (reader, visual) = open(cx, fixture.path());
        let (release, hold) = async_channel::bounded(1);
        reader.update_in(visual, |r, window, cx| {
            assert!(r.watcher.is_some(), "old watcher positive control");
            r.watcher_poll_hold = Some(hold);
            r.poll_vault(window, cx);
            assert!(r.watcher.is_none(), "old watcher borrowed by worker");
            r.start_loading(
                Opts {
                    open_path: Some(new_root.join("start.md")),
                    session_directory: Some(fixture.path().join("state")),
                    panel_settings_override: Some(fixture.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.vault_root, new_root.canonicalize().unwrap());
            assert!(r.note_source.contains("New root"));
        });
        std::fs::remove_file(old_root.join("start.md")).unwrap();
        std::fs::write(old_root.join("old-only.md"), "# Old event\n").unwrap();
        release.try_send(()).unwrap();
        visual.run_until_parked();
        // A detected event from the new watcher is the positive control for
        // timer progress after releasing the obsolete worker.
        std::fs::write(new_root.join("new-only.md"), "# New event\n").unwrap();
        settle(visual, &reader, |r, _| listed(r, "new-only.md"));
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "start.md");
            assert!(r.note_source.contains("New root"));
            assert!(!listed(r, "old-only.md"));
            assert!(r.deleted_note.is_none());
            assert!(r.single_file && r.searcher.is_none() && r.index_dir.is_none());
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn external_symlink_redirect_never_publishes_outside_source(cx: &mut TestAppContext) {
        let fixture = fixture();
        let path = fixture.path().join("notes/start.md");
        let outside = fixture.path().join("outside.md");
        std::fs::write(&outside, "# Private outside source\n").unwrap();
        let (reader, visual) = open(cx, fixture.path());
        // Prove refresh works before asserting an absence after redirection.
        std::fs::write(&path, "# Inside update\n").unwrap();
        settle(visual, &reader, |r, _| {
            r.note_source.contains("Inside update")
        });
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        settle(visual, &reader, |r, _| {
            r.deleted_note.is_some() || r.link_notice.is_some()
        });
        reader.read_with(visual, |r, _| {
            assert!(!r.note_source.contains("Private outside"))
        });
        assert_eq!(
            std::fs::read_to_string(outside).unwrap(),
            "# Private outside source\n"
        );
    }
}
