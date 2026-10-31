use super::*;
use ::core::prelude::v1::test;

#[test]
fn catalog_uses_available_names_and_embedded_fallbacks() {
    let catalog = Catalog::new(vec![
        "Example Sans".into(),
        "example sans".into(),
        "".into(),
    ]);
    assert_eq!(
        catalog.0.len(),
        3,
        "deduplicated system plus two embedded families"
    );
    assert_eq!(
        catalog.resolve("EXAMPLE SANS", Role::Note).to_lowercase(),
        "example sans"
    );
    assert_eq!(
        catalog.resolve("font removed from this OS", Role::Note),
        brand::SANS_FONT
    );
    assert_eq!(
        catalog.resolve("font removed from this OS", Role::Code),
        brand::MONO_FONT
    );
}

#[gpui::test]
fn document_fonts_persist_and_missing_fonts_fall_back_without_rewriting(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        brand::apply_theme(cx);
        brand::load_fonts(cx).unwrap();
        install(cx);
        reader_ui_state::install(directory.path(), cx);
        assert_eq!(note(cx), brand::SANS_FONT);
        assert_eq!(code(cx), brand::MONO_FONT);
        reader_ui_state::set_selected_font(Role::Note, brand::MONO_FONT, cx);
        reader_ui_state::set_selected_font(Role::Code, brand::SANS_FONT, cx);
        reader_ui_state::flush(cx);
        reader_ui_state::install(directory.path(), cx);
        assert_eq!(note(cx), brand::MONO_FONT);
        assert_eq!(code(cx), brand::SANS_FONT);
        let style = reader_text_zoom::document_style(cx);
        assert_eq!(
            style.inline_code_font_family.as_deref(),
            Some(brand::SANS_FONT)
        );
        assert_eq!(
            style.code_block.text.font_family.as_deref(),
            Some(brand::SANS_FONT)
        );
        assert_eq!(
            cx.theme().font_family.as_ref(),
            brand::SANS_FONT,
            "chrome unchanged"
        );
        reader_ui_state::set_selected_font(Role::Note, "Missing Note Font", cx);
        reader_ui_state::set_selected_font(Role::Code, "Missing Code Font", cx);
        reader_ui_state::flush(cx);
        let path = directory.path().join("reader-ui.json");
        let bytes = std::fs::read(&path).unwrap();
        reader_ui_state::install(directory.path(), cx);
        assert_eq!(note(cx), brand::SANS_FONT);
        assert_eq!(code(cx), brand::MONO_FONT);
        assert_eq!(
            reader_ui_state::selected_font(Role::Note, cx),
            "Missing Note Font"
        );
        reader_ui_state::flush(cx);
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "fallback is not a preference write"
        );
    });
}

#[gpui::test]
fn font_picker_selects_via_keyboard_and_defaults_apply_without_restart(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        brand::apply_theme(cx);
        brand::load_fonts(cx).unwrap();
        install(cx);
        reader_ui_state::install(directory.path(), cx);
    });
    let mut picker = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| Picker::default());
        picker = Some(view.clone());
        Root::new(view, window, cx)
    });
    let picker = picker.unwrap();
    let draw = |visual: &mut VisualTestContext| {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
    };
    draw(visual);
    for (id, query, role, expected) in [
        ("note-font", brand::MONO_FONT, Role::Note, brand::MONO_FONT),
        ("code-font", brand::SANS_FONT, Role::Code, brand::SANS_FONT),
    ] {
        let bounds = visual
            .debug_bounds(id)
            .expect("actual font select rendered");
        visual.simulate_click(bounds.center(), Modifiers::default());
        draw(visual);
        visual.simulate_input(query);
        draw(visual);
        visual.simulate_keystrokes("enter");
        draw(visual);
        visual.update(|_, cx| assert_eq!(family(role, cx), expected));
    }
    let reset = visual.debug_bounds("document-fonts-default").unwrap();
    visual.simulate_click(reset.center(), Modifiers::default());
    draw(visual);
    picker.read_with(visual, |picker, cx| {
        assert_eq!(note(cx), brand::SANS_FONT);
        assert_eq!(code(cx), brand::MONO_FONT);
        for role in [Role::Note, Role::Code] {
            assert_eq!(
                picker.choices.as_ref().unwrap()[role.index()]
                    .read(cx)
                    .selected_value()
                    .unwrap()
                    .as_ref(),
                role.default_family()
            );
        }
    });
    visual.update(|_, cx| {
        reader_ui_state::flush(cx);
        reader_ui_state::install(directory.path(), cx);
        assert_eq!(note(cx), brand::SANS_FONT);
        assert_eq!(code(cx), brand::MONO_FONT);
    });
}
