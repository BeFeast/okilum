//! A font-aware unit-test shaper. GPUI's default NoopTextSystem gives every
//! family the same metrics, so it cannot detect a missed font relayout.
use gpui::*;
use std::borrow::Cow;

pub(crate) struct FontAware;
fn scale(id: FontId) -> f32 {
    if id == FontId(2) {
        2.
    } else {
        1.
    }
}
impl PlatformTextSystem for FontAware {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        NoopTextSystem.add_fonts(fonts)
    }
    fn all_font_names(&self) -> Vec<String> {
        vec![
            super::Role::Note.default_family().into(),
            super::Role::Code.default_family().into(),
        ]
    }
    fn font_id(&self, font: &Font) -> anyhow::Result<FontId> {
        Ok(FontId(
            if font.family.as_ref() == super::Role::Code.default_family() {
                2
            } else {
                1
            },
        ))
    }
    fn font_metrics(&self, id: FontId) -> FontMetrics {
        NoopTextSystem.font_metrics(id)
    }
    fn typographic_bounds(&self, id: FontId, glyph: GlyphId) -> anyhow::Result<Bounds<f32>> {
        NoopTextSystem.typographic_bounds(id, glyph)
    }
    fn advance(&self, id: FontId, glyph: GlyphId) -> anyhow::Result<Size<f32>> {
        let mut advance = NoopTextSystem.advance(id, glyph)?;
        advance.width *= scale(id);
        Ok(advance)
    }
    fn glyph_for_char(&self, id: FontId, ch: char) -> Option<GlyphId> {
        NoopTextSystem.glyph_for_char(id, ch)
    }
    fn glyph_raster_bounds(
        &self,
        params: &RenderGlyphParams,
    ) -> anyhow::Result<Bounds<DevicePixels>> {
        NoopTextSystem.glyph_raster_bounds(params)
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> anyhow::Result<(Size<DevicePixels>, Vec<u8>)> {
        NoopTextSystem.rasterize_glyph(params, bounds)
    }
    fn layout_line(&self, text: &str, size: Pixels, runs: &[FontRun]) -> LineLayout {
        let mut result = NoopTextSystem.layout_line("", size, &[]);
        result.len = text.len();
        let mut at = 0;
        for run in runs {
            let mut line = NoopTextSystem.layout_line(&text[at..at + run.len], size, &[]);
            for shaped in &mut line.runs {
                shaped.font_id = run.font_id;
                for glyph in &mut shaped.glyphs {
                    glyph.index += at;
                    glyph.position.x = result.width + glyph.position.x * scale(run.font_id);
                }
            }
            result.width += line.width * scale(run.font_id);
            result.runs.extend(line.runs);
            at += run.len;
        }
        result
    }
    fn recommended_rendering_mode(&self, id: FontId, size: Pixels) -> TextRenderingMode {
        NoopTextSystem.recommended_rendering_mode(id, size)
    }
}
