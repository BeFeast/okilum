//! Optional upload of pasted and dropped files (#1073).
//!
//! Off by default (Settings → Editor). When it is on, a file that a paste or
//! drop saved into the vault's attachment folder is uploaded afterwards, and
//! once the upload is confirmed the note's link to it is rewritten to the
//! uploaded copy. The local file stays where it is.
//!
//! The first provider is pomoi (`pomoi.co`): every upload is private and never
//! expires, so a link neither rots nor opens for someone who only has the URL.
//! Okilum shows such images itself with the saved token ([`image_source`]);
//! other apps get pomoi's sign-in answer (401). The token lives in the OS
//! credential store, never in the vault or the settings file.
use gpui::{
    App, Asset, Global, Image, ImageCacheError, ImageFormat, ImageSource, RenderImage, Window,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// An upload service. The note only ever sees the URL it answers with.
pub(crate) trait Provider: Send + Sync {
    /// Shown in Settings and messages.
    fn name(&self) -> &'static str;
    /// The credential store entry that holds this provider's token.
    fn credential_key(&self) -> &'static str;
    /// Whether `url` is a file this provider stores, so that Okilum loads it
    /// with the token instead of anonymously.
    fn hosts(&self, url: &str) -> bool;
    /// Upload one file. Blocking; call it off the UI thread.
    fn upload(&self, path: &Path, token: &str) -> Result<Uploaded, UploadError>;
    /// Download a stored file with the token. Blocking.
    fn fetch(&self, url: &str, token: &str) -> Result<Fetched, UploadError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Uploaded {
    /// The direct file URL (never a short link: an image embed needs the
    /// file itself, and Okilum's client follows no redirects).
    pub url: String,
}

pub(crate) struct Fetched {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UploadError {
    NoToken,
    Unauthorized,
    TooLarge,
    Read(String),
    Status(u16),
    Network(String),
    Response,
}

impl UploadError {
    pub(crate) fn user_text(&self, provider: &str) -> String {
        match self {
            Self::NoToken => format!(
                "Upload to {provider} is on, but no token is saved. Add one in Settings → Editor."
            ),
            Self::Unauthorized => {
                format!("{provider} refused the saved token. Replace it in Settings → Editor.")
            }
            Self::TooLarge => format!("The file is larger than {provider} accepts."),
            Self::Read(reason) => format!("The file couldn’t be read for upload: {reason}"),
            Self::Status(status) => format!("{provider} returned HTTP {status}."),
            Self::Network(reason) => format!("{provider} couldn’t be reached: {reason}"),
            Self::Response => format!("{provider} sent an answer Okilum doesn’t understand."),
        }
    }
}

pub(crate) struct Pomoi;

/// Distinct from anything a browser saves for the site itself, so a password
/// saved by Safari for pomoi.co is never read as this token.
const POMOI_CREDENTIAL_KEY: &str = "https://pomoi.co/okilum-upload-token";
const POMOI_ORIGIN: &str = "https://pomoi.co/";
const POMOI_FILES: &str = "https://pomoi.co/f/";
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, UploadError> {
    okilum_http::client(POMOI_ORIGIN, env!("OKILUM_RELEASE_VERSION"), timeout)
        .map_err(|error| UploadError::Network(okilum_http::reason(&error)))
}

fn network(error: reqwest::Error) -> UploadError {
    UploadError::Network(okilum_http::reason(&anyhow::Error::from(error)))
}

fn status_error(status: u16) -> UploadError {
    match status {
        401 | 403 => UploadError::Unauthorized,
        413 => UploadError::TooLarge,
        status => UploadError::Status(status),
    }
}

impl Provider for Pomoi {
    fn name(&self) -> &'static str {
        "pomoi.co"
    }

    fn credential_key(&self) -> &'static str {
        POMOI_CREDENTIAL_KEY
    }

    fn hosts(&self, url: &str) -> bool {
        url.starts_with(POMOI_FILES) && url.len() > POMOI_FILES.len()
    }

    fn upload(&self, path: &Path, token: &str) -> Result<Uploaded, UploadError> {
        if token.is_empty() {
            return Err(UploadError::NoToken);
        }
        let form = reqwest::blocking::multipart::Form::new()
            .file("file", path)
            .map_err(|error| UploadError::Read(error.kind().to_string()))?;
        let response = client(UPLOAD_TIMEOUT)?
            .post(POMOI_ORIGIN)
            .bearer_auth(token)
            // Notes keep their links: never the 72-hour default, never public.
            .header("expire", "never")
            .header("private", "true")
            .multipart(form)
            .send()
            .map_err(network)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(status_error(status));
        }
        let body = response.bytes().map_err(|_| UploadError::Response)?;
        let body: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| UploadError::Response)?;
        parse_upload(self, &body)
    }

    fn fetch(&self, url: &str, token: &str) -> Result<Fetched, UploadError> {
        if !self.hosts(url) {
            return Err(UploadError::Response);
        }
        if token.is_empty() {
            return Err(UploadError::NoToken);
        }
        let response = client(FETCH_TIMEOUT)?
            .get(url)
            .bearer_auth(token)
            .send()
            .map_err(network)?;
        if !response.status().is_success() {
            return Err(status_error(response.status().as_u16()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned());
        let bytes = response.bytes().map_err(network)?;
        Ok(Fetched {
            bytes: bytes.to_vec(),
            content_type,
        })
    }
}

/// The upload answer: `{"url": "https://pomoi.co/f/<id>/<name>", "private":
/// true, …}`. A URL the provider would not load back, or an upload that did
/// not stay private, is refused rather than linked.
fn parse_upload(
    provider: &dyn Provider,
    body: &serde_json::Value,
) -> Result<Uploaded, UploadError> {
    let url = body
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| provider.hosts(url) && !url.contains(char::is_whitespace))
        .ok_or(UploadError::Response)?;
    if body.get("private").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(UploadError::Response);
    }
    Ok(Uploaded {
        url: url.to_owned(),
    })
}

/// The provider in use. One today; the interface keeps the note side
/// independent of it.
pub(crate) fn provider() -> &'static dyn Provider {
    &Pomoi
}

/// The links a paste inserts for a saved vault file, exactly as
/// [`crate::note_paste::files::wikilink`] writes them: by name, or by
/// vault-relative path when the name is ambiguous. A pasted note (`.md`) is
/// a note link, not an attachment, and is never uploaded.
pub(crate) fn local_links(vault_root: &Path, path: &Path) -> Vec<String> {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    if name.to_ascii_lowercase().ends_with(".md") {
        return Vec::new();
    }
    let mut links = vec![crate::note_paste::files::wikilink(&name)];
    if let Ok(relative) = path.strip_prefix(vault_root) {
        let relative = okilum_core::vault::note_path(relative);
        if relative != name {
            links.push(crate::note_paste::files::wikilink(&relative));
        }
    }
    links
}

/// The Markdown link to an uploaded copy: an image stays an image
/// (`![name](url)`), anything else becomes a link (`[name](url)`), since
/// Okilum renders a remote embed only as an image.
pub(crate) fn remote_link(name: &str, url: &str) -> String {
    let label = name.replace(['[', ']'], "");
    let image = name.rsplit_once('.').is_some_and(|(_, extension)| {
        crate::note_paste::attachments::is_image_extension(extension)
    });
    let target = if url.contains(['(', ')', ' ', '<', '>']) {
        format!("<{url}>")
    } else {
        url.to_owned()
    };
    format!("{}[{label}]({target})", if image { "!" } else { "" })
}

/// Where `links` stand in `source`, last first, so replacing them in order
/// keeps the earlier ranges valid. `[[x.zip]]` inside `![[x.zip]]` is not a
/// match: that embed is not what the paste wrote.
pub(crate) fn link_ranges(source: &str, links: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    for link in links {
        let mut from = 0;
        while let Some(found) = source[from..].find(link.as_str()) {
            let start = from + found;
            let end = start + link.len();
            let embedded = !link.starts_with('!') && source[..start].ends_with('!');
            if !embedded
                && !ranges
                    .iter()
                    .any(|r: &std::ops::Range<usize>| r.start < end && start < r.end)
            {
                ranges.push(start..end);
            }
            from = end;
        }
    }
    ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
    ranges
}

/// The token as Okilum knows it this session. Read from the credential store
/// only when first needed (an upload, a hosted image, Settings), never at
/// launch. `generation` changes with the token so hosted images reload.
#[derive(Default)]
pub(crate) struct Auth {
    state: TokenState,
    generation: u64,
}
impl Global for Auth {}

#[derive(Clone, Default)]
pub(crate) enum TokenState {
    #[default]
    Unread,
    Reading,
    Known(Option<Arc<str>>),
}

pub(crate) fn token_state(cx: &App) -> TokenState {
    cx.try_global::<Auth>()
        .map(|auth| auth.state.clone())
        .unwrap_or_default()
}

fn generation(cx: &App) -> u64 {
    cx.try_global::<Auth>().map_or(0, |auth| auth.generation)
}

/// Record the token after a read, a save or a removal.
pub(crate) fn set_token(token: Option<String>, cx: &mut App) {
    let token = token.filter(|t| !t.is_empty()).map(Arc::<str>::from);
    let auth = cx.default_global::<Auth>();
    auth.state = TokenState::Known(token);
    auth.generation += 1;
    cx.refresh_windows();
}

/// Start reading the saved token if nobody has yet.
pub(crate) fn ensure_token(cx: &mut App) {
    if !matches!(token_state(cx), TokenState::Unread) {
        return;
    }
    cx.default_global::<Auth>().state = TokenState::Reading;
    let read = cx.read_credentials(provider().credential_key());
    cx.spawn(async move |cx| {
        let token = match read.await {
            Ok(saved) => saved.and_then(|(_, bytes)| String::from_utf8(bytes).ok()),
            Err(error) => {
                diagnostic_error!("Upload: credential store not readable ({error:#})");
                None
            }
        };
        cx.update(|cx| set_token(token, cx));
    })
    .detach();
}

/// The token for an upload, read from the credential store if needed.
pub(crate) async fn token(cx: &gpui::AsyncApp) -> Option<Arc<str>> {
    cx.update(ensure_token);
    loop {
        match cx.update(|cx| token_state(cx)) {
            TokenState::Known(token) => return token,
            TokenState::Unread => return None,
            TokenState::Reading => {
                cx.background_executor()
                    .timer(Duration::from_millis(20))
                    .await
            }
        }
    }
}

/// A remote Markdown image: the provider's files load with the token, any
/// other URL through GPUI's ordinary resource path.
pub(crate) fn image_source(url: &str) -> ImageSource {
    if !provider().hosts(url) {
        return ImageSource::from(url);
    }
    let url: Arc<str> = url.into();
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| {
        ensure_token(cx);
        match token_state(cx) {
            TokenState::Known(_) => {}
            _ => return None,
        }
        let key = (url.clone(), generation(cx));
        window.use_asset::<HostedImage>(&key, cx)
    }))
}

struct HostedImage;

impl Asset for HostedImage {
    type Source = (Arc<str>, u64);
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        (url, _): Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        let token = match token_state(cx) {
            TokenState::Known(token) => token,
            _ => None,
        };
        cx.background_executor().spawn(async move {
            let provider = provider();
            let fetched = provider
                .fetch(&url, token.as_deref().unwrap_or_default())
                .map_err(|error| anyhow::anyhow!(error.user_text(provider.name())))?;
            let format = fetched
                .content_type
                .as_deref()
                .and_then(ImageFormat::from_mime_type)
                .or_else(|| sniff(&fetched.bytes))
                .ok_or_else(|| anyhow::anyhow!("The uploaded file is not an image"))?;
            Image::from_bytes(format, fetched.bytes)
                .to_image_data(renderer)
                .map_err(Into::into)
        })
    }
}

fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    Some(match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => ImageFormat::Png,
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::Gif => ImageFormat::Gif,
        image::ImageFormat::WebP => ImageFormat::Webp,
        image::ImageFormat::Bmp => ImageFormat::Bmp,
        image::ImageFormat::Tiff => ImageFormat::Tiff,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pomoi_hosts_only_its_file_urls() {
        assert!(Pomoi.hosts("https://pomoi.co/f/0b6c/shot.png"));
        assert!(!Pomoi.hosts("https://pomoi.co/f/"));
        assert!(!Pomoi.hosts("https://pomoi.co/"));
        assert!(!Pomoi.hosts("https://oklb.uk/abc"));
        assert!(!Pomoi.hosts("https://pomoi.co.evil.example/f/x.png"));
        assert!(!Pomoi.hosts("http://pomoi.co/f/x/y.png"));
    }

    #[test]
    fn an_upload_answer_must_be_a_private_file_url() {
        let ok = serde_json::json!({
            "url": "https://pomoi.co/f/1234/Pasted%20image.png",
            "short": "https://oklb.uk/x", "private": true, "expires_at": null
        });
        assert_eq!(
            parse_upload(&Pomoi, &ok),
            Ok(Uploaded {
                url: "https://pomoi.co/f/1234/Pasted%20image.png".into()
            })
        );
        let public = serde_json::json!({"url": "https://pomoi.co/f/1/a.png", "private": false});
        assert_eq!(parse_upload(&Pomoi, &public), Err(UploadError::Response));
        let elsewhere = serde_json::json!({"url": "https://oklb.uk/x", "private": true});
        assert_eq!(parse_upload(&Pomoi, &elsewhere), Err(UploadError::Response));
        assert_eq!(
            parse_upload(&Pomoi, &serde_json::json!({})),
            Err(UploadError::Response)
        );
    }

    #[test]
    fn no_token_never_reaches_the_network() {
        assert_eq!(
            Pomoi.upload(Path::new("/nonexistent/x.png"), ""),
            Err(UploadError::NoToken)
        );
        assert!(matches!(
            Pomoi.fetch("https://pomoi.co/f/1/a.png", ""),
            Err(UploadError::NoToken)
        ));
        assert!(matches!(
            Pomoi.fetch("https://example.com/a.png", "secret"),
            Err(UploadError::Response)
        ));
    }

    #[test]
    fn local_links_match_what_a_paste_writes() {
        let root = Path::new("/vault");
        assert_eq!(
            local_links(root, Path::new("/vault/attachments/Pasted image 1.png")),
            vec![
                "![[Pasted image 1.png]]".to_string(),
                "![[attachments/Pasted image 1.png]]".to_string()
            ]
        );
        assert_eq!(
            local_links(root, Path::new("/vault/spec.pdf")),
            vec!["![[spec.pdf]]".to_string()]
        );
        assert_eq!(
            local_links(root, Path::new("/vault/a/data.zip")),
            vec!["[[data.zip]]".to_string(), "[[a/data.zip]]".to_string()]
        );
        assert!(local_links(root, Path::new("/vault/a/Note.md")).is_empty());
    }

    #[test]
    fn remote_links_keep_images_as_images() {
        assert_eq!(
            remote_link(
                "Pasted image 1.png",
                "https://pomoi.co/f/1/Pasted%20image%201.png"
            ),
            "![Pasted image 1.png](https://pomoi.co/f/1/Pasted%20image%201.png)"
        );
        assert_eq!(
            remote_link("spec [v2].pdf", "https://pomoi.co/f/1/spec.pdf"),
            "[spec v2.pdf](https://pomoi.co/f/1/spec.pdf)"
        );
        assert_eq!(
            remote_link("a.png", "https://pomoi.co/f/1/a(1).png"),
            "![a.png](<https://pomoi.co/f/1/a(1).png>)"
        );
    }

    #[test]
    fn link_ranges_find_only_the_pasted_links_last_first() {
        let links = vec!["[[data.zip]]".to_string(), "[[a/data.zip]]".to_string()];
        let source = "x [[data.zip]] y ![[data.zip]] z [[a/data.zip]]";
        let ranges = link_ranges(source, &links);
        assert_eq!(ranges.len(), 2);
        assert!(ranges[0].start > ranges[1].start);
        assert_eq!(&source[ranges[0].clone()], "[[a/data.zip]]");
        assert_eq!(&source[ranges[1].clone()], "[[data.zip]]");
        assert!(link_ranges("nothing here", &links).is_empty());
    }

    #[test]
    fn upload_errors_name_the_fix_and_never_the_token() {
        let text = UploadError::NoToken.user_text("pomoi.co");
        assert!(text.contains("Settings → Editor"));
        assert_eq!(status_error(401), UploadError::Unauthorized);
        assert_eq!(status_error(413), UploadError::TooLarge);
        assert_eq!(status_error(500), UploadError::Status(500));
    }
}
