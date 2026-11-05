//! Manager surface, mounted independently of the note outline.
use super::*;
use okilum_t3::projects::{Manager, Project};

#[cfg(any(target_os = "macos", test))]
#[path = "reader_manager_watchdog.rs"]
mod watchdog;

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

const UNSUPPORTED_WEBKIT: &str = "This Mac’s embedded browser cannot run this version of T3. Open the Manager thread in the T3 desktop app or an up to date system browser.";

struct ManagerLinks {
    browser: url::Url,
    desktop: String,
}

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
    links: Option<ManagerLinks>,
    unsupported_webkit: bool,
    webview_unavailable: bool,
    #[cfg(any(target_os = "macos", test))]
    watchdog: watchdog::Watchdog,
    #[cfg(target_os = "macos")]
    occlusion_hidden: bool,
    #[cfg(any(target_os = "macos", test))]
    checking_webkit: bool,
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
            links: None,
            unsupported_webkit: false,
            webview_unavailable: false,
            #[cfg(any(target_os = "macos", test))]
            watchdog: watchdog::Watchdog::default(),
            #[cfg(target_os = "macos")]
            occlusion_hidden: false,
            #[cfg(any(target_os = "macos", test))]
            checking_webkit: false,
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
        #[cfg(any(target_os = "macos", test))]
        {
            self.watchdog = watchdog::Watchdog::default();
        }
        #[cfg(target_os = "macos")]
        self.focus_routing.take();
        #[cfg(target_os = "macos")]
        if let Some(view) = self.webview.take() {
            view.update(cx, |view, _| view.hide());
        }
        let _ = cx;
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
        if self.unsupported_webkit {
            return (Some("Embedded T3 is unavailable on this Mac".into()), None);
        }
        if self.webview_unavailable {
            return (Some("T3 unavailable — reconnecting…".into()), None);
        }
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
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        self.hide(cx);
        self.closed = false;
        self.loading = false;
        self.failures = 0;
        self.error = None;
        self.retrying = false;
        self.links = None;
        self.unsupported_webkit = false;
        self.webview_unavailable = false;
        #[cfg(any(target_os = "macos", test))]
        {
            self.checking_webkit = false;
        }
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

    /// A capability failure cannot heal through network retries. Keep details
    /// in diagnostics and release the native child so it cannot cover the UI.
    #[cfg(any(target_os = "macos", test))]
    fn finish_webkit_check(
        &mut self,
        generation: u64,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generation || self.closed || !self.checking_webkit {
            return false;
        }
        self.checking_webkit = false;
        if let Some(trace) = reader_diagnostics::trace(cx) {
            trace.event(
                "manager_webkit_capability",
                serde_json::json!({
                    "generation": generation, "supported": result.is_ok(),
                }),
            );
        }
        if let Err(detail) = result {
            self.log_failure(
                &format!("Embedded WebKit lacks T3 regex support: {detail}"),
                cx,
            );
            self.unsupported_webkit = true;
            self.loading = false;
            self.retrying = false;
            self.error = None;
            self.focused = false;
            self.watchdog = watchdog::Watchdog::default();
            #[cfg(target_os = "macos")]
            {
                self.focus_routing.take();
                if let Some(view) = self.webview.take() {
                    view.update(cx, |view, _| view.hide());
                }
            }
        }
        cx.notify();
        true
    }

    #[cfg(any(target_os = "macos", test))]
    fn watch_browser(
        &mut self,
        stage: watchdog::Stage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let generation = self.generation;
        let now = std::time::Instant::now();
        let delay = match stage {
            watchdog::Stage::Capability => {
                self.watchdog.capability(generation, now);
                watchdog::CAPABILITY_TIMEOUT
            }
            watchdog::Stage::Page => {
                if !self.watchdog.page(generation, now) {
                    return;
                }
                watchdog::PAGE_TIMEOUT
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation || this.closed {
                    return;
                }
                if let Some(stage) = this.watchdog.expire(generation, std::time::Instant::now()) {
                    let reason = match stage {
                        watchdog::Stage::Capability => "capability report timed out",
                        watchdog::Stage::Page => "page load timed out",
                    };
                    this.browser_failed(reason, window, cx);
                }
            });
        })
        .detach();
    }

    /// Release the native child before painting fallback, invalidate queued
    /// callbacks, and reuse the connection backoff without resetting failures.
    #[cfg(any(target_os = "macos", test))]
    fn browser_failed(&mut self, reason: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(trace) = reader_diagnostics::trace(cx) {
            trace.event("manager_webview_failure", serde_json::json!({
                "generation": self.generation, "reason": reason,
                "url": self.links.as_ref().map(|l| watchdog::diagnostic_url(l.browser.as_str())),
            }));
        }
        self.hide(cx);
        self.closed = false;
        self.loading = false;
        self.checking_webkit = false;
        self.webview_unavailable = true;
        self.failed(format!("Embedded T3: {reason}"), window, cx);
        cx.notify();
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
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        if self.loading || self.unsupported_webkit {
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
                    Ok::<_, anyhow::Error>(ManagerLinks {
                        browser: url,
                        desktop: identity.thread_url(&thread),
                    })
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(links) => {
                        #[cfg(target_os = "macos")]
                        match Self::browser(&links.browser, generation, window, cx) {
                            Ok((view, routing)) => {
                                this.checking_webkit = true;
                                this.focus_routing = Some(routing);
                                this.webview = Some(view);
                                this.error = None;
                                this.retrying = false;
                                this.webview_unavailable = false;
                                this.occlusion_hidden = false;
                                this.watch_browser(watchdog::Stage::Capability, window, cx);
                            }
                            Err(e) => {
                                let error = format!("Manager could not be opened: {e:#}");
                                this.failed(error, window, cx);
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            let _ = window;
                            this.error = None;
                            this.retrying = false;
                            this.failures = 0;
                        }
                        this.links = Some(links);
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
                        if this.generation != generation || this.closed {
                            return;
                        }
                        if let Some(result) =
                            crate::reader_manager_compatibility::report(&event, "about:blank")
                        {
                            let supported = result.is_ok();
                            if this.finish_webkit_check(generation, result, cx) && supported {
                                this.watch_browser(watchdog::Stage::Page, window, cx);
                                if let (Some(view), Some(links)) = (&this.webview, &this.links) {
                                    if view
                                        .read(cx)
                                        .handle()
                                        .raw()
                                        .load_url(links.browser.as_str())
                                        .is_err()
                                    {
                                        if let Some(trace) = reader_diagnostics::trace(cx) {
                                            // Wry errors can embed the original URL. Keep
                                            // those out of the diagnostic log as well.
                                            trace.event("manager_navigation_failure", serde_json::json!({
                                                "generation": generation,
                                                "url": watchdog::diagnostic_url(links.browser.as_str()),
                                                "reason": "load_url rejected",
                                            }));
                                        }
                                        this.browser_failed("navigation could not start", window, cx);
                                    }
                                }
                            }
                            return;
                        }
                        if let Some(event) = event.strip_prefix("page-load:") {
                            let Some((phase, url)) = event.split_once(':') else { return; };
                            if let Some(trace) = reader_diagnostics::trace(cx) {
                                trace.event("manager_page_load", serde_json::json!({
                                    "generation": generation, "phase": phase,
                                    "url": watchdog::diagnostic_url(url),
                                }));
                            }
                            if phase == "started" && !this.checking_webkit && url != "about:blank" {
                                this.watch_browser(watchdog::Stage::Page, window, cx);
                            }
                            if phase == "finished" {
                                if this.watchdog.finished(generation, url) {
                                    this.failures = 0;
                                    cx.notify();
                                }
                                if let Some(routing) = &this.focus_routing {
                                    routing.document_loaded();
                                }
                            }
                            return;
                        }
                        if let Some(detail) = event.strip_prefix("navigation-failed:") {
                            if let Ok(mut detail) = serde_json::from_str::<serde_json::Value>(detail) {
                                let url = watchdog::diagnostic_url(detail["url"].as_str().unwrap_or_default());
                                detail["url"] = url.into();
                                detail["generation"] = generation.into();
                                if let Some(trace) = reader_diagnostics::trace(cx) {
                                    trace.event("manager_navigation_failure", detail.clone());
                                }
                                // Cancellation is normal when navigation is superseded.
                                if detail["domain"] != "NSURLErrorDomain" || detail["code"] != -999 {
                                    this.browser_failed("page could not be loaded", window, cx);
                                }
                            }
                            return;
                        }
                        if let Some(url) = event.strip_prefix("navigation-blocked:") {
                            if let Some(trace) = reader_diagnostics::trace(cx) {
                                trace.event("manager_navigation_failure", serde_json::json!({
                                    "generation": generation, "reason": "navigation blocked",
                                    "url": watchdog::diagnostic_url(url),
                                }));
                            }
                            return;
                        }
                        let owns_native_focus =
                            this.focus_routing.as_ref().is_some_and(|routing| {
                                routing.refresh();
                                routing.focused()
                            });
                        if event == "open-terminal" && owns_native_focus && !this.closed {
                            crate::project_terminal::open(&this.project, window, cx);
                            return;
                        }
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
        let navigation_events = send.clone();
        let browser = wry::WebViewBuilder::new()
            .with_incognito(false)
            .with_data_store_identifier(store)
            .with_html(crate::reader_manager_compatibility::PREFLIGHT)
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
                if crate::reader_manager_compatibility::report(
                    request.body(),
                    &request.uri().to_string(),
                ).is_some() || matches!(request.body().as_str(), "focus" | "blur" | "escape") {
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
            .with_on_page_load_handler(move |event, url| {
                let event = match event {
                    wry::PageLoadEvent::Started => "page-load:started",
                    wry::PageLoadEvent::Finished => "page-load:finished",
                };
                let _ = page_events.try_send(format!("{event}:{url}"));
            })
            .with_navigation_handler(move |url| {
                let allowed = url == "about:blank" || url::Url::parse(&url).is_ok_and(|url| {
                    url.username().is_empty()
                        && url.password().is_none()
                        && okilum_t3::Environment::new(
                            "navigation",
                            &url.origin().ascii_serialization(),
                        )
                        .is_ok()
                });
                if !allowed {
                    let _ = navigation_events.try_send(format!("navigation-blocked:{url}"));
                }
                allowed
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
            return v_flex()
                .p_2()
                .gap_2()
                .child("Manager не запущен")
                .child(t3_settings_button())
                .into_any_element();
        }
        if self.unsupported_webkit || self.webview_unavailable {
            return v_flex()
                .id("manager-unsupported-webkit")
                .p_3()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if self.unsupported_webkit {
                    UNSUPPORTED_WEBKIT
                } else {
                    "T3 unavailable. Trying to reconnect. You can open the Manager in T3 or your browser."
                })
                .when_some(self.links.as_ref(), |panel, links| {
                    let desktop = links.desktop.clone();
                    let browser = links.browser.to_string();
                    panel
                        .child(
                            Button::new("manager-fallback-desktop")
                                .small()
                                .label("Open in T3")
                                .on_click(move |_, _, cx| cx.open_url(&desktop)),
                        )
                        .child(
                            Button::new("manager-fallback-browser")
                                .small()
                                .label("Open in browser")
                                .on_click(move |_, _, cx| cx.open_url(&browser)),
                        )
                })
                .child(
                    Button::new("manager-fallback-retry")
                        .small()
                        .label("Retry")
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.unsupported_webkit = false;
                            this.connect(window, cx);
                        })),
                )
                .into_any_element();
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
                            // A transient layout frame says nothing about overlay
                            // coverage. Hiding here can leave a white native pane.
                            if full.size.width <= px(0.) || full.size.height <= px(0.) {
                                return;
                            }
                            let overlays = gpui_base::GlobalState::native_overlays(window, cx).to_vec();
                            let clear =
                                crate::reader_manager_occlusion::uncovered_bounds(full, &overlays);
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
                                if result.is_err() {
                                    let _ = panel.update(cx, |panel, cx| {
                                        if !panel.occlusion_hidden {
                                            if let Some(trace) = reader_diagnostics::trace(cx) {
                                                trace.event("manager_webview_occlusion", serde_json::json!({
                                                    "hidden": true, "reason": "set_bounds failed",
                                                    "generation": panel.generation,
                                                }));
                                            }
                                        }
                                        panel.occlusion_hidden = true;
                                        panel.browser_failed("view could not be displayed", window, cx);
                                    });
                                    return;
                                }
                                let _ = native.raw().set_visible(true);
                            } else {
                                // A full-window modal backdrop leaves no free
                                // rectangle. Do not let its native child cover it.
                                let _ = native.raw().set_visible(false);
                            }
                            let _ = panel.update(cx, |panel, cx| {
                                let hidden = clear.is_none();
                                if panel.occlusion_hidden != hidden {
                                    if let Some(trace) = reader_diagnostics::trace(cx) {
                                        trace.event("manager_webview_occlusion", serde_json::json!({
                                            "hidden": hidden, "reason": if hidden { "fully covered by overlays" } else { "overlay cleared" },
                                            "generation": panel.generation,
                                            "bounds": { "x": f32::from(full.origin.x), "y": f32::from(full.origin.y),
                                                "width": f32::from(full.size.width), "height": f32::from(full.size.height) },
                                            "overlays": overlays.iter().map(|b| serde_json::json!({
                                                "x": f32::from(b.origin.x), "y": f32::from(b.origin.y),
                                                "width": f32::from(b.size.width), "height": f32::from(b.size.height),
                                            })).collect::<Vec<_>>(),
                                        }));
                                    }
                                    panel.occlusion_hidden = hidden;
                                }
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
        let line = line.or_else(|| (!self.loading).then(|| "T3 not connected".into()));
        v_flex()
            .gap_2()
            .id("manager-t3-status")
            .px_3()
            .py_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .children(line)
            .child(t3_settings_button())
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

    #[gpui::test]
    fn webkit_failure_preserves_thread_actions_without_retries_or_raw_ui_errors(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let project = Project {
            folder: temp.path().into(),
            name: "Compatibility fixture".into(),
            kind: okilum_t3::projects::Kind::Knowledge,
            environment: Some("local-alias".into()),
            manager_project: None,
            manager: Manager::Thread("mcp:manager".into()),
            appearance: Default::default(),
        };
        let (root, visual) = cx.add_window_view(|window, cx| {
            let panel = cx
                .new(|cx| ManagerPanel::new(project, Some(temp.path().join("state")), window, cx));
            Root::new(panel, window, cx)
        });
        visual.run_until_parked();
        let panel = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ManagerPanel>().unwrap()
        });
        panel.update_in(visual, |panel, window, cx| {
            let environment =
                okilum_t3::Environment::new("local-alias", "https://t3.example:8443").unwrap();
            let identity = okilum_t3::ServerIdentity {
                environment_id: "actual-server".into(),
                label: "Fixture".into(),
            };
            panel.links = Some(ManagerLinks {
                browser: okilum_t3::projects::manager_url(&environment, &identity, "mcp:manager")
                    .unwrap(),
                desktop: identity.thread_url("mcp:manager"),
            });
            panel.generation = 2;
            panel.checking_webkit = true;
            // The engine that passes the check remains eligible for the normal page.
            assert!(panel.finish_webkit_check(2, Ok(()), cx));
            assert!(!panel.unsupported_webkit);
            assert!(!panel.finish_webkit_check(2, Err("duplicate report".into()), cx));
            panel.checking_webkit = true;
            assert!(!panel.finish_webkit_check(1, Err("old browser".into()), cx));
            assert!(panel.checking_webkit);
            panel.error = Some("previous network failure".into());
            panel.retrying = true;
            panel.loading = true;
            panel.focused = true;
            assert!(panel.finish_webkit_check(
                2,
                Err("SyntaxError: invalid group specifier name".into()),
                cx
            ));
            assert!(panel.unsupported_webkit);
            assert!(!panel.loading && !panel.retrying && !panel.focused);
            assert!(panel.error.is_none());
            assert_eq!(
                panel.connection_status(),
                (Some("Embedded T3 is unavailable on this Mac".into()), None)
            );
            let links = panel.links.as_ref().unwrap();
            assert_eq!(links.desktop, "t3code://threads/actual-server/mcp:manager");
            assert_eq!(
                links.browser.as_str(),
                "https://t3.example:8443/actual-server/mcp:manager"
            );
            panel.retry_now(window, cx);
            panel.connect(window, cx);
            assert_eq!(
                panel.generation, 2,
                "activation and retries must not reload an incompatible bundle"
            );
            panel.hide(cx);
            panel.checking_webkit = true;
            assert!(
                !panel.finish_webkit_check(panel.generation, Ok(()), cx),
                "a closed panel ignores callbacks"
            );
            // A timeout is transient, unlike a definitive unsupported engine.
            panel.closed = false;
            panel.unsupported_webkit = false;
            panel.watch_browser(watchdog::Stage::Capability, window, cx);
            let expired_generation = panel.generation;
            panel.browser_failed("capability report timed out", window, cx);
            assert!(panel.webview_unavailable);
            assert!(!panel.closed && !panel.loading && !panel.checking_webkit);
            assert!(panel.retrying);
            assert_eq!(panel.failures, 1);
            assert!(panel.links.is_some(), "fallback retains both open actions");
            assert_eq!(
                panel.connection_status(),
                (Some("T3 unavailable — reconnecting…".into()), None)
            );
            assert!(!panel.finish_webkit_check(expired_generation, Ok(()), cx));
            panel.watch_browser(watchdog::Stage::Page, window, cx);
            panel.browser_failed("page load timed out", window, cx);
            assert_eq!(
                panel.failures, 2,
                "creating a view is not a successful load"
            );
            panel.hide(cx);
        });
    }

    #[gpui::test]
    fn shared_connection_refreshes_only_panels_without_project_overrides(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let project = Project {
            folder: temp.path().into(),
            name: "Fixture".into(),
            kind: okilum_t3::projects::Kind::Knowledge,
            environment: Some("fixture".into()),
            manager_project: None,
            // No network path: this fixture has no thread to connect.
            manager: Manager::NotStarted,
            appearance: Default::default(),
        };
        let (root, visual) = cx.add_window_view(|window, cx| {
            let panel = cx
                .new(|cx| ManagerPanel::new(project, Some(temp.path().join("state")), window, cx));
            Root::new(panel, window, cx)
        });
        visual.run_until_parked();
        let panel = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ManagerPanel>().unwrap()
        });
        panel.read_with(visual, |panel, _| assert!(!panel.project_override));
        visual.update(|_, cx| {
            cx.set_global(reader_t3_settings::Connection {
                origin: Some("https://shared.example".into()),
            })
        });
        visual.run_until_parked();
        panel.update_in(visual, |panel, _, cx| {
            assert_eq!(
                panel.origin.read(cx).value().as_str(),
                "https://shared.example"
            );
            panel.project_override = true;
        });
        visual.update(|_, cx| {
            cx.set_global(reader_t3_settings::Connection {
                origin: Some("https://changed.example".into()),
            })
        });
        visual.run_until_parked();
        panel.read_with(visual, |panel, cx| {
            assert_eq!(
                panel.origin.read(cx).value().as_str(),
                "https://shared.example"
            )
        });
    }

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
        cx.update(crate::test_init);
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
