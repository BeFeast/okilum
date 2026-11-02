use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::{
    live::Loaded,
    result::{AuthError, HitTotal, ResultStatus, TotalRelation},
};

#[gpui::test]
fn direct_source_and_auth_replacement_keep_investigation_and_use_os_credentials(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    let state = directory.path().join("state");
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
    visual.simulate_resize(size(px(1500.), px(1100.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let (window_data, source) = panel.update_in(visual, |panel, window, cx| {
        panel._events = Task::ready(());
        panel.name.update(cx, |i,cx| i.set_value("Synthetic",window,cx));
        panel.url.update(cx, |i,cx| i.set_value("https://logs.example/s/demo",window,cx));
        panel.pattern.update(cx, |i,cx| i.set_value("remote-*:events",window,cx));
        panel.time_field.update(cx, |i,cx| i.set_value("event.time",window,cx));
        panel.import(window,cx);
        let source = panel.candidate(cx).unwrap();
        assert_eq!(source.index_pattern.as_deref(),Some("remote-*:events"));
        assert_eq!(source.time_field,"event.time"); assert_eq!(source.discover.sort[0].field,"event.time");
        assert_eq!(panel.worker.generation(),panel.generation,"adding a source never applies a query");
        let data = Arc::new(EventWindow::new(vec![Arc::new(serde_json::json!({"_index":"remote-a:events","_id":"kept","_source":{"service":"Checkout","duration":3}}))],&source).unwrap());
        let quality = ResultStatus { total:Some(HitTotal { value:19, relation:TotalRelation::Gte }),partial:true,failed_shards:1,..Default::default() };
        panel.receive(Update { generation:panel.generation,following:false,loaded:Some(Arc::new(Loaded {window:data.clone(),source:source.clone(),range:source.discover.time.clone(),more:false,quality:quality.clone()})),status:Status::History {complete:false,limited:false,warning:None,quality}},cx);
        panel.requested = Some(source.clone());
        panel.filters.update(cx,|i,cx|i.set_value("[unapplied invalid draft",window,cx));
        panel.kql.update(cx,|i,cx|i.set_value("unapplied query",window,cx));
        panel.receive(Update {generation:panel.generation,following:false,loaded:None,status:Status::AuthError(AuthError {status:401})},cx);
        (data,source)
    });
    visual.run_until_parked();
    let view_id = panel.update(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap();
        view.update(cx, |view, _| {
            view.selected = Some(0);
            view.anchor = Some(0);
            view.marks.insert((0, 0));
        });
        view.entity_id()
    });
    let replace = visual.debug_bounds("remote-replace-key").unwrap();
    visual.simulate_click(replace.center(), Modifiers::default());
    visual.run_until_parked();
    panel.read_with(visual,|panel,cx|{
        assert!(panel.error.as_ref().unwrap().contains("No OS-stored key"),"positive control: Replace reached the OS credential API; TestPlatform returns no credential");
        assert_eq!(panel.requested.as_ref().unwrap().id,source.id);
        assert_eq!(panel.filters.read(cx).value().as_str(),"[unapplied invalid draft");
        assert_eq!(panel.kql.read(cx).value().as_str(),"unapplied query");
        assert_eq!(panel.pattern.read(cx).value().as_str(),"remote-*:events");
        assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(),&window_data));
        assert!(panel.quality.as_ref().unwrap().label(1).contains("at least 19"));
        assert!(panel.quality.as_ref().unwrap().partial);
        let view=panel.view.as_ref().unwrap();assert_eq!(view.entity_id(),view_id);
        let view=view.read(cx);assert_eq!(view.selected,Some(0));assert!(view.marks.contains(&(0,0)));
    });
    assert!(
        remote_log::load_sources(&state, &vault).unwrap().is_empty(),
        "credential refusal cannot create a source or credential-file fallback"
    );
    cx.quit();
}

#[gpui::test]
fn stale_key_replacement_cannot_use_another_sources_input_or_interrupt_credentials(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    let state = directory.path().join("state");
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
    visual.run_until_parked();
    let panel = panel.unwrap();
    let first = Source::direct("First", "https://first.example", "events", "@timestamp").unwrap();
    let second =
        Source::direct("Second", "https://second.example", "events", "@timestamp").unwrap();
    panel.update_in(visual, |panel, window, cx| {
        panel._events = Task::ready(());
        panel.choose(first.clone(), window, cx);
        panel.requested = Some(first.clone());
        panel.auth_error = true;
        panel.choose(second.clone(), window, cx);
        panel.key.update(cx, |i, cx| {
            i.set_value("synthetic-second-source-key", window, cx)
        });
        let generation = panel.generation;
        // Deliver the callback of the now-dismissed replacement button.
        panel.replace_key(window, cx);
        assert_eq!(panel.generation, generation);
        assert!(panel.requested.is_none());
        assert!(panel._credential.is_none());
        assert_eq!(
            panel.key.read(cx).value().as_str(),
            "synthetic-second-source-key"
        );
        // Start follows the applied query, even when another source is a draft.
        panel.applied = Some(first.clone());
        panel.begin_live(Mode::Fixed(TimeRange {
            from: "2026-11-01T00:00:00Z".into(),
            to: "2026-11-05T00:00:00Z".into(),
        }));
        assert_eq!(panel.requested.as_ref().unwrap().id, first.id);
        panel.busy = false;
        panel.auth_error = true;
        let generation = panel.generation;
        panel.replace_key(window, cx);
        assert_eq!(panel.generation, generation);
        assert!(panel.error.as_ref().unwrap().contains("failed source"));
        assert_eq!(
            panel.key.read(cx).value().as_str(),
            "synthetic-second-source-key"
        );
        assert!(panel._credential.is_none());
        panel.requested = Some(second.clone());
        for pending in [false, true] {
            panel.busy = !pending;
            panel.credential_pending = pending;
            panel.replace_key(window, cx);
            assert_eq!(panel.generation, generation);
            assert!(panel._credential.is_none());
        }
        panel.busy = false;
        panel.credential_pending = false;
    });
    cx.quit();
}
