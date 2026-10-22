//! Manager surface, mounted independently of the note outline.
use super::*;
use okilum_t3::projects::{Manager, Project};

pub(super) struct ManagerPanel {
    project: Project,
    state: Option<PathBuf>,
    pub(super) focused: bool,
    #[cfg(any(target_os = "macos", test))]
    previous_gpui_focus: Option<FocusHandle>,
    origin: Entity<InputState>,
    error: Option<String>,
    loading: bool,
    generation: u64,
    #[cfg(target_os = "macos")]
    focus_routing: Option<crate::reader_manager_focus::FocusRouting>,
    #[cfg(target_os = "macos")]
    webview: Option<Entity<gpui_wry::WebView>>,
}

impl ManagerPanel {
    pub(super) fn new(
        project: Project,
        state: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let origin = cx.new(|cx| InputState::new(window, cx).placeholder("T3 server (https://…)"));
        let alias = project.environment.clone();
        let saved_state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move {
                    let Some(state) = state else {
                        return Ok(None);
                    };
                    let bytes = match std::fs::read(state.join("t3-connections.json")) {
                        Ok(bytes) => bytes,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                        Err(e) => return Err(anyhow::Error::from(e)),
                    };
                    let saved: std::collections::BTreeMap<String, String> =
                        serde_json::from_slice(&bytes)?;
                    Ok(alias.and_then(|alias| saved.get(&alias).cloned()))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok(Some(origin)) => {
                        this.origin
                            .update(cx, |input, cx| input.set_value(origin, window, cx));
                        this.connect(window, cx);
                    }
                    Ok(None) => {}
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            project,
            state: saved_state,
            focused: false,
            #[cfg(any(target_os = "macos", test))]
            previous_gpui_focus: None,
            origin,
            error: None,
            loading: false,
            generation: 0,
            #[cfg(target_os = "macos")]
            focus_routing: None,
            #[cfg(target_os = "macos")]
            webview: None,
        }
    }

    // Called for the AppKit mouse-down notification, not merely DOM focus.
    #[cfg(any(target_os = "macos", test))]
    fn native_focus_entered(
        &mut self,
        generation: u64,
        owns_native_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        if !owns_native_focus {
            self.focused = false;
            return;
        }
        if let Some(focus) = window.focused(cx) {
            self.previous_gpui_focus = Some(focus);
        }
        // Window::blur clears GPUI's logical input and pending chords without
        // changing AppKit's first responder, which is already the WKWebView.
        window.blur(cx);
        self.focused = true;
        cx.notify();
    }

    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(view) = &self.webview {
            view.update(cx, |view, _| {
                if visible != view.visible() {
                    if visible {
                        view.show();
                    } else {
                        view.hide();
                    }
                }
            });
        }
        #[cfg(target_os = "macos")]
        if let Some(routing) = &self.focus_routing {
            routing.refresh();
        }
        if !visible && self.focused {
            self.focused = false;
            cx.notify();
        }
    }

    pub(super) fn hide(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.focused = false;
        #[cfg(target_os = "macos")]
        self.focus_routing.take();
        #[cfg(target_os = "macos")]
        if let Some(view) = self.webview.take() {
            view.update(cx, |view, _| view.hide());
        }
        let _ = cx;
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Manager::Thread(thread) = self.project.manager.clone() else {
            return;
        };
        let env = match okilum_t3::Environment::new(
            self.project.environment.as_deref().unwrap_or("project"),
            &self.origin.read(cx).value(),
        ) {
            Ok(env) => env,
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let identity = okilum_t3::projects::browser_identity(&env)?;
                    if let Some(state) = &state {
                        project::save_browser_environment(state, &env)?;
                    }
                    okilum_t3::projects::manager_url(&env, &identity, &thread)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(url) => {
                        #[cfg(target_os = "macos")]
                        match Self::browser(&url, generation, window, cx) {
                            Ok((view, routing)) => {
                                this.focus_routing = Some(routing);
                                this.webview = Some(view);
                            }
                            Err(e) => {
                                this.error = Some(format!("Manager could not be opened: {e:#}"))
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            let _ = (url, window);
                        }
                    }
                    Err(e) => this.error = Some(format!("T3 is unreachable: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    fn browser(
        url: &url::Url,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<(
        Entity<gpui_wry::WebView>,
        crate::reader_manager_focus::FocusRouting,
    )> {
        use raw_window_handle::HasWindowHandle;
        use wry::WebViewBuilderExtDarwin;
        let handle = HasWindowHandle::window_handle(window)?;
        // Stable application-specific WKWebsiteDataStore. Cookies never enter
        // Okilum settings or the MCP keychain; login happens in this view.
        let (send, receive) = async_channel::unbounded::<String>();
        let native_events = send.clone();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = receive.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        // An old browser's queued callbacks must not affect its successor.
                        if this.generation != generation {
                            return;
                        }
                        let owns_native_focus =
                            this.focus_routing.as_ref().is_some_and(|routing| {
                                routing.refresh();
                                routing.focused()
                            });
                        if event == "native-focus" {
                            this.native_focus_entered(generation, owns_native_focus, window, cx);
                        } else {
                            this.focused = owns_native_focus;
                        }
                        // Escape already returned native focus. Do not overwrite a newer
                        // outside input or a subsequent re-entry into the webview.
                        if event == "escape" && !owns_native_focus && window.focused(cx).is_none() {
                            if let Some(focus) = this.previous_gpui_focus.take() {
                                focus.focus(window, cx);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let browser = wry::WebViewBuilder::new()
            .with_data_store_identifier(*b"OkilumManagerWeb")
            .with_url(url.as_str())
            .with_focused(false)
            .with_initialization_script(
                r#"
                window.addEventListener('focus', () => window.ipc.postMessage('focus'));
                window.addEventListener('blur', () => window.ipc.postMessage('blur'));
                document.addEventListener('focusin', () => window.ipc.postMessage('focus'));
                document.addEventListener('keydown', event => {
                    if (event.key === 'Escape') {
                        event.preventDefault(); event.stopImmediatePropagation();
                        window.ipc.postMessage('escape');
                    }
                }, true);
            "#,
            )
            .with_ipc_handler(move |request| {
                if matches!(request.body().as_str(), "focus" | "blur" | "escape") {
                    let _ = send.try_send(request.body().clone());
                }
            })
            .with_navigation_handler(|url| {
                url::Url::parse(&url).is_ok_and(|url| matches!(url.scheme(), "https" | "http"))
            })
            .build_as_child(&handle)?;
        let routing =
            crate::reader_manager_focus::FocusRouting::new(&browser, window, native_events)?;
        Ok((
            cx.new(|cx| gpui_wry::WebView::new(browser, window, cx)),
            routing,
        ))
    }
}

impl Render for ManagerPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = v_flex()
            .size_full()
            .min_h_0()
            .gap_2()
            .p_2()
            .child(div().child(format!("{} · Manager", self.project.name)));
        if self.project.manager == Manager::NotStarted {
            return body.child("Manager не запущен").into_any_element();
        }
        body = body.child(
            h_flex()
                .gap_2()
                .child(Input::new(&self.origin).flex_1())
                .child(
                    Button::new("manager-browser-connect")
                        .ghost()
                        .label("Connect")
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                ),
        );
        if self.loading {
            body = body.child("Connecting to T3…");
        }
        if let Some(error) = &self.error {
            body = body.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        #[cfg(target_os = "macos")]
        if let Some(view) = &self.webview {
            let native = view.read(cx).handle();
            let view_state = view.clone();
            let panel = cx.weak_entity();
            body = body.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(view.clone())
                    .child(
                        gpui::canvas(
                            |bounds, window, _| bounds.intersect(&window.content_mask().bounds),
                            move |_, full, window, cx| {
                                if !view_state.read(cx).visible() {
                                    return;
                                }
                                let clear = crate::reader_manager_occlusion::uncovered_bounds(
                                    full,
                                    gpui_base::GlobalState::native_overlays(window, cx),
                                );
                                // All ordinary AND deferred overlays have completed
                                // prepaint before this paint callback. Never use the
                                // previous frame's popup geometry.
                                let obscured = clear != Some(full);
                                let _ = panel.update(cx, |panel, _| {
                                    if obscured
                                        && panel.focus_routing.as_ref().is_some_and(|r| r.focused())
                                    {
                                        let _ = native.raw().focus_parent();
                                        panel.focused = false;
                                    }
                                });
                                if let Some(bounds) = clear {
                                    let result = native.raw().set_bounds(wry::Rect {
                                        position: wry::dpi::LogicalPosition::new(
                                            f64::from(bounds.origin.x),
                                            f64::from(bounds.origin.y),
                                        )
                                        .into(),
                                        size: wry::dpi::LogicalSize::new(
                                            f64::from(bounds.size.width),
                                            f64::from(bounds.size.height),
                                        )
                                        .into(),
                                    });
                                    let _ = native.raw().set_visible(result.is_ok());
                                } else {
                                    // A full-window modal backdrop leaves no free
                                    // rectangle. Do not let its native child cover it.
                                    let _ = native.raw().set_visible(false);
                                }
                                let _ = panel.update(cx, |panel, _| {
                                    if let Some(routing) = &panel.focus_routing {
                                        routing.refresh();
                                    }
                                });
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            );
        }
        body.into_any_element()
    }
}

impl Reader {
    pub(super) fn manager_browser_focused(&self, cx: &App) -> bool {
        self.project_navigation
            .manager_panel
            .as_ref()
            .is_some_and(|panel| {
                let panel = panel.read(cx);
                #[cfg(target_os = "macos")]
                if let Some(routing) = &panel.focus_routing {
                    return routing.focused();
                }
                panel.focused
            })
    }

    pub(super) fn render_manager_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if self
            .project_navigation
            .active
            .as_ref()
            .is_some_and(|p| p.manager == Manager::NotStarted)
        {
            if let Some(project) = &self.project {
                return div().size_full().child(project.clone()).into_any_element();
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.project_navigation.manager_panel {
            return div().size_full().child(panel.clone()).into_any_element();
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(project) = &self.project {
            return div().size_full().child(project.clone()).into_any_element();
        }
        let _ = cx;
        div().p_4().child("Loading manager…").into_any_element()
    }
}

#[cfg(test)]
mod native_focus_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn webview_mouse_down_notification_clears_focused_gpui_origin_input(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let temp = tempfile::tempdir().unwrap();
        let project = Project {
            folder: temp.path().into(),
            name: "Test".into(),
            kind: okilum_t3::projects::Kind::Knowledge,
            environment: Some("test".into()),
            manager_project: Some("brain".into()),
            manager: Manager::Thread("mcp:test".into()),
        };
        let (root, visual) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| ManagerPanel::new(project, None, window, cx));
            Root::new(panel, window, cx)
        });
        visual.run_until_parked();
        let panel = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ManagerPanel>().unwrap()
        });
        panel.update_in(visual, |panel, window, cx| {
            panel.origin.update(cx, |input, cx| {
                input.set_value("https://t3.oklabs.uk", window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
        });
        visual.run_until_parked();
        panel.update_in(visual, |panel, window, cx| {
            assert_eq!(
                window.focused(cx),
                Some(panel.origin.read(cx).focus_handle(cx)),
                "positive control: origin owns GPUI input"
            );
            // Same handler called by the native mouse-down callback. A real
            // WKWebView first-responder check remains Oleg's Mac acceptance.
            panel.native_focus_entered(panel.generation, true, window, cx);
            assert!(
                window.focused(cx).is_none(),
                "webview entry must release every GPUI input"
            );
            assert!(panel.focused);
            assert!(
                panel.previous_gpui_focus.is_some(),
                "Escape can restore the prior GPUI focus"
            );
            assert_eq!(
                panel.origin.read(cx).value().as_str(),
                "https://t3.oklabs.uk"
            );
            // Entry was queued, but an outside click acquired another input
            // before delivery. The live native-focus check must preserve it.
            let queued_generation = panel.generation;
            let outside = cx.new(|cx| InputState::new(window, cx));
            let outside_focus = outside.read(cx).focus_handle(cx);
            outside_focus.focus(window, cx);
            panel.native_focus_entered(queued_generation, false, window, cx);
            assert_eq!(
                window.focused(cx),
                Some(outside_focus.clone()),
                "late entry must not clear the outside-click target"
            );
            assert!(!panel.focused);
            // A replaced browser's entry remains stale even if its successor
            // now owns native focus.
            panel.generation += 1;
            panel.native_focus_entered(queued_generation, true, window, cx);
            assert_eq!(
                window.focused(cx),
                Some(outside_focus),
                "old routing instance cannot blur a successor's GPUI target"
            );
        });
    }
}
