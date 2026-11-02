use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::live::Loaded;

fn source(name: &str) -> Source {
    let mut source = Source::import(name, "https://logs.example/app/discover#/?_a=(index:events,query:(language:kuery,query:'service:checkout'),columns:!(description,level),sort:!(!('@timestamp',desc)))&_g=(time:(from:'2026-10-20T12:00:00.123456789Z',to:'2026-10-20T12:15:00Z'),refreshInterval:(pause:!t,value:0))").unwrap();
    source.discover.filters = vec![
        serde_json::json!({"$state":{"store":"globalState"},"meta":{"disabled":false,"negate":true},"query":{"term":{"service":"noise"}}}),
        serde_json::json!({"meta":{"disabled":true,"negate":false},"query":{"match_phrase":{"description":"old"}}}),
    ];
    source.validate().unwrap();
    source
}
fn loaded(source: &Source, id: &str) -> Arc<Loaded> {
    Arc::new(Loaded {
        source: source.clone(), range: source.discover.time.clone(), more: true,
        window: Arc::new(EventWindow::new(vec![Arc::new(serde_json::json!({"_index":"events","_id":id,"_source":{"@timestamp":"2026-10-20T12:01:00.123456789Z","description":id,"nested":{"all":[1,2]}}}))], source).unwrap()),
    })
}
fn history(generation: u64, loaded: Arc<Loaded>) -> Update {
    Update {
        generation,
        following: false,
        loaded: Some(loaded),
        status: Status::History {
            complete: false,
            limited: false,
            warning: None,
        },
    }
}

#[gpui::test]
fn restore_applied_settings_recovers_failed_persisted_draft_without_query_or_row_replacement(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&state, cx);
    });
    let good = source("Working source");
    let good_loaded = loaded(&good, "kept");
    let mut failed = good.clone();
    failed.name = "Failed candidate".into();
    failed.discover.query.query = "server-rejected candidate".into();
    failed.discover.sort[0].order = "asc".into();
    failed.discover.columns = vec!["other".into()];
    failed.discover.time = TimeRange {
        from: "now-1h".into(),
        to: "now".into(),
    };
    failed.discover.filters.clear();
    // Apply saves a candidate before requesting history. Model that existing
    // state directly, without using real credentials or an endpoint.
    remote_log::save_source(&state, &vault, &failed).unwrap();
    let saved_path = state
        .join("remote-log-sources")
        .join(format!("{}.json", remote_log::vault_scope(&vault)));
    let saved = std::fs::read(&saved_path).unwrap();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1350.), px(950.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    panel.update(visual, |panel, cx| {
        panel.receive(history(panel.generation, good_loaded.clone()), cx)
    });
    visual.run_until_parked();
    let (view_id, old_generation) = panel.update_in(visual, |panel, window, cx| {
        let view = panel.view.as_ref().unwrap().clone();
        let id = view.entity_id();
        view.update(cx, |view, _| {
            view.selected = Some(0);
            view.anchor = Some(0);
            view.marks.insert((0, 0));
        });
        panel.choose(failed.clone(), window, cx);
        let generation = panel.generation;
        panel.receive(
            Update {
                generation,
                following: false,
                loaded: Some(good_loaded.clone()),
                status: Status::Error("Synthetic query rejection".into()),
            },
            cx,
        );
        assert!(panel.error.as_ref().unwrap().contains("query rejection"));
        // Recovery must also work when the current draft cannot even validate.
        panel.filters.update(cx, |input, cx| {
            input.set_value("[invalid draft", window, cx)
        });
        panel.url.update(cx, |input, cx| {
            input.set_value("https://other.example/unimported-draft", window, cx)
        });
        panel.key.update(cx, |input, cx| {
            input.set_value("synthetic-not-a-real-key", window, cx)
        });
        panel.proposal = Some(Mode::Fixed(good.discover.time.clone()));
        (id, generation)
    });
    visual.run_until_parked();
    let restore = visual.debug_bounds("remote-restore-applied").unwrap();
    visual.simulate_click(restore.center(), Modifiers::default());
    visual.run_until_parked();
    panel.read_with(visual, |panel, cx| {
        let restored = panel.candidate(cx).unwrap();
        assert_eq!(restored.id, good.id);
        assert_eq!(restored.endpoint, good.endpoint);
        assert_eq!(restored.name, good.name);
        assert_eq!(
            restored.discover, good.discover,
            "query, pinned/disabled/negated filters, time, columns and sort restore exactly"
        );
        assert!(
            panel.url.read(cx).value().is_empty(),
            "pending import URL cannot masquerade as the restored connection"
        );
        assert!(
            panel.key.read(cx).value().is_empty() && panel.key.read(cx).presentation().is_masked()
        );
        assert_eq!(
            panel.generation,
            old_generation + 1,
            "one Stop, no Apply/Start command"
        );
        assert_eq!(panel.worker.generation(), panel.generation);
        assert!(
            !panel.following
                && !panel.busy
                && !panel.credential_pending
                && panel._credential.is_none()
                && panel.proposal.is_none()
        );
        assert!(Arc::ptr_eq(
            panel.data.as_ref().unwrap(),
            &good_loaded.window
        ));
        assert_eq!(panel.range.as_ref().unwrap(), &good_loaded.range);
        let entity = panel.view.as_ref().unwrap();
        assert_eq!(entity.entity_id(), view_id);
        let view = entity.read(cx);
        assert_eq!(view.selected, Some(0));
        assert_eq!(view.anchor, Some(0));
        assert!(view.marks.contains(&(0, 0)));
        assert_eq!(
            view.raw_at(0).unwrap(),
            good_loaded
                .window
                .index
                .raw(&good_loaded.window.bytes, 0)
                .unwrap()
        );
    });
    assert_eq!(
        std::fs::read(&saved_path).unwrap(),
        saved,
        "restore is draft-only; it never rewrites saved sources or stores credentials"
    );
    let foreign = loaded(&source("Other connection"), "stale");
    panel.update(visual, |panel, cx| {
        panel.receive(history(old_generation, foreign), cx);
        assert!(
            Arc::ptr_eq(panel.data.as_ref().unwrap(), &good_loaded.window),
            "pre-restore reply cannot replace the retained snapshot"
        );
    });
    let mut exported = Vec::new();
    good_loaded.window.export(&mut exported).unwrap();
    let event: serde_json::Value = serde_json::from_slice(&exported).unwrap();
    assert_eq!(event["_id"], "kept");
    assert_eq!(event["_source"]["nested"]["all"], serde_json::json!([1, 2]));
}

#[gpui::test]
fn restore_applied_control_requires_successful_history_and_waits_for_pending_credentials(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&state, cx);
    });
    let draft = source("Draft only");
    let good = loaded(&source("Applied"), "applied");
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1350.), px(950.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let generation = panel.update_in(visual, |panel, window, cx| {
        panel.choose(draft.clone(), window, cx);
        panel.generation
    });
    visual.run_until_parked();
    let restore = visual.debug_bounds("remote-restore-applied").unwrap();
    visual.simulate_click(restore.center(), Modifiers::default());
    panel.read_with(visual, |panel, cx| {
        assert_eq!(panel.generation, generation);
        assert_eq!(panel.candidate(cx).unwrap().id, draft.id);
        assert!(panel.data.is_none());
    });
    panel.update(visual, |panel, cx| {
        panel.receive(history(panel.generation, good.clone()), cx);
        panel.busy = true;
        panel.credential_pending = true;
    });
    visual.run_until_parked();
    let restore = visual.debug_bounds("remote-restore-applied").unwrap();
    visual.simulate_click(restore.center(), Modifiers::default());
    let foreign = loaded(&source("Pending foreign"), "not accepted");
    panel.update(visual, |panel, cx| {
        assert_eq!(panel.generation, generation);
        panel.receive(history(generation, foreign), cx);
        assert!(
            Arc::ptr_eq(panel.data.as_ref().unwrap(), &good.window),
            "credential-pending guard rejects even a matching-generation update"
        );
        panel.busy = false;
        panel.credential_pending = false;
    });
    visual.run_until_parked();
    let restore = visual.debug_bounds("remote-restore-applied").unwrap();
    visual.simulate_click(restore.center(), Modifiers::default());
    visual.run_until_parked();
    panel.read_with(visual, |panel, cx| {
        assert_eq!(panel.candidate(cx).unwrap().id, good.source.id);
        assert_eq!(panel.generation, generation + 1);
        assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &good.window));
    });
}
