//! Explicit embedded application shell. No filesystem paths or API data can be
//! served through this router, and the service worker caches only these assets.
use axum::{http::header, routing::get, Router};

pub(crate) fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    macro_rules! asset {
        ($router:expr, $route:literal, $mime:literal, $file:literal) => {
            $router.route(
                $route,
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, $mime)],
                        include_str!(concat!("../../../../web/inbox/", $file)),
                    )
                }),
            )
        };
    }
    let router = Router::new();
    let router = asset!(router, "/", "text/html; charset=utf-8", "index.html");
    let router = asset!(
        router,
        "/app.js",
        "text/javascript; charset=utf-8",
        "app.js"
    );
    let router = asset!(
        router,
        "/outbox.js",
        "text/javascript; charset=utf-8",
        "outbox.js"
    );
    let router = asset!(
        router,
        "/webauthn.js",
        "text/javascript; charset=utf-8",
        "webauthn.js"
    );
    let router = asset!(
        router,
        "/publication-form.js",
        "text/javascript; charset=utf-8",
        "publication-form.js"
    );
    let router = asset!(
        router,
        "/questions.js",
        "text/javascript; charset=utf-8",
        "questions.js"
    );
    let router = asset!(
        router,
        "/questions-view.js",
        "text/javascript; charset=utf-8",
        "questions-view.js"
    );
    let router = asset!(router, "/sw.js", "text/javascript; charset=utf-8", "sw.js");
    let router = asset!(router, "/style.css", "text/css; charset=utf-8", "style.css");
    let router = asset!(router, "/icon.svg", "image/svg+xml", "icon.svg");
    asset!(
        router,
        "/manifest.webmanifest",
        "application/manifest+json",
        "manifest.webmanifest"
    )
}
