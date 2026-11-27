//! Reader-hosted structure tests moved from reader_code_file/structure.rs (#3238).
use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn code2810_tree_value_and_match_styles_follow_light_dark_and_high_contrast(
    cx: &mut TestAppContext,
) {
    let (code, v, _temp) = super::view_tests::fixture(
        cx,
        "data.json",
        "{\"title\":\"Account\",\"group\":{\"other\":\"Account\"}}",
    );
    code.update_in(v, |s, w, cx| {
        s.set_view(View::Tree, w, cx);
        let target = s.find_target(cx.entity(), cx);
        target.set_query("Account", w, cx);
    });
    super::view_tests::settle(v);
    for theme in [brand::ThemeId::Okilum, brand::ThemeId::HighContrast] {
        for mode in [
            gpui_component::ThemeMode::Light,
            gpui_component::ThemeMode::Dark,
        ] {
            v.update(|_, cx| {
                cx.set_global(brand::ThemeChoice(theme));
                gpui_component::Theme::change(mode, None, cx);
                brand::apply_theme(cx);
            });
            code.read_with(v, |s, cx| {
                let doc = s.document.as_ref().unwrap();
                assert_eq!(
                    s.structure.match_node,
                    Some(1),
                    "positive control: the actual query selected title"
                );
                let styles = s.row_highlights(1, &doc.row_label(1), cx);
                let string = cx
                    .theme()
                    .highlight_theme
                    .style
                    .syntax
                    .style("string")
                    .unwrap()
                    .color
                    .expect("real syntax token");
                assert!(
                    styles.iter().any(|(_, h)| h.color == Some(string)),
                    "string value retains the syntax color"
                );
                assert!(
                    styles.iter().any(|(_, h)| h.color == Some(string)
                        && h.background_color == Some(brand::reader_palette(cx).find_current)),
                    "current-match amber combines with syntax, without replacing it"
                );
                let styles = s.row_highlights(2, &doc.row_label(2), cx);
                assert!(
                    styles
                        .iter()
                        .any(|(_, h)| h.color == Some(brand::reader_palette(cx).text_faint)),
                    "counts use the theme count token"
                );
            });
        }
    }
}

#[test]
fn data_view_2669_minified_structure_folds_all_and_individual_nodes_across_pages() {
    crate::source_presentation::code_languages::install();
    let text = format!(
        "{{\"outer\":{{\"long\":\"{}\",\"inner\":{{\"answer\":42}}}},\"tail\":true}}",
        "x".repeat(100_000)
    );
    let doc = Document::new(text.clone(), "json");
    let mut view = Structure::default();
    view.reset(&doc);
    assert_eq!(view.visible, vec![1, 5]); // top-level keys; hidden document root
    view.all(true, &doc);
    assert_eq!(view.visible, vec![1, 5]);
    view.toggle(1, &doc);
    assert_eq!(view.visible.len(), 4);
    view.all(false, &doc);
    assert_eq!(view.visible.len(), doc.structure.len() - 1);
    assert_eq!(view.visible.len(), 5);
    let nested = doc
        .structure
        .iter()
        .position(|r| &doc.text[r.range.clone()] == "\"inner\":{\"answer\":42}")
        .unwrap();
    assert_eq!(doc.structure[nested].depth, 2);
    view.toggle(nested, &doc);
    assert!(!view.visible.contains(&(nested + 1)));
    assert!(
        view.visible.contains(&(doc.structure.len() - 1)),
        "a sibling remains visible"
    );
    let restored = doc
        .pages
        .iter()
        .map(|r| &doc.text[r.clone()])
        .collect::<String>();
    assert_eq!(restored, text);
}
#[gpui::test]
fn json2787_structure_labels_stay_inside_fixed_rows_at_every_width(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("__test2.json");
    std::fs::write(
        &path,
        format!(
            "{{\"info\":{{\"description\":\"{}\"}},\"paths\":{{\"long\":true}}}}",
            "wide label ".repeat(20_000)
        ),
    )
    .unwrap();
    cx.update(crate::test_init);
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            CodePreview::new(
                fixture.path().to_owned(),
                "__test2.json".into(),
                path.clone(),
                window,
                cx,
            )
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    preview.update_in(visual, |p, window, cx| p.set_view(View::Tree, window, cx));
    visual.run_until_parked();
    for width in [360., 640., 1200.] {
        visual.simulate_resize(size(px(width), px(600.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let visible = preview.read_with(visual, |p, _| {
            assert!(p.view == View::Tree);
            assert!(
                p.outline_input().is_none(),
                "a page is not an outline document"
            );
            p.structure.visible.clone()
        });
        assert!(
            visible.len() >= 2,
            "positive control: multiple structure rows are drawn"
        );
        for (row_selector, label_selector) in [
            ("data-structure-row-1", "data-node-label-1"),
            ("data-structure-row-3", "data-node-label-3"),
        ] {
            let row = visual.debug_bounds(row_selector).unwrap();
            let label = visual.debug_bounds(label_selector).unwrap();
            assert_eq!(row.size.height, px(32.));
            assert!(
                label.top() >= row.top() && label.bottom() <= row.bottom(),
                "label overlaps a neighbouring row at width {width}: {label:?} outside {row:?}"
            );
        }
    }
}
