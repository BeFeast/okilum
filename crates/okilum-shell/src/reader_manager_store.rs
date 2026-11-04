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
