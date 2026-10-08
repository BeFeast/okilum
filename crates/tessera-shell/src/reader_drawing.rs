//! Asynchronous native drawing rendering with a dedicated licensed font database.
use super::*;
use base64::Engine as _;
use gpui_component::WindowExt as _;
use std::{
    cell::Cell,
    hash::{Hash, Hasher},
    sync::LazyLock,
};
use tessera_core::excalidraw::{Scene, MAX_SOURCE_BYTES};

#[derive(Clone, Hash, PartialEq, Eq)]
struct Source {
    root: PathBuf,
    path: PathBuf,
    revision: Option<std::time::SystemTime>,
    size: u64,
}
struct Document {
    id: u64,
    tree: resvg::usvg::Tree,
    json: String,
    warnings: Vec<String>,
}
struct DrawingAsset;
impl Asset for DrawingAsset {
    type Source = Source;
    type Output = Result<Arc<Document>, Arc<String>>;
    fn load(
        source: Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        cx.background_executor().spawn(async move {
            load_document(source)
                .map(Arc::new)
                .map_err(|e| Arc::new(format!("{e:#}")))
        })
    }
}

// GPUI's asset cache has no automatic eviction. Bound both decoded scenes and
// raster variants, including pending loads; eviction drops the cache's task owner.
#[derive(Default)]
struct DrawingCache {
    sources: Vec<Source>,
    rasters: Vec<(Raster, usize)>,
}
impl Global for DrawingCache {}
const RASTER_BUDGET: usize = 128 * 1024 * 1024;
const DOCUMENT_LIMIT: usize = 16;

fn retain_source(source: &Source, cx: &mut App) {
    let cache = cx.default_global::<DrawingCache>();
    cache.sources.retain(|s| s != source);
    cache.sources.push(source.clone());
    if cache.sources.len() > DOCUMENT_LIMIT {
        let old = cache.sources.remove(0);
        cx.remove_asset::<DrawingAsset>(&old);
    }
}
fn retain_raster(request: &Raster, cx: &mut App) {
    let (w, h, _) = raster_size(request);
    let cache = cx.default_global::<DrawingCache>();
    cache.rasters.retain(|(r, _)| r != request);
    cache
        .rasters
        .push((request.clone(), w as usize * h as usize * 4));
    let mut removed = Vec::new();
    while cache.rasters.len() > 64
        || cache.rasters.iter().map(|(_, bytes)| bytes).sum::<usize>() > RASTER_BUDGET
    {
        removed.push(cache.rasters.remove(0).0);
    }
    for old in removed {
        cx.remove_asset::<RasterAsset>(&old);
    }
}

/// Any vault write can change an image dependency or suffix resolution. Invalidate
/// even when size/mtime are preserved; do not require the containing note to change.
pub(super) fn invalidate(root: &Path, cx: &mut App) {
    let cache = cx.default_global::<DrawingCache>();
    let sources = cache
        .sources
        .iter()
        .filter(|s| s.root == root)
        .cloned()
        .collect::<Vec<_>>();
    cache.sources.retain(|s| s.root != root);
    // Rasters are content-addressed and bounded; unchanged drawings can reuse them.
    for source in sources {
        cx.remove_asset::<DrawingAsset>(&source);
    }
    cx.refresh_windows();
}

fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: LazyLock<Arc<resvg::usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        // Latin subsets first; fallback covers Cyrillic and the other subsets.
        for bytes in [
            include_bytes!("../assets/drawings/fonts/excalifont-4.ttf").as_slice(),
            include_bytes!("../assets/drawings/fonts/excalifont-0.ttf"),
            include_bytes!("../assets/drawings/fonts/excalifont-1.ttf"),
            include_bytes!("../assets/drawings/fonts/excalifont-2.ttf"),
            include_bytes!("../assets/drawings/fonts/excalifont-3.ttf"),
            include_bytes!("../assets/drawings/fonts/excalifont-5.ttf"),
            include_bytes!("../assets/drawings/fonts/excalifont-6.ttf"),
            include_bytes!("../assets/drawings/fonts/virgil-0.ttf"),
            include_bytes!("../assets/drawings/fonts/liberation-sans.ttf"),
            include_bytes!("../assets/drawings/fonts/nunito-0.ttf"),
            include_bytes!("../assets/drawings/fonts/nunito-1.ttf"),
            include_bytes!("../assets/drawings/fonts/nunito-2.ttf"),
            include_bytes!("../assets/drawings/fonts/nunito-3.ttf"),
            include_bytes!("../assets/drawings/fonts/nunito-4.ttf"),
            include_bytes!("../assets/brand/fonts/cascadia-code-400.ttf"),
            include_bytes!("../assets/brand/fonts/noto-sans-400.ttf"),
        ] {
            db.load_font_data(bytes.to_vec());
        }
        // The upstream Nunito subset is named Nunito ExtraLight internally.
        // Register Excalidraw's public family alias without modifying font bytes.
        let nunito = db
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.starts_with("Nunito"))
            })
            .cloned()
            .collect::<Vec<_>>();
        for mut face in nunito {
            let language = face.families[0].1;
            face.families.insert(0, ("Nunito".into(), language));
            db.push_face_info(face);
        }
        db.set_sans_serif_family("Noto Sans");
        db.set_monospace_family("Cascadia Code");
        Arc::new(db)
    });
    FONTS.clone()
}
fn load_document(source: Source) -> anyhow::Result<Document> {
    use std::io::Read as _;
    let root = source.root.canonicalize()?;
    let path = source.path.canonicalize()?;
    anyhow::ensure!(path.starts_with(&root), "Drawing is outside the vault");
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?
        .take((MAX_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAX_SOURCE_BYTES, "Drawing is too large");
    let scene = Scene::parse(std::str::from_utf8(&bytes)?)?;
    let mut inventory = None;
    let vector = scene.vectors(|id| {
        if let Some(data) = scene
            .json
            .get("files")
            .and_then(|f| f.get(id))
            .and_then(|f| f.get("dataURL"))
            .and_then(|v| v.as_str())
        {
            if [
                "data:image/png;base64,",
                "data:image/jpeg;base64,",
                "data:image/webp;base64,",
            ]
            .iter()
            .any(|p| data.starts_with(p))
                && data.len() <= 16 * 1024 * 1024
            {
                return Some(data.into());
            }
        }
        let target = scene.embedded_files.get(id)?;
        let candidates = [path.parent()?.join(target), root.join(target)];
        let mut found = candidates
            .into_iter()
            .filter_map(|p| p.canonicalize().ok())
            .find(|p| p.is_file() && p.starts_with(&root));
        if found.is_none() {
            // Use the same explicit ambiguity rule as Reader; never pick the first duplicate.
            let vault = inventory
                .get_or_insert_with(|| Vault::scan_metadata(&root).ok())
                .as_ref()?;
            let matches = vault
                .entries
                .iter()
                .filter(|e| e.path == *target || e.path.ends_with(&format!("/{target}")))
                .collect::<Vec<_>>();
            if let [entry] = matches.as_slice() {
                found = root
                    .join(&entry.path)
                    .canonicalize()
                    .ok()
                    .filter(|p| p.starts_with(&root));
            }
        }
        let path = found?;
        let mime = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            _ => return None,
        };
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > 16 * 1024 * 1024 {
            return None;
        }
        Some(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    })?;
    let options = resvg::usvg::Options {
        fontdb: fonts(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(&vector.svg, &options)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    vector.svg.hash(&mut hasher);
    Ok(Document {
        id: hasher.finish(),
        tree,
        json: scene.json.to_string(),
        warnings: vector.warnings,
    })
}
#[derive(Clone)]
struct Raster {
    document: Arc<Document>,
    width: u32,
}
impl PartialEq for Raster {
    fn eq(&self, other: &Self) -> bool {
        self.document.id == other.document.id && self.width == other.width
    }
}
impl Eq for Raster {}
impl Hash for Raster {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.document.id.hash(h);
        self.width.hash(h);
    }
}
fn raster_size(request: &Raster) -> (u32, u32, f32) {
    let size = request.document.tree.size();
    let scale = (request.width as f32 / size.width())
        .min(4096. / size.height())
        .min(4096. / size.width());
    (
        (size.width() * scale).ceil().max(1.) as u32,
        (size.height() * scale).ceil().max(1.) as u32,
        scale,
    )
}
struct RasterAsset;
impl Asset for RasterAsset {
    type Source = Raster;
    type Output = Result<Arc<RenderImage>, Arc<String>>;
    fn load(
        request: Raster,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        cx.background_executor().spawn(async move {
            let (w, h, scale) = raster_size(&request);
            let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
                .ok_or_else(|| Arc::new("Drawing raster allocation failed".into()))?;
            resvg::render(
                &request.document.tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            let mut pixels = pixmap.take();
            for p in pixels.as_chunks_mut::<4>().0 {
                gpui::swap_rgba_pa_to_bgra(p);
            }
            let buffer = image::RgbaImage::from_raw(w, h, pixels)
                .ok_or_else(|| Arc::new("Invalid drawing raster".into()))?;
            Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
        })
    }
}
fn content(document: Arc<Document>, width: f32, window: &mut Window, cx: &mut App) -> AnyElement {
    let requested = (width * window.scale_factor()).ceil().clamp(128., 4096.) as u32;
    let request = Raster {
        document: document.clone(),
        width: requested.div_ceil(128) * 128,
    };
    retain_raster(&request, cx);
    match window.use_asset::<RasterAsset>(&request, cx) {
        Some(Ok(image)) => img(image)
            .w(px(width))
            .h(px(
                width * document.tree.size().height() / document.tree.size().width()
            ))
            .into_any_element(),
        Some(Err(error)) => diagnostic("Drawing could not be displayed", error.as_str(), cx),
        None => div().p_3().child("Rendering drawing…").into_any_element(),
    }
}
pub(super) fn render(
    root: &Path,
    path: &Path,
    requested_size: Option<(f32, Option<f32>)>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let metadata = std::fs::metadata(path).ok();
    let source = Source {
        root: root.into(),
        path: path.into(),
        revision: metadata.as_ref().and_then(|m| m.modified().ok()),
        size: metadata.map_or(0, |m| m.len()),
    };
    retain_source(&source, cx);
    match window.use_asset::<DrawingAsset>(&source, cx) {
        Some(Ok(document)) => {
            let expand = document.clone();
            let open = document.clone();
            let natural = document.tree.size();
            let width = requested_size.map_or(natural.width(), |(width, height)| {
                height.map_or(width, |height| {
                    width.min(height * natural.width() / natural.height())
                })
            });
            v_flex()
                .max_w_full()
                .gap_2()
                .p_3()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            reader_icon_button(
                                "drawing-expand",
                                IconName::Maximize,
                                "Expand drawing",
                                cx,
                            )
                            .on_click(move |_, window, cx| open_expand(expand.clone(), window, cx)),
                        )
                        .child(
                            reader_icon_button(
                                "drawing-open",
                                IconName::ExternalLink,
                                "Open in Excalidraw",
                                cx,
                            )
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(open.json.clone()));
                                reader_toast::transient(
                                    "Scene copied — paste it into Excalidraw",
                                    window,
                                    cx,
                                );
                                cx.open_url("https://draw.oklabs.uk");
                            }),
                        ),
                )
                .child(DrawingCanvas {
                    document: document.clone(),
                    max_width: width,
                })
                .children(
                    document
                        .warnings
                        .iter()
                        .map(|s| div().text_sm().child(s.clone())),
                )
                .into_any_element()
        }
        Some(Err(error)) => diagnostic("Drawing could not be displayed", error.as_str(), cx),
        None => div().p_3().child("Loading drawing…").into_any_element(),
    }
}
fn unavailable_message(payload: &str) -> (&'static str, String, String) {
    let decoded = tessera_core::document_links::decode(payload);
    let (summary, target, details) =
        match serde_json::from_str::<tessera_core::excalidraw::UnavailableDrawing>(&decoded) {
            Ok(failure) if failure.candidates.len() > 1 => (
                "Several drawings match this name",
                failure.target.clone(),
                format!(
                    "Target: {}\nMatches:\n{}",
                    failure.target,
                    failure.candidates.join("\n")
                ),
            ),
            Ok(failure) => (
                "Drawing not found",
                failure.target.clone(),
                format!(
                    "Target: {}\nNo drawing could be opened at this location.",
                    failure.target
                ),
            ),
            // Old cached placeholders did not retain the resolution reason. Do not
            // claim absence when that legacy target might have been ambiguous.
            Err(_) => (
                "Drawing unavailable",
                decoded.clone(),
                format!("Target: {decoded}"),
            ),
        };
    let name = target.rsplit('/').next().unwrap_or(&target);
    let lower = name.to_ascii_lowercase();
    let name = if lower.ends_with(".excalidraw.md") {
        &name[..name.len() - ".excalidraw.md".len()]
    } else if lower.ends_with(".excalidraw") {
        &name[..name.len() - ".excalidraw".len()]
    } else {
        name
    };
    (summary, name.to_owned(), details)
}

pub(super) fn unavailable(payload: &str, cx: &App) -> AnyElement {
    let (summary, name, details) = unavailable_message(payload);
    diagnostic_with_name(summary, Some(name), &details, cx)
}

// Keep renderer diagnostics available without making them document content.
fn diagnostic(summary: &'static str, details: &str, cx: &App) -> AnyElement {
    diagnostic_with_name(summary, None, details, cx)
}

fn diagnostic_with_name(
    summary: &'static str,
    name: Option<String>,
    details: &str,
    cx: &App,
) -> AnyElement {
    let details = SharedString::from(details.to_owned());
    let copy = details.clone();
    h_flex()
        .id(summary)
        .p_3()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(summary)
                .when_some(name, |view, name| {
                    view.child(
                        div()
                            .text_sm()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(name),
                    )
                }),
        )
        .child(
            div()
                .id("drawing-diagnostic-details")
                .tooltip(move |window, cx| {
                    let details = details.clone();
                    gpui_component::tooltip::Tooltip::element(move |_, _| {
                        div()
                            .max_w(px(480.))
                            .whitespace_normal()
                            .child(details.clone())
                    })
                    .build(window, cx)
                })
                .child(Icon::new(IconName::Info).size_4()),
        )
        .child(
            reader_icon_button(
                "drawing-copy-details",
                IconName::Copy,
                "Copy drawing details",
                cx,
            )
            .on_click(move |_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()));
                reader_toast::transient("Copied", window, cx);
            }),
        )
        .into_any_element()
}

fn open_expand(document: Arc<Document>, window: &mut Window, cx: &mut App) {
    let zoom = Rc::new(Cell::new(1f32));
    window.open_dialog(cx, move |dialog, window, cx| {
        let minus = zoom.clone();
        let plus = zoom.clone();
        let reset = zoom.clone();
        let w = (f32::from(window.viewport_size().width) - 96.).max(200.);
        let h = (f32::from(window.viewport_size().height) - 180.).max(150.);
        dialog
            .title("Drawing")
            .width(px(w))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        reader_icon_button("drawing-zoom-out", IconName::Minus, "Zoom out", cx)
                            .on_click(move |_, _, cx| {
                                minus.set((minus.get() / 1.25).max(0.25));
                                cx.refresh_windows();
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{}%", (zoom.get() * 100.).round())),
                    )
                    .child(
                        reader_icon_button(
                            "drawing-zoom-reset",
                            IconName::RotateCw,
                            "Reset zoom to 100%",
                            cx,
                        )
                        .on_click(move |_, _, cx| {
                            reset.set(1.);
                            cx.refresh_windows();
                        }),
                    )
                    .child(
                        reader_icon_button("drawing-zoom-in", IconName::Plus, "Zoom in", cx)
                            .on_click(move |_, _, cx| {
                                plus.set((plus.get() * 1.25).min(8.));
                                cx.refresh_windows();
                            }),
                    ),
            )
            .child(
                div()
                    .id("drawing-pan")
                    .w_full()
                    .h(px(h))
                    .overflow_x_scroll()
                    .overflow_y_scroll()
                    .child(content(
                        document.clone(),
                        (w - 48.) * zoom.get(),
                        window,
                        cx,
                    )),
            )
    });
}

pub(super) fn parse_size(title: &str) -> Option<(f32, Option<f32>)> {
    let value = title.strip_prefix("tessera-drawing-size:")?;
    let (width, height) = value
        .split_once('x')
        .map_or((value, None), |(w, h)| (w, Some(h)));
    let width: f32 = width.parse().ok()?;
    let height = height.map(str::parse::<f32>).transpose().ok()?;
    (width.is_finite() && width > 0. && height.is_none_or(|h| h.is_finite() && h > 0.))
        .then_some((width.min(20_000.), height.map(|h| h.min(20_000.))))
}
struct DrawingCanvas {
    document: Arc<Document>,
    max_width: f32,
}
impl IntoElement for DrawingCanvas {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for DrawingCanvas {
    type RequestLayoutState = ();
    type PrepaintState = AnyElement;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let ratio = self.document.tree.size().height() / self.document.tree.size().width();
        let max = px(self.max_width);
        // Constrain cross-axis stretching as well as the measured size: otherwise
        // a wide column can stretch paint width without increasing layout height.
        let mut style = Style::default();
        style.max_size.width = max.into();
        let layout = window.request_measured_layout(style, move |known, available, _, _| {
            let available = known
                .width
                .or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    _ => None,
                })
                .unwrap_or(max);
            let w = available.min(max).max(px(0.));
            size(w, w * ratio)
        });
        (layout, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let mut element = div()
            .debug_selector(|| "drawing-canvas-content".into())
            .w(bounds.size.width)
            .h(bounds.size.height)
            .child(content(
                self.document.clone(),
                f32::from(bounds.size.width),
                window,
                cx,
            ))
            .into_any_element();
        element.prepaint_as_root(
            bounds.origin,
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::Definite(bounds.size.height),
            ),
            window,
            cx,
        );
        element
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn painted_document(style: &str, dark: bool) -> Document {
        let scene = Scene::parse(&serde_json::json!({
            "type": "excalidraw", "version": 2,
            "appState": {"theme": if dark {"dark"} else {"light"}, "viewBackgroundColor": "#ffffff"},
            "elements": [{"id":"fill", "type":"rectangle", "x":0,"y":0,
                "width":100,"height":100,"strokeColor":"transparent",
                "backgroundColor":"#ff0000", "fillStyle":style,"seed":42,"roughness":0}]
        }).to_string()).unwrap();
        let vector = scene.vectors(|_| None).unwrap();
        Document {
            id: 1,
            tree: resvg::usvg::Tree::from_str(&vector.svg, &Default::default()).unwrap(),
            json: scene.json.to_string(),
            warnings: vector.warnings,
        }
    }
    #[test]
    fn unresolved_drawing_messages_keep_paths_in_details() {
        let payload = |candidates| {
            tessera_core::document_links::encode(
                &serde_json::to_string(&tessera_core::excalidraw::UnavailableDrawing {
                    target: "Sketches/Weekend.EXCALIDRAW.md".into(),
                    candidates,
                })
                .unwrap(),
            )
        };
        let (summary, name, details) = unavailable_message(&payload(vec![]));
        assert_eq!(summary, "Drawing not found");
        assert_eq!(name, "Weekend");
        assert!(details.contains("Sketches/Weekend.EXCALIDRAW.md"));
        let (summary, name, details) = unavailable_message(&payload(vec![
            "Home/Weekend.excalidraw.md".into(),
            "Work/Weekend.excalidraw.md".into(),
        ]));
        assert_eq!(summary, "Several drawings match this name");
        assert_eq!(name, "Weekend");
        assert!(details.contains("Home/Weekend.excalidraw.md\nWork/Weekend.excalidraw.md"));
        assert_eq!(
            unavailable_message("Old.excalidraw").0,
            "Drawing unavailable"
        );
    }

    #[test]
    fn native_fills_and_dark_filter_have_pixel_positive_controls() {
        for style in ["solid", "hachure", "cross-hatch"] {
            let mut results = Vec::new();
            for dark in [false, true] {
                let doc = painted_document(style, dark);
                let mut pixels = resvg::tiny_skia::Pixmap::new(148, 148).unwrap();
                resvg::render(&doc.tree, Default::default(), &mut pixels.as_mut());
                assert_eq!(
                    pixels.pixel(0, 0).unwrap().red(),
                    255,
                    "background stays unfiltered"
                );
                if !dark {
                    assert!(
                        pixels
                            .pixels()
                            .iter()
                            .filter(|p| p.red() > 200 && p.green() < 80)
                            .count()
                            > 100,
                        "{style}: fill-only positive control (transparent stroke)"
                    );
                }
                results.push(pixels.take());
            }
            assert_ne!(
                results[0], results[1],
                "dark filter must change {style} ink"
            );
        }
    }
    #[gpui::test]
    fn asset_cache_bounds_zoom_and_invalidates_watcher_sources(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let doc = Arc::new(painted_document("solid", false));
            for width in (128..=4096).step_by(128) {
                let request = Raster {
                    document: doc.clone(),
                    width,
                };
                retain_raster(&request, cx);
                let _ = cx.fetch_asset::<RasterAsset>(&request);
            }
            let cache = cx.default_global::<DrawingCache>();
            assert!(
                cache.rasters.len() > 1,
                "positive control: retain multiple widths"
            );
            assert!(cache.rasters.iter().map(|(_, bytes)| bytes).sum::<usize>() <= RASTER_BUDGET);
            assert!(!cx.has_asset::<RasterAsset>(&Raster {
                document: doc,
                width: 128
            }));
            let root = PathBuf::from("/drawing-cache-test");
            let source = Source {
                root: root.clone(),
                path: root.join("x.excalidraw"),
                revision: None,
                size: 0,
            };
            retain_source(&source, cx);
            let _ = cx.fetch_asset::<DrawingAsset>(&source);
            assert!(cx.has_asset::<DrawingAsset>(&source));
            invalidate(&root, cx);
            assert!(
                !cx.has_asset::<DrawingAsset>(&source),
                "watcher evicts unchanged metadata keys"
            );
        });
    }
    struct CanvasHarness {
        document: Arc<Document>,
        width: f32,
        scroll: ScrollHandle,
    }
    impl Render for CanvasHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("drawing-test-scroll")
                .w(px(self.width))
                .h(px(180.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(
                    v_flex()
                        .p(px(20.))
                        .gap(px(10.))
                        .child(div().h(px(100.)).flex_shrink_0())
                        .child(DrawingCanvas {
                            document: self.document.clone(),
                            max_width: 500.,
                        })
                        .child(
                            div()
                                .debug_selector(|| "drawing-following-content".into())
                                .h(px(400.))
                                .flex_shrink_0(),
                        ),
                )
        }
    }
    #[gpui::test]
    fn nested_canvas_tracks_column_width_and_scroll_origin(cx: &mut gpui::TestAppContext) {
        let doc = Arc::new(painted_document("solid", false));
        let (view, visual) = cx.add_window_view(|_, _| CanvasHarness {
            document: doc,
            width: 300.,
            scroll: ScrollHandle::new(),
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let first = visual
            .debug_bounds("drawing-canvas-content")
            .expect("canvas prepainted");
        assert_eq!(first.size, size(px(260.), px(260.)));
        assert!(
            first.origin.y >= px(100.),
            "nested offset must reach the image"
        );
        view.update(visual, |v, cx| {
            v.width = 220.;
            v.scroll.set_offset(point(px(0.), px(-50.)));
            cx.notify();
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let second = visual.debug_bounds("drawing-canvas-content").unwrap();
        assert_eq!(second.size, size(px(180.), px(180.)));
        assert_eq!(second.origin.y, first.origin.y - px(50.));
    }
    #[gpui::test]
    fn wide_column_preserves_canvas_aspect_ratio(cx: &mut gpui::TestAppContext) {
        let doc = Arc::new(painted_document("solid", false));
        let (_, visual) = cx.add_window_view(|_, _| CanvasHarness {
            document: doc,
            width: 700.,
            scroll: ScrollHandle::new(),
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let canvas = visual
            .debug_bounds("drawing-canvas-content")
            .expect("canvas painted");
        assert_eq!(
            canvas.size,
            size(px(500.), px(500.)),
            "natural/explicit width cap must constrain both layout and paint"
        );
        let following = visual
            .debug_bounds("drawing-following-content")
            .expect("following content laid out");
        assert!(
            following.origin.y >= canvas.origin.y + canvas.size.height,
            "following content starts below the full painted canvas"
        );
    }

    #[test]
    fn embed_size_is_bounded_and_aliases_are_not_sizes() {
        assert_eq!(parse_size("tessera-drawing-size:300"), Some((300., None)));
        assert_eq!(
            parse_size("tessera-drawing-size:300x200"),
            Some((300., Some(200.)))
        );
        for value in ["alias", "0", "-1", "NaN", "inf", "300xNaN"] {
            assert!(parse_size(&format!("tessera-drawing-size:{value}")).is_none());
        }
    }
    #[test]
    fn native_renderer_uses_bundled_fonts_and_real_scene_geometry() {
        let root = std::env::temp_dir().join(format!("tessera-drawing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("drawing.excalidraw.md");
        std::fs::write(
            &path,
            include_str!("../../tessera-core/tests/fixtures/excalidraw/elements.excalidraw.md"),
        )
        .unwrap();
        let document = load_document(Source {
            root: root.clone(),
            path,
            revision: None,
            size: 0,
        })
        .unwrap();
        assert!(document.tree.size().width() > 100.);
        let db = fonts();
        for family in [
            "Excalifont",
            "Virgil",
            "Nunito",
            "Cascadia Code",
            "Liberation Sans",
        ] {
            assert!(
                db.query(&resvg::usvg::fontdb::Query {
                    families: &[resvg::usvg::fontdb::Family::Name(family)],
                    ..Default::default()
                })
                .is_some(),
                "{family}"
            );
        }
        let mut pixels = resvg::tiny_skia::Pixmap::new(300, 300).unwrap();
        resvg::render(
            &document.tree,
            resvg::tiny_skia::Transform::from_scale(0.5, 0.5),
            &mut pixels.as_mut(),
        );
        let ink = pixels
            .pixels()
            .iter()
            .filter(|p| p.alpha() > 0 && p.red() < 150)
            .count();
        assert!(
            ink > 100,
            "positive control: native renderer must paint shapes and text"
        );
        assert!(document
            .warnings
            .iter()
            .any(|w| w.contains("Image unavailable")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
