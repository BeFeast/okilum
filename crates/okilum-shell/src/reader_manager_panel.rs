//! Manager surface, mounted independently of the note outline.
use super::*;
use okilum_t3::projects::{Manager, Project};

pub(super) struct ManagerPanel {
    project: Project,
    state: Option<PathBuf>,
    pub(super) focused: bool,
    origin: Entity<InputState>,
    error: Option<String>,
    transport: Option<String>,
    origin_edited: bool,
    _origin_events: Subscription,
    loading: bool,
    generation: u64,
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
        let alias = project
            .environment
            .clone()
            .unwrap_or_else(|| "project".into());
        let area = project.folder.clone();
        let saved_state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(
                    async move { project::prefill_browser_origin(&area, &alias, state.as_deref()) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // A delayed disk read must never replace typing or a manual connection.
                if this.origin_edited || this.generation != 0 {
                    return;
                }
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
            origin,
            error: None,
            transport: None,
            origin_edited: false,
            _origin_events: origin_events,
            loading: false,
            generation: 0,
            #[cfg(target_os = "macos")]
            webview: None,
        }
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
        if !visible && self.focused {
            self.focused = false;
            cx.notify();
        }
    }

    pub(super) fn hide(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.focused = false;
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
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let identity = okilum_t3::projects::browser_identity(&env)?;
                    let url = okilum_t3::projects::manager_url(&env, &identity, &thread)?;
                    if let Some(state) = &state {
                        project::save_browser_environment(state, &env)?;
                    }
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
                        match Self::browser(&url, window, cx) {
                            Ok(view) => this.webview = Some(view),
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Entity<gpui_wry::WebView>> {
        use raw_window_handle::HasWindowHandle;
        use wry::WebViewBuilderExtDarwin;
        let handle = HasWindowHandle::window_handle(window)?;
        // Stable application-specific WKWebsiteDataStore. Cookies never enter
        // Okilum settings or the MCP keychain; login happens in this view.
        let (send, receive) = async_channel::unbounded::<String>();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = receive.recv().await {
                if this
                    .update_in(cx, |this, _window, cx| {
                        this.focused = event == "focus";
                        if event == "escape" {
                            if let Some(view) = &this.webview {
                                let _ = view.read(cx).raw().focus_parent();
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
        Ok(cx.new(|cx| gpui_wry::WebView::new(browser, window, cx)))
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
                        .small()
                        .h(px(28.))
                        .rounded(px(brand::CONTROL_RADIUS))
                        .icon(IconName::ArrowRight)
                        .tooltip("Connect to T3 (Enter)")
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                ),
        );
        if let Some(transport) = &self.transport {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(brand::palette(cx).text_muted)
                    .child(transport.clone()),
            );
        }
        if self.loading {
            body = body.child("Connecting to T3…");
        }
        if let Some(error) = &self.error {
            body = body.child(div().text_color(cx.theme().danger).child(error.clone()));
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
            .is_some_and(|panel| panel.read(cx).focused)
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
mod tests {
    use super::*;

    #[gpui::test]
    fn enter_submits_origin_validation_and_editing_clears_the_error(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let temp = tempfile::tempdir().unwrap();
        let area = temp.path().join("area");
        std::fs::create_dir_all(area.join("ops")).unwrap();
        std::fs::write(area.join("ops/project.yaml"), "t3: {environment: test}").unwrap();
        let project = Project {
            folder: area,
            name: "Test".into(),
            kind: okilum_t3::projects::Kind::Knowledge,
            environment: Some("test".into()),
            manager_project: Some("brain".into()),
            manager: Manager::Thread("mcp:test".into()),
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
            panel.origin.update(cx, |input, cx| {
                input.set_value("not a host", window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
        });
        visual.run_until_parked();
        assert!(panel.read_with(visual, |panel, _| panel.error.is_none()));
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        panel.read_with(visual, |panel, _| {
            assert!(panel
                .error
                .as_deref()
                .unwrap()
                .contains("Enter a T3 address"));
            assert!(
                !panel.loading,
                "invalid input never starts a network request"
            );
            assert_eq!(panel.generation, 0);
        });
        panel.update_in(visual, |panel, window, cx| {
            panel
                .origin
                .update(cx, |input, cx| input.set_value("t3.oklabs.uk", window, cx));
        });
        visual.run_until_parked();
        assert!(panel.read_with(visual, |panel, _| panel.error.is_none()));
    }
}
