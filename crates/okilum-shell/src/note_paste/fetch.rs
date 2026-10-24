//! The one network request a paste may make (#1611): a page title for Paste
//! as Titled Link. Blocking; run on the background executor only.

use super::title::TITLE_READ_LIMIT;
use okilum_paste::extract_title;
use std::io::Read;
use std::time::Duration;

/// Whole request, connect to last byte.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The title of the HTML page at `url`, or `None` (not HTML, an error status,
/// a timeout, no title). Reads at most the first 256 KB.
pub fn page_title(url: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("Okilum/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if content_type
        .as_deref()
        .is_some_and(|ct| !ct.contains("html"))
    {
        return None;
    }
    let mut bytes = Vec::with_capacity(16 * 1024);
    // A body cut off by the timeout may still hold the title.
    let _ = response
        .take(TITLE_READ_LIMIT as u64)
        .read_to_end(&mut bytes);
    extract_title(&bytes, content_type.as_deref())
}
