//! Check the actual embedded engine before importing any T3 modules (#2381).

// Only the native macOS panel loads the preflight page.
#[cfg_attr(any(not(target_os = "macos"), test), allow(dead_code))]
pub(crate) const PREFLIGHT: &str = include_str!("reader_manager_compatibility.html");

/// Only the local preflight document can report capabilities. Once consumed,
/// the panel ignores further reports, including from navigated pages.
pub(crate) fn report(body: &str, page: &str) -> Option<Result<(), String>> {
    if page != "about:blank" {
        return None;
    }
    match body.strip_prefix("webkit-capability:")? {
        "supported" => Some(Ok(())),
        detail if detail.starts_with("unsupported:") => Some(Err(detail.to_owned())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn only_the_local_preflight_can_report_a_capability_result() {
        assert_eq!(
            report("webkit-capability:supported", "about:blank"),
            Some(Ok(()))
        );
        assert_eq!(
            report("webkit-capability:unsupported:lookbehind", "about:blank"),
            Some(Err("unsupported:lookbehind".into()))
        );
        for page in [
            "https://t3.example/env/thread",
            "https://login.example",
            "about:srcdoc",
        ] {
            assert_eq!(
                report("webkit-capability:unsupported:lookbehind", page),
                None
            );
            assert_eq!(report("webkit-capability:supported", page), None);
        }
        assert_eq!(report("focus", "about:blank"), None);
        assert_eq!(report("webkit-capability:unknown", "about:blank"), None);
    }
}
