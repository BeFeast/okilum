use super::*;
use okilum_t3::message_types::{Author, Provenance};
use std::{cell::RefCell, rc::Rc};

struct Harness(Entity<Composer>);
impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .child(v_flex().w(px(640.)).child(self.0.clone()))
            .child(TypedPanelSlot(self.0.clone()))
    }
}
fn scope() -> TypedScope {
    TypedScope {
        environment: "https://typed.invalid".into(),
        project: "project".into(),
        thread: "thread".into(),
        generation: 1,
    }
}

#[gpui::test]
fn typed_1478_mounted_type_token_undo_filters_and_exact_source_controls(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let retained = opened.clone();
    let corrections = Rc::new(RefCell::new(Vec::new()));
    let retained_corrections = corrections.clone();
    let sent = Rc::new(RefCell::new(Vec::new()));
    let retained_sent = sent.clone();
    let mut mounted = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true));
        let composer = cx.new(|cx| {
            let mut composer = Composer::new(input, cx);
            composer.set_controls(true, false, cx);
            let mut draft = TypedDraft::from_text("BUG: Original **bytes**\r\n🧠  ".into());
            assert!(draft.detect_token());
            composer
                .restore_typed_context(
                    scope(),
                    draft.text.clone(),
                    Some(draft.retained_metadata()),
                    window,
                    cx,
                )
                .unwrap();
            let manager = TypedMessage {
                key: MessageKey {
                    environment: scope().environment,
                    project: "project".into(),
                    thread: "thread".into(),
                    message: "exact-manager".into(),
                },
                text: "Do we record this decision?".into(),
                metadata: Some(TypeMetadata {
                    kind: MessageType::Decision,
                    provenance: Provenance::UserCorrection,
                    author: Author::Manager,
                }),
            };
            let legacy = TypedMessage {
                key: MessageKey {
                    message: "legacy".into(),
                    ..manager.key.clone()
                },
                text: "plain chat".into(),
                metadata: None,
            };
            composer.set_typed_sources(
                &scope(),
                vec![manager.clone(), legacy.clone()],
                [
                    (manager.key.clone(), Author::Manager),
                    (legacy.key, Author::User),
                ]
                .into_iter()
                .collect(),
                true,
                cx,
            );
            cx.subscribe(&cx.entity(), move |_, _, event: &TypedMessageOpened, _| {
                retained
                    .borrow_mut()
                    .push((event.0.clone(), event.1.clone()))
            })
            .detach();
            cx.subscribe(&cx.entity(), move |_, _, event: &RetypeRequested, _| {
                retained_corrections
                    .borrow_mut()
                    .push((event.expected.clone(), event.replacement.clone()))
            })
            .detach();
            cx.subscribe(&cx.entity(), move |this, _, _: &SendRequested, cx| {
                retained_sent.borrow_mut().push(this.typed_source_text(cx))
            })
            .detach();
            composer
        });
        mounted = Some(composer.clone());
        Root::new(cx.new(|_| Harness(composer)), window, cx)
    });
    let composer = mounted.unwrap();
    visual.simulate_resize(size(px(1100.), px(800.)));
    visual.run_until_parked();
    let undo = visual
        .debug_bounds("project-type-undo")
        .expect("positive control: real token pill Undo is mounted");
    visual.simulate_click(undo.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-type-undo").is_none());
    composer.read_with(visual, |composer, cx| {
        assert_eq!(
            composer.typed_source_text(cx),
            "BUG: Original **bytes**\r\n🧠  "
        );
        assert!(composer.typed_metadata().is_none());
    });
    let status = visual.debug_bounds("project-type-Status").unwrap();
    visual.simulate_click(status.center(), Modifiers::default());
    visual.run_until_parked();
    composer.read_with(visual, |composer, _| {
        assert_eq!(composer.typed_metadata().unwrap().kind, MessageType::Status)
    });
    assert!(
        sent.borrow().is_empty(),
        "changing a type never sends a message"
    );
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        *sent.borrow(),
        vec!["BUG: Original **bytes**\r\n🧠  ".to_owned()]
    );
    let panel = visual.debug_bounds("project-type-panel").unwrap();
    visual.simulate_click(panel.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-typed-panel").is_some());
    let source = visual.debug_bounds("typed-source-1").unwrap();
    visual.simulate_click(source.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(opened.borrow()[0].0, scope());
    assert_eq!(opened.borrow()[0].1.key.message, "exact-manager");
    let correction = visual.debug_bounds("typed-retype-1-Bug").unwrap();
    visual.simulate_click(correction.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(corrections.borrow().len(), 1);
    let corrections = corrections.borrow();
    assert_eq!(corrections[0].0.text, corrections[0].1.text);
    assert_eq!(corrections[0].1.metadata.unwrap().author, Author::Manager);
    assert_eq!(
        corrections[0].1.metadata.unwrap().provenance,
        Provenance::UserCorrection
    );
    drop(corrections);
    let filter = visual.debug_bounds("typed-filter-Chat").unwrap();
    visual.simulate_click(filter.center(), Modifiers::default());
    visual.run_until_parked();
    let source = visual.debug_bounds("typed-source-1").unwrap();
    visual.simulate_click(source.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        opened.borrow()[1].1.key.message,
        "legacy",
        "actual Chat filter keeps legacy messages discoverable"
    );
}

#[gpui::test]
fn typed_1478_history_recall_replaces_the_old_token_instead_of_prepending_it(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let mut retained_parent = None;
    let mut mounted = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let parent = cx.new(|cx| { let mut view = ProjectView::empty(None, window, cx); view.active = false; view });
        let feed = parent.read(cx).feed.clone();
        let item = serde_json::from_value::<okilum_t3::TimelineItem>(serde_json::json!({"position":1,"itemId":"prompt","type":"user_message","status":"completed","text":"recalled original","updatedAt":"now"})).unwrap();
        feed.update(cx, |feed, cx| feed.apply(std::iter::once(&item), None, cx));
        let input = cx.new(|cx| TextareaState::new(window, cx));
        let composer = cx.new(|cx| {
            let mut composer = Composer::new(input, cx);
            composer.bind_history(feed);
            composer.set_controls(true, false, cx);
            let mut draft = TypedDraft::from_text("BUG ".into());
            assert!(draft.detect_token());
            composer.restore_typed_context(scope(), draft.text.clone(), Some(draft.retained_metadata()), window, cx).unwrap();
            composer.input.update(cx, |input, cx| input.focus(window, cx));
            composer
        });
        retained_parent = Some(parent);
        mounted = Some(composer.clone());
        Root::new(composer, window, cx)
    });
    let composer = mounted.unwrap();
    visual.run_until_parked();
    composer.read_with(visual, |composer, cx| {
        assert_eq!(composer.input.read(cx).value().as_str(), "");
        assert_eq!(
            composer.typed_source_text(cx),
            "BUG ",
            "positive control: a hidden prefix exists before recall"
        );
    });
    visual.simulate_keystrokes("up");
    visual.run_until_parked();
    composer.read_with(visual, |composer, cx| {
        assert_eq!(composer.typed_source_text(cx), "recalled original");
        assert_eq!(
            composer.input.read(cx).value().as_str(),
            "recalled original"
        );
        assert!(composer.typed_metadata().is_none());
        assert!(!composer.typed_draft.has_token_pill());
    });
    drop(retained_parent);
}
