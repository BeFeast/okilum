//! Right-to-left text in visual order on Windows (#1121). Everything is the
//! platform text system except `layout_line`, whose glyph positions go
//! through `bidi_layout::repair`. `text_platform` puts it under the
//! shaped-line cache. Delegation only: when GPUI's `PlatformTextSystem`
//! trait changes, this file fails to compile instead of drifting.
use super::bidi_layout;
use anyhow::Result;
use gpui::{
    DevicePixels, Font, FontId, FontMetrics, FontRun, GlyphId, Hsla, LineLayout, Pixels,
    PlatformTextSystem, RenderGlyphParams, Size, TextRenderingMode,
};
use std::{borrow::Cow, sync::Arc};

pub(crate) struct BidiTextSystem(pub(crate) Arc<dyn PlatformTextSystem>);

impl PlatformTextSystem for BidiTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.0.add_fonts(fonts)
    }
    fn all_font_names(&self) -> Vec<String> {
        self.0.all_font_names()
    }
    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        self.0.font_id(descriptor)
    }
    fn prewarm_fonts(&self, font_ids: &[FontId]) {
        self.0.prewarm_fonts(font_ids)
    }
    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.0.font_metrics(font_id)
    }
    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<gpui::Bounds<f32>> {
        self.0.typographic_bounds(font_id, glyph_id)
    }
    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.0.advance(font_id, glyph_id)
    }
    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.glyph_for_char(font_id, ch)
    }
    fn glyph_raster_bounds(
        &self,
        params: &RenderGlyphParams,
    ) -> Result<gpui::Bounds<DevicePixels>> {
        self.0.glyph_raster_bounds(params)
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: gpui::Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.rasterize_glyph(params, raster_bounds)
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        bidi_layout::repair(text, self.0.layout_line(text, font_size, runs))
    }
    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.0.recommended_rendering_mode(font_id, font_size)
    }
    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.0.glyph_dilation_for_color(color)
    }
}
