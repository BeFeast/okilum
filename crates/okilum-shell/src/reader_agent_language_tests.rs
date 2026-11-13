//! #1593: inline language marks and term hover in the source editor, and a
//! policy-only change refreshing them without a source edit.
use super::*;
use ::core::prelude::v1::test;

const SOURCE: &str = "# Rules\n\nВоркер мержит.\n\n`воркер`\n";
const POLICY: &str = "backlog:\n  - pattern: 'воркер'\n    good: worker\n";

fn replacements(reader: &Reader) -> Option<Vec<String>> {
    let published = reader.agent_language.as_ref()?.published()?;
    Some(
        published
            .report
            .diagnostics
            .iter()
            .map(|d| d.replacement.clone())
            .collect(),
    )
}

/// The checker runs on its own thread; drive the UI until it publishes.
fn settle(
    reader: &Entity<Reader>,
    visual: &mut VisualTestContext,
    until: impl Fn(&Reader) -> bool,
) -> bool {
    for _ in 0..300 {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        if reader.read_with(visual, |r, _| until(r)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[gpui::test]
fn issue1593_marks_hover_and_policy_refresh_without_a_source_edit(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("vault");
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("@Schema")).unwrap();
    std::fs::write(root.join("project/AGENTS.md"), SOURCE).unwrap();
    std::fs::write(root.join("@Schema/language.yaml"), POLICY).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("project/AGENTS.md".into()),
                    session_directory: Some(directory.path().join("state")),
                    index_dir: Some(directory.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
    assert!(
        settle(&reader, visual, |r| replacements(r)
            .is_some_and(|v| v.len() == 1)),
        "positive control: the vault policy finds the prose term only"
    );
    let input = reader.read_with(visual, |r, _| r.source_input().unwrap());
    let term = SOURCE.find("Воркер").unwrap();
    let term_range = term..term + "Воркер".len();
    let marked = reader.read_with(visual, |r, cx| {
        r.agent_language_marks
            .as_ref()
            .map(|marks| marks.collection.get_ranges(cx))
    });
    assert_eq!(
        marked,
        Some(vec![term_range.clone()]),
        "inline mark on the term"
    );

    let provider = reader_image_actions::source_hover(reader.downgrade());
    let text = input.read_with(visual, |input, _| input.text().clone());
    let hover = visual.update(|window, cx| provider.hover(&text, term + 2, window, cx));
    let hover = futures::executor::block_on(hover)
        .unwrap()
        .expect("term hover");
    let lsp_types::HoverContents::Markup(markup) = hover.contents else {
        panic!("plain text hover");
    };
    assert!(
        markup.value.contains("Latin form: worker"),
        "{}",
        markup.value
    );

    // Policy-only change: the source stays byte-identical. The watcher's
    // delivery of this write is covered in okilum-core watch tests.
    std::fs::write(
        root.join("@Schema/language.yaml"),
        "backlog:\n  - pattern: 'воркер'\n    good: worker-new\n  - pattern: 'мерж\\w*'\n    good: merge\n",
    )
    .unwrap();
    reader.update(visual, |r, cx| r.agent_language_policy_changed(cx));
    assert!(
        settle(&reader, visual, |r| replacements(r)
            == Some(vec!["worker-new".to_string(), "merge".to_string()])),
        "the open file's check uses the new policy without a source edit"
    );
    // Marks are applied from the published result on a later frame; wait for
    // them too, so a mark that never follows the policy still fails here.
    let marks = |r: &Reader, cx: &App| {
        r.agent_language_marks
            .as_ref()
            .map(|marks| marks.collection.get_ranges(cx).len())
    };
    let mut marked = None;
    for _ in 0..300 {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        marked = reader.read_with(visual, |r, cx| marks(r, cx));
        if marked == Some(2) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(marked, Some(2), "marks follow the new policy");
    assert_eq!(
        std::fs::read_to_string(root.join("project/AGENTS.md")).unwrap(),
        SOURCE
    );
}
