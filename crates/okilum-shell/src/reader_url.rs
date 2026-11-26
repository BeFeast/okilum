//! Rich preview viewer for `.url` Internet Shortcut files (#3288).
//!
//! Parses `[InternetShortcut]` / `URL=...`, displays a rich preview card with
//! cached Open Graph metadata and thumbnail (falling back cleanly when offline
//! or unavailable), and keeps access to the raw shortcut source and file info.
//! Derived metadata and thumbnails live in the app cache directory outside the
//! vault so they remain rebuildable from canonical `.url` files.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_URL_FILE_BYTES: u64 = 64 * 1024;
const MAX_HTML_BYTES: u64 = 256 * 1024;
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4096;
const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HTML_TIMEOUT: Duration = Duration::from_secs(8);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const CACHE_VERSION: u32 = 1;

pub(crate) fn eligible(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("url"))
}

/// Parsed `.url` shortcut contents alongside the raw source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedUrlFile {
    pub url: Option<String>,
    pub parsed_url: Option<url::Url>,
    pub raw_source: String,
    pub truncated: bool,
}

/// Extract the `URL=` entry from the `[InternetShortcut]` section of a `.url`
/// file. Handles UTF-8 BOM, CRLF/LF/CR line endings, case-insensitive section
/// and key names, surrounding whitespace, and `;`/`#` comments.
pub(crate) fn parse_url_shortcut(content: &str) -> Option<String> {
    let text = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut in_section = false;
    for line in text.split(['\n', '\r']) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
            let section = trimmed[1..trimmed.len() - 1].trim();
            in_section = section.eq_ignore_ascii_case("InternetShortcut");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            if key.trim().eq_ignore_ascii_case("URL") {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    None
}

fn read_url_file(path: &Path) -> anyhow::Result<ParsedUrlFile> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_URL_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > MAX_URL_FILE_BYTES;
    if truncated {
        bytes.truncate(MAX_URL_FILE_BYTES as usize);
    }
    let raw_source = String::from_utf8_lossy(&bytes).into_owned();
    let url = parse_url_shortcut(&raw_source);
    let parsed_url = url.as_deref().and_then(|u| url::Url::parse(u).ok());
    Ok(ParsedUrlFile {
        url,
        parsed_url,
        raw_source,
        truncated,
    })
}

fn is_openable_scheme(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "mailto")
}

fn is_fetchable_url(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
}

/// Derived cache record stored outside the vault under `<cache>/url-previews/`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedPreview {
    pub version: u32,
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub thumbnail_file: Option<String>,
    #[serde(default)]
    pub thumbnail_is_icon: bool,
    #[serde(default)]
    pub fetched_at_unix: i64,
}

pub(crate) fn cache_dir() -> anyhow::Result<PathBuf> {
    Ok(crate::reader_open::cache_base()?.join("url-previews"))
}

pub(crate) fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn is_safe_cache_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && Path::new(name).file_name().and_then(|s| s.to_str()) == Some(name)
}

pub(crate) fn read_cache(dir: &Path, url: &str) -> Option<(CachedPreview, Option<PathBuf>)> {
    let key = cache_key(url);
    let meta_path = dir.join(format!("{key}.json"));
    let raw = std::fs::read_to_string(&meta_path).ok()?;
    let entry: CachedPreview = serde_json::from_str(&raw).ok()?;
    if entry.version != CACHE_VERSION || entry.url != url {
        return None;
    }
    let thumb_path = entry
        .thumbnail_file
        .as_deref()
        .filter(|name| is_safe_cache_filename(name))
        .map(|name| dir.join(name))
        .filter(|path| path.is_file());
    Some((entry, thumb_path))
}

fn write_atomic(dir: &Path, target: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut temp, bytes)?;
    temp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

pub(crate) fn write_cache(
    dir: &Path,
    entry: &CachedPreview,
    thumbnail: Option<(&str, &[u8])>,
) -> anyhow::Result<Option<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let key = cache_key(&entry.url);
    let mut stored = entry.clone();
    let thumb_path = if let Some((ext, bytes)) = thumbnail {
        let filename = format!("{key}.{ext}");
        let path = dir.join(&filename);
        write_atomic(dir, &path, bytes)?;
        stored.thumbnail_file = Some(filename);
        Some(path)
    } else {
        stored.thumbnail_file = None;
        None
    };
    let json = serde_json::to_vec_pretty(&stored)?;
    write_atomic(dir, &dir.join(format!("{key}.json")), &json)?;
    Ok(thumb_path)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FetchPolicy {
    pub allow_loopback: bool,
}

impl FetchPolicy {
    pub(crate) const fn production() -> Self {
        Self {
            allow_loopback: false,
        }
    }
}

fn is_disallowed_ipv4(ip: Ipv4Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return !allow_loopback;
    }
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || (a == 0)
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0)
        || (a == 198 && (b == 18 || b == 19))
        || (a >= 240)
}

fn is_disallowed_ipv6(ip: Ipv6Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return !allow_loopback;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_disallowed_ipv4(v4, allow_loopback);
    }
    let first = ip.segments()[0];
    ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
}

fn is_disallowed_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => is_disallowed_ipv4(v4, allow_loopback),
        IpAddr::V6(v6) => is_disallowed_ipv6(v6, allow_loopback),
    }
}

pub(crate) fn validate_and_resolve(
    url: &url::Url,
    policy: FetchPolicy,
) -> anyhow::Result<Vec<SocketAddr>> {
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Unsupported URL scheme"
    );
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("Missing port"))?;
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            anyhow::ensure!(
                !is_disallowed_ipv4(ip, policy.allow_loopback),
                "Blocked non-public IP address"
            );
            Ok(vec![SocketAddr::new(IpAddr::V4(ip), port)])
        }
        Some(url::Host::Ipv6(ip)) => {
            anyhow::ensure!(
                !is_disallowed_ipv6(ip, policy.allow_loopback),
                "Blocked non-public IP address"
            );
            Ok(vec![SocketAddr::new(IpAddr::V6(ip), port)])
        }
        Some(url::Host::Domain(domain)) => {
            let lower = domain.trim_end_matches('.').to_ascii_lowercase();
            let is_localhost = lower == "localhost" || lower.ends_with(".localhost");
            if is_localhost && !policy.allow_loopback {
                anyhow::bail!("Blocked localhost host");
            }
            if lower.ends_with(".local") || lower.ends_with(".internal") {
                anyhow::bail!("Blocked local network host");
            }
            let addrs: Vec<SocketAddr> = (domain, port)
                .to_socket_addrs()
                .map_err(|e| anyhow::anyhow!("DNS resolution failed: {e}"))?
                .collect();
            anyhow::ensure!(!addrs.is_empty(), "No addresses resolved for host");
            for addr in &addrs {
                anyhow::ensure!(
                    !is_disallowed_ip(addr.ip(), policy.allow_loopback),
                    "Blocked non-public resolved address"
                );
            }
            Ok(addrs)
        }
        None => anyhow::bail!("URL has no host"),
    }
}

struct BoundedResponse {
    final_url: url::Url,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

fn fetch_bounded(
    start_url: &url::Url,
    accept: &str,
    timeout: Duration,
    max_bytes: u64,
    policy: FetchPolicy,
) -> anyhow::Result<BoundedResponse> {
    let mut current = start_url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let addrs = validate_and_resolve(&current, policy)?;
        let mut builder = okilum_http::per_request_builder(env!("CARGO_PKG_VERSION"), timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(url::Host::Domain(domain)) = current.host() {
            builder = builder.resolve_to_addrs(domain, &addrs);
        }
        let client = builder.build()?;
        let response = client
            .get(current.as_str())
            .header(reqwest::header::ACCEPT, accept)
            .send()?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("Redirect missing Location header"))?;
            current = current.join(location)?;
            continue;
        }
        anyhow::ensure!(status.is_success(), "HTTP {}", status.as_u16());
        if response.content_length().is_some_and(|len| len > max_bytes) {
            anyhow::bail!("Response exceeds size limit");
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut bytes = Vec::new();
        response.take(max_bytes + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= max_bytes,
            "Response exceeds size limit"
        );
        return Ok(BoundedResponse {
            final_url: current,
            content_type,
            bytes,
        });
    }
    anyhow::bail!("Too many redirects")
}

fn image_format_from_response(
    content_type: Option<&str>,
    bytes: &[u8],
) -> Option<(&'static str, ImageFormat)> {
    let mime = content_type
        .and_then(|ct| ct.split(';').next())
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let by_mime = match mime.as_str() {
        "image/png" | "image/apng" => Some(("png", ImageFormat::Png)),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some(("jpg", ImageFormat::Jpeg)),
        "image/gif" => Some(("gif", ImageFormat::Gif)),
        "image/webp" => Some(("webp", ImageFormat::Webp)),
        "image/bmp" | "image/x-ms-bmp" => Some(("bmp", ImageFormat::Bmp)),
        "image/svg+xml" => Some(("svg", ImageFormat::Svg)),
        _ => None,
    };
    if by_mime.is_some() {
        return by_mime;
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(("png", ImageFormat::Png));
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(("jpg", ImageFormat::Jpeg));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(("gif", ImageFormat::Gif));
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(("webp", ImageFormat::Webp));
    }
    if bytes.starts_with(b"BM") {
        return Some(("bmp", ImageFormat::Bmp));
    }
    None
}

fn image_format_from_path(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "bmp" => Some(ImageFormat::Bmp),
        "svg" => Some(ImageFormat::Svg),
        _ => None,
    }
}

fn validate_image_dimensions(format: ImageFormat, bytes: &[u8]) -> anyhow::Result<()> {
    let raster = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Svg => return Ok(()),
        _ => anyhow::bail!("Unsupported thumbnail format"),
    };
    let (width, height) =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), raster).into_dimensions()?;
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= MAX_IMAGE_DIMENSION
            && height <= MAX_IMAGE_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS,
        "Thumbnail dimensions exceed preview limits"
    );
    Ok(())
}

fn decode_thumbnail(path: &Path, renderer: gpui::SvgRenderer) -> Option<Arc<RenderImage>> {
    let format = image_format_from_path(path)?;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    validate_image_dimensions(format, &bytes).ok()?;
    Image::from_bytes(format, bytes)
        .to_image_data(renderer)
        .ok()
}

fn fetch_thumbnail(url_str: &str, policy: FetchPolicy) -> Option<(&'static str, Vec<u8>)> {
    let parsed = url::Url::parse(url_str).ok()?;
    let response = fetch_bounded(
        &parsed,
        "image/png,image/jpeg,image/webp,image/gif,image/svg+xml,image/*;q=0.8",
        IMAGE_TIMEOUT,
        MAX_IMAGE_BYTES,
        policy,
    )
    .ok()?;
    let (ext, format) =
        image_format_from_response(response.content_type.as_deref(), &response.bytes)?;
    validate_image_dimensions(format, &response.bytes).ok()?;
    Some((ext, response.bytes))
}

pub(crate) fn fetch_url_preview(
    url: &url::Url,
    cache_dir: &Path,
    policy: FetchPolicy,
) -> anyhow::Result<(CachedPreview, Option<PathBuf>)> {
    let response = fetch_bounded(
        url,
        "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1",
        HTML_TIMEOUT,
        MAX_HTML_BYTES,
        policy,
    )?;
    if let Some(ct) = response.content_type.as_deref() {
        let lower = ct.to_ascii_lowercase();
        anyhow::ensure!(
            lower.contains("html") || lower.contains("xml"),
            "Target is not an HTML document"
        );
    }
    let meta = okilum_paste::extract_page_metadata(
        &response.bytes,
        response.content_type.as_deref(),
        Some(&response.final_url),
    );
    let mut thumbnail: Option<(&'static str, Vec<u8>)> = None;
    let mut thumbnail_is_icon = false;
    if let Some(image_url) = meta.image_url.as_deref() {
        if let Some(fetched) = fetch_thumbnail(image_url, policy) {
            thumbnail = Some(fetched);
            thumbnail_is_icon = false;
        }
    }
    if thumbnail.is_none() {
        let icon_candidates: Vec<String> = meta
            .favicon_url
            .clone()
            .into_iter()
            .chain(
                response
                    .final_url
                    .join("/favicon.ico")
                    .ok()
                    .map(String::from),
            )
            .collect();
        for candidate in icon_candidates {
            if let Some(fetched) = fetch_thumbnail(&candidate, policy) {
                thumbnail = Some(fetched);
                thumbnail_is_icon = true;
                break;
            }
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let entry = CachedPreview {
        version: CACHE_VERSION,
        url: url.as_str().to_owned(),
        title: meta.title,
        description: meta.description,
        site_name: meta.site_name,
        thumbnail_file: None,
        thumbnail_is_icon,
        fetched_at_unix: now,
    };
    let thumb_ref = thumbnail
        .as_ref()
        .map(|(ext, bytes)| (*ext, bytes.as_slice()));
    let thumb_path = write_cache(cache_dir, &entry, thumb_ref)?;
    let stored = CachedPreview {
        thumbnail_file: thumb_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(str::to_owned),
        ..entry
    };
    Ok((stored, thumb_path))
}

/// Load from local cache first; only fetch over the network on a cache miss or
/// when `force_refresh` is true.
pub(crate) fn load_or_fetch_preview(
    url: &url::Url,
    cache_dir: &Path,
    force_refresh: bool,
    policy: FetchPolicy,
) -> anyhow::Result<(CachedPreview, Option<PathBuf>, bool)> {
    if !force_refresh {
        if let Some((cached, thumb)) = read_cache(cache_dir, url.as_str()) {
            return Ok((cached, thumb, true));
        }
    }
    let (fetched, thumb) = fetch_url_preview(url, cache_dir, policy)?;
    Ok((fetched, thumb, false))
}

#[derive(Clone)]
enum FetchState {
    Idle,
    Loading,
    Ready(CachedPreview),
    Unavailable(SharedString),
}

pub(crate) struct UrlPreview {
    path: PathBuf,
    details: String,
    raw_source: String,
    source_truncated: bool,
    show_source: bool,
    target_url: Option<String>,
    parsed_url: Option<url::Url>,
    fetch_state: FetchState,
    thumbnail: Option<Arc<RenderImage>>,
    thumbnail_is_icon: bool,
    from_cache: bool,
    cache_dir_override: Option<PathBuf>,
    fetch_policy: FetchPolicy,
    _task: Option<Task<()>>,
}

impl UrlPreview {
    pub(crate) fn new(path: PathBuf, details: String, cx: &mut Context<Self>) -> Self {
        Self::new_with_options(path, details, None, FetchPolicy::production(), cx)
    }

    pub(crate) fn new_with_options(
        path: PathBuf,
        details: String,
        cache_dir_override: Option<PathBuf>,
        fetch_policy: FetchPolicy,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            path,
            details,
            raw_source: String::new(),
            source_truncated: false,
            show_source: false,
            target_url: None,
            parsed_url: None,
            fetch_state: FetchState::Loading,
            thumbnail: None,
            thumbnail_is_icon: false,
            from_cache: false,
            cache_dir_override,
            fetch_policy,
            _task: None,
        };
        view.start_load(false, cx);
        view
    }

    pub(crate) fn reload(&mut self, path: PathBuf, details: String, cx: &mut Context<Self>) {
        self.path = path;
        self.details = details;
        self.start_load(false, cx);
    }

    fn start_load(&mut self, force_refresh: bool, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let cache_override = self.cache_dir_override.clone();
        let policy = self.fetch_policy;
        let renderer = cx.svg_renderer();
        self.fetch_state = FetchState::Loading;
        if force_refresh {
            self.from_cache = false;
        }
        cx.notify();

        let task = cx.spawn(async move |this, cx| {
            let parsed_res = cx
                .background_executor()
                .spawn(async move { read_url_file(&path) })
                .await;
            let parsed = match parsed_res {
                Ok(parsed) => parsed,
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.fetch_state =
                            FetchState::Unavailable("This shortcut couldn’t be read.".into());
                        cx.notify();
                    });
                    return;
                }
            };
            let fetch_target = parsed
                .parsed_url
                .as_ref()
                .filter(|u| is_fetchable_url(u))
                .cloned();
            let has_url = parsed.url.is_some();
            let _ = this.update(cx, |this, cx| {
                this.raw_source = parsed.raw_source;
                this.source_truncated = parsed.truncated;
                this.target_url = parsed.url;
                this.parsed_url = parsed.parsed_url;
                if !has_url {
                    this.show_source = true;
                    this.fetch_state =
                        FetchState::Unavailable("No URL entry found in [InternetShortcut].".into());
                } else if fetch_target.is_none() {
                    this.fetch_state = FetchState::Idle;
                }
                cx.notify();
            });

            let Some(target) = fetch_target else {
                return;
            };
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    let dir = match cache_override {
                        Some(dir) => dir,
                        None => cache_dir()?,
                    };
                    let (entry, thumb_path, from_cache) =
                        load_or_fetch_preview(&target, &dir, force_refresh, policy)?;
                    let image = thumb_path
                        .as_deref()
                        .and_then(|p| decode_thumbnail(p, renderer));
                    Ok::<_, anyhow::Error>((entry, image, from_cache))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match fetched {
                    Ok((entry, image, from_cache)) => {
                        this.thumbnail = image;
                        this.thumbnail_is_icon = entry.thumbnail_is_icon;
                        this.from_cache = from_cache;
                        this.fetch_state = FetchState::Ready(entry);
                    }
                    Err(_) => {
                        this.thumbnail = None;
                        this.thumbnail_is_icon = false;
                        this.from_cache = false;
                        this.fetch_state = FetchState::Unavailable(
                            "Preview unavailable. Open the link in your browser or try refreshing."
                                .into(),
                        );
                    }
                }
                cx.notify();
            });
        });
        self._task = Some(task);
    }

    fn display_host(&self) -> Option<String> {
        self.parsed_url
            .as_ref()
            .and_then(|u| u.host_str())
            .map(str::to_owned)
    }

    fn fallback_title(&self) -> String {
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| self.display_host())
            .unwrap_or_else(|| "URL Shortcut".into())
    }
}

impl Render for UrlPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = brand::palette(cx);
        let column_width = px(reader_ui_state::note_column_width(cx));
        let host = self.display_host();
        let openable_url = self
            .parsed_url
            .as_ref()
            .filter(|u| is_openable_scheme(u))
            .map(|u| u.as_str().to_owned());
        let raw_url = self.target_url.clone();

        let (title, description, site_name, status_note) = match &self.fetch_state {
            FetchState::Ready(meta) => (
                meta.title.clone().unwrap_or_else(|| self.fallback_title()),
                meta.description.clone(),
                meta.site_name.clone(),
                None,
            ),
            FetchState::Loading => (
                self.fallback_title(),
                None,
                None,
                Some(SharedString::from("Loading preview…")),
            ),
            FetchState::Unavailable(message) => {
                (self.fallback_title(), None, None, Some(message.clone()))
            }
            FetchState::Idle => (self.fallback_title(), None, None, None),
        };

        let site_label = match (site_name.as_deref(), host.as_deref()) {
            (Some(site), Some(host)) if !site.eq_ignore_ascii_case(host) => {
                format!("{site} · {host}")
            }
            (Some(site), _) => site.to_owned(),
            (None, Some(host)) => host.to_owned(),
            (None, None) => "URL Shortcut".to_owned(),
        };

        let mut card = v_flex()
            .debug_selector(|| "url-preview-card".into())
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .bg(theme.secondary.opacity(0.35))
            .overflow_hidden();

        if let Some(image) = self.thumbnail.as_ref().filter(|_| !self.thumbnail_is_icon) {
            card = card.child(
                div()
                    .debug_selector(|| "url-preview-banner".into())
                    .w_full()
                    .max_h(px(320.))
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(image.clone())
                            .w_full()
                            .max_h(px(320.))
                            .object_fit(ObjectFit::Contain),
                    ),
            );
        }

        let mut site_row = h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground);
        if let Some(icon_img) = self.thumbnail.as_ref().filter(|_| self.thumbnail_is_icon) {
            site_row = site_row.child(
                img(icon_img.clone())
                    .size(px(18.))
                    .rounded_sm()
                    .object_fit(ObjectFit::Contain),
            );
        } else {
            site_row = site_row.child(Icon::new(IconName::Network).size(px(14.)));
        }
        site_row = site_row.child(div().child(site_label));
        if let Some(note) = status_note {
            site_row = site_row.child(div().child("·")).child(div().child(note));
        }

        let mut body = v_flex().p_5().gap_3().child(site_row).child(
            div()
                .debug_selector(|| "url-preview-title".into())
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(title),
        );

        if let Some(description) = description {
            body = body.child(
                div()
                    .debug_selector(|| "url-preview-description".into())
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(description),
            );
        }

        if let Some(url_text) = raw_url.clone() {
            if let Some(open_target) = openable_url.clone() {
                body = body.child(
                    div()
                        .id("url-preview-link")
                        .debug_selector(|| "url-preview-link".into())
                        .text_sm()
                        .text_color(theme.primary)
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .on_click(move |_, _, cx| cx.open_url(&open_target))
                        .child(url_text),
                );
            } else {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(url_text),
                );
            }
        }

        let mut actions = h_flex().gap_2().pt_1().flex_wrap();
        if let Some(open_target) = openable_url {
            actions = actions.child(
                Button::new("url-preview-open")
                    .debug_selector(|| "url-preview-open".into())
                    .small()
                    .primary()
                    .icon(IconName::ExternalLink)
                    .label("Open URL")
                    .on_click(move |_, _, cx| cx.open_url(&open_target)),
            );
        }
        if let Some(copy_target) = raw_url {
            actions = actions.child(
                Button::new("url-preview-copy")
                    .debug_selector(|| "url-preview-copy".into())
                    .small()
                    .outline()
                    .icon(IconName::Copy)
                    .label("Copy URL")
                    .on_click(move |_, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_target.clone()));
                        reader_toast::transient("Copied", window, cx);
                    }),
            );
        }
        if self.parsed_url.as_ref().is_some_and(is_fetchable_url) {
            actions = actions.child(
                Button::new("url-preview-refresh")
                    .debug_selector(|| "url-preview-refresh".into())
                    .small()
                    .outline()
                    .icon(IconName::RotateCw)
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_load(true, cx);
                    })),
            );
        }
        actions = actions.child(
            Button::new("url-preview-toggle-source")
                .debug_selector(|| "url-preview-toggle-source".into())
                .small()
                .ghost()
                .icon(IconName::FileText)
                .label(if self.show_source {
                    "Hide source"
                } else {
                    "Raw source"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_source = !this.show_source;
                    cx.notify();
                })),
        );

        body = body.child(actions);
        card = card.child(body);

        let mut container = v_flex()
            .w_full()
            .max_w(column_width)
            .mx_auto()
            .px(px(READER_SIDE_PADDING))
            .py_6()
            .gap_4()
            .child(card)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.details.clone()),
            );

        if self.show_source {
            let mut source_box = v_flex()
                .debug_selector(|| "url-preview-raw-source".into())
                .w_full()
                .p_4()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(palette.border_subtle)
                .bg(theme.secondary.opacity(0.4))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child("Shortcut source"),
                )
                .child(
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .text_color(theme.foreground)
                        .child(self.raw_source.clone()),
                );
            if self.source_truncated {
                source_box = source_box.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Showing the first 64 KB of this file."),
                );
            }
            container = container.child(source_box);
        }

        div()
            .id("reader-url-scroll")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .child(container)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn eligible_matches_url_extension_case_insensitively() {
        assert!(eligible("docs/Guide.url"));
        assert!(eligible("Guide.URL"));
        assert!(!eligible("Guide.url.md"));
        assert!(!eligible("Guide.txt"));
    }

    #[test]
    fn parser_handles_bom_crlf_case_insensitivity_whitespace_and_other_sections() {
        let standard = "\u{feff}[InternetShortcut]\r\nURL=https://example.org/docs\r\n";
        assert_eq!(
            parse_url_shortcut(standard).as_deref(),
            Some("https://example.org/docs")
        );

        let mixed = "; comment\n# another\n[OtherSection]\nURL=https://wrong.example/\n\n  [ internetshortcut ] \r\n  url =  https://right.example/path?a=1&b=2  \r\n";
        assert_eq!(
            parse_url_shortcut(mixed).as_deref(),
            Some("https://right.example/path?a=1&b=2")
        );

        let missing = "[OtherSection]\nURL=https://ignored.example/\n";
        assert_eq!(parse_url_shortcut(missing), None);

        let empty_value = "[InternetShortcut]\nURL=   \n";
        assert_eq!(parse_url_shortcut(empty_value), None);
    }

    #[test]
    fn ssrf_policy_blocks_loopback_private_link_local_and_non_http_schemes() {
        let prod = FetchPolicy::production();
        for blocked in [
            "http://127.0.0.1/admin",
            "http://localhost/secret",
            "http://sub.localhost/secret",
            "http://10.0.0.5/internal",
            "http://192.168.1.1/router",
            "http://172.16.0.1/net",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/tailscale",
            "http://[::1]/loopback",
            "http://[fe80::1]/link-local",
            "http://[fc00::1]/ula",
            "file:///etc/passwd",
            "ftp://example.org/file",
        ] {
            let parsed = url::Url::parse(blocked).unwrap();
            assert!(
                validate_and_resolve(&parsed, prod).is_err(),
                "must block {blocked}"
            );
        }
    }

    #[test]
    fn fetch_caches_metadata_and_thumbnail_and_reuses_cache_on_reopen() {
        let png = crate::note_paste::image_import::fixture_png();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let server_hits = hits.clone();
        let server_png = png.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = server.accept().unwrap();
                server_hits.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                if req.starts_with("GET /thumb.png ") {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        server_png.len()
                    )
                    .unwrap();
                    stream.write_all(&server_png).unwrap();
                } else {
                    let html = "<!doctype html><html><head>\
                        <meta property=\"og:title\" content=\"Cached Page Title\">\
                        <meta property=\"og:description\" content=\"Saved summary text.\">\
                        <meta property=\"og:site_name\" content=\"Local Fixture\">\
                        <meta property=\"og:image\" content=\"/thumb.png\">\
                        </head><body>ok</body></html>";
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        html.len(),
                        html
                    )
                    .unwrap();
                }
            }
        });

        let cache = tempfile::tempdir().unwrap();
        let target = url::Url::parse(&format!("http://{addr}/article")).unwrap();
        let test_policy = FetchPolicy {
            allow_loopback: true,
        };

        let (first, first_thumb, from_cache) =
            load_or_fetch_preview(&target, cache.path(), false, test_policy).unwrap();
        handle.join().unwrap();
        assert!(!from_cache);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(first.title.as_deref(), Some("Cached Page Title"));
        assert_eq!(first.description.as_deref(), Some("Saved summary text."));
        assert_eq!(first.site_name.as_deref(), Some("Local Fixture"));
        assert!(!first.thumbnail_is_icon);
        let thumb_path = first_thumb.expect("thumbnail saved to cache");
        assert!(thumb_path.is_file());
        assert_eq!(std::fs::read(&thumb_path).unwrap(), png);

        // Reopen with server already stopped: must hit cache with zero network requests.
        let (reopened, reopened_thumb, from_cache_again) =
            load_or_fetch_preview(&target, cache.path(), false, test_policy).unwrap();
        assert!(from_cache_again);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(reopened, first);
        assert_eq!(reopened_thumb, Some(thumb_path));
    }

    #[gpui::test]
    fn url_preview_renders_cached_card_opens_url_and_toggles_raw_source(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let file = dir.path().join("Example.url");
        let source = "[InternetShortcut]\r\nURL=https://example.invalid/guide\r\n";
        std::fs::write(&file, source).unwrap();

        let png = crate::note_paste::image_import::fixture_png();
        let entry = CachedPreview {
            version: CACHE_VERSION,
            url: "https://example.invalid/guide".into(),
            title: Some("Example Guide".into()),
            description: Some("Fast local-first reader.".into()),
            site_name: Some("Example".into()),
            thumbnail_file: None,
            thumbnail_is_icon: false,
            fetched_at_unix: 1,
        };
        write_cache(&cache, &entry, Some(("png", &png))).unwrap();

        cx.update(crate::test_init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                UrlPreview::new_with_options(
                    file.clone(),
                    "URL · 56 bytes".into(),
                    Some(cache.clone()),
                    FetchPolicy::production(),
                    cx,
                )
            });
            preview = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let preview = preview.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            visual.run_until_parked();
            let ready = visual.read(|cx| {
                let p = preview.read(cx);
                matches!(p.fetch_state, FetchState::Ready(_)) && p.thumbnail.is_some()
            });
            if ready {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cached preview did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        visual.read(|cx| {
            let p = preview.read(cx);
            assert!(p.from_cache);
            assert!(!p.show_source);
        });

        // Clicking Open URL opens the parsed target URL.
        let open_btn = visual
            .debug_bounds("url-preview-open")
            .expect("Open URL button");
        visual.simulate_click(open_btn.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            visual.opened_url().as_deref(),
            Some("https://example.invalid/guide")
        );

        // Clicking Raw source toggles the source panel.
        let toggle_btn = visual
            .debug_bounds("url-preview-toggle-source")
            .expect("Raw source button");
        visual.simulate_click(toggle_btn.center(), Modifiers::default());
        visual.run_until_parked();
        visual.read(|cx| assert!(preview.read(cx).show_source));
        assert!(visual.debug_bounds("url-preview-raw-source").is_some());
    }

    #[gpui::test]
    fn url_preview_shows_offline_fallback_without_crashing(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let file = dir.path().join("Offline.url");
        std::fs::write(
            &file,
            "[InternetShortcut]\nURL=http://127.0.0.1:1/offline\n",
        )
        .unwrap();

        cx.update(crate::test_init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                UrlPreview::new_with_options(
                    file.clone(),
                    "URL · 48 bytes".into(),
                    Some(cache.clone()),
                    FetchPolicy::production(),
                    cx,
                )
            });
            preview = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let preview = preview.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            visual.run_until_parked();
            let unavailable = visual
                .read(|cx| matches!(preview.read(cx).fetch_state, FetchState::Unavailable(_)));
            if unavailable {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "offline fallback did not settle"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        visual.read(|cx| {
            let p = preview.read(cx);
            assert_eq!(p.target_url.as_deref(), Some("http://127.0.0.1:1/offline"));
            assert!(p.thumbnail.is_none());
        });
    }
}
