//! Project icons, shared with T3 Code (#1627).
//!
//! One icon per project for both apps. A vault project with a T3
//! counterpart reads and writes T3's `projectIcon` and uses T3's
//! `faviconPath` as its logo; a vault project without one keeps the same
//! shape in `ops/project.yaml` under `icon:`. What is shown follows one
//! order: a manual icon, then a logo, then a monogram whose letters and
//! colour come from the project's name.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// T3's eighteen icon colours, in T3's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconColor {
    Gray,
    Red,
    Orange,
    Amber,
    Yellow,
    Lime,
    Green,
    Emerald,
    Teal,
    Cyan,
    Sky,
    Blue,
    Indigo,
    Violet,
    Purple,
    Fuchsia,
    Pink,
    Rose,
}

impl IconColor {
    pub const ALL: [IconColor; 18] = [
        Self::Gray,
        Self::Red,
        Self::Orange,
        Self::Amber,
        Self::Yellow,
        Self::Lime,
        Self::Green,
        Self::Emerald,
        Self::Teal,
        Self::Cyan,
        Self::Sky,
        Self::Blue,
        Self::Indigo,
        Self::Violet,
        Self::Purple,
        Self::Fuchsia,
        Self::Pink,
        Self::Rose,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }

    /// The colour's name as T3 writes it.
    pub fn name(self) -> &'static str {
        [
            "gray", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal", "cyan",
            "sky", "blue", "indigo", "violet", "purple", "fuchsia", "pink", "rose",
        ][self.index()]
    }

    /// «Gray», for a swatch's tooltip.
    pub fn label(self) -> String {
        let name = self.name();
        name[..1].to_uppercase() + &name[1..]
    }

    /// The swatch T3 paints (Tailwind 500), `0xRRGGBB`.
    pub fn swatch(self) -> u32 {
        [
            0x6b7280, 0xef4444, 0xf97316, 0xf59e0b, 0xeab308, 0x84cc16, 0x22c55e, 0x10b981,
            0x14b8a6, 0x06b6d4, 0x0ea5e9, 0x3b82f6, 0x6366f1, 0x8b5cf6, 0xa855f7, 0xd946ef,
            0xec4899, 0xf43f5e,
        ][self.index()]
    }

    /// A glyph in this colour, as T3 draws icons: 600 on light, 400 on dark.
    pub fn glyph(self, dark: bool) -> u32 {
        const LIGHT: [u32; 18] = [
            0x4b5563, 0xdc2626, 0xea580c, 0xd97706, 0xca8a04, 0x65a30d, 0x16a34a, 0x059669,
            0x0d9488, 0x0891b2, 0x0284c7, 0x2563eb, 0x4f46e5, 0x7c3aed, 0x9333ea, 0xc026d3,
            0xdb2777, 0xe11d48,
        ];
        const DARK: [u32; 18] = [
            0x9ca3af, 0xf87171, 0xfb923c, 0xfbbf24, 0xfacc15, 0xa3e635, 0x4ade80, 0x34d399,
            0x2dd4bf, 0x22d3ee, 0x38bdf8, 0x60a5fa, 0x818cf8, 0xa78bfa, 0xc084fc, 0xe879f9,
            0xf472b6, 0xfb7185,
        ];
        if dark {
            DARK[self.index()]
        } else {
            LIGHT[self.index()]
        }
    }

    /// A monogram tile's background: white letters stay readable on every
    /// colour, yellow and lime included.
    pub fn tile(self) -> u32 {
        self.glyph(false)
    }
}

/// T3's `projectIcon`: a Lucide icon in a colour, an emoji, or 1–2 letters
/// on a colour. The same shape lives under `icon:` in project.yaml.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ProjectIcon {
    Lucide { name: String, color: IconColor },
    Emoji { emoji: String },
    Monogram { text: String, color: IconColor },
}

impl ProjectIcon {
    /// T3's `kind`: `lucide`, `emoji` or `monogram`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Lucide { .. } => "lucide",
            Self::Emoji { .. } => "emoji",
            Self::Monogram { .. } => "monogram",
        }
    }

    /// The icon if every field is usable: a kebab-case Lucide name, a short
    /// emoji, 1–2 monogram characters.
    pub fn validated(self) -> Option<Self> {
        let ok = match &self {
            Self::Lucide { name, .. } => {
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            }
            Self::Emoji { emoji } => {
                !emoji.trim().is_empty() && emoji.len() <= 64 && !emoji.contains('\n')
            }
            Self::Monogram { text, .. } => {
                let count = text.chars().count();
                (1..=2).contains(&count) && !text.chars().any(char::is_whitespace)
            }
        };
        ok.then_some(self)
    }

    /// Any JSON value: an unknown kind or a bad field is no icon, never an
    /// error that hides the project.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value::<Self>(value.clone())
            .ok()
            .and_then(Self::validated)
    }

    pub fn from_yaml(value: &serde_yaml::Value) -> Option<Self> {
        serde_yaml::from_value::<Self>(value.clone())
            .ok()
            .and_then(Self::validated)
    }

    /// One flow mapping on one line: `{kind: emoji, emoji: "🚀"}`. Strings
    /// are JSON-quoted, which YAML reads as double-quoted scalars.
    pub fn to_yaml_flow(&self) -> String {
        let quote = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
        match self {
            Self::Lucide { name, color } => format!(
                "{{kind: lucide, name: {}, color: {}}}",
                quote(name),
                color.name()
            ),
            Self::Emoji { emoji } => format!("{{kind: emoji, emoji: {}}}", quote(emoji)),
            Self::Monogram { text, color } => format!(
                "{{kind: monogram, text: {}, color: {}}}",
                quote(text),
                color.name()
            ),
        }
    }
}

/// Serde helper for T3 records: a missing, null or unreadable `projectIcon`
/// is `None`, so a new icon kind in T3 cannot break the project list.
pub fn lenient<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<ProjectIcon>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(ProjectIcon::from_json(&value))
}

/// What a project shows, after the priority order is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Avatar {
    Icon(ProjectIcon),
    Logo(PathBuf),
    Monogram { text: String, color: IconColor },
}

/// Manual icon > logo > monogram.
pub fn resolve(manual: Option<&ProjectIcon>, logo: Option<&Path>, name: &str) -> Avatar {
    if let Some(icon) = manual {
        return Avatar::Icon(icon.clone());
    }
    if let Some(logo) = logo {
        return Avatar::Logo(logo.to_owned());
    }
    Avatar::Monogram {
        text: monogram(name),
        color: monogram_color(name),
    }
}

/// The words of a name: split on anything not a letter or digit, and
/// between a lower-case letter or digit and the upper-case letter after it
/// («HomeLab» is two words).
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous = None;
            continue;
        }
        if c.is_uppercase()
            && previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit())
            && !current.is_empty()
        {
            words.push(std::mem::take(&mut current));
        }
        current.push(c);
        previous = Some(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// 1–2 upper-case letters: the initials of the first two words, or the
/// first letter of a one-word name. «OK Folio» → «OF», «ai-bills» → «AB»,
/// «HomeLab» → «HL», «Okilum» → «O».
pub fn monogram(name: &str) -> String {
    let words = words(name);
    let initial = |word: &String| {
        word.chars()
            .next()
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_default()
    };
    match words.as_slice() {
        [] => "?".into(),
        [one] => initial(one),
        [first, second, ..] => initial(first) + &initial(second),
    }
}

/// A stable colour from the name: FNV-1a over the lower-cased name's UTF-16
/// units (T3's `stableIndex`), modulo the eighteen colours.
pub fn monogram_color(name: &str) -> IconColor {
    let key = name.trim().to_lowercase();
    let mut hash: u32 = 2_166_136_261;
    for unit in key.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(16_777_619);
    }
    IconColor::ALL[hash as usize % IconColor::ALL.len()]
}

/// A logo or app icon in the project's own folder, in this order.
pub const LOGO_CANDIDATES: [&str; 12] = [
    "icon.svg",
    "icon.png",
    "logo.svg",
    "logo.png",
    "favicon.svg",
    "favicon.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    "attachments/icon.png",
    "attachments/logo.png",
];
/// Larger files are not icons; they are never decoded for a 16 px slot.
const LOGO_MAX_BYTES: u64 = 2 * 1024 * 1024;

fn usable_logo(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_file() && m.len() > 0 && m.len() <= LOGO_MAX_BYTES)
}

/// The first logo candidate that is a regular, reasonably small file.
pub fn find_logo(folder: &Path) -> Option<PathBuf> {
    LOGO_CANDIDATES
        .iter()
        .map(|name| folder.join(name))
        .find(|path| usable_logo(path))
}

/// T3's workspace root is this vault folder: its path ends with the
/// folder's vault-relative path. The vault lives at different absolute
/// paths on the server and on this machine.
pub fn same_folder(workspace_root: &str, vault_relative: &str) -> bool {
    let relative = vault_relative.trim_matches('/');
    if relative.is_empty() {
        return false;
    }
    let root = workspace_root
        .trim_end_matches(['/', '\\'])
        .replace('\\', "/");
    root.ends_with(&format!("/{relative}"))
}

/// Two workspace roots name the same directory, trailing slashes aside.
fn same_root(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.trim().trim_end_matches(['/', '\\']).to_owned();
    let (a, b) = (trim(a), trim(b));
    !a.is_empty() && a == b
}

/// The T3 project that is this vault project: its executor project, the
/// T3 project of its repository checkout (`execution.source_checkout`), or
/// a T3 project rooted at the same vault folder. The manager's project is
/// often shared (Brain) and never counts on its own.
pub fn t3_counterpart<'a>(
    project: &super::Project,
    vault_relative: &str,
    t3: &'a [crate::Project],
) -> Option<&'a crate::Project> {
    let appearance = &project.appearance;
    appearance
        .executor_project
        .as_deref()
        .and_then(|id| t3.iter().find(|p| p.id == id))
        .or_else(|| {
            let checkout = appearance.source_checkout.as_deref()?;
            t3.iter().find(|p| same_root(&p.workspace_root, checkout))
        })
        .or_else(|| {
            t3.iter()
                .find(|p| same_folder(&p.workspace_root, vault_relative))
        })
}

/// T3's `faviconPath` relative to the workspace root, when it stays inside
/// it and names an image this app draws.
fn favicon_relative(t3_project: &crate::Project) -> Option<PathBuf> {
    let favicon = t3_project.favicon_path.as_deref()?;
    // T3 writes some favicons as absolute paths under the workspace root.
    let favicon = favicon
        .strip_prefix(t3_project.workspace_root.trim_end_matches(['/', '\\']))
        .filter(|rest| rest.starts_with(['/', '\\']) && Path::new(favicon).has_root())
        .map(|rest| rest.trim_start_matches(['/', '\\']))
        .unwrap_or(favicon);
    let relative = Path::new(favicon);
    let inside = relative.components().next().is_some()
        && relative
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    let drawable = relative
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| LOGO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
    (inside && drawable).then(|| relative.to_owned())
}

/// The image kinds a logo is drawn from (GPUI's image loader). T3 also
/// accepts `.ico`, `.icns` and `.avif`; those show the monogram.
const LOGO_EXTENSIONS: [&str; 6] = ["svg", "png", "jpg", "jpeg", "gif", "webp"];

/// T3's `faviconPath` as a file on this machine: under the vault folder
/// when the T3 project is rooted there, or under T3's workspace root when
/// that checkout is on this machine (the T3 host itself).
pub fn t3_logo(
    folder: &Path,
    vault_relative: &str,
    t3_project: &crate::Project,
) -> Option<PathBuf> {
    let relative = favicon_relative(t3_project)?;
    if same_folder(&t3_project.workspace_root, vault_relative) {
        let path = folder.join(&relative);
        if usable_logo(&path) {
            return Some(path);
        }
    }
    let root = Path::new(t3_project.workspace_root.trim());
    if root.is_absolute() && root.is_dir() {
        let path = root.join(&relative);
        if usable_logo(&path) {
            return Some(path);
        }
    }
    None
}

/// Where the repository host serves T3's `faviconPath` of the default
/// branch: GitHub's raw host, or a Forgejo/Gitea API raw file. `None` for
/// other hosts or without a repository identity.
pub fn remote_logo_url(t3_project: &crate::Project) -> Option<String> {
    let repo = t3_project.repository_identity.as_ref()?;
    let relative = favicon_relative(t3_project)?;
    let segments: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let provider = repo.provider.as_deref()?.to_ascii_lowercase();
    let web = url::Url::parse(repo.web_url.as_deref()?).ok()?;
    if !matches!(web.scheme(), "https" | "http") {
        return None;
    }
    let path: Vec<String> = web
        .path_segments()?
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let (owner, name) = match (repo.owner.as_deref(), repo.name.as_deref(), path.as_slice()) {
        (Some(owner), Some(name), _) => (owner.to_owned(), name.to_owned()),
        (_, _, [owner, name]) => (owner.clone(), name.clone()),
        _ => return None,
    };
    let mut url = match provider.as_str() {
        "github" => {
            let mut url = url::Url::parse("https://raw.githubusercontent.com/").ok()?;
            url.path_segments_mut().ok()?.pop_if_empty().extend([
                owner.as_str(),
                name.as_str(),
                "HEAD",
            ]);
            url
        }
        "forgejo" | "gitea" => {
            let mut url = web.clone();
            url.set_query(None);
            url.set_fragment(None);
            url.path_segments_mut().ok()?.clear().extend([
                "api",
                "v1",
                "repos",
                owner.as_str(),
                name.as_str(),
                "raw",
            ]);
            url
        }
        _ => return None,
    };
    url.path_segments_mut().ok()?.extend(segments.iter());
    Some(url.into())
}

/// A remote logo read again after this long; a missing one is not asked
/// for again sooner.
const LOGO_CACHE_SECS: u64 = 24 * 60 * 60;

/// T3's logo for a repository whose checkout is not on this machine,
/// fetched from the repository host once a day into `cache`. A failed
/// fetch keeps the last copy. Blocks; run it off the UI thread.
pub fn cached_remote_logo(cache: &Path, t3_project: &crate::Project) -> Option<PathBuf> {
    use sha2::Digest as _;
    let url = remote_logo_url(t3_project)?;
    let extension = favicon_relative(t3_project)?
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase();
    let key: String = sha2::Sha256::digest(url.as_bytes())[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = cache.join(format!("{key}.{extension}"));
    let missing = cache.join(format!("{key}.missing"));
    let fresh = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() < LOGO_CACHE_SECS)
    };
    let kept = || usable_logo(&path).then(|| path.clone());
    if (usable_logo(&path) && fresh(&path)) || fresh(&missing) {
        return kept();
    }
    match fetch_logo(&url) {
        Ok(bytes) => {
            std::fs::create_dir_all(cache).ok()?;
            let temp = cache.join(format!(".{key}.{}.tmp", uuid::Uuid::new_v4()));
            std::fs::write(&temp, &bytes).ok()?;
            if std::fs::rename(&temp, &path).is_err() {
                let _ = std::fs::remove_file(&temp);
                return kept();
            }
            let _ = std::fs::remove_file(&missing);
            Some(path)
        }
        Err(error) => {
            eprintln!("Project logo: {url}: {error:#}");
            if std::fs::create_dir_all(cache).is_ok() {
                let _ = std::fs::write(&missing, b"");
            }
            kept()
        }
    }
}

fn fetch_logo(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("okilum/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let response = client.get(url).send()?;
    ensure!(response.status().is_success(), "HTTP {}", response.status());
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= LOGO_MAX_BYTES),
        "larger than a logo"
    );
    let bytes = response.bytes()?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= LOGO_MAX_BYTES,
        "not a logo ({} bytes)",
        bytes.len()
    );
    Ok(bytes.to_vec())
}

/// A vault project's T3 counterpart, as last read from T3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counterpart {
    /// The connection alias (`t3.environment`).
    pub environment: String,
    pub project_id: String,
    /// T3's manual `projectIcon`.
    pub icon: Option<ProjectIcon>,
    /// T3's `faviconPath` as a file here, when the T3 project is rooted at
    /// the vault folder.
    pub logo: Option<PathBuf>,
}

impl Counterpart {
    fn new(
        environment: &str,
        folder: &Path,
        vault_relative: &str,
        t3_project: &crate::Project,
        logos: Option<&Path>,
    ) -> Self {
        // A manual icon wins; the logo is only looked for without one.
        let logo = match t3_project.project_icon {
            Some(_) => None,
            None => t3_logo(folder, vault_relative, t3_project)
                .or_else(|| logos.and_then(|cache| cached_remote_logo(cache, t3_project))),
        };
        Self {
            environment: environment.to_owned(),
            project_id: t3_project.id.clone(),
            icon: t3_project.project_icon.clone(),
            logo,
        }
    }
}

/// `folder` relative to the vault, `/`-separated; `None` outside it.
pub fn vault_relative(vault: &Path, folder: &Path) -> Option<String> {
    let relative = folder.strip_prefix(vault).ok()?;
    let parts: Vec<_> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// The T3 counterparts of `projects`: one listing per environment. A
/// project whose environment could not be listed is absent, and keeps its
/// project.yaml icon until T3 answers.
pub fn counterparts(
    vault: &Path,
    projects: &[super::Project],
    gateways: &std::collections::BTreeMap<String, std::sync::Arc<dyn crate::ManagerGateway>>,
) -> std::collections::BTreeMap<PathBuf, Counterpart> {
    counterparts_with_logos(vault, projects, gateways, None)
}

/// `counterparts`, with T3 logos whose checkout is elsewhere fetched from
/// the repository host into `logos` (a cache folder).
pub fn counterparts_with_logos(
    vault: &Path,
    projects: &[super::Project],
    gateways: &std::collections::BTreeMap<String, std::sync::Arc<dyn crate::ManagerGateway>>,
    logos: Option<&Path>,
) -> std::collections::BTreeMap<PathBuf, Counterpart> {
    let mut listings = std::collections::BTreeMap::<String, Option<Vec<crate::Project>>>::new();
    let mut result = std::collections::BTreeMap::new();
    for project in projects {
        let Some(alias) = project.environment.as_ref() else {
            continue;
        };
        let Some(gateway) = gateways.get(alias) else {
            continue;
        };
        let listing = listings
            .entry(alias.clone())
            .or_insert_with(|| gateway.list_projects().ok());
        let (Some(listing), Some(relative)) =
            (listing.as_ref(), vault_relative(vault, &project.folder))
        else {
            continue;
        };
        if let Some(t3_project) = t3_counterpart(project, &relative, listing) {
            result.insert(
                project.folder.clone(),
                Counterpart::new(alias, &project.folder, &relative, t3_project, logos),
            );
        }
    }
    result
}

/// Where a chosen icon went.
#[derive(Clone, Debug)]
pub enum Saved {
    /// T3's `projectIcon`, read back from T3.
    T3(Counterpart),
    /// `icon:` in project.yaml, with the project read back.
    Manifest(Box<super::Project>),
}

/// Store a manual icon (`None` resets to automatic) where it belongs: in
/// T3 when the project has a T3 counterpart there, in project.yaml
/// otherwise. A project bound to a T3 executor project is never written to
/// project.yaml behind T3's back: without a connection it is an error.
/// Blocks; run it off the UI thread.
pub fn save_icon(
    vault: &Path,
    project: &super::Project,
    gateway: Option<&dyn crate::ManagerGateway>,
    icon: Option<&ProjectIcon>,
) -> Result<Saved> {
    save_icon_with_logos(vault, project, gateway, icon, None)
}

/// `save_icon`; a reset reads the logo with `counterparts_with_logos`.
pub fn save_icon_with_logos(
    vault: &Path,
    project: &super::Project,
    gateway: Option<&dyn crate::ManagerGateway>,
    icon: Option<&ProjectIcon>,
    logos: Option<&Path>,
) -> Result<Saved> {
    let relative =
        vault_relative(vault, &project.folder).context("Project is outside this vault")?;
    match (gateway, project.environment.as_deref()) {
        (Some(gateway), Some(alias)) => {
            let listing = gateway
                .list_projects()
                .map_err(|e| anyhow::anyhow!("{e}; the icon was not changed"))?;
            if let Some(t3_project) = t3_counterpart(project, &relative, &listing) {
                let updated = gateway
                    .set_project_icon(&t3_project.id, icon)
                    .map_err(|e| anyhow::anyhow!("{e}; the icon was not changed"))?;
                return Ok(Saved::T3(Counterpart::new(
                    alias,
                    &project.folder,
                    &relative,
                    &updated,
                    logos,
                )));
            }
            // A project bound to a T3 project T3 does not list is not
            // given a project.yaml icon T3 would never show.
            if let Some(id) = project.appearance.executor_project.as_deref() {
                bail!(
                    "T3 has no project {id} for {}; the icon was not changed",
                    project.name
                )
            }
        }
        _ if project.appearance.executor_project.is_some() => {
            bail!("Connect to T3 to change this project's icon: T3 keeps it")
        }
        _ => {}
    }
    save_manifest_icon(&project.folder, icon)?;
    Ok(Saved::Manifest(Box::new(super::Project::read(
        &project.folder,
    )?)))
}

/// Lines ending in `\r\n` keep that ending.
fn line_ending(source: &str) -> &'static str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Replace, add or remove the top-level `icon:` entry of a project.yaml
/// source, leaving every other byte as it was. The result is parsed back:
/// every other key must read the same and the icon must read as written.
pub fn write_manifest_icon(source: &str, icon: Option<&ProjectIcon>) -> Result<String> {
    let before: serde_yaml::Value = serde_yaml::from_str(source).context("project.yaml")?;
    ensure!(before.is_mapping(), "project.yaml must be a mapping");
    let eol = line_ending(source);
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let start = lines.iter().position(|line| {
        line.strip_prefix("icon")
            .map(str::trim_start)
            .is_some_and(|rest| rest.starts_with(':'))
    });
    let mut out = String::with_capacity(source.len() + 64);
    let entry = icon.map(|icon| format!("icon: {}", icon.to_yaml_flow()));
    match start {
        Some(start) => {
            // The entry's own block: the key line and the indented lines
            // under it. A blank line or the next column-0 line ends it.
            let mut end = start + 1;
            while end < lines.len()
                && lines[end].starts_with([' ', '\t'])
                && !lines[end].trim().is_empty()
            {
                end += 1;
            }
            lines[..start].iter().for_each(|l| out.push_str(l));
            if let Some(entry) = &entry {
                out.push_str(entry);
                if lines[end - 1].ends_with('\n') {
                    out.push_str(eol);
                }
            }
            lines[end..].iter().for_each(|l| out.push_str(l));
        }
        None => {
            out.push_str(source);
            if let Some(entry) = &entry {
                if !source.is_empty() && !source.ends_with('\n') {
                    out.push_str(eol);
                }
                out.push_str(entry);
                out.push_str(eol);
            }
        }
    }
    let after: serde_yaml::Value =
        serde_yaml::from_str(&out).context("the edited project.yaml does not parse")?;
    let (Some(before), Some(after_map)) = (before.as_mapping(), after.as_mapping()) else {
        bail!("the edited project.yaml is not a mapping");
    };
    let key = serde_yaml::Value::from("icon");
    let others = |map: &serde_yaml::Mapping| {
        map.iter()
            .filter(|(k, _)| **k != key)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>()
    };
    ensure!(
        others(before) == others(after_map),
        "writing the icon would change other project.yaml fields"
    );
    let written = after_map.get(&key).and_then(ProjectIcon::from_yaml);
    ensure!(
        written.as_ref() == icon,
        "the icon did not read back as written"
    );
    Ok(out)
}

/// Write `icon:` into `<folder>/ops/project.yaml` through a temporary file
/// in the same folder, so a reader never sees half a manifest.
pub fn save_manifest_icon(folder: &Path, icon: Option<&ProjectIcon>) -> Result<()> {
    let ops = folder.join("ops");
    ensure!(
        std::fs::symlink_metadata(&ops)?.is_dir(),
        "Project ops must not be a symlink"
    );
    let manifest = ops.join("project.yaml");
    ensure!(
        std::fs::symlink_metadata(&manifest)?.is_file(),
        "Project manifest must not be a symlink"
    );
    let source = std::fs::read_to_string(&manifest)?;
    let updated = write_manifest_icon(&source, icon)?;
    if updated == source {
        return Ok(());
    }
    let temp = ops.join(format!(".project.yaml.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, updated.as_bytes())?;
    if let Ok(metadata) = std::fs::metadata(&manifest) {
        let _ = std::fs::set_permissions(&temp, metadata.permissions());
    }
    std::fs::rename(&temp, &manifest).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })?;
    Ok(())
}

/// The dialog's three tabs, T3's «Choose project icon» plus Monogram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerTab {
    Icons,
    Emoji,
    Monogram,
}

impl PickerTab {
    pub const ALL: [PickerTab; 3] = [Self::Icons, Self::Emoji, Self::Monogram];
    pub fn label(self) -> &'static str {
        match self {
            Self::Icons => "Icons",
            Self::Emoji => "Emoji",
            Self::Monogram => "Monogram",
        }
    }
}

/// T3's emoji grid, in T3's order.
pub const PROJECT_EMOJIS: [(&str, &str); 30] = [
    ("💻", "Computer"),
    ("🛠️", "Tools"),
    ("🚀", "Rocket"),
    ("🤖", "Robot"),
    ("✨", "Sparkles"),
    ("⚡", "Lightning"),
    ("🌐", "Web"),
    ("📱", "Mobile"),
    ("🖥️", "Desktop"),
    ("⌨️", "Keyboard"),
    ("⚙️", "Gear"),
    ("🗄️", "Database"),
    ("☁️", "Cloud"),
    ("📦", "Package"),
    ("📚", "Books"),
    ("🧪", "Test tube"),
    ("🔒", "Lock"),
    ("🎮", "Game"),
    ("🎵", "Music"),
    ("🎬", "Movie"),
    ("🖼️", "Picture"),
    ("🛍️", "Shopping"),
    ("🔥", "Fire"),
    ("💡", "Idea"),
    ("🧩", "Puzzle"),
    ("📊", "Chart"),
    ("🧠", "Brain"),
    ("🦄", "Unicorn"),
    ("🐙", "Octopus"),
    ("🌱", "Seedling"),
];

/// T3's popular icons first when the search is empty; those this build
/// does not carry are skipped.
pub const POPULAR_ICONS: [&str; 24] = [
    "folder-code",
    "code-xml",
    "square-terminal",
    "globe",
    "server",
    "hard-drive",
    "bot",
    "cpu",
    "monitor",
    "network",
    "book-open",
    "file-text",
    "map",
    "inbox",
    "calendar",
    "building-2",
    "chart-pie",
    "palette",
    "heart",
    "star",
    "bell",
    "layout-dashboard",
    "folder",
    "settings",
];

/// The icon names a search shows: popular first, then the rest by name;
/// with a query, every name containing it (spaces read as `-`).
pub fn filter_icons(available: &[String], query: &str) -> Vec<String> {
    let normalized = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    let mut names: Vec<String> = Vec::new();
    if normalized.is_empty() {
        names.extend(
            POPULAR_ICONS
                .iter()
                .filter(|name| available.iter().any(|a| a == *name))
                .map(|name| (*name).to_owned()),
        );
    }
    let mut rest: Vec<String> = available
        .iter()
        .filter(|name| !names.contains(name))
        .filter(|name| normalized.is_empty() || name.contains(&normalized))
        .cloned()
        .collect();
    rest.sort();
    rest.dedup();
    names.extend(rest);
    names
}

/// The first emoji of pasted text, joiners, variation selectors, skin
/// tones, keycaps and flag pairs included; `None` for plain text.
pub fn first_emoji(value: &str) -> Option<String> {
    let mut chars = value.trim().chars().peekable();
    let first = chars.next()?;
    let pictographic = |c: char| {
        let u = c as u32;
        (0x1F000..=0x1FAFF).contains(&u)
            || (0x2600..=0x27BF).contains(&u)
            || (0x2300..=0x23FF).contains(&u)
            || (0x2B00..=0x2BFF).contains(&u)
            || matches!(
                u,
                0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x3030 | 0x303D
            )
    };
    let regional = |c: char| (0x1F1E6..=0x1F1FF).contains(&(c as u32));
    let keycap_base = first.is_ascii_digit() || first == '#' || first == '*';
    if !pictographic(first) && !keycap_base {
        return None;
    }
    let mut emoji = String::from(first);
    if regional(first) {
        if let Some(&next) = chars.peek() {
            if regional(next) {
                emoji.push(next);
            }
        }
        return Some(emoji);
    }
    let mut joined = false;
    while let Some(&c) = chars.peek() {
        let u = c as u32;
        let extends = matches!(u, 0xFE0E | 0xFE0F | 0x20E3)
            || (0x1F3FB..=0x1F3FF).contains(&u)
            || (0xE0020..=0xE007F).contains(&u);
        if extends {
            emoji.push(c);
        } else if u == 0x200D {
            emoji.push(c);
            joined = true;
        } else if joined && pictographic(c) {
            emoji.push(c);
            joined = false;
        } else {
            break;
        }
        chars.next();
    }
    if keycap_base && !emoji.ends_with('\u{20E3}') {
        return None;
    }
    Some(emoji)
}

/// The dialog's state, kept apart from rendering so it can be tested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconDraft {
    pub tab: PickerTab,
    pub color: IconColor,
    pub lucide: String,
    pub emoji: String,
    pub monogram: String,
}

/// The Icons tab's starting icon when there is no manual one.
pub const DEFAULT_LUCIDE: &str = "folder";

impl IconDraft {
    /// Opens on the current manual icon's tab and values; otherwise on
    /// Icons, with the project's own monogram letters and colour ready.
    pub fn new(current: Option<&ProjectIcon>, project_name: &str) -> Self {
        let mut draft = Self {
            tab: PickerTab::Icons,
            color: monogram_color(project_name),
            lucide: DEFAULT_LUCIDE.into(),
            emoji: "💻".into(),
            monogram: monogram(project_name),
        };
        match current {
            Some(ProjectIcon::Lucide { name, color }) => {
                draft.lucide = name.clone();
                draft.color = *color;
            }
            Some(ProjectIcon::Emoji { emoji }) => {
                draft.tab = PickerTab::Emoji;
                draft.emoji = emoji.clone();
            }
            Some(ProjectIcon::Monogram { text, color }) => {
                draft.tab = PickerTab::Monogram;
                draft.monogram = text.clone();
                draft.color = *color;
            }
            None => {}
        }
        draft
    }

    /// Keeps at most two non-space characters of what was typed.
    pub fn set_monogram(&mut self, text: &str) {
        self.monogram = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(2)
            .collect();
    }

    /// A pasted emoji replaces the selection; plain text is ignored.
    pub fn paste_emoji(&mut self, text: &str) -> bool {
        match first_emoji(text) {
            Some(emoji) => {
                self.emoji = emoji;
                true
            }
            None => false,
        }
    }

    /// What Save icon stores; `None` while the current tab has nothing
    /// usable (an empty monogram).
    pub fn result(&self) -> Option<ProjectIcon> {
        match self.tab {
            PickerTab::Icons => ProjectIcon::Lucide {
                name: self.lucide.clone(),
                color: self.color,
            },
            PickerTab::Emoji => ProjectIcon::Emoji {
                emoji: self.emoji.clone(),
            },
            PickerTab::Monogram => ProjectIcon::Monogram {
                text: self.monogram.clone(),
                color: self.color,
            },
        }
        .validated()
    }
}

#[cfg(test)]
mod tests;
