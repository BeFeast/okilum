//! Read-only Canvas scene. Card navigation uses the Reader's ordinary link handler.
use super::*;
use okilum_core::canvas::{Content, Scene, Viewport};
use std::{collections::HashMap, io::Read};

pub(super) fn eligible(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("canvas"))
}

// Called only by the background scene loader, never during rendering.
fn checked_file_identity(root: &Path, file: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !Path::new(file).is_absolute(),
        "File path must be relative to this vault"
    );
    let path = reader_files::checked_path(root, file)
        .map_err(|_| anyhow::anyhow!("File is missing or unavailable in this vault"))?;
    anyhow::ensure!(path.is_file(), "The target is not a file");
    let root = root.canonicalize()?;
    Ok(okilum_core::vault::note_path(path.strip_prefix(root)?))
}

fn file_target_url(path: &str, subpath: Option<&str>) -> String {
    let note = Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    let suffix = if note {
        subpath
            .and_then(|value| value.strip_prefix('#'))
            .filter(|value| !value.is_empty())
            .map(|value| format!("#{}", okilum_core::document_links::encode(value)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    format!(
        "{}{}{suffix}",
        if note {
            WIKI_SCHEME
        } else {
            "okilum://attachment/"
        },
        okilum_core::document_links::encode(path),
    )
}

pub(super) struct CanvasView {
    root: PathBuf,
    rel: String,
    file_targets: HashMap<String, Result<String, String>>,
    reader: WeakEntity<Reader>,
    pub(super) scene: Option<Result<Arc<Scene>, String>>,
    pub(super) view: Viewport,
    bounds: Bounds<Pixels>,
    fitted: bool,
    drag: Option<Point<Pixels>>,
    _load: Option<Task<()>>,
}
impl CanvasView {
    pub(super) fn new(
        root: PathBuf,
        rel: String,
        reader: WeakEntity<Reader>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            root,
            rel,
            file_targets: HashMap::new(),
            reader,
            scene: None,
            view: Viewport::default(),
            bounds: Bounds::default(),
            fitted: false,
            drag: None,
            _load: None,
        };
        view.reload(cx);
        view
    }
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        let (root, rel) = (self.root.clone(), self.rel.clone());
        self.scene = None;
        self.file_targets.clear();
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    (|| -> anyhow::Result<_> {
                        let path = reader_files::checked_path(&root, &rel)?;
                        let mut bytes = Vec::new();
                        std::fs::File::open(path)?
                            .take(okilum_core::canvas::MAX_BYTES as u64 + 1)
                            .read_to_end(&mut bytes)?;
                        let scene = Arc::new(Scene::parse(&bytes)?);
                        // JSON Canvas paths are exact vault-root identities, not
                        // Markdown links. Startup inventory may still be partial.
                        // Verify unique files off the UI thread with the same
                        // canonical root boundary as ordinary file previews.
                        let mut paths = HashMap::new();
                        let mut targets = HashMap::new();
                        for node in &scene.nodes {
                            if let Content::File { file, subpath } = &node.content {
                                let path = paths.entry(file.clone()).or_insert_with(|| {
                                    checked_file_identity(&root, file).map_err(|e| e.to_string())
                                });
                                targets.insert(
                                    node.id.clone(),
                                    path.clone()
                                        .map(|path| file_target_url(&path, subpath.as_deref())),
                                );
                            }
                        }
                        Ok((scene, targets))
                    })()
                    .map_err(|e| format!("Cannot open Canvas: {e}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.scene = Some(match result {
                    Ok((scene, targets)) => {
                        this.file_targets = targets;
                        Ok(scene)
                    }
                    Err(error) => Err(error),
                });
                cx.notify();
            });
        }));
    }
    fn fit(&mut self) {
        if let Some(Ok(scene)) = &self.scene {
            self.view = Viewport::fit(
                scene.bounds(),
                self.bounds.size.width.into(),
                self.bounds.size.height.into(),
            );
            self.fitted = true;
        }
    }
    fn zoom(&mut self, factor: f32) {
        self.view.zoom(
            factor,
            [
                f32::from(self.bounds.size.width) / 2.,
                f32::from(self.bounds.size.height) / 2.,
            ],
        );
    }

    fn card(&self, node: &okilum_core::canvas::Node, cx: &mut Context<Self>) -> AnyElement {
        let at = self.view.point([node.x, node.y]);
        let scale = self.view.scale;
        let mut card = v_flex()
            .id(SharedString::from(node.id.clone()))
            .absolute()
            .left(px(at[0]))
            .top(px(at[1]))
            .w(px(node.width * scale))
            .h(px(node.height * scale))
            .overflow_hidden()
            .p(px(12. * scale))
            .gap(px(8. * scale))
            .text_size(px(14. * scale))
            .border_1()
            .rounded_md()
            .border_color(color(node.color.as_deref(), cx.theme().border))
            .bg(cx.theme().background);
        match &node.content {
            Content::Text { text } => {
                card = card.child(text.clone());
            }
            Content::File { file, .. } => {
                let target = self.file_targets.get(&node.id);
                let url = target.and_then(|result| result.as_ref().ok()).cloned();
                let reader = self.reader.clone();
                let selector = format!("canvas-open-note-{}", node.id);
                card = card.child(file.clone()).child(
                    Button::new("canvas-open-note")
                        .small()
                        .label("Open note")
                        .disabled(url.is_none())
                        .debug_selector(move || selector.clone())
                        .on_click(move |_, window, cx| {
                            if let Some(url) = &url {
                                handle_link(&reader, url, window, cx);
                            }
                        }),
                );
                if let Some(Err(error)) = target {
                    card = card.child(error.clone());
                }
            }
            Content::Link { url } => {
                card = card.child(url.clone());
                if url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
                    let url = url.clone();
                    card = card.child(
                        Button::new("canvas-open-link")
                            .small()
                            .label("Open link")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                } else {
                    card = card.child("Unsupported link");
                }
            }
            Content::Group { label, background } => {
                card = card
                    .bg(cx.theme().muted.opacity(0.08))
                    .child(label.clone().unwrap_or_else(|| "Group".into()));
                if background.is_some() {
                    card = card.child("Group background image is not previewed");
                }
            }
            Content::Unsupported => {
                card = card.child("Unsupported Canvas card type");
            }
        }
        card.into_any_element()
    }
}

fn color(value: Option<&str>, fallback: Hsla) -> Hsla {
    match value {
        Some("1") => rgb(0xe05252).into(),
        Some("2") => rgb(0xe69841).into(),
        Some("3") => rgb(0xc9b242).into(),
        Some("4") => rgb(0x52a76b).into(),
        Some("5") => rgb(0x4a9fb5).into(),
        Some("6") => rgb(0x9473cd).into(),
        Some(hex) if hex.starts_with('#') && hex.len() == 7 => u32::from_str_radix(&hex[1..], 16)
            .map(|c| rgb(c).into())
            .unwrap_or(fallback),
        _ => fallback,
    }
}

fn edges(scene: Arc<Scene>, view: Viewport, foreground: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let nodes: HashMap<_, _> = scene.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            for edge in &scene.edges {
                let (Some(from), Some(to)) = (
                    nodes.get(edge.from_node.as_str()),
                    nodes.get(edge.to_node.as_str()),
                ) else {
                    continue;
                };
                let a = view.point(from.anchor(edge.from_side));
                let b = view.point(to.anchor(edge.to_side));
                let origin = bounds.origin;
                let point_at = |p: [f32; 2]| point(origin.x + px(p[0]), origin.y + px(p[1]));
                let mut path = gpui::PathBuilder::stroke(px(1.5));
                path.move_to(point_at(a));
                path.line_to(point_at(b));
                // Default JSON Canvas edge has an arrow at its destination.
                let mut arrow = |tip: [f32; 2], other: [f32; 2]| {
                    let angle = (tip[1] - other[1]).atan2(tip[0] - other[0]);
                    for delta in [-0.5f32, 0.5] {
                        path.move_to(point_at(tip));
                        path.line_to(point_at([
                            tip[0] - 10. * (angle + delta).cos(),
                            tip[1] - 10. * (angle + delta).sin(),
                        ]));
                    }
                };
                if edge.to_end.as_deref() != Some("none") {
                    arrow(b, a);
                }
                if edge.from_end.as_deref() == Some("arrow") {
                    arrow(a, b);
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color(edge.color.as_deref(), foreground));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

impl Render for CanvasView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(result) = &self.scene else {
            return div().p_4().child("Loading Canvas…").into_any_element();
        };
        let scene = match result {
            Ok(scene) => scene.clone(),
            Err(error) => return div().p_4().child(error.clone()).into_any_element(),
        };
        if !self.fitted && self.bounds.size.width > px(0.) {
            self.fit();
        }
        let observed = cx.entity().downgrade();
        let mut area = div()
            .id("canvas-scene")
            .debug_selector(|| "canvas-scene".into())
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let _ = observed.update(cx, |this, cx| {
                            if this.bounds != bounds {
                                this.bounds = bounds;
                                // Notify after this draw has finished. A notification
                                // during prepaint can be consumed by the current frame,
                                // leaving the first scene at its unmeasured viewport.
                                window.request_animation_frame();
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(edges(scene.clone(), self.view, cx.theme().muted_foreground))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.drag = Some(event.position);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Middle) {
                    if let Some(previous) = this.drag.replace(event.position) {
                        this.view.x += f32::from(event.position.x - previous.x);
                        this.view.y += f32::from(event.position.y - previous.y);
                        cx.notify();
                    }
                } else {
                    this.drag = None;
                }
            }))
            .on_pinch(cx.listener(|this, event: &gpui::PinchEvent, _, cx| {
                this.view.zoom(
                    1. + event.delta.clamp(-0.9, 1.),
                    [
                        f32::from(event.position.x - this.bounds.origin.x),
                        f32::from(event.position.y - this.bounds.origin.y),
                    ],
                );
                cx.stop_propagation();
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let delta = match event.delta {
                    ScrollDelta::Pixels(p) => p,
                    ScrollDelta::Lines(p) => point(px(p.x * 32.), px(p.y * 32.)),
                };
                if event.modifiers.control || event.modifiers.platform {
                    this.view.zoom(
                        (f32::from(delta.y) / 250.).clamp(-1., 1.).exp(),
                        [
                            f32::from(event.position.x - this.bounds.origin.x),
                            f32::from(event.position.y - this.bounds.origin.y),
                        ],
                    );
                } else {
                    this.view.x += f32::from(delta.x);
                    this.view.y += f32::from(delta.y);
                }
                cx.stop_propagation();
                cx.notify();
            }));
        let width = f32::from(self.bounds.size.width);
        let height = f32::from(self.bounds.size.height);
        for node in &scene.nodes {
            let p = self.view.point([node.x, node.y]);
            if p[0] > width
                || p[1] > height
                || p[0] + node.width * self.view.scale < 0.
                || p[1] + node.height * self.view.scale < 0.
            {
                continue;
            }
            area = area.child(self.card(node, cx));
        }
        let nodes: HashMap<_, _> = scene.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        for edge in &scene.edges {
            if let Some(label) = &edge.label {
                if let (Some(a), Some(b)) = (
                    nodes.get(edge.from_node.as_str()),
                    nodes.get(edge.to_node.as_str()),
                ) {
                    let a = self.view.point(a.anchor(edge.from_side));
                    let b = self.view.point(b.anchor(edge.to_side));
                    area = area.child(
                        div()
                            .absolute()
                            .left(px((a[0] + b[0]) / 2.))
                            .top(px((a[1] + b[1]) / 2.))
                            .text_sm()
                            .bg(cx.theme().background)
                            .child(label.clone()),
                    );
                }
            }
        }
        v_flex()
            .size_full()
            .min_h_0()
            .child(area)
            .into_any_element()
    }
}

fn status_line(viewer: &CanvasView) -> reader_status_line::StatusLine {
    let text = match &viewer.scene {
        Some(Ok(scene)) => format!(
            "Scroll or middle-drag to pan · Ctrl/Cmd + scroll to zoom · {} cards, {} edges{}",
            scene.nodes.len(),
            scene.edges.len(),
            if scene.missing_edges() > 0 {
                format!(" · {} edges reference missing cards", scene.missing_edges())
            } else {
                String::new()
            }
        ),
        _ => String::new(),
    };
    reader_status_line::StatusLine {
        text: text.into(),
        ..Default::default()
    }
}

pub(crate) fn chrome(viewer: &Entity<CanvasView>, cx: &App) -> reader_viewer_chrome::ViewerChrome {
    use reader_viewer_chrome::{ViewerAction, ViewerChrome};
    let state = viewer.read(cx);
    let mut lock = ViewerAction::new(
        "canvas-read-only",
        Icon::default().path(brand::READER_LOCK_ICON),
        "Read only",
        std::rc::Rc::new(|_, _| {}),
    );
    lock.tooltip = "Read-only: canvas files are view-only".into();
    let mut actions = Vec::new();
    for (id, icon, label, factor) in [
        (
            "canvas-fit",
            Icon::default().path(brand::NOTE_WIDTH_FULL_ICON),
            "Fit to content",
            0.,
        ),
        ("canvas-minus", Icon::new(IconName::Minus), "Zoom out", 0.8),
        ("canvas-plus", Icon::new(IconName::Plus), "Zoom in", 1.25),
    ] {
        let target = viewer.clone();
        let mut action = ViewerAction::new(
            id,
            icon,
            label,
            std::rc::Rc::new(move |_, cx| {
                target.update(cx, |this, cx| {
                    if factor == 0. {
                        this.fit();
                    } else {
                        this.zoom(factor);
                    }
                    cx.notify();
                })
            }),
        );
        action = match id {
            "canvas-fit" => action.with_action(CanvasFit, cx),
            "canvas-minus" => action.with_action(CanvasZoomOut, cx),
            _ => action.with_action(CanvasZoomIn, cx),
        };
        action.enabled = matches!(&state.scene, Some(Ok(_)));
        action.disabled_reason = (!action.enabled).then(|| "Canvas is not loaded".into());
        actions.push(action);
    }
    ViewerChrome {
        actions,
        state: Some(lock),
        status: Some(status_line(state)),
        action_text: Some(format!("{:.0}%", state.view.scale * 100.).into()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn exact_file_targets_preserve_delimiters_and_reject_missing_or_outside_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("canvas")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("canvas/linked #%.md"), "# Acceptance C# %\n").unwrap();
        std::fs::write(
            root.join("other/linked #%.md"),
            "Wrong duplicate basename\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("outside.md"), "Outside\n").unwrap();
        let path = checked_file_identity(&root, "canvas/linked #%.md").unwrap();
        assert_eq!(path, "canvas/linked #%.md");
        let url = file_target_url(&path, Some("#Acceptance C# %"));
        assert_eq!(
            url,
            "okilum://open/canvas/linked%20%23%25.md#Acceptance%20C%23%20%25"
        );
        assert_eq!(
            split_open_url(url.strip_prefix(WIKI_SCHEME).unwrap()),
            (path, Some("Acceptance C# %".into()))
        );
        assert!(checked_file_identity(&root, "canvas/missing.md").is_err());
        assert!(checked_file_identity(&root, "../outside.md").is_err());
        assert!(checked_file_identity(&root, "canvas").is_err());
        assert!(
            checked_file_identity(&root, dir.path().join("outside.md").to_str().unwrap()).is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("outside.md"), root.join("alias.md"))
                .unwrap();
            assert!(checked_file_identity(&root, "alias.md").is_err());
        }
        assert_eq!(
            file_target_url("images/photo.jpg", None),
            "okilum://attachment/images/photo.jpg"
        );
    }
}
