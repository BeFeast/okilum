//! Manager surface, mounted independently of the note outline.
use super::*;
use okilum_t3::projects::{Manager, Project};

/// Used when no origin was saved for this T3 environment.
pub(super) const DEFAULT_T3_ORIGIN: &str = "https://t3.oklabs.uk";

/// Wait before the next automatic connection attempt. The Mac's network or
/// VPN may come up after Okilum starts; never wait for a click to recover.
fn retry_delay(failures: u32) -> std::time::Duration {
    let seconds = match failures {
        0 | 1 => 2,
        2 => 5,
        3 => 10,
        _ => 30,
    };
    std::time::Duration::from_secs(seconds)
}

pub(super) struct ManagerPanel {
    project: Project,
    state: Option<PathBuf>,
    pub(super) focused: bool,
    #[cfg(any(target_os = "macos", test))]
    previous_gpui_focus: Option<FocusHandle>,
    origin: Entity<InputState>,
    error: Option<String>,
    /// The last error is transient: another attempt is scheduled.
    retrying: bool,
    /// Consecutive failed attempts since the last success or address change.
    failures: u32,
    /// Set by `hide`: a released panel never reconnects.
    closed: bool,
    loading: bool,
    generation: u64,
    _activation: Option<Subscription>,
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
                let origin = match saved {
                    Ok(Some(origin)) if !origin.trim().is_empty() => origin,
                    Ok(_) => DEFAULT_T3_ORIGIN.into(),
                    Err(e) => {
                        this.log_failure(&format!("saved T3 origin unreadable: {e:#}"), cx);
                        DEFAULT_T3_ORIGIN.into()
                    }
                };
                this.origin
                    .update(cx, |input, cx| input.set_value(origin, window, cx));
                // The app connects by itself; Linux shows the project view instead.
                #[cfg(all(target_os = "macos", not(test)))]
                this.connect(window, cx);
                cx.notify();
            });
        })
        .detach();
        // Coming back to the window (wake, VPN up, app switch) retries at once.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.retry_now(window, cx);
            }
        });
        Self {
            project,
            state: saved_state,
            focused: false,
            #[cfg(any(target_os = "macos", test))]
            previous_gpui_focus: None,
            origin,
            error: None,
            retrying: false,
            failures: 0,
            closed: false,
            loading: false,
            generation: 0,
            _activation: Some(activation),
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
        self.closed = true;
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

    /// The T3 origin this panel uses (saved, or typed in Settings → T3).
    pub(super) fn origin_value(&self, cx: &App) -> String {
        self.origin.read(cx).value().to_string()
    }

    fn connected(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.webview.is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// One short status line (panel and Settings → T3) and the full error,
    /// shown only as a tooltip.
    pub(super) fn connection_status(&self) -> (String, Option<String>) {
        if self.connected() {
            return ("Connected".into(), None);
        }
        let line = match (&self.error, self.retrying) {
            (Some(_), true) => "T3 unreachable — retrying",
            (Some(_), false) => "T3 not connected — check the address in Settings › T3",
            (None, _) => "Connecting to T3…",
        };
        (line.into(), self.error.clone())
    }

    /// Settings → T3 (#1507): replace the current browser with one for `origin`.
    pub(super) fn connect_to(
        &mut self,
        origin: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hide(cx);
        self.closed = false;
        self.loading = false;
        self.failures = 0;
        self.error = None;
        self.retrying = false;
        self.origin
            .update(cx, |input, cx| input.set_value(origin, window, cx));
        self.connect(window, cx);
    }

    /// Window became active: skip the backoff wait if still disconnected.
    fn retry_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed || self.loading || self.connected() || !self.retrying {
            return;
        }
        self.failures = 0;
        self.connect(window, cx);
    }

    fn log_failure(&self, error: &str, cx: &App) {
        eprintln!(
            "okilum: manager T3 connect failed (attempt {}): {error}",
            self.failures
        );
        if let Some(trace) = reader_diagnostics::trace(cx) {
            trace.event(
                "manager_t3_connect",
                serde_json::json!({"failures": self.failures, "error": error}),
            );
        }
    }

    /// A failed attempt: keep one quiet line and try again with backoff.
    fn failed(&mut self, error: String, window: &mut Window, cx: &mut Context<Self>) {
        self.failures = self.failures.saturating_add(1);
        self.log_failure(&error, cx);
        self.error = Some(error);
        self.retrying = true;
        let generation = self.generation;
        let delay = retry_delay(self.failures);
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update_in(cx, |this, window, cx| {
                // A newer attempt, an address change or `hide` supersedes this one.
                if this.generation != generation || this.closed || this.loading || this.connected()
                {
                    return;
                }
                this.connect(window, cx);
            });
        })
        .detach();
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Manager::Thread(thread) = self.project.manager.clone() else {
            return;
        };
        if self.closed {
            return;
        }
        let env = match okilum_t3::Environment::new(
            self.project.environment.as_deref().unwrap_or("project"),
            &self.origin.read(cx).value(),
        ) {
            Ok(env) => env,
            Err(e) => {
                // A malformed address does not heal by retrying.
                self.error = Some(e.to_string());
                self.retrying = false;
                self.log_failure(&e.to_string(), cx);
                cx.notify();
                return;
            }
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        // The previous error stays until this attempt ends: no flicker.
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
                                this.error = None;
                                this.retrying = false;
                                this.failures = 0;
                            }
                            Err(e) => {
                                let error = format!("Manager could not be opened: {e:#}");
                                this.failed(error, window, cx);
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            let _ = (url, window);
                            this.error = None;
                            this.retrying = false;
                        }
                    }
                    Err(e) => this.failed(format!("T3 is unreachable: {e:#}"), window, cx),
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
        // #1596: links and copies the T3 page cannot carry out in a WKWebView.
        let (link_send, link_receive) =
            async_channel::unbounded::<crate::reader_manager_links::PageRequest>();
        cx.spawn_in(window, async move |_, cx| {
            while let Ok(request) = link_receive.recv().await {
                let handled = cx.update(|_, cx| match request {
                    crate::reader_manager_links::PageRequest::OpenExternal(url) => {
                        cx.open_url(url.as_str())
                    }
                    crate::reader_manager_links::PageRequest::Copy(text) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text))
                    }
                });
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
        let new_windows = link_send.clone();
        let t3 = url.clone();
        let store = crate::reader_manager_store::application_identifier();
        let browser = wry::WebViewBuilder::new()
            .with_incognito(false)
            .with_data_store_identifier(store)
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
            .with_initialization_script(&crate::reader_manager_links::page_script(&url))
            .with_ipc_handler(move |request| {
                if matches!(request.body().as_str(), "focus" | "blur" | "escape") {
                    let _ = send.try_send(request.body().clone());
                } else if let Some(request) = crate::reader_manager_links::page_request(
                    request.body(),
                    &request.uri().to_string(),
                    &t3,
                ) {
                    let _ = link_send.try_send(request);
                }
            })
            // target=_blank links and any window.open the page script did not
            // take: WebKit gets no second web view; the system browser opens it.
            .with_new_window_req_handler(move |url, _| {
                if let Some(url) = crate::reader_manager_links::external_url(&url) {
                    let _ = new_windows
                        .try_send(crate::reader_manager_links::PageRequest::OpenExternal(url));
                }
                wry::NewWindowResponse::Deny
            })
            .with_navigation_handler(|url| {
                url::Url::parse(&url).is_ok_and(|url| matches!(url.scheme(), "https" | "http"))
            })
            .build_as_child(&handle)?;
        crate::reader_manager_store::verify(&browser, &store)?;
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
        if self.project.manager == Manager::NotStarted {
            return div().p_2().child("Manager не запущен").into_any_element();
        }
        // No Okilum header or project line above T3: T3's own thread header
        // says where you are. Until connected, one quiet status line.
        #[cfg(target_os = "macos")]
        if let Some(view) = &self.webview {
            return div().size_full().child(view.clone()).into_any_element();
        }
        let (line, error) = self.connection_status();
        div()
            .id("manager-t3-status")
            .px_3()
            .py_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(line)
            .when_some(error, |status, error| {
                status.tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
            })
            .into_any_element()
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
