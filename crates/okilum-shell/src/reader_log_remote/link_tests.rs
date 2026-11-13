use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::link::RemoteLink;
use okilum_core::remote_log::live::Loaded;

fn source() -> Source {
    Source::import("Linked", "https://logs.example/app/discover#/?_a=(index:events,query:(language:kuery,query:'service:checkout'),columns:!(description),sort:!(!('@timestamp',desc)))&_g=(time:(from:now-15m,to:now),refreshInterval:(pause:!f,value:5000))").unwrap()
}

/// #1441: Copy Okilum link freezes what the window shows (applied query and
/// filters over the loaded absolute range, live paused) and carries neither
/// the endpoint nor event data; before any load it stays unavailable.
#[gpui::test]
fn copy_link_reopens_the_applied_query_over_the_loaded_range(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    let source = source();
    remote_log::save_source(&state, &vault, &source).unwrap();
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
    // Control: nothing loaded, nothing copied.
    panel.update_in(visual, |panel, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("untouched".into()));
        panel.copy_link(cx);
    });
    assert_eq!(
        visual
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|c| c.text())
            .as_deref(),
        Some("untouched")
    );
    let loaded = TimeRange {
        from: "2026-11-13T10:00:00Z".into(),
        to: "2026-11-13T10:15:00Z".into(),
    };
    let data = Arc::new(EventWindow::new(vec![Arc::new(serde_json::json!({"_index":"events","_id":"one","_source":{"description":"private event text"}}))], &source).unwrap());
    panel.update_in(visual, |panel, window, cx| {
        panel._events = Task::ready(());
        panel.choose(source.clone(), window, cx);
        panel.receive(
            Update {
                generation: panel.generation,
                following: false,
                loaded: Some(Arc::new(Loaded {
                    quality: Default::default(),
                    source: source.clone(),
                    range: loaded.clone(),
                    window: data.clone(),
                    more: false,
                })),
                status: Status::History {
                    quality: Default::default(),
                    complete: true,
                    limited: false,
                    warning: None,
                },
            },
            cx,
        );
        panel.copy_link(cx);
        assert!(panel.error.is_none(), "{:?}", panel.error);
        assert!(panel.message.contains("Okilum link copied"));
    });
    let text = visual
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|c| c.text())
        .expect("a link was copied");
    assert!(text.starts_with("okilum://logs?v=1&payload="), "{text}");
    let link = RemoteLink::parse(&text).unwrap().unwrap();
    assert_eq!(
        link,
        RemoteLink::for_view(&vault, &source, &loaded).unwrap()
    );
    assert_eq!(link.state.time, loaded);
    assert!(link.state.refresh_paused);
    assert_eq!(link.state.query, source.discover.query);
    let payload = serde_json::to_string(&link).unwrap();
    assert!(!payload.contains("logs.example") && !payload.contains("private event text"));
    cx.quit();
}
