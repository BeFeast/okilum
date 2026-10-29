//! Sign-in uses exactly the Manager panel's persistent, app-specific WebKit store.
use super::*;
use raw_window_handle::HasWindowHandle;
use wry::WebViewBuilderExtDarwin;

#[derive(Default)]
struct Shown(Option<String>);
impl Global for Shown {}

struct SignIn {
    view: Entity<gpui_wry::WebView>,
    _session: Subscription,
    routing: crate::reader_manager_focus::FocusRouting,
}
impl Render for SignIn {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.view.clone())
    }
}

pub(super) fn show(
    url: &str,
    env: &okilum_t3::Environment,
    _: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    if cx.try_global::<Shown>().and_then(|s| s.0.as_deref()) == Some(url) {
        return Ok(());
    }
    let origin = env.origin();
    let url_owned = url.to_owned();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window_state::default_bounds(
            size(px(900.), px(720.)),
            cx,
        ))),
        window_min_size: Some(size(px(600.), px(480.))),
        ..TitleBar::window_options()
    };
    cx.open_window(options, move |window, cx| {
        window.set_window_title("Sign in to T3 — Okilum");
        let store = crate::reader_manager_store::application_identifier();
        let handle = HasWindowHandle::window_handle(window).expect("sign-in window handle");
        let (events, receive) = async_channel::unbounded::<String>();
        let page_events = events.clone();
        let browser =
            (|| -> anyhow::Result<(wry::WebView, crate::reader_manager_focus::FocusRouting)> {
                let browser = wry::WebViewBuilder::new()
                    .with_incognito(false)
                    .with_data_store_identifier(store)
                    .with_url(&url_owned)
                    .with_focused(true)
                    .with_on_page_load_handler(move |event, _| {
                        if matches!(event, wry::PageLoadEvent::Finished) {
                            let _ = page_events.try_send("loaded".into());
                        }
                    })
                    .build_as_child(&handle)?;
                crate::reader_manager_store::verify(&browser, &store)?;
                let routing =
                    crate::reader_manager_focus::FocusRouting::new(&browser, window, events)?;
                Ok((browser, routing))
            })();
        // Keep failures in the ordinary Settings error path rather than using
        // a temporary store which would require a second pairing.
        let content = match browser {
            Ok((browser, routing)) => {
                let view = cx.new(|cx| gpui_wry::WebView::new(browser, window, cx));
                let sign_in = cx.new(|cx: &mut Context<SignIn>| {
                    let session = window.observe_global::<auth::Sessions>(cx, move |window, cx| {
                        let entry = auth::entry(&origin, cx);
                        if !entry.busy && entry.pending.is_none() {
                            window.remove_window();
                        }
                    });
                    cx.spawn_in(window, async move |this, cx| {
                        while let Ok(event) = receive.recv().await {
                            if this
                                .update_in(cx, |this, window, cx| {
                                    this.routing.refresh();
                                    if event == "loaded" {
                                        this.routing.document_loaded();
                                    }
                                    if event == "native-focus" && this.routing.focused() {
                                        window.blur(cx);
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                    SignIn {
                        view,
                        _session: session,
                        routing,
                    }
                });
                BrowserContent {
                    view: Some(sign_in),
                    error: None,
                }
            }
            Err(e) => BrowserContent {
                view: None,
                error: Some(format!(
                    "Could not open T3 sign-in: {e}. Return to Settings and use Open in browser."
                )),
            },
        };
        let content = cx.new(|_| content);
        cx.new(|cx| Root::new(content, window, cx))
    })?;
    cx.set_global(Shown(Some(url.to_owned())));
    Ok(())
}

struct BrowserContent {
    view: Option<Entity<SignIn>>,
    error: Option<String>,
}
impl Render for BrowserContent {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .children(self.view.clone())
            .children(self.error.clone())
    }
}
