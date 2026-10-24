//! Stable WebKit storage identity, independent of releases and checkout paths.
use sha2::{Digest, Sha256};

/// Keep the original regular-app store so an upgrade does not discard an
/// existing pairing. Other bundles (including Sprint) get their own fixed UUID.
fn identifier(bundle: &str) -> [u8; 16] {
    if bundle == "com.befeast.okilum" {
        return *b"OkilumManagerWeb";
    }
    let digest = Sha256::digest(format!("okilum.manager.website-data.v1\0{bundle}"));
    let mut bytes: [u8; 16] = digest[..16].try_into().unwrap();
    // RFC 9562 UUIDv8: application-defined, stable payload.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
}

#[cfg(target_os = "macos")]
pub(super) fn application_identifier() -> [u8; 16] {
    let bundle = objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "com.befeast.okilum.development".into());
    identifier(&bundle)
}

#[cfg(target_os = "macos")]
pub(super) fn verify(view: &wry::WebView, expected: &[u8; 16]) -> anyhow::Result<()> {
    use wry::WebViewExtMacOS;
    extern "C" {
        fn okilum_manager_store_matches(view: *mut std::ffi::c_void, uuid: *const u8) -> bool;
    }
    let native = view.webview();
    // SAFETY: a live WKWebView and 16 readable bytes on its owning UI thread.
    let valid = unsafe {
        okilum_manager_store_matches(
            objc2::rc::Retained::as_ptr(&native).cast_mut().cast(),
            expected.as_ptr(),
        )
    };
    anyhow::ensure!(
        valid,
        "Manager requires persistent app-specific browser storage; refusing a temporary session"
    );
    Ok(())
}

/// The T3 session the Manager panel signed in with (#1627): its cookies,
/// read from the panel's persistent store whether or not the panel is open.
/// Blocks for at most `timeout`; never call it on the main thread, which
/// answers it.
#[cfg(target_os = "macos")]
pub(super) struct ManagerCookies;

#[cfg(target_os = "macos")]
impl okilum_t3::SessionSource for ManagerCookies {
    fn cookies(
        &self,
        host: &str,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Vec<okilum_t3::SessionCookie>> {
        use std::ffi::{c_char, c_void, CStr, CString};
        type Sender = std::sync::mpsc::Sender<String>;
        extern "C" {
            fn okilum_manager_store_cookies(
                uuid: *const u8,
                host: *const c_char,
                context: *mut c_void,
                done: extern "C" fn(*mut c_void, *const c_char),
            );
        }
        extern "C" fn done(context: *mut c_void, json: *const c_char) {
            // SAFETY: `context` is the Box leaked below; the bridge calls `done` once.
            let sender = unsafe { Box::from_raw(context.cast::<Sender>()) };
            let text = if json.is_null() {
                "[]".to_owned()
            } else {
                // SAFETY: a NUL-terminated string the bridge keeps alive for this call.
                unsafe { CStr::from_ptr(json) }
                    .to_string_lossy()
                    .into_owned()
            };
            // The waiter may have timed out; then nobody listens.
            let _ = sender.send(text);
        }
        anyhow::ensure!(
            objc2::MainThreadMarker::new().is_none(),
            "the T3 session store is read off the main thread"
        );
        let uuid = application_identifier();
        let host = CString::new(host)?;
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        let context = Box::into_raw(Box::new(sender)).cast::<c_void>();
        // SAFETY: 16 readable bytes and a C string for the duration of the
        // call (the bridge copies both); `context` is consumed by `done`.
        unsafe { okilum_manager_store_cookies(uuid.as_ptr(), host.as_ptr(), context, done) };
        let text = receiver.recv_timeout(timeout).map_err(|_| {
            anyhow::anyhow!(
                "the Manager web view's cookie store did not answer within {} s",
                timeout.as_secs()
            )
        })?;
        #[derive(serde::Deserialize)]
        struct Cookie {
            name: String,
            value: String,
        }
        let cookies: Vec<Cookie> = serde_json::from_str(&text)?;
        Ok(cookies
            .into_iter()
            .map(|cookie| okilum_t3::SessionCookie {
                name: cookie.name,
                value: cookie.value,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn updates_keep_the_legacy_store_and_sprint_is_isolated() {
        assert_eq!(identifier("com.befeast.okilum"), *b"OkilumManagerWeb");
        let sprint = identifier("com.befeast.okilum.sprint");
        assert_eq!(sprint, identifier("com.befeast.okilum.sprint"));
        assert_ne!(sprint, identifier("com.befeast.okilum"));
        assert_ne!(sprint, identifier("com.befeast.okilum.development"));
        assert_eq!(sprint[6] >> 4, 8);
        assert_eq!(sprint[8] >> 6, 2);
    }
}
