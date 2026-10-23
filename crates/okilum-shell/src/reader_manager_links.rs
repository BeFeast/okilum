//! Link and clipboard actions of the T3 page in the Manager panel (#1596).
//!
//! Outside Electron, T3 shows its own DOM menu on a right-clicked link. Its
//! three actions end in browser APIs that a bare WKWebView does not carry out:
//!
//! - "Open in system browser" calls `window.open(url, "_blank")`. WebKit asks
//!   the UI delegate for a new web view and gets none, so nothing opens.
//! - "Copy Link" awaits `navigator.clipboard.writeText` after the menu's own
//!   promise settles; T3 reports that failure silently.
//! - "Open in integrated browser" opens T3's server-side browser in its right
//!   panel, which the narrow Manager panel does not show. Okilum has no
//!   browser of its own, so the link goes to the system browser.
//!
//! The page script hands these to Okilum over `window.ipc`; Okilum opens the
//! URL with the system browser and writes the text to the pasteboard.

/// Message prefix: open the URL that follows in the system browser.
pub(crate) const OPEN_PREFIX: &str = "open-external:";
/// Message prefix: put the text that follows on the pasteboard.
pub(crate) const COPY_PREFIX: &str = "clipboard:";

/// Injected after the focus script. Capture listeners on `window` run before
/// T3's own handlers on the menu buttons.
pub(crate) const PAGE_SCRIPT: &str = r#"
(() => {
    const okilumOpen = url => {
        let parsed;
        try { parsed = new URL(String(url), location.href); } catch { return false; }
        if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return false;
        window.ipc.postMessage('open-external:' + parsed.href);
        return true;
    };
    const openWindow = window.open;
    window.open = function (url, target) {
        const blank = target === undefined || target === null || target === '' || target === '_blank';
        if (blank && url !== undefined && okilumOpen(url)) return null;
        return openWindow.apply(this, arguments);
    };
    const writeText = text => {
        window.ipc.postMessage('clipboard:' + String(text));
        return Promise.resolve();
    };
    if (navigator.clipboard) {
        try {
            Object.defineProperty(navigator.clipboard, 'writeText', { value: writeText, configurable: true });
        } catch {}
    }
    const element = node => node instanceof Element ? node : node?.parentElement;
    let menuLink = null;
    window.addEventListener('contextmenu', event => {
        menuLink = element(event.target)?.closest('a[href]')?.href ?? null;
    }, true);
    window.addEventListener('click', event => {
        const item = element(event.target)?.closest('button');
        if (!item || !menuLink || !item.closest('.dropdown-glass')) return;
        if (item.textContent.trim() !== 'Open in integrated browser') return;
        const link = menuLink;
        let parsed;
        try { parsed = new URL(link); } catch { return; }
        if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return;
        event.preventDefault();
        event.stopImmediatePropagation();
        menuLink = null;
        // An outside press closes T3's menu without choosing an item.
        document.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
        okilumOpen(link);
    }, true);
})();
"#;

/// What the T3 page asked Okilum to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PageRequest {
    OpenExternal(url::Url),
    Copy(String),
}

/// An http(s) URL the system browser may open; anything else is refused.
pub(crate) fn external_url(raw: &str) -> Option<url::Url> {
    url::Url::parse(raw)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

/// Parses an IPC message. Only the T3 page itself may ask: a page reached by
/// navigating inside the panel gets no pasteboard or app launching.
pub(crate) fn page_request(body: &str, page: &str, t3: &url::Url) -> Option<PageRequest> {
    let page = url::Url::parse(page).ok()?;
    if page.origin() != t3.origin() {
        return None;
    }
    if let Some(url) = body.strip_prefix(OPEN_PREFIX) {
        return external_url(url).map(PageRequest::OpenExternal);
    }
    body.strip_prefix(COPY_PREFIX)
        .map(|text| PageRequest::Copy(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn t3() -> url::Url {
        url::Url::parse("https://t3.oklabs.uk/").unwrap()
    }

    #[test]
    fn t3_page_opens_web_links_and_copies_text() {
        let page = "https://t3.oklabs.uk/environment/thread";
        assert_eq!(
            page_request(
                "open-external:https://git.oklabs.uk/BeFeast/okilum/issues/1596",
                page,
                &t3()
            ),
            Some(PageRequest::OpenExternal(
                url::Url::parse("https://git.oklabs.uk/BeFeast/okilum/issues/1596").unwrap()
            ))
        );
        assert_eq!(
            page_request("clipboard:https://example.com/a?b=c", page, &t3()),
            Some(PageRequest::Copy("https://example.com/a?b=c".into()))
        );
        // Focus messages stay with the focus routing.
        assert_eq!(page_request("focus", page, &t3()), None);
    }

    #[test]
    fn other_pages_and_schemes_are_refused() {
        assert_eq!(
            page_request(
                "open-external:https://example.com",
                "https://evil.example/",
                &t3()
            ),
            None
        );
        assert_eq!(
            page_request("clipboard:x", "http://t3.oklabs.uk/", &t3()),
            None,
            "scheme is part of the origin"
        );
        for raw in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "okilum://x",
            "nonsense",
        ] {
            assert_eq!(external_url(raw), None, "{raw}");
            assert_eq!(
                page_request(
                    &format!("open-external:{raw}"),
                    "https://t3.oklabs.uk/",
                    &t3()
                ),
                None
            );
        }
    }

    #[test]
    fn page_script_speaks_the_parser_prefixes_and_handles_all_three_items() {
        assert!(PAGE_SCRIPT.contains(&format!("'{OPEN_PREFIX}'")));
        assert!(PAGE_SCRIPT.contains(&format!("'{COPY_PREFIX}'")));
        // T3's DOM menu (contextMenuFallback.ts) and its labels.
        assert!(PAGE_SCRIPT.contains(".dropdown-glass"));
        assert!(PAGE_SCRIPT.contains("'Open in integrated browser'"));
        assert!(PAGE_SCRIPT.contains("window.open = function"));
        assert!(PAGE_SCRIPT.contains("'writeText'"));
    }
}
