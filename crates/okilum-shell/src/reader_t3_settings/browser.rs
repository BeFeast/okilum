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
        let browser = (|| -> anyhow::Result<wry::WebView> {
            let browser = wry::WebViewBuilder::new()
                .with_incognito(false)
                .with_data_store_identifier(store)
                .with_url(&url_owned)
                .with_focused(true)
                .build_as_child(&handle)?;
            crate::reader_manager_store::verify(&browser, &store)?;
            Ok(browser)
        })();
        // Keep failures in the ordinary Settings error path rather than using
        // a temporary store which would require a second pairing.
        let content = match browser {
            Ok(browser) => {
                let view = cx.new(|cx| gpui_wry::WebView::new(browser, window, cx));
                let sign_in = cx.new(|cx| {
                    let session = window.observe_global::<auth::Sessions>(cx, move |window, cx| {
                        let entry = auth::entry(&origin, cx);
                        if !entry.busy && entry.pending.is_none() {
                            window.remove_window();
                        }
                    });
                    SignIn {
                        view,
                        _session: session,
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
        Root::new(cx.new(|_| content), window, cx)
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
