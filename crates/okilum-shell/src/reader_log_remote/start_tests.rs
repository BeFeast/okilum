use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::live::Loaded;

fn source() -> Source {
    Source::import("Applied", "https://logs.example/app/discover#/?_a=(index:events,query:(language:kuery,query:'service:checkout'),columns:!(description),sort:!(!('@timestamp',desc)))&_g=(time:(from:'2026-10-20T12:00:00Z',to:'2026-10-20T12:15:00Z'),refreshInterval:(pause:!t,value:0))").unwrap()
}

#[gpui::test]
fn cancel_live_start_keeps_draft_history_paging_and_marks_without_worker_command(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    let source = source();
    remote_log::save_source(&state, &vault, &source).unwrap();
    let saved = std::fs::read(
        state
            .join("remote-log-sources")
            .join(format!("{}.json", remote_log::vault_scope(&vault))),
    )
    .unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1500.), px(1050.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let data = Arc::new(EventWindow::new(vec![Arc::new(serde_json::json!({"_index":"events","_id":"kept","_source":{"description":"retained"}}))], &source).unwrap());
    panel.update_in(visual, |panel, window, cx| {
        // Isolate synthetic history from Stop acknowledgements produced by
        // choosing the fixture source; the real worker generation stays live.
        panel._events = Task::ready(());
        panel.choose(source.clone(), window, cx);
        panel.receive(
            Update {
                generation: panel.generation,
                following: false,
                loaded: Some(Arc::new(Loaded {
                    source: source.clone(),
                    range: source.discover.time.clone(),
                    window: data.clone(),
                    more: true,
                })),
                status: Status::History {
                    complete: false,
                    limited: false,
                    warning: Some("Retained coverage warning".into()),
                },
            },
            cx,
        );
        panel.kql.update(cx, |input, cx| {
            input.set_value("unapplied draft", window, cx)
        });
        panel.filters.update(cx, |input, cx| {
            input.set_value("[invalid draft", window, cx)
        });
    });
    visual.run_until_parked();
    let (generation, view_id, message) = panel.update(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap();
        view.update(cx, |view, _| {
            view.selected = Some(0);
            view.anchor = Some(0);
            view.marks.insert((0, 0));
        });
        (
            panel.worker.generation(),
            view.entity_id(),
            panel.message.clone(),
        )
    });
    let start = visual.debug_bounds("remote-start").unwrap();
    visual.simulate_click(start.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("remote-confirm-fixed").is_some());
    assert!(visual.debug_bounds("remote-confirm-rolling").is_some());
    panel.read_with(visual, |panel, _| {
        assert!(
            panel.proposal.is_some(),
            "positive control: Start opened consent"
        );
        assert_eq!(panel.worker.generation(), generation);
    });
    let cancel = visual.debug_bounds("remote-cancel-start").unwrap();
    visual.simulate_click(cancel.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("remote-confirm-fixed").is_none());
    panel.update(visual, |panel, cx| {
        // A confirmation already queued from the dismissed disclosure must
        // remain inert, including after the UI returns to its idle state.
        panel.confirm_start(false, cx);
        panel.confirm_start(true, cx);
        assert_eq!(panel.worker.generation(), generation);
        assert_eq!(panel.generation, generation);
        assert!(!panel.following && !panel.busy && !panel.credential_pending);
        assert!(panel.proposal.is_none() && panel.more);
        assert_eq!(panel.message, message);
        assert_eq!(panel.error.as_deref(), Some("Retained coverage warning"));
        assert_eq!(panel.kql.read(cx).value().as_str(), "unapplied draft");
        assert_eq!(panel.filters.read(cx).value().as_str(), "[invalid draft");
        assert_eq!(panel.range.as_ref(), Some(&source.discover.time));
        assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &data));
        let view = panel.view.as_ref().unwrap();
        assert_eq!(view.entity_id(), view_id);
        let view = view.read(cx);
        assert_eq!(view.selected, Some(0));
        assert!(view.marks.contains(&(0, 0)));
    });
    assert_eq!(
        std::fs::read(
            state
                .join("remote-log-sources")
                .join(format!("{}.json", remote_log::vault_scope(&vault)))
        )
        .unwrap(),
        saved
    );
}

#[gpui::test]
fn live_start_choices_require_current_consent_and_dispatch_only_once(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1500.), px(1050.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    panel.update(visual, |panel, _| {
        // Synthetic history has no worker-owned credentials/session. Keep real
        // worker replies out of this UI consent test; no endpoint is opened.
        panel._events = Task::ready(());
        panel.applied = Some(source());
        panel.range = Some(source().discover.time);
    });
    for rolling in [false, true] {
        let generation = panel.update(visual, |panel, cx| {
            panel.stop();
            panel.propose_start(cx);
            assert!(panel.proposal.is_some());
            let generation = panel.worker.generation();
            // Both the callbacks and rendered buttons guard credential work,
            // paging and following, even if an older disclosure still exists.
            for state in 0..3 {
                panel.credential_pending = state == 0;
                panel.busy = state == 1;
                panel.following = state == 2;
                panel.confirm_start(rolling, cx);
                assert_eq!(panel.worker.generation(), generation);
                assert!(panel.proposal.is_some());
            }
            panel.credential_pending = false;
            panel.busy = false;
            panel.following = false;
            cx.notify();
            generation
        });
        visual.run_until_parked();
        let choice = visual
            .debug_bounds(if rolling {
                "remote-confirm-rolling"
            } else {
                "remote-confirm-fixed"
            })
            .unwrap();
        visual.simulate_click(choice.center(), Modifiers::default());
        panel.update(visual, |panel, cx| {
            assert_eq!(
                panel.worker.generation(),
                generation + 1,
                "positive control: chosen mode dispatched"
            );
            assert!(panel.following && panel.busy);
            assert!(panel.proposal.is_none());
            panel.confirm_start(false, cx);
            panel.confirm_start(true, cx);
            assert_eq!(panel.worker.generation(), generation + 1);
            // Even after an error clears in-flight flags, old consent cannot
            // become a new Start. A fresh explicit proposal is required.
            panel.following = false;
            panel.busy = false;
            panel.confirm_start(false, cx);
            panel.confirm_start(true, cx);
            assert_eq!(panel.worker.generation(), generation + 1);
            panel.propose_start(cx);
            panel.stop();
            let stopped = panel.worker.generation();
            panel.confirm_start(false, cx);
            panel.confirm_start(true, cx);
            assert_eq!(panel.worker.generation(), stopped);
        });
    }
}
