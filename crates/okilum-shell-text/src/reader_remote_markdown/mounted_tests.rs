// Included in the existing offline window fixtures: no new Reader test hooks.
use okilum_core::remote_markdown::save::{Attempt, Outcome, Revision, Snapshot};
fn save_click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .expect("actual remote editing control must paint");
    visual.simulate_click(bounds.center(), Modifiers::default());
    draw(visual);
}
fn replace_remote_draft(panel: &Entity<Panel>, visual: &mut VisualTestContext, text: &str) {
    let input = panel.read_with(visual, |panel, _| {
        panel.edit.as_ref().unwrap().input.clone()
    });
    input.update_in(visual, |state, window, cx| {
        let length = state.value().encode_utf16().count();
        state.replace_text_in_range(Some(0..length), text, window, cx);
    });
    draw(visual);
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.edit.as_ref().unwrap().record.text,
            text,
            "actual input mutation updates local draft"
        )
    });
}
async fn edit_ready(panel: &Entity<Panel>, visual: &mut VisualTestContext) {
    save_click(visual, "remote-edit");
    visual
        .condition(panel, |panel, _| {
            panel.edit.is_some() || panel.edit_task.is_none()
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert!(
            panel.edit.is_some(),
            "Edit must open a protected editor: {:?}",
            panel.notice
        )
    });
    draw(visual);
}
async fn save_done(panel: &Entity<Panel>, visual: &mut VisualTestContext) {
    visual
        .condition(panel, |panel, _| {
            panel.edit.as_ref().is_some_and(|e| e.saving.is_none())
        })
        .await;
    draw(visual);
}
fn saved(attempt: &Attempt) -> Outcome {
    Outcome::Saved {
        revision: Revision::of(&attempt.text),
        already_present: false,
        recovery: vec![],
    }
}

#[gpui::test]
async fn safe1610_mounted_explicit_save_protects_exact_draft_and_never_autosaves(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/a.md", "base\n");
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = calls.clone();
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.saver = Arc::new(move |source, attempt, _| {
            capture
                .lock()
                .unwrap()
                .push((source.url().to_string(), attempt.clone()));
            Ok(saved(attempt))
        });
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "my exact draft\n");
    assert!(
        calls.lock().unwrap().is_empty(),
        "typing and local protection must never save remotely"
    );
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    let captured = calls.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].0, "ssh://offline/a.md");
    assert_eq!(captured[0].1.text, "my exact draft\n");
    assert_eq!(captured[0].1.base, Revision::of("base\n"));
    drop(captured);
    panel.read_with(visual, |panel, _| {
        assert!(!panel.edit.as_ref().unwrap().record.dirty());
        assert!(panel
            .notice
            .as_ref()
            .unwrap()
            .starts_with("Saved atomically"));
    });
}
#[gpui::test]
async fn safe1610_mounted_conflict_compare_keep_mine_and_use_remote_are_explicit(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/a.md", "base\n");
    let calls = Arc::new(std::sync::Mutex::new(Vec::<Attempt>::new()));
    let capture = calls.clone();
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.saver = Arc::new(move |_, attempt, _| {
            let mut calls = capture.lock().unwrap();
            calls.push(attempt.clone());
            if calls.len() == 2 {
                return Ok(saved(attempt));
            }
            Ok(Outcome::Conflict {
                remote: Some(Snapshot::new("remote changed\n".into())),
                raced: Some(Snapshot::new("retained race\n".into())),
                recovery: vec!["retained".into()],
                rollback_race: true,
                message: "Both versions retained".into(),
            })
        });
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "mine\n");
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    assert!(
        visual.debug_bounds("remote-keep-mine").is_none(),
        "no unchecked overwrite control before Compare"
    );
    save_click(visual, "remote-compare");
    panel.read_with(visual,|panel,_|assert!(matches!(&panel.edit.as_ref().unwrap().conflict,Some(Outcome::Conflict{remote:Some(remote),raced:Some(raced),..}) if remote.text=="remote changed\n" && raced.text=="retained race\n")));
    save_click(visual, "remote-keep-mine");
    assert_eq!(calls.lock().unwrap().len(), 1, "review must not write");
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls[1].base, Revision::of("remote changed\n"));
        assert_ne!(calls[0].id, calls[1].id);
        assert_eq!(calls[1].text, "mine\n");
    }
    replace_remote_draft(&panel, visual, "another draft\n");
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    save_click(visual, "remote-compare");
    save_click(visual, "remote-use-remote");
    panel.read_with(visual, |panel, _| {
        let record = &panel.edit.as_ref().unwrap().record;
        assert_eq!(record.text, "remote changed\n");
        assert!(!record.dirty());
    });
    assert_eq!(calls.lock().unwrap().len(), 3);
}
#[gpui::test]
async fn safe1610_mounted_disconnect_close_reopen_recovers_draft_and_retry_identity(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/a.md", "base\n");
    let calls = Arc::new(std::sync::Mutex::new(Vec::<Attempt>::new()));
    let capture = calls.clone();
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.saver = Arc::new(move |_, attempt, _| {
            let mut calls = capture.lock().unwrap();
            calls.push(attempt.clone());
            if calls.len() == 1 {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "offline dropped acknowledgement",
                ))
            } else {
                Ok(saved(attempt))
            }
        });
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "survive restart\n");
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    panel.update(visual, |panel, cx| {
        assert!(panel.stop(cx));
    });
    draw(visual);
    // Drop the old editor and its exclusive recovery handle through the real
    // window source-replacement boundary; a fresh editor reloads disk state.
    struct Disconnected;
    impl Transport for Disconnected {
        fn open(&mut self, _: &okilum_core::log::sftp::Target) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "offline restart"))
        }
        fn read_at(&mut self, _: u64, _: usize) -> io::Result<Vec<u8>> {
            unreachable!()
        }
        fn disconnect(&mut self) {}
    }
    panel.update(visual, |panel, cx| {
        panel.open_with(
            Source::parse("ssh://offline/a.md").unwrap(),
            None,
            |controller, url| {
                let reply = controller
                    .start(Source::parse(url)?, Disconnected)
                    .recv_blocking()
                    .unwrap();
                Ok(queued(reply))
            },
            cx,
        )
    });
    draw(visual);
    panel.read_with(visual, |panel, _| {
        assert!(matches!(panel.controller.state, State::Disconnected(_)))
    });
    edit_ready(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.edit.as_ref().unwrap().record.text,
            "survive restart\n"
        )
    });
    save_click(visual, "remote-retry");
    save_done(&panel, visual).await;
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, calls[1].id);
    assert_eq!(calls[1].text, "survive restart\n");
}
#[gpui::test]
async fn safe1610_mounted_unsupported_keeps_draft_and_stale_completion_cannot_rebind(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/a.md", "base\n");
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.saver = Arc::new(|_, _, _| {
            Ok(Outcome::Unsupported {
                message: "untrusted wording".into(),
            })
        });
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "keep unsupported\n");
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.notice.as_deref(),
            Some(okilum_core::remote_markdown::save::UNSUPPORTED)
        );
        assert_eq!(
            panel.edit.as_ref().unwrap().record.text,
            "keep unsupported\n"
        );
        assert!(panel.edit.as_ref().unwrap().unsupported);
    });
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    let (started, entered) = std::sync::mpsc::channel();
    panel.update(visual, |panel, _| {
        panel.saver = Arc::new(move |_, attempt, cancel| {
            started.send(()).unwrap();
            held.lock().unwrap().recv().unwrap();
            assert!(cancel.load(Ordering::Acquire));
            Ok(saved(attempt))
        })
    });
    save_click(visual, "remote-retry");
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    load(&panel, visual, "ssh://offline/b.md", "fresh owner\n");
    release.send(()).unwrap();
    draw(visual);
    panel.read_with(visual,|panel,_|{assert!(panel.edit.is_none());assert!(matches!(&panel.controller.state,State::Ready{source,markdown} if source.url()=="ssh://offline/b.md" && markdown=="fresh owner\n"));});
    // Positive control: a new explicit Save for the accepted owner publishes.
    panel.update(visual, |panel, _| {
        panel.saver = Arc::new(|_, attempt, _| Ok(saved(attempt)))
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "fresh saved\n");
    save_click(visual, "remote-save");
    save_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        assert!(!panel.edit.as_ref().unwrap().record.dirty())
    });
}

#[gpui::test]
async fn safe1610_mounted_failed_local_protection_blocks_save_and_close(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/a.md", "base\n");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let capture = calls.clone();
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.saver = Arc::new(move |_, attempt, _| {
            capture.fetch_add(1, Ordering::SeqCst);
            Ok(saved(attempt))
        });
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "must not lose\n");
    let directory = std::fs::read_dir(local.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let displaced = local.path().join("kept-recovery");
    std::fs::rename(&directory, &displaced).unwrap();
    std::fs::write(&directory, "storage unavailable").unwrap();
    save_click(visual, "remote-save");
    panel.update(visual, |panel, cx| {
        assert!(!panel.stop(cx), "cannot close an unprotected draft");
        assert_eq!(panel.edit.as_ref().unwrap().record.text, "must not lose\n");
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no remote request before durable draft barrier"
    );
    std::fs::remove_file(&directory).unwrap();
    std::fs::rename(&displaced, &directory).unwrap();
    save_click(visual, "remote-retry");
    save_done(&panel, visual).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[gpui::test]
async fn safe1610_mounted_save_ack_keeps_later_edits_and_close_cancels_owner(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let local = tempfile::tempdir().unwrap();
    let (panel, visual) = fixture(cx);
    load(&panel, visual, "ssh://offline/a.md", "base\n");
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    let (started, entered) = std::sync::mpsc::channel();
    panel.update(visual, |panel, _| {
        panel.recovery_root = Some(local.path().into());
        panel.saver = Arc::new(move |_, attempt, _| {
            started.send(()).unwrap();
            held.lock().unwrap().recv().unwrap();
            Ok(saved(attempt))
        });
    });
    edit_ready(&panel, visual).await;
    replace_remote_draft(&panel, visual, "submitted\n");
    save_click(visual, "remote-save");
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    replace_remote_draft(&panel, visual, "typed while saving\n");
    release.send(()).unwrap();
    save_done(&panel, visual).await;
    panel.read_with(visual, |panel, _| {
        let record = &panel.edit.as_ref().unwrap().record;
        assert_eq!(record.base, "submitted\n");
        assert_eq!(record.text, "typed while saving\n");
        assert!(record.dirty());
    });
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    let (started, entered) = std::sync::mpsc::channel();
    panel.update(visual, |panel, _| {
        panel.saver = Arc::new(move |_, attempt, cancel| {
            started.send(()).unwrap();
            held.lock().unwrap().recv().unwrap();
            assert!(cancel.load(Ordering::Acquire));
            Ok(saved(attempt))
        })
    });
    save_click(visual, "remote-save");
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    panel.update(visual, |panel, cx| assert!(panel.stop(cx)));
    release.send(()).unwrap();
    draw(visual);
    panel.read_with(visual, |panel, _| {
        assert!(matches!(panel.controller.state, State::Cancelled));
        assert_eq!(panel.edit.as_ref().unwrap().record.base, "submitted\n");
        assert_eq!(
            panel.edit.as_ref().unwrap().record.text,
            "typed while saving\n"
        );
        assert!(panel.edit.as_ref().unwrap().record.pending.is_some());
    });
}
