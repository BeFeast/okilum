//! Manager surface, mounted independently of the note outline.
use super::*;
use okilum_t3::projects::{Manager, Project};

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

/// Quiet period after start or an address change: the network, VPN or T3
/// itself may need a moment, so no error is shown until it has passed.
const CONNECT_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// The one status line for the panel and Settings › T3, or `None` while
/// there is nothing worth saying (connecting within the grace period).
fn status_line(
    connected: bool,
    error: bool,
    retrying: bool,
    loading: bool,
    elapsed: std::time::Duration,
) -> Option<&'static str> {
    if connected {
        return Some("Connected");
    }
    if error && !retrying {
        // A malformed address does not heal by waiting.
        return Some("T3 unavailable — check the address in Settings › T3");
    }
    if elapsed < CONNECT_GRACE {
        return None;
    }
    (error || loading).then_some("T3 unavailable — reconnecting…")
}

pub(super) struct ManagerPanel {
    project: Project,
    pub(super) focused: bool,
    #[cfg(any(target_os = "macos", test))]
    previous_gpui_focus: Option<FocusHandle>,
    origin: Entity<InputState>,
    project_override: bool,
    _connection: Subscription,
    error: Option<String>,
    /// The last error is transient: another attempt is scheduled.
    retrying: bool,
    /// Consecutive failed attempts since the last success or address change.
    failures: u32,
    /// Set by `hide`: a released panel never reconnects.
    closed: bool,
    loading: bool,
    /// Start of the current grace period (panel creation or address change).
    started: std::time::Instant,
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
        let origin = cx
            .new(|cx| InputState::new(window, cx).placeholder("t3.oklabs.uk or 10.10.0.23:23110"));
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
                        this.connect_to(origin, window, cx);
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
                if this.generation != 0 || this.closed {
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
                        #[cfg(all(target_os = "macos", not(test)))]
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
                            #[cfg(all(target_os = "macos", not(test)))]
                            this.connect(window, cx);
                        }
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        Self::refresh_after_grace(window, cx);
        // Coming back to the window (wake, VPN up, app switch) retries at once.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.retry_now(window, cx);
            }
        });
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
            retrying: false,
            failures: 0,
            closed: false,
            loading: false,
            started: std::time::Instant::now(),
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
    #[cfg(test)]
    pub(super) fn origin_value(&self, cx: &App) -> String {
        self.origin.read(cx).value().to_string()
    }

    fn connected(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.webview.is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Re-render once the grace period ends so a still-failing connection
    /// shows its quiet line without waiting for the next attempt.
    fn refresh_after_grace(window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(CONNECT_GRACE).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// One short status line (panel and Settings → T3) and the full error,
    /// shown only as a tooltip. No line and no error during the grace period.
    pub(super) fn connection_status(&self) -> (Option<String>, Option<String>) {
        let connected = self.connected();
        let line = status_line(
            connected,
            self.error.is_some(),
            self.retrying,
            self.loading,
            self.started.elapsed(),
        );
        let error = if connected || line.is_none() {
            None
        } else {
            self.error.clone()
        };
        (line.map(Into::into), error)
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
        self.started = std::time::Instant::now();
        Self::refresh_after_grace(window, cx);
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
        if self.loading {
            return;
        }
        let Manager::Thread(thread) = self.project.manager.clone() else {
            return;
        };
        if self.closed {
            return;
        }
        let env = match okilum_t3::Environment::from_input(
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
        self.origin
            .update(cx, |input, cx| input.set_value(env.origin(), window, cx));
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        // The previous error stays until this attempt ends: no flicker.
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
                            this.failures = 0;
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
                        if event.starts_with("page-load:") {
                            if event == "page-load:finished" {
                                if let Some(routing) = &this.focus_routing {
                                    routing.document_loaded();
                                }
                            }
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
        let page_events = send.clone();
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
                // Preserve the approved c1dc8dc6 behavior: after page handlers,
                // keep the active form focused until a pressed button can click.
                // WebKit otherwise blurs to body and T3 collapses/unmounts the X.
                window.addEventListener('mousedown', event => {
                    const target = event.target instanceof Element ? event.target : event.target?.parentElement;
                    const button = target?.closest('button, [role="button"]');
                    const form = button?.closest('form');
                    const active = document.activeElement;
                    const keep = event.button === 0 && !event.defaultPrevented && !!form
                        && !button.disabled && !button.closest('[draggable="true"]')
                        && active !== button && form.contains(active);
                    if (keep) event.preventDefault();
                });
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
            .with_on_page_load_handler(move |event, _url| {
                let event = match event {
                    wry::PageLoadEvent::Started => "page-load:started",
                    wry::PageLoadEvent::Finished => "page-load:finished",
                };
                let _ = page_events.try_send(event.into());
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
        crate::reader_manager_store::verify(&browser, &store)?;
        let routing =
            crate::reader_manager_focus::FocusRouting::new(&browser, window, native_events)?;
        Ok((
            cx.new(|cx| gpui_wry::WebView::new(browser, window, cx)),
            routing,
        ))
    }
}

fn t3_settings_button() -> Button {
    Button::new("manager-open-t3-settings")
        .ghost()
        .small()
        .h(px(28.))
        .rounded(px(brand::CONTROL_RADIUS))
        .label("Open T3 settings")
        .on_click(|_, _, cx| reader_settings::show_t3(cx))
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
            let native = view.read(cx).handle();
            let view_state = view.clone();
            let panel = cx.weak_entity();
            return div()
                .size_full()
                .flex_1()
                .min_h_0()
                .relative()
                .child(view.clone())
                .child(
                    gpui::canvas(
                        |_, _, _| (),
                        move |_, (), window, cx| {
                            if !view_state.read(cx).visible() {
                                return;
                            }
                            // Wry's element has already laid out the actual native
                            // child. A sibling absolute canvas can have a different
                            // static position: never use it to move/hide WebKit.
                            let full = view_state.read(cx).bounds();
                            let overlays = gpui_base::GlobalState::native_overlays(window, cx);
                            let clear =
                                crate::reader_manager_occlusion::uncovered_bounds(full, overlays);
                            // All ordinary AND deferred overlays have completed
                            // prepaint before this paint callback. Never use the
                            // previous frame's popup geometry.
                            let obscured = clear != Some(full);
                            let _ = panel.update(cx, |panel, cx| {
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
                    .inset_0(),
                )
                .into_any_element();
        }
        let (line, error) = self.connection_status();
        div()
            .id("manager-t3-status")
            .px_3()
            .py_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .children(line)
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
mod status_tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::time::Duration;

    #[test]
    fn retries_back_off_2_5_10_then_every_30_seconds() {
        let delays: Vec<u64> = (1..=6).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, [2, 5, 10, 30, 30, 30]);
        assert_eq!(retry_delay(u32::MAX).as_secs(), 30);
        assert!(CONNECT_GRACE > retry_delay(1) + retry_delay(2));
    }

    #[test]
    fn no_status_before_grace_deadline_quiet_line_after() {
        let before = CONNECT_GRACE - Duration::from_millis(1);
        // First attempts, failing or still loading: nothing at all.
        assert_eq!(status_line(false, false, false, true, Duration::ZERO), None);
        assert_eq!(status_line(false, true, true, false, before), None);
        assert_eq!(status_line(false, true, true, true, before), None);
        // Grace over without success: one quiet line.
        assert_eq!(
            status_line(false, true, true, false, CONNECT_GRACE),
            Some("T3 unavailable — reconnecting…")
        );
        assert_eq!(
            status_line(false, false, false, true, CONNECT_GRACE),
            Some("T3 unavailable — reconnecting…")
        );
        // Nothing attempted (Linux): still nothing to say.
        assert_eq!(status_line(false, false, false, false, CONNECT_GRACE), None);
        // A malformed address is reported at once.
        assert_eq!(
            status_line(false, true, false, false, Duration::ZERO),
            Some("T3 unavailable — check the address in Settings › T3")
        );
        assert_eq!(
            status_line(true, false, false, false, Duration::ZERO),
            Some("Connected")
        );
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
            appearance: Default::default(),
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
