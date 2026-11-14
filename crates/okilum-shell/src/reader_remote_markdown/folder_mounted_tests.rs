use okilum_core::{
    log::sftp::directory::{DirectoryTransport, Entry, Kind},
    remote_markdown::folder::{self, Folder},
};
struct FolderOffline {
    pages: std::collections::VecDeque<Vec<Entry>>,
    fail: Option<io::ErrorKind>,
    ui: std::thread::ThreadId,
}
impl DirectoryTransport for FolderOffline {
    fn open_directory(&mut self, _: &okilum_core::log::sftp::Target) -> io::Result<()> {
        assert_ne!(
            self.ui,
            std::thread::current().id(),
            "directory IO runs on the existing remote background-worker boundary"
        );
        if let Some(kind) = self.fail {
            Err(io::Error::new(
                kind,
                match kind {
                    io::ErrorKind::PermissionDenied => "Permission denied for this remote folder",
                    _ => "The remote path does not exist",
                },
            ))
        } else {
            Ok(())
        }
    }
    fn read_directory(&mut self) -> io::Result<Option<Vec<Entry>>> {
        Ok(self.pages.pop_front())
    }
    fn disconnect_directory(&mut self) {}
}
fn folder_entry(name: &str, kind: Kind) -> Entry {
    Entry {
        name: name.as_bytes().to_vec(),
        kind,
    }
}
fn folder_list(
    target: &Folder,
    rows: Vec<Entry>,
    fail: Option<io::ErrorKind>,
    ui: std::thread::ThreadId,
    cancel: &AtomicBool,
) -> io::Result<folder::Listing> {
    folder::list(
        target,
        &mut FolderOffline {
            pages: [rows].into(),
            fail,
            ui,
        },
        || cancel.load(Ordering::Acquire),
    )
}
fn folder_file_loader(
    controller: &mut Controller,
    url: &str,
) -> io::Result<async_channel::Receiver<Reply>> {
    Ok(queued(offline_reply(
        controller,
        url,
        &format!("# Exact selected file\n\n{url}\n"),
        Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    )?))
}
async fn folder_done(panel: &Entity<Panel>, visual: &mut VisualTestContext) {
    visual
        .condition(panel, |panel, _| {
            panel.browser.as_ref().is_some_and(|b| b.result.is_some())
        })
        .await;
    draw(visual);
}
#[gpui::test]
async fn folder1610_mounted_browse_sorted_folders_up_down_and_open_exact_markdown_into_edit(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/notes/a.md", "original\n");
    let ui = std::thread::current().id();
    let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let capture = calls.clone();
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.fixture_loader = Some(folder_file_loader);
        panel.lister = Arc::new(move |target, cancel| {
            capture.lock().unwrap().push(target.url().to_string());
            let rows = if target.url().ends_with("/sub/") {
                vec![folder_entry("a.md", Kind::File)]
            } else {
                vec![
                    folder_entry("z.txt", Kind::File),
                    folder_entry("a.md", Kind::File),
                    folder_entry("sub", Kind::Directory),
                ]
            };
            folder_list(target, rows, None, ui, cancel)
        });
    });
    save_click(visual, "remote-folder-up");
    folder_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        let browser = panel.browser.as_ref().unwrap();
        let rows = &browser.result.as_ref().unwrap().as_ref().unwrap().entries;
        assert_eq!(
            rows.iter().map(Entry::label).collect::<Vec<_>>(),
            ["sub", "a.md", "z.txt"]
        );
        assert!(rows[1].is_markdown());
    });
    save_click(visual, "remote-folder-entry-0");
    folder_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.browser.as_ref().unwrap().folder.url(),
            "ssh://offline/notes/sub/"
        )
    });
    save_click(visual, "remote-folder-up");
    folder_done(&panel, visual).await;
    save_click(visual, "remote-folder-entry-0");
    folder_done(&panel, visual).await;
    save_click(visual, "remote-folder-entry-0");
    draw(visual);
    panel.read_with(visual,|panel,_|assert!(matches!(&panel.controller.state,State::Ready{source,markdown} if source.url()=="ssh://offline/notes/sub/a.md" && markdown.contains("ssh://offline/notes/sub/a.md"))));
    let saves = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = saves.clone();
    panel.update(visual, |panel, _| {
        panel.saver = Arc::new(move |source, attempt, _| {
            capture.lock().unwrap().push(source.url().to_string());
            Ok(saved(attempt))
        })
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "nested edited\n");
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    assert_eq!(*saves.lock().unwrap(), ["ssh://offline/notes/sub/a.md"]);
    assert_eq!(calls.lock().unwrap().len(), 4);
}
#[gpui::test]
async fn folder1610_mounted_permission_missing_errors_and_actual_retry_keep_path(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/private/a.md", "base\n");
    let ui = std::thread::current().id();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let capture = attempts.clone();
    panel.update(visual, |panel, _| {
        panel.lister = Arc::new(move |target, cancel| {
            let count = capture.fetch_add(1, Ordering::SeqCst);
            let error = if target.url().ends_with("/missing/") {
                Some(io::ErrorKind::NotFound)
            } else if count == 0 {
                Some(io::ErrorKind::PermissionDenied)
            } else {
                None
            };
            folder_list(
                target,
                vec![folder_entry("missing", Kind::Directory)],
                error,
                ui,
                cancel,
            )
        })
    });
    save_click(visual, "remote-folder-up");
    folder_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        let b = panel.browser.as_ref().unwrap();
        assert_eq!(b.folder.url(), "ssh://offline/private/");
        assert_eq!(
            b.result.as_ref().unwrap().as_ref().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    });
    assert!(visual.debug_bounds("remote-folder-error").is_some());
    assert!(visual.debug_bounds("remote-folder-entry-0").is_none());
    save_click(visual, "remote-folder-retry");
    folder_done(&panel, visual).await;
    save_click(visual, "remote-folder-entry-0");
    folder_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        let b = panel.browser.as_ref().unwrap();
        assert_eq!(b.folder.url(), "ssh://offline/private/missing/");
        assert_eq!(
            b.result.as_ref().unwrap().as_ref().unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    });
    assert!(visual.debug_bounds("remote-folder-error").is_some());
    save_click(visual, "remote-folder-up");
    folder_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert!(panel
            .browser
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .is_ok())
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
}
#[gpui::test]
async fn folder1610_mounted_listing_bound_no_partial_rows_and_browse_preserves_dirty_draft(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/notes/a.md", "base\n");
    let ui = std::thread::current().id();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let capture = attempts.clone();
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.fixture_loader = Some(folder_file_loader);
        panel.lister = Arc::new(move |target, cancel| {
            let rows = if capture.fetch_add(1, Ordering::SeqCst) == 0 {
                (0..=folder::ENTRY_LIMIT)
                    .map(|i| folder_entry(&format!("{i}.md"), Kind::File))
                    .collect()
            } else {
                vec![folder_entry("a.md", Kind::File)]
            };
            folder_list(target, rows, None, ui, cancel)
        });
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "kept while browsing\n");
    save_click(visual, "remote-folder-up");
    folder_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert!(panel
            .browser
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("1000 entries"))
    });
    assert!(visual.debug_bounds("remote-folder-entry-0").is_none());
    save_click(visual, "remote-folder-retry");
    folder_done(&panel, visual).await;
    save_click(visual, "remote-folder-entry-0");
    draw(visual);
    edit_ready(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.edit.as_ref().unwrap().record.text,
            "kept while browsing\n"
        )
    });
}
#[gpui::test]
async fn folder1610_mounted_late_listing_cannot_replace_up_navigation_or_stopped_owner(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/notes/a.md", "base\n");
    let ui = std::thread::current().id();
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    let (started, entered) = std::sync::mpsc::channel();
    panel.update(visual, |panel, _| {
        panel.lister = Arc::new(move |target, cancel| {
            if target.url() == "ssh://offline/notes/" {
                started.send(()).unwrap();
                held.lock().unwrap().recv().unwrap();
                assert!(cancel.load(Ordering::Acquire));
            }
            folder_list(
                target,
                vec![folder_entry("fresh.md", Kind::File)],
                None,
                ui,
                cancel,
            )
        })
    });
    save_click(visual, "remote-folder-up");
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    save_click(visual, "remote-folder-up");
    folder_done(&panel, visual).await;
    release.send(()).unwrap();
    draw(visual);
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.browser.as_ref().unwrap().folder.url(),
            "ssh://offline/"
        )
    });
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    let (started, entered) = std::sync::mpsc::channel();
    panel.update(visual, |panel, _| {
        panel.lister = Arc::new(move |target, cancel| {
            started.send(()).unwrap();
            held.lock().unwrap().recv().unwrap();
            assert!(cancel.load(Ordering::Acquire));
            folder_list(
                target,
                vec![folder_entry("stale.md", Kind::File)],
                None,
                ui,
                cancel,
            )
        })
    });
    save_click(visual, "remote-folder-retry");
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    save_click(visual, "remote-markdown-stop");
    release.send(()).unwrap();
    draw(visual);
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel
                .browser
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        )
    });
    panel.update(visual, |panel, _| {
        panel.lister = Arc::new(move |target, cancel| {
            folder_list(
                target,
                vec![folder_entry("accepted.md", Kind::File)],
                None,
                ui,
                cancel,
            )
        })
    });
    save_click(visual, "remote-folder-retry");
    folder_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel
                .browser
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .entries[0]
                .label(),
            "accepted.md"
        )
    });
}

#[test]
fn folder1610_existing_ssh_entrypoint_routes_folder_or_document_before_any_worker() {
    for url in [
        "ssh://same_alias/notes/",
        "ssh://same_alias/notes",
        "ssh://same_alias/folder.md/",
    ] {
        assert!(
            matches!(location(url).unwrap(), Location::Folder(_)),
            "{url}"
        );
    }
    assert!(
        matches!(location("ssh://same_alias/notes/a.md#Part%20One").unwrap(),Location::Document(source,Some(heading)) if source.url()=="ssh://same_alias/notes/a.md" && heading=="Part One")
    );
    // #1610: user and port from the link are part of the authority.
    assert!(matches!(
        location("ssh://user@same_alias:2222/notes/").unwrap(),
        Location::Folder(_)
    ));
    for invalid in [
        "ssh://user:password@same_alias/notes/",
        "ssh://same_alias/notes/#heading",
        "ssh://same_alias/notes/?query",
    ] {
        assert!(location(invalid).is_err());
    }
}
