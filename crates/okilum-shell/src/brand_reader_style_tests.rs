//! Brand themes as the Reader draws them: `reader_text_style` lives in the
//! app crate, so these two brand tests moved here from
//! `okilum_shell_kit::brand::tests` (#3233).
use super::*;
// `gpui::test` expands to `#[test]`; without this the glob import makes it recurse.
use ::core::prelude::v1::test;
use brand::{
    apply_theme, palette, reader_palette_for_theme, syntax_theme, theme_palette, ThemeChoice,
    ThemeId,
};
use gpui_component::ThemeMode;

#[gpui::test]
fn issue3156_theme_switch_swaps_the_live_syntax_palette(cx: &mut gpui::TestAppContext) {
    cx.update(crate::test_init);
    cx.update(|cx| {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            Theme::change(mode, None, cx);
            let dark = mode == ThemeMode::Dark;
            // The picker sets the choice and re-applies, without
            // `Theme::change`, so the palette must also reset on the way back.
            for theme in [
                ThemeId::Obsidianite,
                ThemeId::Graphite,
                ThemeId::Obsidianite,
            ] {
                cx.set_global(ThemeChoice(theme));
                apply_theme(cx);
                let live = Theme::global(cx).highlight_theme.clone();
                assert!(
                    std::sync::Arc::ptr_eq(&live, &syntax_theme(theme, dark)),
                    "{} {mode:?}",
                    theme.key()
                );
                assert!(std::sync::Arc::ptr_eq(
                    &reader_text_style(Theme::global(cx)).highlight_theme,
                    &live
                ));
            }
        }
    });
}

#[gpui::test]
fn selected_theme_drives_palette_and_native_tokens(cx: &mut gpui::TestAppContext) {
    cx.update(crate::test_init);
    cx.update(|cx| {
        for theme in [
            ThemeId::Nord,
            ThemeId::HighContrast,
            ThemeId::Okilum,
            ThemeId::Obsidianite,
        ] {
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                cx.set_global(ThemeChoice(theme));
                Theme::change(mode, None, cx);
                apply_theme(cx);
                let expected = theme_palette(theme, mode == ThemeMode::Dark);
                let p = palette(cx);
                assert_eq!(p.surface, expected.surface);
                let t = Theme::global(cx);
                assert_eq!(t.background, expected.canvas);
                assert_eq!(t.tokens.button_primary.color, expected.accent);
                assert_eq!(t.danger_foreground, expected.on_status);
                // #3147: Reader headings follow the theme's heading colour.
                let heading = reader_palette_for_theme(t).heading;
                assert_ne!(heading, expected.text);
                // #3153: per level, with `heading` as the fallback.
                let levels = reader_palette_for_theme(t).headings;
                assert_eq!(reader_text_style(t).heading_colors, levels.map(Some));
                // #3154: the Reader draws the theme's decoration slots, and
                // `<mark>` takes the theme's highlight (C3).
                let reader = reader_palette_for_theme(t);
                assert_eq!(
                    reader_text_style(t).decorations,
                    gpui_base::text::TextDecorations {
                        highlight: Some(reader.highlight),
                        ..reader.decorations
                    }
                );
            }
        }
    });
}
