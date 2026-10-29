//! Linux color-emoji regression against the real platform shaper and rasterizer.

#[cfg(test)]
mod tests {
    use gpui::{font, point, px, FontRun, PlatformTextSystem, RenderGlyphParams};
    use std::{borrow::Cow, sync::Arc};

    fn fixture() -> Arc<dyn PlatformTextSystem> {
        let system = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Noto Sans");
        system
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../../assets/brand/fonts/noto-sans-400.ttf")),
                Cow::Borrowed(include_bytes!(
                    "../../assets/brand/fonts/cascadia-code-400.ttf"
                )),
                Cow::Borrowed(include_bytes!(
                    "../../tests/fixtures/fonts/noto-color-emoji-cbdt.ttf"
                )),
            ])
            .unwrap();
        Arc::new(system)
    }

    #[test]
    fn ubuntu_cbdt_font_is_reachable_through_platform_fallback() {
        let system = fixture();
        assert!(system
            .all_font_names()
            .iter()
            .any(|name| name == "Noto Color Emoji"));
        let body = system.font_id(&font("Noto Sans")).unwrap();
        assert!(
            system.glyph_for_char(body, 'm').is_some(),
            "real Latin font control"
        );
        let line = system.layout_line(
            "🧠",
            px(18.),
            &[FontRun {
                len: "🧠".len(),
                font_id: body,
            }],
        );
        let glyphs: Vec<_> = line.runs.iter().flat_map(|run| &run.glyphs).collect();
        assert!(!glyphs.is_empty(), "real shaping produced glyphs");
        assert!(
            glyphs.iter().all(|glyph| glyph.id.0 != 0 && glyph.is_emoji),
            "CBDT fallback exists"
        );
    }

    #[test]
    fn ubuntu_cbdt_emoji_shape_and_raster_in_note_and_code_runs() {
        for family in ["Noto Sans", "Cascadia Code"] {
            // Isolate the font database: the test cannot borrow a successful
            // lookup or installed font from a previous test or the host OS.
            let system = super::super::text_platform::shaper(fixture());
            let body = system.font_id(&font(family)).unwrap();
            let emoji = system
                .font_id(&font("Noto Color Emoji"))
                .expect("named emoji font remains available");
            assert!(
                system.glyph_for_char(emoji, 'm').is_none(),
                "stock emoji face has no Latin m"
            );
            for sample in ["✅", "❌", "🐕", "🧠", "👩‍💻"] {
                let prefix = if family == "Noto Sans" {
                    "Note: "
                } else {
                    "// code: "
                };
                let text = format!("{prefix}{sample} tail");
                let runs = [
                    FontRun {
                        len: prefix.len(),
                        font_id: body,
                    },
                    FontRun {
                        len: sample.len(),
                        font_id: emoji,
                    },
                    FontRun {
                        len: " tail".len(),
                        font_id: body,
                    },
                ];
                let layout = system.layout_line(&text, px(18.), &runs);
                assert_eq!(layout.len, text.len(), "source bytes remain exact");
                let glyphs: Vec<_> = layout
                    .runs
                    .iter()
                    .flat_map(|run| {
                        run.glyphs
                            .iter()
                            .filter(|glyph| {
                                (prefix.len()..prefix.len() + sample.len()).contains(&glyph.index)
                            })
                            .map(move |glyph| (run.font_id, glyph))
                    })
                    .collect();
                assert!(!glyphs.is_empty(), "{family}: {sample} was shaped");
                for (font_id, glyph) in glyphs {
                    assert!(
                        glyph.id.0 != 0 && glyph.is_emoji,
                        "{family}: {sample} uses the color face"
                    );
                    let params = RenderGlyphParams {
                        font_id,
                        glyph_id: glyph.id,
                        font_size: px(18.),
                        subpixel_variant: point(0, 0),
                        scale_factor: 1.,
                        is_emoji: glyph.is_emoji,
                        subpixel_rendering: false,
                        dilation: 0,
                    };
                    let bounds = system.glyph_raster_bounds(&params).unwrap();
                    let (size, pixels) = system.rasterize_glyph(&params, bounds).unwrap();
                    assert!(size.width.0 > 0 && size.height.0 > 0);
                    assert_eq!(
                        pixels.len(),
                        size.width.0 as usize * size.height.0 as usize * 4
                    );
                    assert!(
                        pixels
                            .chunks_exact(4)
                            .any(|pixel| pixel[3] > 0
                                && (pixel[0] != pixel[1] || pixel[1] != pixel[2])),
                        "{family}: {sample} has actual color pixels, not a mask/tofu"
                    );
                }
            }
        }
    }
}
