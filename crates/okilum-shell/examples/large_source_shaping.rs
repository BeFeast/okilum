//! Focused native text-system regressions without compiling the entire Reader test harness.
#![allow(dead_code)]
mod brand {
    pub const SANS_FONT: &str = "Noto Sans";
}
#[path = "../src/platform/linux_emoji.rs"]
mod linux_emoji;
#[path = "../src/platform/text_diagnostics.rs"]
mod text_diagnostics;
mod text_platform {
    pub fn shaper(
        inner: std::sync::Arc<dyn gpui::PlatformTextSystem>,
    ) -> std::sync::Arc<dyn gpui::PlatformTextSystem> {
        #[cfg(target_os = "linux")]
        {
            std::sync::Arc::new(super::linux_emoji::EmojiTextSystem::new(inner))
        }
        #[cfg(not(target_os = "linux"))]
        {
            inner
        }
    }
}
#[path = "../src/platform/shaped_line_cache.rs"]
mod shaped_line_cache;
#[cfg(test)]
#[path = "../../../vendor/gpui-component/crates/base/src/input/bidi_geometry.rs"]
mod vendor_bidi_geometry;
fn main() -> anyhow::Result<()> {
    use gpui::{px, FontRun, PlatformTextSystem};
    use std::{borrow::Cow, io::Write as _, time::Instant};
    let file = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Pass a synthetic ASCII source file"))?;
    let mut text = std::fs::read_to_string(file)?;
    let platform = gpui_platform::current_platform(true);
    let shaper = text_platform::shaper(platform.text_system());
    shaper.add_fonts(vec![Cow::Borrowed(include_bytes!(
        "../assets/brand/fonts/cascadia-code-400.ttf"
    ))])?;
    let font = shaper.font_id(&gpui::font("Cascadia Code"))?;
    let cache =
        shaped_line_cache::ShapedLineCache::new(shaper, shaped_line_cache::SHAPED_LINES_MAX_BYTES);
    for iteration in 0..6 {
        let started = Instant::now();
        let layout = cache.layout_line(
            &text,
            px(16.),
            &[FontRun {
                len: text.len(),
                font_id: font,
            }],
        );
        let elapsed = started.elapsed();
        writeln!(
            std::io::stdout(),
            "iteration={iteration} bytes={} glyphs={} shape_ms={:.3}",
            text.len(),
            layout.runs.iter().map(|r| r.glyphs.len()).sum::<usize>(),
            elapsed.as_secs_f64() * 1000.
        )?;
        text.insert(1, ' ');
    }
    Ok(())
}
