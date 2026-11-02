use super::*;
use okilum_t3::{message_types::Author, projection::TurnRow};

#[gpui::test]
fn typed_1478_snapshot_caches_data_only_and_tracks_native_role_identity_and_removal(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let parent = cx.new(|cx| {
            let mut view = ProjectView::empty(None, window, cx);
            view.active = false;
            view
        });
        let feed = parent.read(cx).feed.clone();
        owner = Some(parent);
        Root::new(feed, window, cx)
    });
    let parent = owner.unwrap();
    let feed = parent.read_with(visual, |parent, _| parent.feed.clone());
    let scope = super::super::composer::TypedScope {
        environment: "https://typed.invalid".into(),
        project: "project".into(),
        thread: "thread".into(),
        generation: 1,
    };
    let key = |thread: &str, id: &str| RowKey {
        thread: thread.into(),
        item: id.into(),
    };
    let row = |thread: &str, id: &str, kind: &str, text: &str| {
        Arc::new(TurnRow {
            key: key(thread, id),
            local: true,
            item: Arc::new(
                serde_json::json!({"id":id,"type":kind,"text":text,"status":"completed"}),
            ),
        })
    };
    let source = "FR:  untouched **source**\r\n🧠  ";
    let initial = feed.update(visual, |feed, cx| {
        feed.apply_native(
            NativeChanges {
                rows: vec![
                    row("thread", "preallocated", "user_message", source),
                    row(
                        "thread",
                        "manager",
                        "assistant_message",
                        "DEC: not inferred",
                    ),
                    row("foreign", "secret", "assistant_message", "foreign"),
                ],
                order: Some(vec![
                    key("thread", "preallocated"),
                    key("thread", "manager"),
                    key("foreign", "secret"),
                ]),
                ..Default::default()
            },
            &[],
            cx,
        );
        let sources = feed.typed_snapshot(&scope);
        assert_eq!(sources.len(), 2, "foreign thread source is excluded");
        assert_eq!(sources[0].text, source);
        assert_eq!(sources[0].author, Author::User);
        assert!(sources[0].aliases.contains(&"preallocated".to_owned()));
        assert_eq!(sources[1].author, Author::Manager);
        assert_eq!(sources[1].text, "DEC: not inferred");
        assert_eq!(
            feed.prompt_history().len(),
            1,
            "typed snapshot does not change user-only prompt recall"
        );
        (feed.typed_revision(), sources)
    });
    visual.run_until_parked();
    visual.simulate_resize(size(px(520.), px(440.)));
    visual.run_until_parked();
    feed.update(visual, |feed, cx| {
        feed.set_controls(true, false, cx);
        feed.typed_labels_changed(cx);
        feed.scroller
            .update(cx, |state, cx| state.scroll_to_end(cx));
        assert_eq!(feed.typed_revision(), initial.0);
        assert!(
            Arc::ptr_eq(&feed.typed_snapshot(&scope), &initial.1),
            "paint, scroll and controls reuse the exact cached source allocation"
        );
        feed.apply_native(
            NativeChanges {
                rows: vec![row("thread", "preallocated", "assistant_message", source)],
                ..Default::default()
            },
            &[],
            cx,
        );
        assert!(feed.typed_revision() > initial.0);
        let changed = feed.typed_snapshot(&scope);
        assert!(!Arc::ptr_eq(&changed, &initial.1));
        assert_eq!(
            changed[0].author,
            Author::Manager,
            "role-only publication invalidates the cache"
        );
        let revision = feed.typed_revision();
        feed.apply_native(
            NativeChanges {
                rows: vec![row("thread", "preallocated", "assistant_message", "edited")],
                ..Default::default()
            },
            &[],
            cx,
        );
        assert!(feed.typed_revision() > revision);
        assert_eq!(feed.typed_snapshot(&scope)[0].text, "edited");
        feed.apply_native(
            NativeChanges {
                removed: vec![key("thread", "preallocated")],
                ..Default::default()
            },
            &[],
            cx,
        );
        assert_eq!(feed.typed_snapshot(&scope).len(), 1);
        let revision = feed.typed_revision();
        feed.reset(cx);
        assert!(feed.typed_revision() > revision);
        assert!(feed.typed_snapshot(&scope).is_empty());
    });
}
