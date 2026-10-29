//! CPU work is isolated behind a bounded worker queue and a wall deadline.
use gpui::RenderImage;
use sha2::{Digest, Sha256};
use std::sync::{mpsc, Arc, LazyLock, Mutex};
use std::time::Duration;

pub(super) const DEADLINE: Duration = Duration::from_secs(5);
pub(super) const MAX_SOURCE: usize = 64 * 1024;
const MAX_SVG: usize = 4 * 1024 * 1024;
const MAX_PIXELS: u32 = 4 * 1024 * 1024;
const MAX_SIDE: u32 = 4096;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct Key {
    pub hash: [u8; 32],
    pub width: u32,
    pub dark: bool,
}
pub(super) fn bucket(width: f32) -> u32 {
    (width.clamp(128., 2048.) as u32 / 128).max(1) * 128
}
impl Key {
    pub fn new(source: &str, width: f32, dark: bool) -> Self {
        Self {
            hash: Sha256::digest(source.as_bytes()).into(),
            width: bucket(width),
            dark,
        }
    }
}
pub(super) struct Diagram {
    pub image: Arc<RenderImage>,
    pub width: f32,
    pub height: f32,
    pub bytes: usize,
}
type Outcome = Result<Arc<Diagram>, String>;
struct Job {
    work: Box<dyn FnOnce() -> Outcome + Send>,
    reply: mpsc::SyncSender<Outcome>,
}
static WORKERS: LazyLock<Result<mpsc::SyncSender<Job>, String>> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::sync_channel::<Job>(32);
    let receiver = Arc::new(Mutex::new(receiver));
    for index in 0..2 {
        let receiver = receiver.clone();
        std::thread::Builder::new()
            .name(format!("mermaid-{index}"))
            .stack_size(16 * 1024 * 1024)
            .spawn(move || loop {
                let job = match receiver.lock() {
                    Ok(queue) => queue.recv(),
                    Err(_) => break,
                };
                let Ok(job) = job else { break };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.work))
                    .unwrap_or_else(|_| Err("Mermaid renderer panicked".into()));
                let _ = job.reply.send(result);
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(sender)
});

#[derive(Clone, Copy)]
enum Budget {
    Wall(Duration),
    // Functional fixtures retain Merman's finite interactive resource policy
    // and our source/SVG/pixel caps; host scheduling is not a work measurement.
    #[cfg(test)]
    WorkUnits,
}

impl Budget {
    fn control(self) -> merman::OperationControl {
        let control = merman::OperationControl::new();
        match self {
            Self::Wall(deadline) => control.with_deadline(deadline),
            #[cfg(test)]
            Self::WorkUnits => control,
        }
    }
}

fn bounded(work: impl FnOnce() -> Outcome + Send + 'static, budget: Budget) -> Outcome {
    let pool = WORKERS.as_ref().map_err(Clone::clone)?;
    let (reply, receive) = mpsc::sync_channel(1);
    pool.try_send(Job {
        work: Box::new(work),
        reply,
    })
    .map_err(|_| "Mermaid queue full".to_string())?;
    match budget {
        Budget::Wall(deadline) => receive
            .recv_timeout(deadline)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => "Mermaid deadline exceeded".to_string(),
                mpsc::RecvTimeoutError::Disconnected => "Mermaid worker disconnected".to_string(),
            })?,
        #[cfg(test)]
        Budget::WorkUnits => receive
            .recv()
            .map_err(|_| "Mermaid worker disconnected".to_string())?,
    }
}
pub(super) fn render(source: Arc<str>, key: Key) -> Outcome {
    render_with_budget(source, key, Budget::Wall(DEADLINE))
}

fn render_with_budget(source: Arc<str>, key: Key, budget: Budget) -> Outcome {
    if source.len() > MAX_SOURCE || source.trim().is_empty() {
        return Err("Mermaid source size limit".into());
    }
    // The same deadline covers queuing, parsing, layout and rasterization. The
    // cooperative control stops late layout; only two worker threads can exist.
    let control = budget.control();
    let cancel = control.clone();
    let result = bounded(move || raster(&source, &key, control), budget);
    if result.is_err() {
        cancel.cancel();
    }
    result
}

fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: LazyLock<Arc<resvg::usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        for bytes in [
            include_bytes!("../../assets/mermaid/Arimo-Regular.ttf").as_slice(),
            include_bytes!("../../assets/mermaid/Arimo-Bold.ttf"),
            include_bytes!("../../assets/mermaid/Arimo-Italic.ttf"),
            include_bytes!("../../assets/mermaid/Arimo-BoldItalic.ttf"),
        ] {
            db.load_font_data(bytes.to_vec());
        }
        db.set_sans_serif_family("Arimo");
        Arc::new(db)
    });
    FONTS.clone()
}

/// Merman's default deterministic widths are heuristic (e.g. Ideas was given a
/// 32px viewport and clipped). Use the same embedded face and shaper as resvg.
fn text_width(text: &str, style: &merman::svg::TextStyle) -> f64 {
    let bold = style
        .font_weight
        .as_deref()
        .is_some_and(|s| s == "bold" || s.parse::<u16>().is_ok_and(|n| n >= 600));
    let italic = style
        .font_style
        .as_deref()
        .is_some_and(|s| s == "italic" || s == "oblique");
    let bytes = match (bold, italic) {
        (false, false) => include_bytes!("../../assets/mermaid/Arimo-Regular.ttf").as_slice(),
        (true, false) => include_bytes!("../../assets/mermaid/Arimo-Bold.ttf").as_slice(),
        (false, true) => include_bytes!("../../assets/mermaid/Arimo-Italic.ttf").as_slice(),
        (true, true) => include_bytes!("../../assets/mermaid/Arimo-BoldItalic.ttf").as_slice(),
    };
    let face = rustybuzz::Face::from_slice(bytes, 0).expect("embedded Arimo face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let glyphs = rustybuzz::shape(&face, &[], buffer);
    glyphs
        .glyph_positions()
        .iter()
        .map(|p| p.x_advance as f64)
        .sum::<f64>()
        / face.units_per_em() as f64
        * style.font_size
}
pub(super) fn svg(
    source: &str,
    key: &Key,
    control: merman::OperationControl,
) -> Result<String, String> {
    let config = merman::MermaidConfig::from_value(serde_json::json!({
        "theme": if key.dark { "dark" } else { "default" },
        "fontFamily": "Arimo", "securityLevel": "strict",
        "secure": ["theme", "fontFamily", "securityLevel"]
    }));
    let renderer =
        merman::Renderer::new().with_engine(merman::Engine::new().with_site_config(config));
    let identity = merman::svg::TextMeasurementProfileIdentity::new(
        merman::svg::MeasurementProfileId::new("okilum.arimo").map_err(|e| e.to_string())?,
        "1.23-rustybuzz-0.20.1",
    )
    .map_err(|e| e.to_string())?;
    let profile = merman::svg::TextMeasurementProfile::new(
        identity,
        merman::svg::DeterministicTextMeasurer::default().with_width_callback(text_width),
    );
    let request = merman::SvgRequest {
        environment: merman::SvgEnvironment::deterministic()
            .with_text_measurement_policy(merman::svg::TextMeasurementPolicy::uniform(profile)),
        pipeline: Some(merman::svg::SvgPipeline::resvg_safe()),
        layout: merman::svg::LayoutOptions::headless_svg_defaults()
            .with_container_size(key.width as f64, 600.),
        ..Default::default()
    };
    let output = renderer
        .render(merman::RenderRequest::svg(source, control, request))
        .map_err(|e| e.to_string())?;
    let merman::RenderOutput::Svg(Some(output)) = output else {
        return Err("No Mermaid diagram".into());
    };
    let svg = output
        .svg()
        .replace("url(#drop-shadow)", "url(#merman-drop-shadow)");
    if svg.len() > MAX_SVG || svg.contains("<foreignObject") || svg.contains("<image") {
        return Err("Unsupported or oversized Mermaid SVG".into());
    }
    Ok(svg)
}
/// Retain the preferred 2x density when it fits. Tall/wide diagrams reduce
/// density, not their logical document size, within the same allocation caps.
fn raster_geometry(width: f32, height: f32, logical: f32) -> Result<(u32, u32, f32), String> {
    if !width.is_finite()
        || !height.is_finite()
        || !logical.is_finite()
        || width <= 0.
        || height <= 0.
        || logical <= 0.
    {
        return Err("Invalid Mermaid raster dimensions".into());
    }
    let (width, height) = (f64::from(width), f64::from(height));
    let dimensions = |scale: f64| {
        (
            (width * scale).ceil().max(1.) as u32,
            (height * scale).ceil().max(1.) as u32,
        )
    };
    let fits = |w: u32, h: u32| {
        w <= MAX_SIDE && h <= MAX_SIDE && w.checked_mul(h).is_some_and(|area| area <= MAX_PIXELS)
    };
    let mut scale = f64::from(logical) / width * 2.;
    let (mut w, mut h) = dimensions(scale);
    if !fits(w, h) {
        // Leave room for ceil() at both edges. This avoids clipping the final
        // subpixel or allocating one row beyond the pixel-area budget.
        let side = f64::from(MAX_SIDE - 1);
        let area = f64::from(MAX_PIXELS - 2 * MAX_SIDE - 1);
        scale = scale
            .min(side / width)
            .min(side / height)
            .min((area / width / height).sqrt());
        (w, h) = dimensions(scale);
    }
    if !fits(w, h) {
        return Err("Mermaid raster size limit".into());
    }
    Ok((w, h, scale as f32))
}

/// Logs must not include source-authored labels from parser error messages.
pub(super) fn fallback_reason(error: &str) -> &'static str {
    for reason in [
        "source size limit",
        "raster size limit",
        "deadline exceeded",
        "queue full",
        "renderer panicked",
        "allocation failed",
        "Empty Mermaid raster",
        "Invalid Mermaid raster",
        "Unsupported or oversized Mermaid SVG",
        "No Mermaid diagram",
    ] {
        if error.contains(reason) {
            return reason;
        }
    }
    "parse, layout or SVG rendering error"
}

fn raster(source: &str, key: &Key, control: merman::OperationControl) -> Outcome {
    let vector = svg(source, key, control)?;
    let options = resvg::usvg::Options {
        fontdb: fonts(),
        font_family: "Arimo".into(),
        // SVG must never resolve a source-authored file/URL on the worker.
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(&vector, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    let logical = size.width().min(key.width as f32);
    let (w, h, scale) = raster_geometry(size.width(), size.height(), logical)?;
    let mut pixels = resvg::tiny_skia::Pixmap::new(w, h).ok_or("Mermaid allocation failed")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    let mut pixels = pixels.take();
    if !pixels.as_chunks::<4>().0.iter().any(|p| p[3] > 0) {
        return Err("Empty Mermaid raster".into());
    }
    for p in pixels.as_chunks_mut::<4>().0 {
        gpui::swap_rgba_pa_to_bgra(p);
    }
    let bytes = pixels.len();
    let buffer = image::RgbaImage::from_raw(w, h, pixels).ok_or("Invalid Mermaid raster")?;
    Ok(Arc::new(Diagram {
        image: Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
        width: logical,
        height: logical * size.height() / size.width(),
        bytes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_fixture(source: Arc<str>, key: Key) -> Outcome {
        render_with_budget(source, key, Budget::WorkUnits)
    }

    #[test]
    fn cache_key_separates_source_width_and_theme() {
        let a = Key::new("flowchart TD\nA-->B", 650., false);
        assert_eq!(a, Key::new("flowchart TD\nA-->B", 660., false));
        assert_ne!(a, Key::new("flowchart TD\nA-->B", 800., false));
        assert_ne!(a, Key::new("flowchart TD\nA-->B", 650., true));
        assert_ne!(a, Key::new("flowchart TD\nA-->C", 650., false));
    }
    #[test]
    fn raster_density_fits_height_and_area_without_changing_small_diagrams() {
        assert_eq!(raster_geometry(300., 400., 300.).unwrap(), (600, 800, 2.));
        for (width, height, logical) in [(300., 12_000., 300.), (5000., 5000., 2048.)] {
            let (w, h, scale) = raster_geometry(width, height, logical).unwrap();
            assert!(w <= MAX_SIDE && h <= MAX_SIDE);
            assert!(w.checked_mul(h).unwrap() <= MAX_PIXELS);
            assert!(scale > 0. && scale < logical / width * 2.);
            assert!((w as f32 / h as f32 - width / height).abs() < 0.1);
        }
    }

    #[test]
    fn tall_td_and_tb_render_with_bounded_rasters_and_full_logical_height() {
        for header in ["flowchart TD", "graph TD", "graph TB"] {
            let mut source = format!("{header}\n");
            for index in 0..55 {
                source.push_str(&format!(
                    "N{index}[Step {index}] --> N{}[Step {}]\n",
                    index + 1,
                    index + 1
                ));
            }
            for dark in [false, true] {
                let key = Key::new(&source, 1280., dark);
                let vector = svg(&source, &key, Budget::WorkUnits.control()).unwrap();
                let options = resvg::usvg::Options {
                    fontdb: fonts(),
                    ..Default::default()
                };
                let tree = resvg::usvg::Tree::from_str(&vector, &options).unwrap();
                let natural = tree.size();
                assert!(
                    natural.height() * 2. > MAX_SIDE as f32,
                    "positive control: old 2x raster exceeds the height cap"
                );
                let diagram = render_fixture(source.clone().into(), key).unwrap();
                let pixels = diagram.image.size(0);
                assert!(
                    u32::from(pixels.width) <= MAX_SIDE && u32::from(pixels.height) <= MAX_SIDE
                );
                assert!(diagram.bytes <= MAX_PIXELS as usize * 4);
                assert!(
                    diagram.height > MAX_SIDE as f32,
                    "document geometry retains the tall diagram"
                );
                assert!(
                    (diagram.width / diagram.height - natural.width() / natural.height()).abs()
                        < 0.001
                );
                assert!(
                    diagram.width > u32::from(pixels.width) as f32,
                    "positive control: logical width is independent of reduced pixel density"
                );
            }
        }
    }

    #[test]
    fn worker_panic_and_deadline_return_fallback_errors() {
        let panic = bounded(|| panic!("synthetic renderer panic"), Budget::WorkUnits);
        assert_eq!(panic.err().as_deref(), Some("Mermaid renderer panicked"));

        // Keep work pending until the zero-deadline receive has returned. No
        // sleep or wall-time assertion can race a loaded host scheduler.
        let (release, wait) = mpsc::sync_channel(1);
        let timeout = bounded(
            move || {
                wait.recv().unwrap();
                Err("late".into())
            },
            Budget::Wall(Duration::ZERO),
        );
        release.send(()).unwrap();
        assert_eq!(timeout.err().as_deref(), Some("Mermaid deadline exceeded"));

        // Production keeps its five-second budget; an already-expired control
        // must still stop actual parsing/layout without accepting a diagram.
        assert_eq!(DEADLINE, Duration::from_secs(5));
        let source = "flowchart TD\nA-->B";
        assert!(svg(
            source,
            &Key::new(source, 640., false),
            Budget::Wall(Duration::ZERO).control(),
        )
        .is_err());
    }
    #[test]
    fn work_budget_waits_for_completion_and_rejects_exhaustion() {
        let (started, observed) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let caller = std::thread::spawn(move || {
            bounded(
                move || {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                    Err("completed after controlled scheduling delay".into())
                },
                Budget::WorkUnits,
            )
        });
        observed.recv().unwrap();
        release.send(()).unwrap();
        assert_eq!(
            caller.join().unwrap().err().as_deref(),
            Some("completed after controlled scheduling delay"),
        );

        // A non-wall budget still rejects real excessive layout work. Exercise
        // Merman's same interactive resource policy with a deliberately tiny
        // ceiling, rather than replacing bounded resources with trusted input.
        let policy = merman::svg::RenderResourcePolicy::interactive()
            .with_limit(merman::svg::ResourceLimitId::MaxLayoutWorkUnits, 1)
            .unwrap();
        let request = merman::SvgRequest {
            environment: merman::SvgEnvironment::deterministic().with_resource_policy(policy),
            ..Default::default()
        };
        let result = merman::Renderer::new().render(merman::RenderRequest::svg(
            "flowchart TD\nsubgraph Cluster\nA\nend\nA-->B",
            Budget::WorkUnits.control(),
            request,
        ));
        let error = result
            .err()
            .expect("tiny work budget must reject the fixture");
        assert!(
            error.to_string().contains("max_layout_work_units"),
            "{error}"
        );
    }

    #[test]
    fn mindmap_label_viewports_contain_the_embedded_fonts_shaped_text() {
        let source = "mindmap\n  root((Ideas))\n    Reader\n    Editor";
        let vector = svg(
            source,
            &Key::new(source, 640., false),
            Budget::WorkUnits.control(),
        )
        .unwrap();
        let width = regex::Regex::new(r#"\bwidth="([0-9.]+)""#).unwrap();
        for label in ["Ideas", "Reader", "Editor"] {
            let fragment = vector
                .split("<svg")
                .find(|s| s.contains(&format!(">{label}</text>")))
                .unwrap();
            let actual: f64 = width.captures(fragment).unwrap()[1].parse().unwrap();
            let expected = text_width(label, &merman::svg::TextStyle::default());
            assert!(
                actual + 0.1 >= expected,
                "{label}: viewport {actual} clips shaped width {expected}"
            );
        }
    }
    #[test]
    fn synthetic_families_paint_both_themes_and_invalid_is_an_error() {
        for dark in [false, true] {
            for (name, source) in [
                ("flowchart", "flowchart TD\nA[Start] --> B[Done]"),
                ("sequence", "---\nconfig:\n  look: neo\n---\nsequenceDiagram\nAlice->>Bob: Hello\nNote over Bob: A note"),
                ("mindmap", "mindmap\n  root((Ideas))\n    Reader\n    Editor"),
                ("long-path", "flowchart LR\nA[\"memory/daily/YYYY-MM-DD.md<br/>memory/daily/archive/observations/long-unbreakable-document-reference.md\"] --> B[Next]"),
            ] {
                let key = Key::new(source, 640., dark);
                let svg = svg(
                    source,
                    &key,
                    Budget::WorkUnits.control(),
                )
                .unwrap();
                assert!(!svg.contains("<foreignObject"));
                assert!(!svg.contains("url(#drop-shadow)"));
                let diagram = render_fixture(source.into(), key).unwrap();
                assert!(diagram.bytes > 0);
                if let Some(out) = std::env::var_os("OKILUM_MERMAID_EVIDENCE") {
                    let out = std::path::PathBuf::from(out);
                    std::fs::create_dir_all(&out).unwrap();
                    let stem = format!("{name}-{}", if dark { "dark" } else { "light" });
                    std::fs::write(out.join(format!("{stem}.svg")), svg).unwrap();
                    let size = diagram.image.size(0);
                    let mut rgba = diagram.image.as_bytes(0).unwrap().to_vec();
                    for pixel in rgba.as_chunks_mut::<4>().0 { pixel.swap(0, 2); }
                    image::save_buffer(out.join(format!("{stem}.png")), &rgba,
                        u32::from(size.width), u32::from(size.height), image::ColorType::Rgba8).unwrap();
                }
            }
        }
        assert!(render_fixture(
            "flowchart TD\nA[unclosed".into(),
            Key::new("invalid", 640., false)
        )
        .is_err());
        assert!(render_fixture(
            "x".repeat(MAX_SOURCE + 1).into(),
            Key::new("large", 640., false)
        )
        .is_err());
    }
}
