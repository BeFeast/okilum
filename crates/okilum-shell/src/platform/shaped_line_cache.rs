//! Shaped lines kept longer than GPUI keeps them (#1312).
//!
//! GPUI keeps a shaped line only for the frame after the one that used it. A
//! large note lays a block out once when the list measures it below the
//! viewport and again a few frames later when it scrolls in, so the same
//! lines went to the platform shaper twice. This text system sits in front of
//! the shaper and answers a repeated `layout_line` with a copy of the layout
//! the shaper returned the first time.
//!
//! The key is everything `layout_line` receives: the text, the font size and
//! the runs (byte length and `FontId`). A `FontId` already names the family,
//! features, fallbacks, weight and style it was resolved from, and GPUI's own
//! line cache keys on the same three things. Wrapping and scale do not reach
//! the shaper: GPUI wraps a shaped line itself, DirectWrite lays out at a
//! fixed one pixel per DIP with an identity transform, and CoreText and
//! cosmic-text take only the font size. Even so, the cache is emptied when
//! fonts are loaded, when the theme changes, and when a window's scale factor
//! changes.
//!
//! Memory has a hard cap in bytes; the least recently used lines go first.
//! Nothing runs between calls: no timers, no idle work.

use anyhow::Result;
use gpui::{
    DevicePixels, Font, FontId, FontMetrics, FontRun, GlyphId, Hsla, LineLayout, Pixels,
    PlatformTextSystem, RenderGlyphParams, ShapedGlyph, ShapedRun, Size, TextRenderingMode,
};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    hash::{BuildHasher, Hash, Hasher, RandomState},
    sync::{Arc, Mutex, OnceLock},
};

/// The cap on the bytes the cached lines take. A long line of 200 glyphs is
/// about 7 KB, so this holds well over a thousand of them; a flick needs a
/// few dozen kept between the list measuring a block and showing it.
pub(crate) const SHAPED_LINES_MAX_BYTES: usize = 8 * 1024 * 1024;

/// A line larger than this share of the cap is shaped every time and never
/// stored, so one huge line cannot empty the cache.
const LARGEST_LINE_SHARE: usize = 8;

static INSTALLED: OnceLock<Arc<ShapedLineCache>> = OnceLock::new();

pub(crate) struct ShapedLineCache {
    shaper: Arc<dyn PlatformTextSystem>,
    hasher: RandomState,
    lines: Mutex<Lines>,
}

#[derive(Default)]
struct Lines {
    entries: HashMap<u64, Entry>,
    /// Use order, oldest first: (key hash, stamp). An item whose stamp is no
    /// longer its entry's stamp is stale and skipped.
    order: VecDeque<(u64, u64)>,
    bytes: usize,
    stamp: u64,
    max_bytes: usize,
    #[cfg(test)]
    hits: usize,
}

struct Entry {
    text: Box<str>,
    font_size: Pixels,
    runs: Box<[FontRun]>,
    layout: LineLayout,
    bytes: usize,
    stamp: u64,
}

impl ShapedLineCache {
    /// The application's cache over `shaper`, kept so it can be emptied when
    /// the theme or a window's scale factor changes.
    pub(crate) fn install(shaper: Arc<dyn PlatformTextSystem>) -> Arc<dyn PlatformTextSystem> {
        let cache = Arc::new(Self::new(shaper, SHAPED_LINES_MAX_BYTES));
        // Only the first application installs; another one (tests) still works.
        let _ = INSTALLED.set(cache.clone());
        cache
    }

    pub(crate) fn new(shaper: Arc<dyn PlatformTextSystem>, max_bytes: usize) -> Self {
        Self {
            shaper,
            hasher: RandomState::new(),
            lines: Mutex::new(Lines {
                max_bytes,
                ..Lines::default()
            }),
        }
    }

    /// Empty the application's cache.
    pub(crate) fn clear_installed() {
        if let Some(cache) = INSTALLED.get() {
            cache.clear();
        }
    }

    /// Empty the application's cache when this window's scale factor is not
    /// the one it last drew at. Per window, so two windows on displays with
    /// different scales do not empty it on every frame.
    pub(crate) fn note_scale_factor(window: &gpui::Window, cx: &gpui::App) {
        static SCALES: Mutex<Vec<(gpui::AnyWindowHandle, f32)>> = Mutex::new(Vec::new());
        let handle = window.window_handle();
        let scale = window.scale_factor();
        let mut scales = SCALES.lock().unwrap_or_else(|e| e.into_inner());
        match scales.iter_mut().find(|(seen, _)| *seen == handle) {
            Some((_, seen)) if *seen == scale => {}
            Some((_, seen)) => {
                *seen = scale;
                drop(scales);
                Self::clear_installed();
            }
            None => {
                // A new window: forget the ones that closed, so the list is
                // as long as the open windows.
                let open = cx.windows();
                scales.retain(|(seen, _)| open.contains(seen));
                scales.push((handle, scale));
            }
        }
    }

    pub(crate) fn clear(&self) {
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        lines.entries.clear();
        lines.order.clear();
        lines.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.lines.lock().unwrap().bytes
    }

    #[cfg(test)]
    pub(crate) fn hits(&self) -> usize {
        self.lines.lock().unwrap().hits
    }

    fn key_hash(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        text.hash(&mut hasher);
        f32::from(font_size).to_bits().hash(&mut hasher);
        runs.hash(&mut hasher);
        hasher.finish()
    }
}

impl Lines {
    fn get(
        &mut self,
        hash: u64,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
    ) -> Option<LineLayout> {
        let entry = self.entries.get_mut(&hash)?;
        if &*entry.text != text || entry.font_size != font_size || &*entry.runs != runs {
            return None;
        }
        self.stamp += 1;
        entry.stamp = self.stamp;
        self.order.push_back((hash, self.stamp));
        #[cfg(test)]
        {
            self.hits += 1;
        }
        let layout = copy_layout(&entry.layout);
        self.compact_order();
        Some(layout)
    }

    fn insert(
        &mut self,
        hash: u64,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        layout: LineLayout,
    ) {
        let bytes = entry_bytes(text, runs, &layout);
        if bytes > self.max_bytes / LARGEST_LINE_SHARE {
            return;
        }
        self.stamp += 1;
        let entry = Entry {
            text: text.into(),
            font_size,
            runs: runs.into(),
            layout,
            bytes,
            stamp: self.stamp,
        };
        // A different line with the same hash is replaced.
        if let Some(old) = self.entries.insert(hash, entry) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        self.order.push_back((hash, self.stamp));
        while self.bytes > self.max_bytes {
            let Some((hash, stamp)) = self.order.pop_front() else {
                break;
            };
            if self.entries.get(&hash).is_some_and(|e| e.stamp == stamp) {
                let old = self.entries.remove(&hash).expect("entry checked above");
                self.bytes -= old.bytes;
            }
        }
        self.compact_order();
    }

    /// Hits push a new use and leave the old one stale; drop stale uses once
    /// they outnumber the entries, so the queue stays bounded too.
    fn compact_order(&mut self) {
        if self.order.len() > 2 * self.entries.len() + 64 {
            let entries = &self.entries;
            self.order
                .retain(|(hash, stamp)| entries.get(hash).is_some_and(|e| e.stamp == *stamp));
        }
    }
}

fn copy_layout(layout: &LineLayout) -> LineLayout {
    LineLayout {
        font_size: layout.font_size,
        width: layout.width,
        ascent: layout.ascent,
        descent: layout.descent,
        runs: layout.runs.clone(),
        len: layout.len,
    }
}

/// What an entry holds, with its place in the map and the use order.
fn entry_bytes(text: &str, runs: &[FontRun], layout: &LineLayout) -> usize {
    let glyphs: usize = layout.runs.iter().map(|run| run.glyphs.len()).sum();
    size_of::<Entry>()
        + size_of::<u64>()
        + 2 * size_of::<(u64, u64)>()
        + text.len()
        + size_of_val(runs)
        + layout.runs.len() * size_of::<ShapedRun>()
        + glyphs * size_of::<ShapedGlyph>()
}

impl PlatformTextSystem for ShapedLineCache {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let result = self.shaper.add_fonts(fonts);
        // New fonts can change what an existing run falls back to.
        self.clear();
        result
    }
    fn all_font_names(&self) -> Vec<String> {
        self.shaper.all_font_names()
    }
    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        self.shaper.font_id(descriptor)
    }
    fn prewarm_fonts(&self, font_ids: &[FontId]) {
        self.shaper.prewarm_fonts(font_ids)
    }
    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.shaper.font_metrics(font_id)
    }
    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<gpui::Bounds<f32>> {
        self.shaper.typographic_bounds(font_id, glyph_id)
    }
    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.shaper.advance(font_id, glyph_id)
    }
    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.shaper.glyph_for_char(font_id, ch)
    }
    fn glyph_raster_bounds(
        &self,
        params: &RenderGlyphParams,
    ) -> Result<gpui::Bounds<DevicePixels>> {
        self.shaper.glyph_raster_bounds(params)
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: gpui::Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.shaper.rasterize_glyph(params, raster_bounds)
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let hash = self.key_hash(text, font_size, runs);
        let cached = self
            .lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(hash, text, font_size, runs);
        if let Some(layout) = cached {
            return layout;
        }
        // Shape without the lock: another thread may shape meanwhile.
        let layout = self.shaper.layout_line(text, font_size, runs);
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).insert(
            hash,
            text,
            font_size,
            runs,
            copy_layout(&layout),
        );
        layout
    }
    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.shaper.recommended_rendering_mode(font_id, font_size)
    }
    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.shaper.glyph_dilation_for_color(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, FontFeatures, FontStyle, FontWeight};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The platform shaper the application uses (with the Windows repair).
    fn platform_shaper() -> Arc<dyn PlatformTextSystem> {
        let platform = gpui_platform::current_platform(true);
        let shaper = super::super::text_platform::shaper(platform.text_system());
        // The bundled fonts the reader uses (brand::load_fonts); scripts they
        // lack fall back to the system fonts, as in the application.
        shaper
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../../assets/brand/fonts/noto-sans-400.ttf")),
                Cow::Borrowed(include_bytes!("../../assets/brand/fonts/noto-sans-700.ttf")),
                Cow::Borrowed(include_bytes!(
                    "../../assets/brand/fonts/cascadia-code-400.ttf"
                )),
            ])
            .unwrap();
        shaper
    }

    fn font(family: &str, weight: FontWeight, style: FontStyle) -> Font {
        Font {
            family: family.to_string().into(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight,
            style,
        }
    }

    /// Exact: the Debug form prints every field, floats as round-trip values.
    fn same(a: &LineLayout, b: &LineLayout) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }

    /// #1312: on every script the reader shows, a cached line is exactly what
    /// the shaper returns. Runs through the same chain as the application, so
    /// on Windows it covers the right-to-left repair under the cache.
    #[test]
    fn cached_lines_equal_the_shaper_on_every_script() {
        let shaper = platform_shaper();
        let cache = ShapedLineCache::new(shaper.clone(), SHAPED_LINES_MAX_BYTES);
        let body = cache
            .font_id(&font("Noto Sans", FontWeight::NORMAL, FontStyle::Normal))
            .unwrap();
        let bold = cache
            .font_id(&font("Noto Sans", FontWeight::BOLD, FontStyle::Normal))
            .unwrap();
        let mono = cache
            .font_id(&font(
                "Cascadia Code",
                FontWeight::NORMAL,
                FontStyle::Normal,
            ))
            .unwrap();
        let long = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        let corpus: Vec<(&str, Vec<FontRun>)> = vec![
            ("Plain Latin text, with punctuation!", vec![]),
            ("Кириллица: съешь же ещё этих мягких булок", vec![]),
            ("עברית: שלום עולם, mixed עם English 123", vec![]),
            ("مرحبا بالعالم", vec![]),
            ("中文排版测试，日本語のテキスト、한국어", vec![]),
            ("Emoji 👩‍💻 🇮🇱 👍🏽 and text", vec![]),
            (
                "Combining: e\u{301} a\u{308} n\u{303} \u{5d1}\u{5bc}",
                vec![],
            ),
            ("Tabs\tbetween\twords\t", vec![]),
            (&long, vec![]),
            (
                "bold and code",
                vec![
                    FontRun {
                        len: 5,
                        font_id: bold,
                    },
                    FontRun {
                        len: 4,
                        font_id: body,
                    },
                    FontRun {
                        len: 4,
                        font_id: mono,
                    },
                ],
            ),
        ];
        let mut glyphs = 0;
        for (text, runs) in &corpus {
            let runs = if runs.is_empty() {
                vec![FontRun {
                    len: text.len(),
                    font_id: body,
                }]
            } else {
                runs.clone()
            };
            for size in [px(13.), px(16.), px(21.5)] {
                let direct = shaper.layout_line(text, size, &runs);
                let first = cache.layout_line(text, size, &runs);
                let again = cache.layout_line(text, size, &runs);
                assert!(
                    same(&direct, &first),
                    "first shaping differs for {text:?} at {size:?}"
                );
                assert!(
                    same(&direct, &again),
                    "cached line differs for {text:?} at {size:?}"
                );
                glyphs += direct.runs.iter().map(|r| r.glyphs.len()).sum::<usize>();
            }
        }
        // Positive controls: the corpus really shaped, and every second call
        // was answered from the cache.
        assert!(glyphs > 1000, "the corpus shaped {glyphs} glyphs");
        assert_eq!(cache.hits(), corpus.len() * 3);
    }

    /// Counts the lines it shapes; every line is one glyph per byte.
    #[derive(Default)]
    struct CountingShaper(AtomicUsize);

    impl PlatformTextSystem for CountingShaper {
        fn add_fonts(&self, _: Vec<Cow<'static, [u8]>>) -> Result<()> {
            Ok(())
        }
        fn all_font_names(&self) -> Vec<String> {
            Vec::new()
        }
        fn font_id(&self, _: &Font) -> Result<FontId> {
            Ok(FontId(0))
        }
        fn font_metrics(&self, _: FontId) -> FontMetrics {
            unimplemented!()
        }
        fn typographic_bounds(&self, _: FontId, _: GlyphId) -> Result<gpui::Bounds<f32>> {
            unimplemented!()
        }
        fn advance(&self, _: FontId, _: GlyphId) -> Result<Size<f32>> {
            unimplemented!()
        }
        fn glyph_for_char(&self, _: FontId, _: char) -> Option<GlyphId> {
            None
        }
        fn glyph_raster_bounds(&self, _: &RenderGlyphParams) -> Result<gpui::Bounds<DevicePixels>> {
            unimplemented!()
        }
        fn rasterize_glyph(
            &self,
            _: &RenderGlyphParams,
            _: gpui::Bounds<DevicePixels>,
        ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
            unimplemented!()
        }
        fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
            self.0.fetch_add(1, Ordering::Relaxed);
            let font_id = runs.first().map_or(FontId(0), |r| r.font_id);
            LineLayout {
                font_size,
                runs: vec![ShapedRun {
                    font_id,
                    glyphs: (0..text.len())
                        .map(|index| ShapedGlyph {
                            id: GlyphId(index as u32),
                            position: gpui::point(px(index as f32), px(0.)),
                            index,
                            is_emoji: false,
                        })
                        .collect(),
                }],
                len: text.len(),
                ..Default::default()
            }
        }
        fn recommended_rendering_mode(&self, _: FontId, _: Pixels) -> TextRenderingMode {
            TextRenderingMode::Grayscale
        }
    }

    fn run(text: &str) -> Vec<FontRun> {
        vec![FontRun {
            len: text.len(),
            font_id: FontId(1),
        }]
    }

    /// #1312: a line shaped once is not sent to the shaper again, whatever
    /// happened in between; a different size, font or text is a new line.
    #[test]
    fn a_line_shaped_once_is_not_shaped_again() {
        let shaper = Arc::new(CountingShaper::default());
        let cache = ShapedLineCache::new(shaper.clone(), SHAPED_LINES_MAX_BYTES);
        let lines: Vec<String> = (0..200)
            .map(|i| format!("Block {i}: lorem ipsum"))
            .collect();
        for line in &lines {
            cache.layout_line(line, px(16.), &run(line));
        }
        for line in &lines {
            cache.layout_line(line, px(16.), &run(line));
        }
        assert_eq!(
            shaper.0.load(Ordering::Relaxed),
            200,
            "the second pass is answered from the cache"
        );

        let text = &lines[0];
        cache.layout_line(text, px(17.), &run(text));
        cache.layout_line(
            text,
            px(16.),
            &[FontRun {
                len: text.len(),
                font_id: FontId(2),
            }],
        );
        cache.layout_line(
            "Block 0: lorem ipsun",
            px(16.),
            &run("Block 0: lorem ipsun"),
        );
        assert_eq!(
            shaper.0.load(Ordering::Relaxed),
            203,
            "size, font and text are in the key"
        );
    }

    /// The byte cap holds; the least recently used lines go first, a huge line
    /// is never stored, and loading fonts empties the cache.
    #[test]
    fn the_cache_stays_under_its_byte_cap() {
        let shaper = Arc::new(CountingShaper::default());
        let cap = 64 * 1024;
        let cache = ShapedLineCache::new(shaper.clone(), cap);
        let lines: Vec<String> = (0..2000)
            .map(|i| format!("Line {i:05} {}", "x".repeat(40)))
            .collect();
        cache.layout_line(&lines[0], px(16.), &run(&lines[0]));
        for line in &lines[1..] {
            cache.layout_line(line, px(16.), &run(line));
            // Keep the first line in use.
            cache.layout_line(&lines[0], px(16.), &run(&lines[0]));
            assert!(
                cache.bytes() <= cap,
                "{} bytes over the cap {cap}",
                cache.bytes()
            );
        }
        let shaped = shaper.0.load(Ordering::Relaxed);
        assert_eq!(shaped, 2000, "positive control: every line shaped once");
        cache.layout_line(&lines[0], px(16.), &run(&lines[0]));
        cache.layout_line(&lines[1], px(16.), &run(&lines[1]));
        assert_eq!(
            shaper.0.load(Ordering::Relaxed),
            shaped + 1,
            "the line in use stays, the oldest unused one was evicted"
        );

        let huge = "y".repeat(cap);
        cache.layout_line(&huge, px(16.), &run(&huge));
        cache.layout_line(&huge, px(16.), &run(&huge));
        assert_eq!(
            shaper.0.load(Ordering::Relaxed),
            shaped + 3,
            "a huge line is shaped each time"
        );
        assert!(cache.bytes() <= cap);

        cache.add_fonts(Vec::new()).unwrap();
        assert_eq!(cache.bytes(), 0, "loading fonts empties the cache");
    }
}
