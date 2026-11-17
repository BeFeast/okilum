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
use std::ops::Range;
use std::path::{Path, PathBuf};
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
    // `expire: never` answers `expires_at: null`. A timestamp (pomoi's
    // 72-hour default when the header was missing or ignored) or no field
    // at all would put a link that rots into the note.
    if body.get("expires_at") != Some(&serde_json::Value::Null) {
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
pub(crate) fn link_ranges(source: &str, links: &[String]) -> Vec<Range<usize>> {
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
                    .any(|r: &Range<usize>| r.start < end && start < r.end)
            {
                ranges.push(start..end);
            }
            from = end;
        }
    }
    ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
    ranges
}

/// One link a paste wrote to a file it saved: its exact text and its byte
/// range in the editor's source at revision `revision`. The range follows the
/// editor's own edit receipts ([`TrackedLink::follow`]); any edit that
/// touches the link, or a revision the editor can no longer relate to this
/// one, loses its identity for good. Only a link still tracked, at its range,
/// with its exact text, is ever rewritten: never a copy elsewhere in the note,
/// in a code block or in another note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackedLink {
    pub path: PathBuf,
    pub link: String,
    /// `None` once identity is lost.
    pub range: Option<Range<usize>>,
    pub revision: u64,
}

/// The links to `paths` inside `span`, the text a paste has just inserted
/// into `text` (the whole note at editor revision `revision`). A saved file
/// the inserted text does not link (a "keep original" copy) has none and is
/// not uploaded.
pub(crate) fn inserted_links(
    text: &str,
    revision: u64,
    span: Range<usize>,
    vault_root: &Path,
    paths: &[PathBuf],
) -> Vec<TrackedLink> {
    let Some(inserted) = text.get(span.clone()) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for path in paths {
        let links = local_links(vault_root, path);
        for range in link_ranges(inserted, &links).into_iter().rev() {
            let start = span.start + range.start;
            found.push(TrackedLink {
                path: path.clone(),
                link: inserted[range.clone()].to_owned(),
                range: Some(start..start + range.len()),
                revision,
            });
        }
    }
    found.sort_by_key(|link| link.range.as_ref().map_or(0, |range| range.start));
    found
}

impl TrackedLink {
    /// Move to revision `now`, given the editor's receipt for everything that
    /// changed since [`Self::revision`]: the replaced bytes `old` and the
    /// length inserted there. `None` (no receipt) loses the link, and so does
    /// an edit that touches it, including typing inside it.
    pub(crate) fn follow(&mut self, edit: Option<(Range<usize>, usize)>, now: u64) {
        if now == self.revision {
            return;
        }
        self.revision = now;
        let Some(range) = self.range.clone() else {
            return;
        };
        let Some((old, inserted)) = edit else {
            self.range = None;
            return;
        };
        self.range = if old.end <= range.start {
            let start = range.start - old.len() + inserted;
            Some(start..start + range.len())
        } else if old.start >= range.end {
            Some(range)
        } else {
            None
        };
    }

    /// The link's range in `current` (at [`Self::revision`]), if it is still
    /// tracked and holds exactly the text the paste wrote.
    pub(crate) fn current(&self, current: &str) -> Option<Range<usize>> {
        let range = self.range.clone()?;
        (current.get(range.clone()) == Some(self.link.as_str())).then_some(range)
    }
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
    let started = generation(cx);
    let read = cx.read_credentials(provider().credential_key());
    cx.spawn(async move |cx| {
        let token = match read.await {
            Ok(saved) => saved.and_then(|(_, bytes)| String::from_utf8(bytes).ok()),
            Err(error) => {
                diagnostic_error!("Upload: credential store not readable ({error:#})");
                None
            }
        };
        // A token saved or removed while this read ran is newer: keep it.
        cx.update(|cx| {
            if generation(cx) == started {
                set_token(token, cx);
            }
        });
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

/// A remote Markdown image. A provider file loads with the token only once
/// a token is in memory; until then (no token saved, or the credential read
/// still running) it takes GPUI's ordinary anonymous resource path, exactly
/// as any other URL, and never blanks. Saving a token refreshes windows, so
/// a private image then loads with it.
pub(crate) fn image_source(url: &str) -> ImageSource {
    if !provider().hosts(url) {
        return ImageSource::from(url);
    }
    let url: Arc<str> = url.into();
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| {
        ensure_token(cx);
        if authenticated(&token_state(cx)) {
            let key = (url.clone(), generation(cx));
            window.use_asset::<HostedImage>(&key, cx)
        } else {
            let resource = gpui::Resource::Uri(url.to_string().into());
            window.use_asset::<gpui::ImgResourceLoader>(&resource, cx)
        }
    }))
}

/// Whether hosted images load with the token: only with one in memory.
fn authenticated(state: &TokenState) -> bool {
    matches!(state, TokenState::Known(Some(_)))
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
        // Review of #3114: private but expiring (or silent about it) is refused.
        let expiring = serde_json::json!({
            "url": "https://pomoi.co/f/1/a.png", "private": true,
            "expires_at": "2026-11-20T22:00:00Z"
        });
        assert_eq!(parse_upload(&Pomoi, &expiring), Err(UploadError::Response));
        let silent = serde_json::json!({"url": "https://pomoi.co/f/1/a.png", "private": true});
        assert_eq!(parse_upload(&Pomoi, &silent), Err(UploadError::Response));
    }

    /// Review of #3114: without a token in memory a hosted image keeps the
    /// anonymous path, also while the credential read runs.
    #[test]
    fn hosted_images_use_the_token_only_when_one_is_known() {
        assert!(!authenticated(&TokenState::Unread));
        assert!(!authenticated(&TokenState::Reading));
        assert!(!authenticated(&TokenState::Known(None)));
        assert!(authenticated(&TokenState::Known(Some("t".into()))));
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

    /// Review of #3114: only the link the paste inserted is tracked, never
    /// the same link elsewhere (a code block, an older copy).
    #[test]
    fn only_the_inserted_link_is_tracked() {
        let root = Path::new("/vault");
        let file = PathBuf::from("/vault/attachments/shot.png");
        let pasted = "![[shot.png]]";
        let note = format!("```\n{pasted}\n```\nintro {pasted} outro\n{pasted}\n");
        let start = note.find("intro ").unwrap() + "intro ".len();
        let span = start..start + pasted.len();
        let links = inserted_links(&note, 7, span.clone(), root, std::slice::from_ref(&file));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].current(&note), Some(span.clone()));
        let other = PathBuf::from("/vault/attachments/shot.original.png");
        assert!(inserted_links(&note, 7, span, root, &[other]).is_empty());
    }

    /// Review of #3114 (b): an edit before the link moves it; an edit that
    /// touches it, or no receipt, loses it for good, even if an identical
    /// copy stands where it used to be.
    #[test]
    fn a_tracked_link_follows_edits_and_is_lost_when_touched() {
        let link = "![[shot.png]]";
        let tracked = |start: usize| TrackedLink {
            path: PathBuf::from("/vault/shot.png"),
            link: link.into(),
            range: Some(start..start + link.len()),
            revision: 1,
        };
        // Typed before it: moved by the inserted length.
        let mut moved = tracked(10);
        moved.follow(Some((0..0, 4)), 2);
        assert_eq!(moved.range, Some(14..14 + link.len()));
        // Typed after it: unchanged.
        let mut after = tracked(10);
        after.follow(Some((30..31, 0)), 2);
        assert_eq!(after.range, Some(10..10 + link.len()));
        // Deleted, then an identical copy typed in its place: still lost.
        let mut deleted = tracked(10);
        deleted.follow(Some((10..10 + link.len(), 0)), 2);
        assert_eq!(deleted.range, None);
        deleted.follow(Some((10..10, link.len())), 3);
        assert_eq!(deleted.range, None);
        let copy = format!("0123456789{link}");
        assert_eq!(deleted.current(&copy), None);
        // Typing inside it, or no receipt: lost.
        let mut inside = tracked(10);
        inside.follow(Some((12..12, 1)), 2);
        assert_eq!(inside.range, None);
        let mut unknown = tracked(10);
        unknown.follow(None, 2);
        assert_eq!(unknown.range, None);
        // A tracked range whose text differs is refused.
        assert_eq!(tracked(0).current("![[other.png]]"), None);
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
