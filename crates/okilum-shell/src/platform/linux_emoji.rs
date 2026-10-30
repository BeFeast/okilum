//! Resolve Linux emoji through the platform's working glyph fallback path.
//!
//! GPUI 0.3.3's named-family loader removes faces without a Latin `m`, including
//! Ubuntu's Noto Color Emoji. Its glyph-fallback loader accepts the same face
//! and already supports CBDT rasterization. Obtain that existing platform font
//! ID without invoking the destructive named lookup; all shaping and drawing
//! remain in the platform text system. No GPUI core or font bytes are patched.
use anyhow::{anyhow, Context as _, Result};
use gpui::{
    font, px, Bounds, DevicePixels, Font, FontId, FontMetrics, FontRun, GlyphId, Hsla, LineLayout,
    Pixels, PlatformTextSystem, RenderGlyphParams, Size, TextRenderingMode,
};
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};

pub(crate) struct EmojiTextSystem {
    inner: Arc<dyn PlatformTextSystem>,
    emoji: Mutex<Option<Result<FontId, String>>>,
}

impl EmojiTextSystem {
    pub(crate) fn new(inner: Arc<dyn PlatformTextSystem>) -> Self {
        Self {
            inner,
            emoji: Mutex::new(None),
        }
    }

    fn emoji_font(&self) -> Result<FontId> {
        let mut cached = self.emoji.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(result) = cached.as_ref() {
            return result.clone().map_err(|error| anyhow!(error));
        }
        let result = (|| {
            // The embedded body face has Latin coverage but no emoji. Several
            // default emoji exercise normal fallback so one absent glyph in
            // an older or subset face cannot disable the whole color font.
            let body = self.inner.font_id(&font(crate::brand::SANS_FONT))?;
            let sample = "😀🐕🧠";
            let line = self.inner.layout_line(
                sample,
                px(16.),
                &[FontRun {
                    len: sample.len(),
                    font_id: body,
                }],
            );
            line.runs
                .iter()
                .find_map(|run| {
                    run.glyphs
                        .iter()
                        .any(|glyph| glyph.id.0 != 0 && glyph.is_emoji)
                        .then_some(run.font_id)
                })
                .context("Noto Color Emoji is unavailable through platform glyph fallback")
        })();
        let result = result.map_err(|error: anyhow::Error| error.to_string());
        *cached = Some(result.clone());
        result.map_err(|error| anyhow!(error))
    }
}

impl PlatformTextSystem for EmojiTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let mut cached = self.emoji.lock().unwrap_or_else(|error| error.into_inner());
        let result = self.inner.add_fonts(fonts);
        *cached = None;
        result
    }
    fn all_font_names(&self) -> Vec<String> {
        self.inner.all_font_names()
    }
    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        if descriptor.family.as_ref() == "Noto Color Emoji" {
            return self.emoji_font();
        }
        self.inner.font_id(descriptor)
    }
    fn prewarm_fonts(&self, font_ids: &[FontId]) {
        self.inner.prewarm_fonts(font_ids)
    }
    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.inner.font_metrics(font_id)
    }
    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        self.inner.typographic_bounds(font_id, glyph_id)
    }
    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.inner.advance(font_id, glyph_id)
    }
    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.inner.glyph_for_char(font_id, ch)
    }
    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.inner.glyph_raster_bounds(params)
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.inner.rasterize_glyph(params, bounds)
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.inner.layout_line(text, font_size, runs)
    }
    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.inner.recommended_rendering_mode(font_id, font_size)
    }
    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.inner.glyph_dilation_for_color(color)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{font, point, px, FontRun, PlatformTextSystem, RenderGlyphParams};
    use std::{borrow::Cow, sync::Arc};

    fn fixture() -> Arc<dyn PlatformTextSystem> {
        fixture_with_emoji(include_bytes!(
            "../../tests/fixtures/fonts/noto-color-emoji-cbdt.ttf"
        ))
    }

    fn fixture_with_emoji(emoji: &'static [u8]) -> Arc<dyn PlatformTextSystem> {
        let system = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Noto Sans");
        system
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../../assets/brand/fonts/noto-sans-400.ttf")),
                Cow::Borrowed(include_bytes!(
                    "../../assets/brand/fonts/cascadia-code-400.ttf"
                )),
                Cow::Borrowed(emoji),
            ])
            .unwrap();
        Arc::new(system)
    }

    #[test]
    fn color_font_without_brain_still_resolves_its_available_emoji() {
        let inner = fixture_with_emoji(include_bytes!(
            "../../tests/fixtures/fonts/noto-color-emoji-no-brain-cbdt.ttf"
        ));
        let body = inner.font_id(&font("Noto Sans")).unwrap();
        let line = inner.layout_line(
            "🐕",
            px(18.),
            &[FontRun {
                len: "🐕".len(),
                font_id: body,
            }],
        );
        let fallback = line
            .runs
            .iter()
            .find(|run| {
                run.glyphs
                    .iter()
                    .any(|glyph| glyph.id.0 != 0 && glyph.is_emoji)
            })
            .expect("positive control: the real CBDT face supplies dog fallback");
        assert!(inner.glyph_for_char(fallback.font_id, '🧠').is_none());
        assert!(inner.glyph_for_char(fallback.font_id, '😀').is_none());
        assert!(inner.glyph_for_char(fallback.font_id, 'm').is_none());

        let system = super::super::text_platform::shaper(inner);
        let emoji = system
            .font_id(&font("Noto Color Emoji"))
            .expect("one unavailable probe glyph must not disable the color font");
        assert!(system.glyph_for_char(emoji, '🐕').is_some());
        assert_eq!(system.font_id(&font("Noto Color Emoji")).unwrap(), emoji);
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
            assert_eq!(system.font_id(&font("Noto Color Emoji")).unwrap(), emoji);
            assert!(
                system
                    .all_font_names()
                    .iter()
                    .any(|name| name == "Noto Color Emoji"),
                "lookup must not remove the face"
            );
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
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .any(|pixel| pixel[3] > 0
                                && (pixel[0] != pixel[1] || pixel[1] != pixel[2])),
                        "{family}: {sample} has actual color pixels, not a mask/tofu"
                    );
                }
            }
        }
    }

    #[test]
    fn missing_emoji_face_does_not_poison_a_later_font_addition() {
        let inner = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Noto Sans");
        inner
            .add_fonts(vec![Cow::Borrowed(include_bytes!(
                "../../assets/brand/fonts/noto-sans-400.ttf"
            ))])
            .unwrap();
        let system = super::super::text_platform::shaper(Arc::new(inner));
        let body = system.font_id(&font("Noto Sans")).unwrap();
        assert!(
            system.glyph_for_char(body, 'm').is_some(),
            "ordinary text still resolves"
        );
        assert!(system.font_id(&font("Noto Color Emoji")).is_err());
        system
            .add_fonts(vec![Cow::Borrowed(include_bytes!(
                "../../tests/fixtures/fonts/noto-color-emoji-cbdt.ttf"
            ))])
            .unwrap();
        let emoji = system.font_id(&font("Noto Color Emoji")).unwrap();
        assert!(
            system.glyph_for_char(emoji, '🧠').is_some(),
            "font addition resets the negative lookup cache"
        );
    }
}
