//! A transient, target-bound preview. Hover never changes reading history or focus.
use super::*;
use tessera_core::document_links::{self, prepared::LinkStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub path: String,
    pub heading: Option<String>,
}

#[derive(Default)]
pub(super) struct HoverPreview {
    generation: u64,
    leave_generation: u64,
    source: Option<String>,
    target: Option<Target>,
    anchor: Point<Pixels>,
    over_source: bool,
    over_preview: bool,
    loading: bool,
    visible: bool,
    content: Option<Entity<TextViewState>>,
    states: prepared_links::States,
    message: Option<String>,
    _subscription: Option<Subscription>,
    bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
}

impl HoverPreview {
    pub(super) fn contains(&self, position: Point<Pixels>) -> bool {
        self.visible && self.bounds.get().contains(&position)
    }
    pub(super) fn is_active(&self) -> bool {
        self.source.is_some()
    }
}

fn command(modifiers: gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.platform
    } else {
        modifiers.control
    }
}

pub(super) fn resolved_target(
    url: &str,
    states: &prepared_links::States,
    current: &str,
) -> Option<Target> {
    let state = states.get(url)?;
    if state.status != LinkStatus::Resolved {
        return None;
    }
    let url = state.action_url.as_deref().unwrap_or(url);
    let (path, heading) = split_open_url(url.strip_prefix(WIKI_SCHEME)?);
    let path = if path.is_empty() {
        current.to_owned()
    } else {
        path
    };
    path.to_lowercase()
        .ends_with(".md")
        .then_some(Target { path, heading })
}

impl Reader {
    pub(super) fn clear_hover(&mut self, cx: &mut Context<Self>) {
        let generation = self.hover_preview.generation.wrapping_add(1);
        self.hover_preview = HoverPreview {
            generation,
            ..Default::default()
        };
        cx.notify();
    }

    pub(super) fn hover_link(
        &mut self,
        url: &str,
        active: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !active {
            self.leave_hover(url, cx);
            return;
        }
        if self.quick_open.open || self.editing.is_some() || self.file_menu.is_some() {
            return;
        }
        if let Some(target) = resolved_target(url, &self.prepared_links, &self.current_rel) {
            self.hover_note(url.to_owned(), target, position, window, cx);
        }
    }

    pub(super) fn hover_note(
        &mut self,
        source: String,
        target: Target,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !target.path.to_lowercase().ends_with(".md") {
            return;
        }
        let immediate = command(window.modifiers());
        if self.hover_preview.source.as_ref() == Some(&source)
            && self.hover_preview.target.as_ref() == Some(&target)
        {
            self.hover_preview.over_source = true;
            if immediate && !self.hover_preview.loading && !self.hover_preview.visible {
                self.schedule_hover(Duration::ZERO, window, cx);
            }
            return;
        }
        self.clear_hover(cx);
        self.hover_preview.source = Some(source);
        self.hover_preview.target = Some(target);
        self.hover_preview.anchor = position;
        self.hover_preview.over_source = true;
        self.schedule_hover(
            if immediate {
                Duration::ZERO
            } else {
                Duration::from_millis(350)
            },
            window,
            cx,
        );
    }

    pub(super) fn hover_modifiers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if command(window.modifiers())
            && self.hover_preview.over_source
            && !self.hover_preview.visible
            && !self.hover_preview.loading
        {
            self.schedule_hover(Duration::ZERO, window, cx);
        }
    }

    pub(super) fn leave_hover(&mut self, source: &str, cx: &mut Context<Self>) {
        if self.hover_preview.source.as_deref() != Some(source) {
            return;
        }
        self.hover_preview.over_source = false;
        self.defer_hover_close(cx);
    }

    fn defer_hover_close(&mut self, cx: &mut Context<Self>) {
        self.hover_preview.leave_generation = self.hover_preview.leave_generation.wrapping_add(1);
        let leave = self.hover_preview.leave_generation;
        let generation = self.hover_preview.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(220))
                .await;
            let _ = this.update(cx, |this, cx| {
                let h = &this.hover_preview;
                if h.generation == generation
                    && h.leave_generation == leave
                    && !h.over_source
                    && !h.over_preview
                {
                    this.clear_hover(cx);
                }
            });
        })
        .detach();
    }

    fn schedule_hover(&mut self, delay: Duration, window: &mut Window, cx: &mut Context<Self>) {
        self.hover_preview.generation = self.hover_preview.generation.wrapping_add(1);
        let generation = self.hover_preview.generation;
        let Some(target) = self.hover_preview.target.clone() else {
            return;
        };
        let vault = self.vault.clone();
        let root = self.vault_root.clone();
        let navigation = self.navigation_generation;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let proceed = this
                .update_in(cx, |this, _, cx| {
                    if this.hover_preview.generation != generation
                        || !this.hover_preview.over_source
                    {
                        return false;
                    }
                    this.hover_preview.visible = true;
                    this.hover_preview.loading = true;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !proceed {
                return;
            }
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let document = tessera_core::render::reader_document(&vault, &target.path)?;
                    let heading = target
                        .heading
                        .as_deref()
                        .map(|h| document_links::heading(&document.rendered, h).map(|h| h.block))
                        .transpose()
                        .map_err(anyhow::Error::msg)?;
                    let mut preparation = document_links::prepared::LinkPreparation::new(
                        &vault,
                        &target.path,
                        |path| {
                            let raw = std::fs::read_to_string(vault.root.join(path))
                                .map_err(|e| e.to_string())?;
                            let rendered =
                                tessera_core::render::reader_heading_source(&vault, path, &raw);
                            use sha2::{Digest, Sha256};
                            Ok(document_links::prepared::TargetSnapshot {
                                revision: format!("sha256:{:x}", Sha256::digest(raw.as_bytes())),
                                headings: document_links::HeadingInventory::new(&rendered),
                                supports_setext: true,
                                managed: None,
                            })
                        },
                    );
                    let states = Arc::new(preparation.identities(&document.links));
                    Ok::<_, anyhow::Error>((document, states, heading))
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.hover_preview.generation != generation
                    || this.navigation_generation != navigation
                    || this.vault_root != root
                {
                    return;
                }
                this.hover_preview.loading = false;
                match loaded {
                    Err(error) => {
                        this.hover_preview.message = Some(format!("Preview unavailable: {error}"))
                    }
                    Ok((document, states, heading)) => {
                        let configured = preview_plugins(
                            TextView::markdown("hover-config", ""),
                            cx.entity().downgrade(),
                            states.clone(),
                        );
                        let state = cx.new(|cx| {
                            let mut state = TextViewState::markdown("", cx)
                                .scrollable(true)
                                .selectable(true);
                            configured.prepare_state(&mut state, cx);
                            state.set_text_with_source(
                                &document.rendered,
                                Some(document.original_body.into()),
                                cx,
                            );
                            state
                        });
                        this.hover_preview._subscription =
                            Some(cx.observe(&state, |_, _, cx| cx.notify()));
                        this.hover_preview.states = states;
                        this.hover_preview.content = Some(state);
                        if let Some(block) = heading {
                            this.land_hover(block, generation, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn land_hover(&self, block: usize, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for attempt in 0..100 {
                cx.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
                let done = this
                    .update(cx, |this, cx| {
                        if this.hover_preview.generation != generation {
                            return true;
                        }
                        let Some(state) = this.hover_preview.content.clone() else {
                            return true;
                        };
                        if state
                            .read(cx)
                            .preparation_status()
                            .is_some_and(|r| r.is_ok())
                            && state.read(cx).list_state().item_count() > block
                        {
                            state.update(cx, |s, cx| {
                                s.list_state().scroll_to(ListOffset {
                                    item_ix: block,
                                    offset_in_item: px(0.),
                                });
                                cx.notify();
                            });
                            return true;
                        }
                        if attempt == 99
                            || state
                                .read(cx)
                                .preparation_status()
                                .is_some_and(|r| r.is_err())
                        {
                            this.hover_preview.content = None;
                            this.hover_preview.message =
                                Some("The preview could not render the requested heading.".into());
                            cx.notify();
                            return true;
                        }
                        cx.notify();
                        false
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn render_hover_preview(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let h = &self.hover_preview;
        if !h.visible {
            return None;
        }
        let target = h.target.clone()?;
        let palette = brand::palette(cx);
        let size = window.viewport_size();
        let width = px(520.).min((size.width - px(24.)).max(px(120.)));
        let height = px(420.).min((size.height - px(64.)).max(px(120.)));
        let entity = cx.entity().downgrade();
        let title = self
            .vault
            .notes
            .iter()
            .find(|n| n.path == target.path)
            .map(|n| n.title.clone())
            .unwrap_or_else(|| target.path.clone());
        let body = if let Some(state) = &h.content {
            preview_plugins(
                TextView::new(state)
                    .scrollable(true)
                    .selectable(true)
                    .text_size(px(BODY_FONT_SIZE))
                    .p_3()
                    .w_full()
                    .flex_1()
                    .min_h_0(),
                entity,
                h.states.clone(),
            )
            .into_any_element()
        } else {
            div()
                .p_3()
                .child(
                    h.message
                        .clone()
                        .unwrap_or_else(|| "Loading preview…".into()),
                )
                .into_any_element()
        };
        let bounds = h.bounds.clone();
        Some(
            anchored()
                .position(h.anchor + point(px(12.), px(18.)))
                .snap_to_window_with_margin(px(12.))
                .child(
                    v_flex()
                        .id("note-hover-preview")
                        .debug_selector(|| "note-hover-preview".into())
                        .occlude()
                        .relative()
                        .child(
                            canvas(move |rect, _, _| bounds.set(rect), |_, _, _, _| {})
                                .absolute()
                                .inset_0(),
                        )
                        .w(width)
                        .h(height)
                        .rounded_lg()
                        .bg(palette.surface)
                        .text_color(palette.text)
                        .border_1()
                        .border_color(palette.border)
                        .shadow_lg()
                        .on_hover(cx.listener(|this, active, _, cx| {
                            this.hover_preview.over_preview = *active;
                            if !active {
                                this.defer_hover_close(cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .gap_2()
                                .border_b_1()
                                .border_color(palette.border)
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(title),
                                )
                                .child(
                                    Button::new("open-hover-note")
                                        .debug_selector(|| "open-hover-note".into())
                                        .label("↗")
                                        .ghost()
                                        .small()
                                        .tooltip("Open note")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.clear_hover(cx);
                                            if this.quick_open.open {
                                                this.close_quick_open(window, cx);
                                            }
                                            this.open_note_at(
                                                &target.path,
                                                None,
                                                target.heading.as_deref(),
                                                window,
                                                cx,
                                            );
                                        })),
                                ),
                        )
                        .child(body),
                )
                .into_any_element(),
        )
    }
}

fn preview_plugins(
    view: TextView,
    entity: WeakEntity<Reader>,
    states: prepared_links::States,
) -> TextView {
    let presented = states.clone();
    markdown_plugins(
        view,
        Arc::new(move |url, _, window, cx| {
            let _ = entity.update(cx, |this, cx| {
                let current = this
                    .hover_preview
                    .target
                    .as_ref()
                    .map(|t| t.path.as_str())
                    .unwrap_or("");
                if let Some(target) = resolved_target(url, &states, current) {
                    this.clear_hover(cx);
                    if this.quick_open.open {
                        this.close_quick_open(window, cx);
                    }
                    this.open_note_at(&target.path, None, target.heading.as_deref(), window, cx);
                } else if prepared_links::external_tooltip(url).is_some() {
                    cx.open_url(url);
                } else {
                    this.hover_preview.message =
                        Some("This link cannot be resolved unambiguously.".into());
                    cx.notify();
                }
            });
        }),
        Arc::new(|url| {
            if let Some(path) = url::Url::parse(url)
                .ok()
                .and_then(|u| u.to_file_path().ok())
            {
                Some(MarkdownImage::Source(reader_files::image_source(path)))
            } else if url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("data:")
            {
                Some(MarkdownImage::Source(ImageSource::from(url)))
            } else {
                None
            }
        }),
        SelectionFormat::Plain,
    )
    .link_presentation(move |url| prepared_links::presentation(url, &presented))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn only_resolved_internal_notes_preview() {
        let mut states = std::collections::BTreeMap::new();
        for (url, status) in [
            ("tessera://open/A.md#Heading", LinkStatus::Resolved),
            ("https://example.com", LinkStatus::External),
            ("tessera://open/Bad.md", LinkStatus::Ambiguous),
            ("tessera://open/Missing.md", LinkStatus::MissingDocument),
            ("tessera://attachment/a.html", LinkStatus::Resolved),
        ] {
            states.insert(
                url.into(),
                document_links::prepared::LinkState {
                    status,
                    reason: String::new(),
                    action_url: None,
                    target_revision: None,
                },
            );
        }
        let states = Arc::new(states);
        let resolved = format!("{WIKI_SCHEME}A.md#Heading");
        assert_eq!(
            resolved_target(&resolved, &states, "Start.md"),
            Some(Target {
                path: "A.md".into(),
                heading: Some("Heading".into())
            })
        );
        for url in [
            "https://example.com",
            "tessera://open/Bad.md",
            "tessera://open/Missing.md",
            "tessera://attachment/a.html",
            "tessera://open/Unknown.md",
        ] {
            assert!(resolved_target(url, &states, "Start.md").is_none());
        }
    }

    fn fixture(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<Reader>, &mut gpui::VisualTestContext, PathBuf) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-hover-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(
            root.join("Start.md"),
            "# Start\n\n[[sub/Target#Landing|Preview target]]\n\n[External](https://example.com)\n",
        )
        .unwrap();
        std::fs::write(root.join("sub/Target.md"), format!("# Target\n\n{}\n\n## Landing\n\n> [!note]\n> Nested **body**\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n[[Start]]\n", "Paragraph.\n\n".repeat(40))).unwrap();
        let mut reader = None;
        let vault = root.clone();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let v = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault),
                        note: Some("Start.md".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(v.clone());
            Root::new(v, window, cx)
        });
        visual.simulate_resize(size(px(1400.), px(900.)));
        visual.run_until_parked();
        (reader.unwrap(), visual, root)
    }

    #[gpui::test]
    fn delayed_preview_lands_without_navigating_and_closes(cx: &mut gpui::TestAppContext) {
        let (reader, visual, root) = fixture(cx);
        let original = reader.read_with(visual, |v, _| (v.current_rel.clone(), v.history.len()));
        reader.update_in(visual, |v, window, cx| {
            let url = v
                .link_identities
                .iter()
                .find(|link| {
                    resolved_target(&link.url, &v.prepared_links, &v.current_rel).is_some()
                })
                .expect("resolved internal positive control")
                .url
                .clone();
            assert!(
                resolved_target(&url, &v.prepared_links, &v.current_rel).is_some(),
                "url={url}, states={:?}",
                v.prepared_links
            );
            v.hover_link(&url, true, point(px(650.), px(250.)), window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| assert!(!v.hover_preview.visible));
        visual.executor().advance_clock(Duration::from_millis(350));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(60));
        visual.run_until_parked();
        reader.read_with(visual, |v, cx| {
            assert!(v.hover_preview.visible);
            assert!(
                v.hover_preview.message.is_none(),
                "{:?}",
                v.hover_preview.message
            );
            let text = v
                .hover_preview
                .content
                .as_ref()
                .expect("rendered positive control")
                .read(cx);
            assert!(
                text.list_state().logical_scroll_top().item_ix > 20,
                "heading landing"
            );
            assert_eq!((v.current_rel.clone(), v.history.len()), original);
        });
        assert!(visual.debug_bounds("note-hover-preview").is_some());
        reader.update_in(visual, |v, _, cx| {
            let key = v.hover_preview.source.clone().unwrap();
            v.leave_hover(&key, cx);
            v.hover_preview.over_preview = true;
        });
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(v.hover_preview.visible, "moving into preview keeps it")
        });
        reader.update_in(visual, |v, window, cx| v.dismiss(window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("note-hover-preview").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn stale_delay_and_missing_heading_never_show_wrong_note(cx: &mut gpui::TestAppContext) {
        let (reader, visual, root) = fixture(cx);
        reader.update_in(visual, |v, window, cx| {
            v.hover_note(
                "old".into(),
                Target {
                    path: "Start.md".into(),
                    heading: None,
                },
                point(px(600.), px(200.)),
                window,
                cx,
            );
            v.hover_note(
                "new".into(),
                Target {
                    path: "sub/Target.md".into(),
                    heading: Some("Missing".into()),
                },
                point(px(600.), px(200.)),
                window,
                cx,
            );
        });
        visual.executor().advance_clock(Duration::from_millis(400));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.hover_preview.source.as_deref(), Some("new"));
            assert!(v.hover_preview.content.is_none());
            assert!(v.hover_preview.message.is_some());
        });
        reader.update_in(visual, |v, _, cx| {
            v.clear_hover(cx);
        });
        visual.run_until_parked();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn quick_open_hover_is_immediate_with_command_and_escape_preserves_query(
        cx: &mut gpui::TestAppContext,
    ) {
        let (reader, visual, root) = fixture(cx);
        reader.update_in(visual, |v, window, cx| {
            v.open_quick_open(false, window, cx);
            v.quick_open
                .input
                .update(cx, |input, cx| input.set_value("Target", window, cx));
            v.refresh_quick_open(cx);
        });
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        let row = visual
            .debug_bounds("quick-open-result-0")
            .expect("result positive control");
        let mut modifiers = gpui::Modifiers::default();
        if cfg!(target_os = "macos") {
            modifiers.platform = true;
        } else {
            modifiers.control = true;
        }
        visual.simulate_mouse_move(row.center(), None, modifiers);
        visual.run_until_parked();
        reader.read_with(visual, |v, cx| {
            assert!(v.hover_preview.visible, "held command skips delay");
            assert_eq!(v.quick_open.input.read(cx).value(), "Target");
        });
        reader.update_in(visual, |v, window, cx| {
            assert!(
                v.quick_open
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "hover preserves palette focus"
            );
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |v, cx| {
            assert!(!v.hover_preview.is_active());
            assert!(v.quick_open.open);
            assert_eq!(v.quick_open.input.read(cx).value(), "Target");
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn inline_hover_uses_exact_geometry_and_reports_leave(cx: &mut gpui::TestAppContext) {
        struct Harness {
            text: Entity<TextViewState>,
            calls: Arc<std::sync::Mutex<Vec<bool>>>,
        }
        impl Render for Harness {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let calls = self.calls.clone();
                div()
                    .size_full()
                    .child(TextView::new(&self.text).w_full().on_link_hover(
                        move |url, active, _, _, _| {
                            assert_eq!(url.as_ref(), "tessera://open/Target.md");
                            calls.lock().unwrap().push(active);
                        },
                    ))
            }
        }
        cx.update(gpui_component::init);
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = calls.clone();
        let (_, visual) = cx.add_window_view(|_, cx| Harness {
            text: cx.new(|cx| TextViewState::markdown("[Preview](tessera://open/Target.md)", cx)),
            calls,
        });
        visual.run_until_parked();
        visual.simulate_mouse_move(point(px(12.), px(12.)), None, gpui::Modifiers::default());
        visual.run_until_parked();
        assert!(
            observed.lock().unwrap().contains(&true),
            "internal link positive control"
        );
        visual.simulate_mouse_move(point(px(500.), px(12.)), None, gpui::Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            observed.lock().unwrap().last(),
            Some(&false),
            "blank line area is not a link"
        );
    }
}
