//! Manager surface, mounted independently of the note outline.
use super::*;
use okilum_t3::projects::{Manager, Project};

pub(super) struct ManagerPanel {
    project: Project,
    pub(super) focused: bool,
    #[cfg(any(target_os = "macos", test))]
    previous_gpui_focus: Option<FocusHandle>,
    origin: Entity<InputState>,
    project_override: bool,
    _connection: Subscription,
    error: Option<String>,
    transport: Option<String>,
    origin_edited: bool,
    _origin_events: Subscription,
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
        let origin = cx
            .new(|cx| InputState::new(window, cx).placeholder("t3.oklabs.uk or 10.10.0.23:23110"));
        let origin_events =
            cx.subscribe_in(&origin, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.connect(window, cx),
                InputEvent::Change => {
                    this.origin_edited = true;
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            });
        let weak = cx.weak_entity();
        let connection =
            window.observe_global::<reader_t3_settings::Connection>(cx, move |window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if this.project_override {
                        return;
                    }
                    if let Some(origin) = cx
                        .try_global::<reader_t3_settings::Connection>()
                        .and_then(|c| c.origin.clone())
                    {
                        this.hide(cx);
                        this.loading = false;
                        this.origin
                            .update(cx, |input, cx| input.set_value(origin, window, cx));
                        this.connect(window, cx);
                    }
                });
            });
        let app_origin = cx
            .try_global::<reader_t3_settings::Connection>()
            .and_then(|c| c.origin.clone());
        let alias = project
            .environment
            .clone()
            .unwrap_or_else(|| "project".into());
        let area = project.folder.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move {
                    let override_origin =
                        project::project_browser_origin(&area, &alias, state.as_deref())?;
                    let is_override = override_origin.is_some();
                    let origin = match override_origin.or(app_origin) {
                        Some(origin) => Some(origin),
                        None => project::prefill_app_browser_origin(state.as_deref())?,
                    };
                    Ok::<_, anyhow::Error>((origin, is_override))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // A delayed disk read must never replace typing or a manual connection.
                if this.origin_edited || this.generation != 0 {
                    return;
                }
                match saved {
                    Ok((Some(origin), project_override)) => {
                        this.project_override = project_override;
                        let origin = if project_override {
                            origin
                        } else {
                            cx.try_global::<reader_t3_settings::Connection>()
                                .and_then(|c| c.origin.clone())
                                .unwrap_or(origin)
                        };
                        this.origin
                            .update(cx, |input, cx| input.set_value(origin, window, cx));
                        this.connect(window, cx);
                    }
                    Ok((None, project_override)) => {
                        this.project_override = project_override;
                        if let Some(origin) = cx
                            .try_global::<reader_t3_settings::Connection>()
                            .and_then(|c| c.origin.clone())
                        {
                            this.origin
                                .update(cx, |input, cx| input.set_value(origin, window, cx));
                            this.connect(window, cx);
                        }
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            project,
            focused: false,
            #[cfg(any(target_os = "macos", test))]
            previous_gpui_focus: None,
            origin,
            // Ignore global changes until manifest/registry precedence is known.
            project_override: true,
            _connection: connection,
            error: None,
            transport: None,
            origin_edited: false,
            _origin_events: origin_events,
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
        if self.loading {
            return;
        }
        let Manager::Thread(thread) = self.project.manager.clone() else {
            return;
        };
        let env = match okilum_t3::Environment::from_input(
            self.project.environment.as_deref().unwrap_or("project"),
            &self.origin.read(cx).value(),
        ) {
            Ok(env) => env,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        self.origin
            .update(cx, |input, cx| input.set_value(env.origin(), window, cx));
        self.transport = Some(format!("{} · {}", env.origin(), env.transport_rule()));
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let identity = okilum_t3::projects::browser_identity(&env)?;
                    let url = okilum_t3::projects::manager_url(&env, &identity, &thread)?;
                    Ok::<_, anyhow::Error>(url)
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
                url::Url::parse(&url).is_ok_and(|url| {
                    url.username().is_empty()
                        && url.password().is_none()
                        && okilum_t3::Environment::new(
                            "navigation",
                            &url.origin().ascii_serialization(),
                        )
                        .is_ok()
                })
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
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = v_flex()
            .size_full()
            .min_h_0()
            .gap_2()
            .p_2()
            .child(div().child(format!("{} · Manager", self.project.name)));
        if self.project.manager == Manager::NotStarted {
            return body.child("Manager не запущен").into_any_element();
        }
        if self.loading {
            body = body.child("Connecting to T3…");
        }
        if !self.loading {
            #[cfg(target_os = "macos")]
            let connected = self.webview.is_some();
            #[cfg(not(target_os = "macos"))]
            let connected = false;
            if !connected {
                body = body.child("T3 is not connected or paired.").child(
                    Button::new("manager-open-t3-settings")
                        .ghost()
                        .small()
                        .h(px(28.))
                        .rounded(px(brand::CONTROL_RADIUS))
                        .label("Open T3 settings")
                        .on_click(|_, _, cx| reader_settings::show_t3(cx)),
                );
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(view) = &self.webview {
            body = body.child(div().flex_1().min_h_0().child(view.clone()));
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
