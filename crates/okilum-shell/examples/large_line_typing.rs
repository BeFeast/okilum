//! Per-keystroke layout cost of typing into one very long unwrapped ASCII line
//! (#2803), measured headless with the native shaper behind the application's
//! shaped-line cache and GPUI's own `layout_line` entry point.
//!
//! The fixture is a synthetic minified OpenAPI-like document: string values
//! contain spaces, like the tester's file. Each size types the same words in
//! the middle of the line; a size-independent median is the goal.
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

use std::{borrow::Cow, io::Write as _, sync::Arc, time::Instant};

/// A deterministic minified OpenAPI-like document of at least `bytes`.
fn openapi_line(bytes: usize) -> String {
    let mut text = String::from("{\"openapi\":\"3.0.0\",\"info\":{\"title\":\"Synthetic API\",\"version\":\"1\"},\"paths\":{");
    let mut n = 0;
    while text.len() < bytes {
        text.push_str(&format!(
            "\"/v1/items_{n}/{{id}}\":{{\"get\":{{\"summary\":\"Retrieve item {n}\",\"description\":\"Returns the item with the given identifier if it exists, otherwise an error.\",\"parameters\":[{{\"name\":\"expand\",\"in\":\"query\",\"required\":false,\"schema\":{{\"type\":\"array\",\"items\":{{\"type\":\"string\",\"maxLength\":{}}}}}}}],\"responses\":{{\"200\":{{\"description\":\"Successful response.\",\"content\":{{\"application/json\":{{\"schema\":{{\"$ref\":\"#/components/schemas/item_{n}\"}}}}}}}}}}}}}},",
            5000 + n % 977
        ));
        n += 1;
    }
    text.pop();
    text.push_str("}}");
    text
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

fn main() -> anyhow::Result<()> {
    use gpui::{px, PlatformTextSystem, TextRun};
    let size = std::env::args()
        .nth(1)
        .map(|s| s.parse::<f32>())
        .transpose()?
        .unwrap_or(13.);
    let platform = gpui_platform::current_platform(true);
    let shaper = text_platform::shaper(platform.text_system());
    shaper.add_fonts(vec![Cow::Borrowed(include_bytes!(
        "../assets/brand/fonts/cascadia-code-400.ttf"
    ))])?;
    let cache: Arc<dyn PlatformTextSystem> = Arc::new(shaped_line_cache::ShapedLineCache::new(
        shaper,
        shaped_line_cache::SHAPED_LINES_MAX_BYTES,
    ));
    let text_system = Arc::new(gpui::TextSystem::new(cache));
    let font = gpui::font("Cascadia Code");
    let mut out = std::io::stdout();
    for bytes in [448 * 1024, 896 * 1024, 1792 * 1024, 3_560_394] {
        let mut text = openapi_line(bytes);
        let run = |text: &str| TextRun {
            len: text.len(),
            font: font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // A fresh window cache per frame: GPUI's frame cache never matches an
        // edited line, so each keystroke reaches the platform text system.
        let frame = || gpui::WindowTextSystem::new(text_system.clone());
        let started = Instant::now();
        let first = frame().layout_line(&text, px(size), &[run(&text)], None);
        let cold_ms = started.elapsed().as_secs_f64() * 1000.;
        let glyphs: usize = first.runs.iter().map(|r| r.glyphs.len()).sum();
        drop(first);
        // Type at the start of a description value in the middle of the line.
        let anchor = "\"description\":\"Returns";
        let mut at =
            text.len() / 2 + text[text.len() / 2..].find(anchor).unwrap() + anchor.len() - 7;
        let (mut layout_ms, mut lookup_ms) = (Vec::new(), Vec::new());
        for c in "quick brown fox jumps over ".chars() {
            text.insert(at, c);
            at += 1;
            let window = frame();
            let started = Instant::now();
            let layout = window.layout_line(&text, px(size), &[run(&text)], None);
            layout_ms.push(started.elapsed().as_secs_f64() * 1000.);
            // The editor asks GPUI for caret/selection geometry every frame.
            let started = Instant::now();
            let x = layout.x_for_index(at);
            let back = layout.closest_index_for_x(x);
            lookup_ms.push(started.elapsed().as_secs_f64() * 1000.);
            std::hint::black_box(back);
            drop(layout);
        }
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "bytes": text.len(), "glyphs": glyphs, "font_size": size,
                "cold_full_shape_ms": cold_ms, "keys": layout_ms.len(),
                "layout_line_median_ms": median(&mut layout_ms.clone()),
                "layout_line_max_ms": layout_ms.iter().cloned().fold(0., f64::max),
                "caret_lookup_median_ms": median(&mut lookup_ms),
                "layout_line_ms": layout_ms,
            })
        )?;
    }
    Ok(())
}
