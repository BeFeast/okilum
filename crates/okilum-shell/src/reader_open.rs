//! Validated, local-only Reader entry points. Validation never creates files.
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenIntent {
    pub root: PathBuf,
    pub single_file: bool,
    pub note: Option<String>,
}

impl OpenIntent {
    pub fn validate(
        path: &Path,
        explicit_root: Option<&Path>,
        reusable_root: Option<&Path>,
    ) -> Result<Self> {
        Self::validate_inner(path, explicit_root, reusable_root, true)
    }

    /// A validated snapshot supplies the bytes; still check current path identity
    /// and containment, without hydrating a cloud-backed primary document.
    pub(crate) fn validate_cached(
        path: &Path,
        explicit_root: Option<&Path>,
        reusable_root: Option<&Path>,
    ) -> Result<Self> {
        Self::validate_inner(path, explicit_root, reusable_root, false)
    }

    fn validate_inner(
        path: &Path,
        explicit_root: Option<&Path>,
        reusable_root: Option<&Path>,
        read_primary: bool,
    ) -> Result<Self> {
        let path = okilum_core::vault::canonical_root(path)
            .context("The requested path is missing or inaccessible")?;
        let explicit_root = explicit_root
            .map(|root| {
                let root = okilum_core::vault::canonical_root(root)
                    .context("The selected root is inaccessible")?;
                if !root.is_dir() {
                    bail!("The selected root must be a directory");
                }
                if !path.starts_with(&root) {
                    bail!("The requested path is outside the selected root");
                }
                Ok(root)
            })
            .transpose()?;
        if path.is_dir() {
            std::fs::read_dir(&path).context("The requested directory cannot be read")?;
            if explicit_root.as_ref().is_some_and(|root| root != &path) {
                bail!("A folder and a different explicit root are contradictory");
            }
            return Ok(Self {
                root: path,
                single_file: false,
                note: None,
            });
        }
        let log = okilum_core::log::is_log_path(&path);
        let plain = super::reader_delimited::editable(&path.to_string_lossy());
        let markdown = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
        // Inside an explicitly chosen vault any file opens as it would from the file
        // tree (PDF, image, archive…); a lone file still has to be a document kind.
        let in_vault = explicit_root.is_some();
        if !path.is_file() || !(log || plain || markdown || in_vault) {
            bail!("Choose a local Markdown, CSV, TSV, text, code or log file, or a folder");
        }
        // Fail before constructing a Reader rather than falling back to its first note.
        // A log is bytes, not UTF-8 text: only readability is required.
        if read_primary && (log || plain || !markdown) {
            std::fs::File::open(&path).context("The file cannot be read")?;
        } else if read_primary {
            std::fs::read_to_string(&path).context("The Markdown file cannot be read as UTF-8")?;
        }
        let parent = path
            .parent()
            .context("The document has no containing directory")?;
        let reusable = reusable_root
            .and_then(|root| okilum_core::vault::canonical_root(root).ok())
            .filter(|root| root.is_dir() && path.starts_with(root));
        let single_file = explicit_root.is_none() && reusable.is_none();
        let root = explicit_root
            .or(reusable)
            .unwrap_or_else(|| parent.to_path_buf());
        let relative = path.strip_prefix(&root)?;
        relative
            .to_str()
            .context("The document path is not valid UTF-8")?;
        let note = okilum_core::vault::note_path(relative);
        Ok(Self {
            single_file,
            root,
            note: Some(note),
        })
    }

    /// The caller provides the OS application cache base, never the document root.
    pub fn cache_path(&self, cache_base: &Path) -> PathBuf {
        let digest = Sha256::digest(self.root.as_os_str().as_encoded_bytes());
        cache_base
            .join("okilum")
            .join("reader")
            .join(format!("{digest:x}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("okilum-open-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(p.join("notes space")).unwrap();
            std::fs::write(
                p.join("notes space/Заметка.md"),
                "# Landing\n\n[sibling](same.md)\n",
            )
            .unwrap();
            std::fs::write(p.join("notes space/same.md"), "sibling").unwrap();
            std::fs::write(p.join("same.md"), "root collision").unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    /// #1129: a second vault opened in an isolated instance (QA, a portable copy) must not go to
    /// the user's shared cache, whose retention would evict the user's own Reader caches.
    #[test]
    fn an_isolated_instance_keeps_every_vault_cache_and_its_retention_to_itself() {
        use crate::reader_cache::Lease;
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("qa-state");
        let shared = temp.path().join("user-cache");
        let vault = |name: &str| {
            let root = temp.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            root
        };
        let intent = |root: &Path| OpenIntent {
            root: root.to_path_buf(),
            single_file: false,
            note: None,
        };

        // Not isolated: the OS cache (positive control that the two scopes differ).
        let os_base = cache_base_with(None).unwrap();
        assert!(!os_base.starts_with(&state));
        // Isolated: every vault, whichever window opens it, resolves under the state directory.
        let isolated = cache_base_with(Some(state.clone())).unwrap();
        assert_eq!(isolated, state.join("cache"));
        let (a, b) = (vault("vault-a"), vault("vault-b"));
        for root in [&a, &b] {
            let path = intent(root).cache_path(&isolated);
            assert!(
                path.starts_with(state.join("cache/okilum/reader")),
                "{path:?}"
            );
            assert_ne!(path, intent(root).cache_path(&os_base));
        }

        // Retention orders caches by the modification time of their `.usage/<hash>.opened`
        // marker. Set explicit times instead of waiting for the clock to tick: how far apart two
        // writes land depends on the filesystem's timestamp granularity and on how loaded the
        // machine is, and a test that relies on it fails now and then (#1171).
        let opened_at = |path: &Path, seconds: u64| {
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            let marker = path
                .parent()
                .unwrap()
                .join(".usage")
                .join(format!("{name}.opened"));
            std::fs::OpenOptions::new()
                .write(true)
                .open(marker)
                .unwrap()
                .set_modified(
                    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                )
                .unwrap();
        };
        // The user's own Reader holds three registered vault caches in the shared base.
        let mut theirs = Vec::new();
        for n in 0..3u64 {
            let root = vault(&format!("theirs-{n}"));
            let path = intent(&root).cache_path(&shared);
            let lease = Lease::acquire(path.clone(), &root).unwrap();
            lease.mark_published().unwrap();
            opened_at(&path, 1_000 + n);
            std::fs::write(path.join("reader-startup.json"), "derived cache").unwrap();
            theirs.push(path);
        }
        // The isolated instance opens four vaults, the first the least recently opened;
        // retention (three kept) must evict exactly that one, among its own caches.
        let mut evicted = Vec::new();
        let mut mine = Vec::new();
        for n in 0..4u64 {
            let root = vault(&format!("mine-{n}"));
            let path = intent(&root).cache_path(&isolated);
            let lease = Lease::acquire(path.clone(), &root).unwrap();
            lease.mark_published().unwrap();
            opened_at(&path, 2_000 + n);
            evicted.extend(lease.prune().unwrap().removed);
            mine.push(path);
        }
        assert_eq!(
            evicted,
            [mine[0].clone()],
            "positive control: retention ran, and evicted the oldest of its own caches"
        );
        assert!(evicted[0].starts_with(state.join("cache")), "{evicted:?}");
        for path in theirs {
            assert!(
                path.join("reader-startup.json").is_file(),
                "the user's cache {path:?} is untouched"
            );
        }
    }

    #[test]
    fn exact_file_and_root_precedence_do_not_select_namesake() {
        let f = Fixture::new();
        let file = f.0.join("notes space/Заметка.md");
        let before = std::fs::read(&file).unwrap();
        let alone = OpenIntent::validate(&file, None, None).unwrap();
        assert_eq!(alone.root, f.0.join("notes space"));
        assert_eq!(alone.note.as_deref(), Some("Заметка.md"));
        std::fs::create_dir(f.0.join(".obsidian")).unwrap();
        let hinted = OpenIntent::validate(&file, None, None).unwrap();
        assert_eq!(hinted, alone);
        assert!(hinted.single_file);
        let explicit = OpenIntent::validate(&file, Some(&alone.root), Some(&f.0)).unwrap();
        assert!(!explicit.single_file);
        assert_eq!(explicit.root, alone.root);
        assert_eq!(
            OpenIntent::validate(&file, None, Some(&alone.root)).unwrap(),
            explicit
        );
        assert_eq!(std::fs::read(file).unwrap(), before);
        assert!(!f.0.join(".okilum-index").exists());
        assert!(!hinted
            .cache_path(&std::env::temp_dir().join("cache"))
            .starts_with(&f.0));
    }
    #[test]
    fn refusals_have_successful_exact_open_positive_control() {
        let f = Fixture::new();
        let file = f.0.join("same.md");
        assert_eq!(
            OpenIntent::validate(&file, Some(&f.0), None)
                .unwrap()
                .note
                .as_deref(),
            Some("same.md")
        );
        assert!(
            OpenIntent::validate(&file, Some(&f.0.join("notes space")), None)
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
        assert!(OpenIntent::validate(&f.0.join("absent.md"), None, None).is_err());
        std::fs::write(f.0.join("other.txt"), "text").unwrap();
        assert!(OpenIntent::validate(&f.0.join("other.txt"), None, None).is_ok());
        std::fs::write(f.0.join("other.bin"), "text").unwrap();
        assert!(OpenIntent::validate(&f.0.join("other.bin"), None, None).is_err());
        assert!(OpenIntent::validate(&f.0.join("notes space"), Some(&f.0), None).is_err());
        assert_eq!(OpenIntent::validate(&f.0, None, None).unwrap().root, f.0);
    }
    #[test]
    fn log_files_open_without_utf8_and_other_files_still_refuse() {
        let f = Fixture::new();
        let names = ["service.log", "events.JSONL", "events.ndjson", "app.logfmt"];
        for name in names {
            // Invalid UTF-8 is fine for a log; the Markdown path would refuse it.
            std::fs::write(f.0.join(name), b"{\"level\":\"info\"}\n\xff\xfe\n").unwrap();
            let intent = OpenIntent::validate(&f.0.join(name), None, None).unwrap();
            assert!(intent.single_file, "{name}");
            assert_eq!(intent.note.as_deref(), Some(name));
            assert_eq!(intent.root, f.0.canonicalize().unwrap());
        }
        std::fs::write(f.0.join("binary.md"), b"\xff\xfe").unwrap();
        assert!(OpenIntent::validate(&f.0.join("binary.md"), None, None).is_err());
        // Code files open directly since #998; other kinds stay rejected.
        std::fs::write(f.0.join("data.json"), b"{}").unwrap();
        assert!(
            OpenIntent::validate(&f.0.join("data.json"), None, None)
                .unwrap()
                .single_file
        );
        std::fs::write(f.0.join("archive.zip"), b"PK").unwrap();
        assert!(OpenIntent::validate(&f.0.join("archive.zip"), None, None)
            .unwrap_err()
            .to_string()
            .contains("code or log file"));
        assert!(OpenIntent::validate(&f.0.join("absent.log"), None, None).is_err());
    }
    #[test]
    fn any_file_opens_inside_an_explicit_vault() {
        // `--vault <v> --note x.zip` opens like a click in the file tree (#1080).
        let f = Fixture::new();
        for name in ["archive.zip", "scan.pdf", "photo.png"] {
            std::fs::write(f.0.join(name), b"PK").unwrap();
            let intent = OpenIntent::validate(&f.0.join(name), Some(&f.0), None).unwrap();
            assert!(!intent.single_file, "{name}");
            assert_eq!(intent.note.as_deref(), Some(name));
        }
        assert!(OpenIntent::validate(&f.0.join("absent.zip"), Some(&f.0), None).is_err());
    }
}

pub(crate) enum Command {
    Help,
    Launch(Box<super::Opts>),
    /// Remove this user's Okilum data, keeping vaults (#974).
    UninstallData {
        assume_yes: bool,
    },
    /// Print what the binary was built with, for QA tooling (#1043).
    BuildInfo,
}

pub(crate) fn parse_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
    mut opts: super::Opts,
) -> Result<Command> {
    let mut args = args.into_iter();
    let mut positional = None;
    let mut literal = false;
    let mut explicit_reader = false;
    while let Some(arg) = args.next() {
        if !literal && arg == "--" {
            literal = true;
            continue;
        }
        if !literal && (arg == "--help" || arg == "-h") {
            return Ok(Command::Help);
        }
        if !literal && arg == "--build-info" {
            if args.next().is_some() {
                bail!("--build-info takes no other arguments");
            }
            return Ok(Command::BuildInfo);
        }
        if !literal && arg == "--uninstall-data" {
            // Standalone: never combined with a document or vault launch.
            let rest: Vec<_> = args.by_ref().collect();
            return match rest
                .iter()
                .map(|a| a.to_str())
                .collect::<Vec<_>>()
                .as_slice()
            {
                [] => Ok(Command::UninstallData { assume_yes: false }),
                [Some("--yes")] => Ok(Command::UninstallData { assume_yes: true }),
                _ => bail!("--uninstall-data accepts only --yes"),
            };
        }
        let mut value = || {
            args.next()
                .context("An option is missing its required value")
        };
        if !literal {
            match arg.to_str() {
                Some("--vault") => {
                    explicit_reader = true;
                    opts.vault = Some(value()?.into());
                    continue;
                }
                Some("--index-dir") => {
                    opts.index_dir = Some(value()?.into());
                    continue;
                }
                Some("--note") => {
                    explicit_reader = true;
                    let note = value()?
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("Note path must be UTF-8"))?;
                    opts.note = Some(okilum_core::vault::note_path(Path::new(&note)));
                    continue;
                }
                Some("--query") => {
                    opts.query = Some(
                        value()?
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("Query must be UTF-8"))?,
                    );
                    continue;
                }
                Some("--managed-workspace") => {
                    opts.managed_workspace = true;
                    continue;
                }
                Some("--brain-endpoint") => {
                    let address: std::net::SocketAddr =
                        value()?.to_str().context("Invalid endpoint")?.parse()?;
                    if !address.ip().is_loopback() {
                        bail!("--brain-endpoint requires a loopback IP:PORT");
                    }
                    opts.brain_endpoint = Some(address);
                    continue;
                }
                Some("--html") => {
                    opts.use_html = true;
                    continue;
                }
                Some("--jump") => {
                    opts.jump = true;
                    continue;
                }
                Some("--copy-source") => {
                    opts.copy_source = true;
                    continue;
                }
                Some(s) if s.starts_with('-') => bail!("Unknown option: {s}"),
                _ => {}
            }
        }
        // `okilum okilum://…` (OS handlers pass the clicked link) (#1049).
        if !literal && arg.to_str().is_some_and(|a| has_scheme(a, "okilum:")) {
            if opts
                .link
                .replace(arg.to_string_lossy().into_owned())
                .is_some()
            {
                bail!("Open one link per CLI invocation");
            }
            continue;
        }
        // `%u` in the .desktop file may hand a local file as a file: URL.
        let arg = match arg.to_str() {
            Some(a) if !literal && has_scheme(a, "file:") => file_url_path(a)?.into_os_string(),
            _ => arg,
        };
        // Several log files open together in one view (#1160); anything
        // else is one path per invocation.
        let path = PathBuf::from(arg);
        match &positional {
            None => positional = Some(path),
            Some(first)
                if okilum_core::log::is_log_path(first) && okilum_core::log::is_log_path(&path) =>
            {
                opts.extra_logs.push(path)
            }
            Some(_) => {
                bail!("Open one file or folder per CLI invocation; several log files open together")
            }
        }
    }
    #[cfg(not(all(unix, feature = "brain")))]
    if opts.brain_endpoint.is_some() || opts.managed_workspace {
        bail!("Brain and managed workspace are unavailable in this Reader build");
    }
    if opts.brain_endpoint.is_some() || opts.managed_workspace {
        if positional.is_some() || explicit_reader {
            bail!("A Reader path cannot be combined with a managed launch");
        }
        // An explicit managed launch takes precedence over inherited Reader defaults.
        opts.vault = None;
    }
    if let Some(path) = positional {
        if opts.note.is_some() {
            bail!("A positional path cannot be combined with --note");
        }
        if opts.jump {
            bail!("--jump cannot replace an explicitly requested document");
        }
        opts.open_path = Some(path);
    } else if opts.vault.is_some() {
        if opts
            .note
            .as_ref()
            .is_some_and(|note| Path::new(note).is_absolute())
        {
            bail!("--note must be relative to --vault");
        }
    } else if opts.note.is_some() {
        bail!("--note requires --vault");
    }
    Ok(Command::Launch(Box::new(opts)))
}

/// Where derived caches live. An isolated instance (an absolute `OKILUM_STATE_DIR`: QA, a
/// portable copy) owns them under `<state>/cache`, exactly as it owns its config under
/// `<state>/config`, for every window and the first-run folder picker. Without that, a second
/// vault opened in such an instance went to the user's shared cache, and its retention (the three
/// most recently opened vaults) evicted caches that belong to the user's own Reader (#1129).
pub(crate) fn cache_base() -> Result<PathBuf> {
    cache_base_with(crate::reader_history::isolated_state_directory())
}

fn cache_base_with(isolated_state: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(state) = isolated_state {
        return Ok(state.join("cache"));
    }
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
    #[cfg(windows)]
    let base = dirs::cache_dir();
    base.filter(|p| p.is_absolute())
        .context("No absolute application cache directory is available; supply --index-dir")
}

/// Compute without touching the filesystem; preparation validates before writes.
pub(crate) fn cache_candidate_path(root: &Path) -> Result<PathBuf> {
    Ok(OpenIntent {
        root: root.to_path_buf(),
        single_file: false,
        note: None,
    }
    .cache_path(&cache_base()?))
}

pub(crate) fn cache_path_for(root: &Path, opts: &super::Opts) -> Result<PathBuf> {
    if let Some(path) = &opts.index_dir {
        return Ok(path.clone());
    }
    #[cfg(test)]
    if let Some(base) = &opts.cache_base_override {
        return Ok(OpenIntent {
            root: root.to_owned(),
            single_file: false,
            note: None,
        }
        .cache_path(base));
    }
    cache_candidate_path(root)
}

/// Compatibility seam for the session owner. Cache validation belongs to the
/// background preparation job, never to creation of a Reader window.
#[allow(dead_code)]
pub(crate) fn apply_intent(opts: &mut super::Opts, intent: &OpenIntent) -> Result<()> {
    opts.vault = Some(intent.root.clone());
    opts.note = intent.note.clone();
    Ok(())
}

/// `value` starts with `scheme` (case-insensitive) and has more after it.
/// `get` keeps a multi-byte first character from panicking the slice.
fn has_scheme(value: &str, scheme: &str) -> bool {
    value.len() > scheme.len()
        && value
            .get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
}

pub(crate) fn file_url_path(value: &str) -> Result<PathBuf> {
    let url = url::Url::parse(value).context("Invalid file-open URL")?;
    if url.scheme() != "file"
        || url.query().is_some()
        || url.fragment().is_some()
        || url
            .host_str()
            .is_some_and(|host| host != "localhost" && !host.is_empty())
    {
        bail!("Only local file URLs without query or fragment can be opened");
    }
    url.to_file_path()
        .map_err(|_| anyhow::anyhow!("The file-open URL is not a local path"))
}

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Root, Sizable, TitleBar,
};

#[derive(Default)]
pub(crate) struct Readers(Vec<(WeakEntity<super::Reader>, PathBuf)>);
impl Global for Readers {}

/// Live Readers of this process, any window.
pub(crate) fn readers(cx: &App) -> Vec<Entity<super::Reader>> {
    cx.try_global::<Readers>()
        .map(|readers| readers.0.iter().filter_map(|(r, _)| r.upgrade()).collect())
        .unwrap_or_default()
}

pub(crate) fn register(reader: WeakEntity<super::Reader>, root: PathBuf, cx: &mut App) {
    if cx.try_global::<Readers>().is_some() {
        let readers = &mut cx.global_mut::<Readers>().0;
        readers.retain(|(old, _)| old.entity_id() != reader.entity_id() && old.upgrade().is_some());
        readers.push((reader, root.clone()));
        super::reader_open_windows::record_soon(cx);
        super::vault_name::watch(root, cx);
    }
}

fn reusable_roots(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<Readers>()
        .map(|readers| {
            readers
                .0
                .iter()
                .rev()
                .filter(|(reader, _)| {
                    reader.upgrade().is_some_and(|r| {
                        let reader = r.read(cx);
                        !reader.single_file && cx.windows().contains(&reader.reader_window)
                    })
                })
                .map(|(_, root)| root.clone())
                .collect()
        })
        .unwrap_or_default()
}

actions!(reader_open, [OpenFile, OpenFolder, NewWindow]);

pub(crate) fn install(cx: &mut App) {
    cx.set_global(Readers::default());
    cx.set_global(super::reader_startup::Startup::default());
    #[cfg(target_os = "macos")]
    let (file_key, folder_key) = ("cmd-o", "cmd-shift-o");
    #[cfg(not(target_os = "macos"))]
    let (file_key, folder_key) = ("ctrl-o", "ctrl-shift-o");
    cx.bind_keys([
        KeyBinding::new(file_key, OpenFile, None),
        KeyBinding::new(folder_key, OpenFolder, None),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new("cmd-shift-n", NewWindow, None)]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-shift-n", NewWindow, None)]);
    cx.on_action(|_: &NewWindow, cx| new_window(cx));
    cx.on_action(|_: &OpenFile, cx| pick(false, cx));
    cx.on_action(|_: &OpenFolder, cx| pick(true, cx));
}

pub(crate) fn file_menu() -> Menu {
    Menu {
        name: "File".into(),
        disabled: false,
        items: vec![
            MenuItem::action("Open File…", OpenFile),
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("New Window", NewWindow),
        ],
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlsPresentation {
    Entry,
    Toolbar,
}

pub(crate) fn controls(presentation: ControlsPresentation) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(
            Button::new("open-reader-file")
                .debug_selector(|| "reader-open-file".into())
                .label("Open file…")
                .when(presentation == ControlsPresentation::Toolbar, |button| {
                    button.small()
                })
                .when(presentation == ControlsPresentation::Entry, |button| {
                    button.primary()
                })
                .on_click(|_, _, cx| pick(false, cx)),
        )
        .child(
            Button::new("open-reader-folder")
                .debug_selector(|| "reader-open-folder".into())
                .label("Open folder…")
                .when(presentation == ControlsPresentation::Toolbar, |button| {
                    button.small()
                })
                .when(presentation == ControlsPresentation::Entry, |button| {
                    button.primary()
                })
                .on_click(|_, _, cx| pick(true, cx)),
        )
}

/// The picker's confirm button. A vault opens editable, so the folder button
/// promises nothing more (#1122, #1116); the same text on every OS.
fn picker_prompt(folder: bool) -> &'static str {
    if folder {
        "Open folder"
    } else {
        "Open Markdown, CSV, TSV, text or log file"
    }
}

fn pick(folder: bool, cx: &mut App) {
    let result = cx.prompt_for_paths(PathPromptOptions {
        files: !folder,
        directories: folder,
        multiple: false,
        prompt: Some(picker_prompt(folder).into()),
    });
    cx.spawn(async move |cx| {
        let result = match result.await {
            Ok(result) => result,
            Err(error) => Err(anyhow::anyhow!("File picker failed: {error}")),
        };
        let result = cx
            .background_executor()
            .spawn(async move { picker_path(result, folder) })
            .await;
        cx.update(|cx| match result {
            Ok(Some(path)) => dispatch_path(&path, cx),
            Ok(None) => {}
            Err(error) => show_error(error, cx),
        });
    })
    .detach();
}

pub(crate) fn picker_path(
    result: Result<Option<Vec<PathBuf>>>,
    folder: bool,
) -> Result<Option<PathBuf>> {
    let Some(paths) = result? else {
        return Ok(None);
    };
    if paths.len() != 1 {
        bail!("Select exactly one file or folder");
    }
    let path = paths.into_iter().next().unwrap();
    let metadata =
        std::fs::metadata(&path).context("The selected path is missing or inaccessible")?;
    if (folder && !metadata.is_dir()) || (!folder && !metadata.is_file()) {
        bail!(
            "Select {}",
            if folder {
                "a directory"
            } else {
                "a Markdown, CSV, TSV, text or log file"
            }
        );
    }
    Ok(Some(path))
}

pub(crate) fn dispatch_urls(urls: Vec<String>, cx: &mut App) {
    for url in urls {
        if has_scheme(&url, "okilum:") {
            open_deep_link(&url, cx);
            continue;
        }
        match file_url_path(&url) {
            Ok(path) => dispatch_path(&path, cx),
            Err(error) => show_error(error, cx),
        }
    }
}

/// Vault roots this machine knows: open windows first, then reading history.
fn known_roots(cx: &App) -> Vec<PathBuf> {
    let mut roots = reusable_roots(cx);
    if let Ok(directory) = super::reader_history::state_directory() {
        if let Ok((_, history)) = super::reader_history::ReadingHistory::startup_roots(&directory) {
            for root in history {
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
    }
    roots
        .into_iter()
        .map(|root| root.canonicalize().unwrap_or(root))
        .collect()
}

/// Open an external `okilum:` link (#1049). Links only navigate: they open a
/// vault the user already opened and a note in it, never create anything.
pub(crate) fn open_deep_link(link: &str, cx: &mut App) {
    let known = known_roots(cx);
    open_deep_link_among(link, &known, cx);
}

/// `open_deep_link` against an explicit list of known vault roots.
fn open_deep_link_among(link: &str, known: &[PathBuf], cx: &mut App) {
    use okilum_core::deep_link::{parse, resolve, Resolution};
    super::reader_startup::supersede(cx);
    let link = match parse(link) {
        Ok(link) => link,
        Err(refused) => return show_error(anyhow::anyhow!(refused.message()), cx),
    };
    match resolve(&link, known, &|path| path.is_file()) {
        Resolution::Open {
            root,
            rel,
            position,
        } => {
            // #1269: the position as it arrived, before any window takes it.
            if let Some(trace) = super::reader_diagnostics::trace(cx) {
                trace.event(
                    "link_open",
                    serde_json::json!({
                        "line": position.line,
                        "column": position.column,
                        "fragment": position.heading.is_some() || position.block.is_some(),
                        "on_screen": reusable_roots(cx).contains(&root),
                        "trusted": super::reader_ui_state::link_trusted(&root, cx),
                    }),
                );
            }
            let opts = super::Opts {
                vault: Some(root.clone()),
                note: Some(rel),
                landing: Some(position),
                reusable_roots: reusable_roots(cx),
                ..Default::default()
            };
            // A vault already on screen, or one the user let links open
            // before, opens at once; the first link to any other asks.
            let open_now = opts.reusable_roots.contains(&root)
                || super::reader_ui_state::link_trusted(&root, cx);
            if open_now {
                if let Err(error) = open_window(opts, cx) {
                    show_error(error, cx);
                }
            } else {
                confirm_link_vault(root, opts, cx);
            }
        }
        // Several vaults share the name: the user picks, never a guess.
        Resolution::Choose {
            vault,
            roots,
            position,
        } => choose_link_vault(vault, roots, position, cx),
        Resolution::Unavailable(message) => show_error(anyhow::anyhow!(message), cx),
    }
}

fn dispatch_path(path: &Path, cx: &mut App) {
    super::reader_startup::supersede(cx);
    let opts = super::Opts {
        open_path: Some(path.to_path_buf()),
        reusable_roots: reusable_roots(cx),
        ..Default::default()
    };
    if let Err(error) = open_window(opts, cx) {
        show_error(error, cx);
    }
}

/// Resolve aliases away from the UI thread, then choose or reserve the window
/// in one UI transaction. A window is registered before preparation starts so
/// simultaneous OS deliveries cannot create duplicate vault readers.
pub(crate) fn open_window(opts: super::Opts, cx: &mut App) -> Result<()> {
    let resolving = resolve(opts, cx);
    cx.spawn(async move |cx| {
        let resolved = resolving.await;
        cx.update(|cx| finish_open(resolved, cx));
    })
    .detach();
    Ok(())
}

/// Validate a request off the UI thread; `finish_open` then shows it.
pub(crate) fn resolve(mut opts: super::Opts, cx: &mut App) -> gpui::Task<Result<super::Opts>> {
    if opts.reusable_roots.is_empty() {
        opts.reusable_roots = reusable_roots(cx);
    }
    cx.background_executor().spawn(async move {
        let path = opts
            .open_path
            .clone()
            .or_else(|| {
                opts.vault.as_ref().map(|root| {
                    opts.note
                        .as_ref()
                        .map_or_else(|| root.clone(), |note| root.join(note))
                })
            })
            .context("No file or vault was requested")?;
        let path = okilum_core::vault::canonical_root(&path)
            .context("This file or folder couldn’t be opened.")?;
        let intent = OpenIntent::validate_cached(
            &path,
            opts.vault.as_deref(),
            opts.reusable_roots
                .iter()
                .find(|root| path.starts_with(root))
                .map(PathBuf::as_path),
        )?;
        opts.vault = Some(intent.root.clone());
        opts.single_file |= intent.single_file;
        if intent.note.is_some() {
            opts.note = intent.note;
        }
        opts.open_path = None;
        Ok::<_, anyhow::Error>(opts)
    })
}

pub(crate) fn finish_open(resolved: Result<super::Opts>, cx: &mut App) {
    match resolved {
        Ok(opts) => {
            if !focus_existing(&opts, cx) {
                if let Err(error) = create_window(opts, None, None, cx) {
                    show_error(error, cx);
                }
            }
        }
        Err(error) => show_error(error, cx),
    }
}

fn focus_existing(opts: &super::Opts, cx: &mut App) -> bool {
    let existing = cx.try_global::<Readers>().and_then(|readers| {
        readers.0.iter().rev().find_map(|(weak, root)| {
            let reader = weak.upgrade()?;
            let state = reader.read(cx);
            if state.single_file || !cx.windows().contains(&state.reader_window) {
                return None;
            }
            let note = if opts.single_file {
                let path = opts.vault.as_ref()?.join(opts.note.as_ref()?);
                let relative = path.strip_prefix(root).ok()?;
                Some(okilum_core::vault::note_path(relative))
            } else {
                if opts.vault.as_ref() != Some(root) {
                    return None;
                }
                opts.note.clone()
            };
            Some((reader.clone(), state.reader_window, note))
        })
    });
    let Some((reader, handle, note)) = existing else {
        return false;
    };
    if handle
        .update(cx, |_, window, cx| {
            reader.update(cx, |reader, cx| {
                if !reader.document_ready()
                    && reader.loading.as_ref().is_some_and(|load| !load.active)
                {
                    reader.start_loading(opts.clone(), window, cx);
                } else if let Some(note) = &note {
                    if reader.document_ready() {
                        match &opts.landing {
                            Some(position) => reader.open_link(note, position.clone(), window, cx),
                            None => reader.open_note(note, None, window, cx),
                        }
                    } else {
                        reader.queued_open_note = Some(note.clone());
                        reader.queued_landing = opts.landing.clone();
                    }
                }
            });
            window.activate_window();
        })
        .is_err()
    {
        return false;
    }
    super::reader_startup::supersede(cx);
    cx.activate(true);
    true
}

fn create_window(
    opts: super::Opts,
    shared: Option<super::reader_session::Shared>,
    duplicate_options: Option<WindowOptions>,
    cx: &mut App,
) -> Result<()> {
    let _phase = super::reader_diagnostics::phase(cx, "native_window_open");
    let _key_phase = super::reader_diagnostics::phase(cx, "window_key_and_geometry");
    let key = super::window_state::reader_key(&opts);
    super::reader_ui_state::guard_window_root(&key, cx);
    let (restored_options, frame_key) = super::window_state::prepare(window_options(cx), &key, cx);
    let duplicate = duplicate_options.is_some();
    let options = duplicate_options.unwrap_or(restored_options);
    drop(_key_phase);
    // `open_window` creates the platform window and its renderer, then runs the
    // closure, then draws once. Each step reports its own phase, so the first
    // launch's cost (#1008) is attributed to one of them.
    let platform = super::reader_diagnostics::phase(cx, "window_platform_create");
    let mut first_draw = None;
    let opened = cx.open_window(options, |window, cx| {
        drop(platform);
        let _build = super::reader_diagnostics::phase(cx, "window_view_build");
        super::sync_appearance(window, cx);
        window
            .observe_window_appearance(|window, cx| {
                super::sync_appearance(window, cx);
                window.refresh();
            })
            .detach();
        // A vault window is named by its vault before the first note opens.
        match opts.vault.as_deref().filter(|_| !opts.single_file) {
            Some(root) => window.set_window_title(&super::vault_name::window_title(
                None,
                Some(&super::vault_name::shown(root, cx)),
            )),
            None => window.set_window_title("Okilum — Opening document"),
        }
        let selected = opts.note.clone().unwrap_or_default();
        let root_path = opts.vault.clone();
        let full_vault = !opts.single_file;
        let reader = cx.new(|cx| super::Reader::new(opts, window, cx));
        if full_vault {
            if let Some(root) = root_path {
                register(reader.downgrade(), root, cx);
            }
        }
        if let Some(session) = shared {
            reader.update(cx, |reader, cx| {
                reader.attach_session(session, selected, window, cx)
            });
        }
        let weak = reader.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            super::window_state::record_window(window, cx);
            weak.update(cx, |reader, cx| {
                if !reader.save_source(cx) {
                    return false;
                }
                reader.record_ui_state(reader.ui_state.was_active(), cx);
                super::reader_ui_state::flush(cx);
                true
            })
            .unwrap_or(true)
        });
        let root = cx.new(|cx| Root::new(reader, window, cx));
        super::window_state::track_with_restore(&root, frame_key, !duplicate, window, cx);
        first_draw = super::reader_diagnostics::phase(cx, "window_first_draw");
        root
    });
    drop(first_draw);
    opened?;
    super::reader_startup::supersede(cx);
    cx.activate(true);
    Ok(())
}

fn new_window(cx: &mut App) {
    let active = cx.active_window();
    let source = cx.try_global::<Readers>().and_then(|readers| {
        readers.0.iter().rev().find_map(|(weak, _)| {
            let reader = weak.upgrade()?;
            (Some(reader.read(cx).reader_window) == active && !reader.read(cx).single_file)
                .then_some(weak.clone())
        })
    });
    let Some(source) = source else {
        return;
    };
    cx.spawn(async move |cx| loop {
        let outcome = source.update(cx, |reader, cx| {
            reader.record_ui_state(true, cx);
            let opts = super::Opts {
                vault: Some(reader.vault_root.clone()),
                note: Some(reader.current_rel.clone()),
                session_directory: reader.session_directory.clone(),
                use_html: reader.use_html,
                defer_loading: true,
                ..Default::default()
            };
            let session = reader.share_session();
            let failed = session.is_none()
                && !reader.loading.as_ref().is_some_and(|load| load.active)
                && !reader.incremental_active
                && !reader.incremental_initializing
                && !reader.watcher_poll_active;
            (
                session.map(|session| (opts, session, reader.reader_window)),
                failed,
            )
        });
        match outcome {
            Ok((Some((opts, session, source_window)), _)) => {
                cx.update(|cx| {
                    let options = source_window.update(cx, |_, window, cx| {
                        super::window_state::duplicate_options(window_options(cx), window, cx)
                    });
                    let Ok(options) = options else { return };
                    if let Err(error) = create_window(opts, Some(session), Some(options), cx) {
                        show_error(error, cx);
                    }
                });
                break;
            }
            Err(_) => break,
            Ok((None, true)) => {
                cx.update(|cx| {
                    show_error(
                        anyhow::anyhow!(
                            "Open the vault successfully before creating another window."
                        ),
                        cx,
                    )
                });
                break;
            }
            Ok((None, false)) => {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await
            }
        }
    })
    .detach();
}

pub(crate) fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(super::window_state::default_bounds(
            size(px(1500.), px(1000.)),
            cx,
        ))),
        window_min_size: Some(size(px(600.), px(400.))),
        app_id: Some("okilum".into()),
        #[cfg(target_os = "linux")]
        window_background: WindowBackgroundAppearance::Opaque,
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        titlebar: Some(super::reader_titlebar_options()),
        ..TitleBar::window_options()
    }
}

/// Vaults with a «open from a link?» window up, and the latest link for each:
/// repeated links reuse the one prompt instead of stacking (#1049).
#[derive(Default)]
struct LinkPrompts(std::collections::BTreeMap<PathBuf, (AnyWindowHandle, super::Opts)>);
impl Global for LinkPrompts {}

/// The first external link to a vault asks before opening it (#1049,
/// docs/deep-links.md «Security»). Open remembers the vault for links.
fn confirm_link_vault(root: PathBuf, opts: super::Opts, cx: &mut App) {
    let open = cx
        .default_global::<LinkPrompts>()
        .0
        .get(&root)
        .map(|(handle, _)| *handle)
        .filter(|handle| cx.windows().contains(handle));
    if let Some(handle) = open {
        if let Some((_, latest)) = cx.global_mut::<LinkPrompts>().0.get_mut(&root) {
            *latest = opts;
        }
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }
    // A prompt closed with its window button left a stale entry.
    cx.global_mut::<LinkPrompts>().0.remove(&root);
    let name = super::vault_name::shown(&root, cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(super::window_state::default_bounds(
            size(px(560.), px(240.)),
            cx,
        ))),
        window_min_size: Some(size(px(420.), px(200.))),
        ..window_options(cx)
    };
    let view_root = root.clone();
    let result = cx.open_window(options, |window, cx| {
        window.set_window_title("Okilum — Open link");
        let view = cx.new(|cx| LinkConfirm {
            root: view_root,
            name,
            focus: cx.focus_handle(),
        });
        // Focused from the start, so Enter and Esc answer it (#1271).
        view.read(cx).focus.clone().focus(window, cx);
        cx.new(|cx| Root::new(view, window, cx))
    });
    match result {
        Ok(handle) => {
            cx.global_mut::<LinkPrompts>()
                .0
                .insert(root, (handle.into(), opts));
        }
        Err(error) => show_error(error, cx),
    }
}

/// The user's answer: Open trusts the vault and opens the latest link.
fn answer_link_vault(root: &Path, open: bool, window: &mut Window, cx: &mut App) {
    let pending = cx.default_global::<LinkPrompts>().0.remove(root);
    let Some((_, opts)) = pending.filter(|_| open) else {
        window.remove_window();
        return;
    };
    open_from_prompt(root, opts, window, cx);
}

/// Open a vault the user just chose in a prompt window: choosing it is the
/// consent, so links may open it from now on.
fn open_from_prompt(root: &Path, opts: super::Opts, window: &mut Window, cx: &mut App) {
    super::reader_ui_state::trust_for_links(root, cx);
    if let Err(error) = open_window(opts, cx) {
        window.remove_window();
        show_error(error, cx);
        return;
    }
    // The Reader window is created after a background step. Closing the
    // prompt first would leave no window, which ends the app on Linux, so
    // the prompt goes once the vault's window exists (or after 10 s).
    let prompt = window.window_handle();
    let root = root.to_owned();
    cx.spawn(async move |cx| {
        for _ in 0..200 {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(50))
                .await;
            if cx.update(|cx| reusable_roots(cx).contains(&root)) {
                break;
            }
        }
        let _ = prompt.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
}

/// The chooser window for a link whose vault name several known vaults
/// share; a newer link replaces it, so there is one at a time.
#[derive(Default)]
struct LinkChooser(Option<AnyWindowHandle>);
impl Global for LinkChooser {}

fn choose_link_vault(
    vault: String,
    roots: Vec<(PathBuf, Option<String>)>,
    position: okilum_core::deep_link::Position,
    cx: &mut App,
) {
    if let Some(old) = cx.default_global::<LinkChooser>().0.take() {
        let _ = old.update(cx, |_, window, _| window.remove_window());
    }
    let choices = roots
        .into_iter()
        .map(|(root, rel)| {
            let opts = rel.map(|rel| super::Opts {
                vault: Some(root.clone()),
                note: Some(rel),
                landing: Some(position.clone()),
                ..Default::default()
            });
            (root, opts)
        })
        .collect::<Vec<_>>();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(super::window_state::default_bounds(
            size(px(620.), px(160. + 44. * choices.len() as f32)),
            cx,
        ))),
        window_min_size: Some(size(px(420.), px(200.))),
        ..window_options(cx)
    };
    let result = cx.open_window(options, |window, cx| {
        window.set_window_title("Okilum — Open link");
        let view = cx.new(|_| LinkVaultChoice { vault, choices });
        cx.new(|cx| Root::new(view, window, cx))
    });
    match result {
        Ok(handle) => cx.global_mut::<LinkChooser>().0 = Some(handle.into()),
        Err(error) => show_error(error, cx),
    }
}

struct LinkVaultChoice {
    vault: String,
    /// Each known vault with this name; `None` when the note is not there.
    choices: Vec<(PathBuf, Option<super::Opts>)>,
}
impl Render for LinkVaultChoice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let rows = self.choices.iter().enumerate().map(|(ix, (root, opts))| {
            let button = Button::new(("link-vault-choice", ix)).label("Open");
            let button = match opts.clone() {
                Some(opts) => {
                    let root = root.clone();
                    button.primary().on_click(move |_, window, cx| {
                        cx.default_global::<LinkChooser>().0 = None;
                        let mut opts = opts.clone();
                        opts.reusable_roots = reusable_roots(cx);
                        open_from_prompt(&root, opts, window, cx)
                    })
                }
                None => button.disabled(true),
            };
            h_flex()
                .gap_3()
                .justify_between()
                .child(
                    v_flex()
                        .min_w_0()
                        .when_some(super::vault_name::stored(root, cx), |row, name| {
                            row.child(div().font_weight(FontWeight::MEDIUM).child(name))
                        })
                        .child(okilum_core::vault::display_path(root))
                        .when(opts.is_none(), |row| {
                            row.child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child("The note is not in this vault."),
                            )
                        }),
                )
                .child(button)
        });
        v_flex()
            .p_4()
            .gap_3()
            .child(TitleBar::new().child("Open link"))
            .child(format!(
                "Several vaults are named \u{201c}{}\u{201d}. Choose the one this link is for.",
                self.vault
            ))
            .children(rows)
            .child(
                h_flex().justify_end().child(
                    Button::new("link-vault-choice-cancel")
                        .label("Cancel")
                        .on_click(|_, window, cx| {
                            cx.default_global::<LinkChooser>().0 = None;
                            window.remove_window()
                        }),
                ),
            )
    }
}

struct LinkConfirm {
    root: PathBuf,
    name: String,
    focus: FocusHandle,
}
impl Render for LinkConfirm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (open_root, cancel_root) = (self.root.clone(), self.root.clone());
        v_flex()
            .track_focus(&self.focus)
            // Enter is the primary button (Open), Esc is Cancel (#1271).
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let open = match event.keystroke.key.as_str() {
                    "enter" => true,
                    "escape" => false,
                    _ => return,
                };
                cx.stop_propagation();
                answer_link_vault(&this.root, open, window, cx);
            }))
            .p_4()
            .gap_3()
            .child(TitleBar::new().child("Open link"))
            .child(format!(
                "A link asks to open vault \u{201c}{}\u{201d}.",
                self.name
            ))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    // Without Windows' `\\?\` prefix (#1270).
                    .child(okilum_core::vault::display_path(&self.root)),
            )
            .child("Links only open notes. Okilum asks once per vault.")
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("link-vault-cancel").label("Cancel").on_click(
                        move |_, window, cx| answer_link_vault(&cancel_root, false, window, cx),
                    ))
                    .child(
                        Button::new("link-vault-open")
                            .primary()
                            .label("Open")
                            .on_click(move |_, window, cx| {
                                answer_link_vault(&open_root, true, window, cx)
                            }),
                    ),
            )
    }
}

struct OpenError(String);
impl Render for OpenError {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_4()
            .gap_3()
            .child(TitleBar::new().child("Cannot open document"))
            .child(self.0.clone())
            .child(controls(ControlsPresentation::Entry))
    }
}
/// The open-failure text: plain words for the common OS errors, otherwise
/// the outermost, human-written context. The full chain stays in stderr.
fn open_error_text(error: &anyhow::Error) -> String {
    use std::io::ErrorKind;
    let io = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>());
    match io.map(std::io::Error::kind) {
        Some(ErrorKind::PermissionDenied) => {
            "Okilum doesn’t have permission to open this file or folder. Check its \
             permissions, then try again."
                .into()
        }
        Some(ErrorKind::NotFound) => "This file or folder no longer exists. It may have \
                                      been moved, renamed or deleted."
            .into(),
        // `downcast_ref` looks through context, so inspect only the outermost layer.
        Some(_)
            if error
                .chain()
                .next()
                .is_some_and(|e| e.downcast_ref::<std::io::Error>().is_some()) =>
        {
            "This file or folder couldn’t be opened.".into()
        }
        _ => error.to_string(),
    }
}

fn show_error(error: anyhow::Error, cx: &mut App) {
    eprintln!("Cannot open document: {error:#}");
    let message = open_error_text(&error);
    let (options, frame_key) = super::window_state::prepare(window_options(cx), "open-error", cx);
    let result = cx.open_window(options, |window, cx| {
        window.set_window_title("Okilum — Cannot open document");
        let view = cx.new(|_| OpenError(message));
        let root = cx.new(|cx| Root::new(view, window, cx));
        super::window_state::track(&root, frame_key, window, cx);
        root
    });
    if let Err(error) = result {
        eprintln!("Cannot show open failure: {error:#}");
    }
}

/// Cold events stay queued until application initialization; warm events use the
/// same dispatcher. There is no polling timer and no backend dependency.
pub(crate) fn receive_events(receiver: async_channel::Receiver<Vec<String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(urls) = receiver.recv().await {
            cx.update(|cx| dispatch_urls(urls, cx));
        }
    })
    .detach();
}

#[cfg(test)]
mod entry_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn open_failures_show_plain_words_without_os_errors() {
        use std::io::{Error as Io, ErrorKind};
        let denied = anyhow::Error::from(Io::from(ErrorKind::PermissionDenied))
            .context("This file or folder couldn’t be opened.");
        let missing = anyhow::Error::from(Io::from(ErrorKind::NotFound))
            .context("The requested path is missing or inaccessible");
        let other = anyhow::Error::from(Io::other("device not ready (os error 21)"));
        let utf8 = anyhow::Error::from(Io::from(ErrorKind::InvalidData))
            .context("The Markdown file cannot be read as UTF-8");
        for (error, expected) in [
            (&denied, "Okilum doesn’t have permission"),
            (&missing, "This file or folder no longer exists"),
            (&other, "This file or folder couldn’t be opened."),
            (&utf8, "The Markdown file cannot be read as UTF-8"),
        ] {
            let text = open_error_text(error);
            assert!(text.starts_with(expected), "{text}");
            assert!(!text.contains("os error"), "{text}");
            assert!(!text.contains(':'), "{text}");
        }
        // Positive control: the raw chain still carries the OS detail for stderr.
        assert!(format!("{other:#}").contains("os error 21"));
        let authored = anyhow::anyhow!("Select exactly one file or folder");
        assert_eq!(
            open_error_text(&authored),
            "Select exactly one file or folder"
        );
    }

    fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(
            root: &Path,
            dir: &Path,
            out: &mut std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>,
        ) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    out.insert(rel, None);
                    walk(root, &path, out);
                } else {
                    out.insert(rel, Some(std::fs::read(path).unwrap()));
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(root, root, &mut out);
        out
    }
    fn parse(args: &[&str]) -> Result<super::super::Opts> {
        match parse_args(
            args.iter().map(std::ffi::OsString::from),
            super::super::Opts::default(),
        )? {
            Command::Launch(opts) => Ok(*opts),
            Command::Help => bail!("Unexpected help"),
            Command::UninstallData { .. } => bail!("Unexpected uninstall"),
            Command::BuildInfo => bail!("Unexpected build info"),
        }
    }
    /// #1049: a clicked link arrives as the argument; a `%u` file arrives
    /// as a file: URL. Neither becomes a bogus path.
    #[test]
    fn link_and_file_url_arguments() {
        let opts = parse(&["okilum://v/Notes/Plan.md?line=3"]).unwrap();
        assert_eq!(
            opts.link.as_deref(),
            Some("okilum://v/Notes/Plan.md?line=3")
        );
        assert!(opts.open_path.is_none(), "a link is not a file path");
        assert!(parse(&["OKILUM://v/N/a.md"]).unwrap().link.is_some());
        assert!(parse(&["okilum://v/N/a.md", "okilum://v/N/b.md"]).is_err());
        let file = parse(&["file:///tmp/My%20notes/a.md"]).unwrap();
        assert_eq!(
            file.open_path.as_deref(),
            Some(Path::new("/tmp/My notes/a.md"))
        );
        assert!(file.link.is_none());
        // Positive control: after `--` both are ordinary file names.
        let literal = parse(&["--", "okilum:x"]).unwrap();
        assert!(literal.link.is_none());
        assert_eq!(literal.open_path.as_deref(), Some(Path::new("okilum:x")));
        // Non-ASCII names are paths, not schemes, and never split a character.
        let name = parse(&["öööö.md"]).unwrap();
        assert_eq!(name.open_path.as_deref(), Some(Path::new("öööö.md")));
        assert!(name.link.is_none());
    }

    /// #1160: several log files open together; any other second path is
    /// still refused.
    #[test]
    fn several_log_paths_open_together() {
        let opts = parse(&["a.jsonl", "logs/b.log", "c.ndjson"]).unwrap();
        assert_eq!(opts.open_path.as_deref(), Some(Path::new("a.jsonl")));
        assert_eq!(
            opts.extra_logs,
            [PathBuf::from("logs/b.log"), PathBuf::from("c.ndjson")]
        );
        assert!(parse(&["a.jsonl", "notes.md"]).is_err());
        assert!(parse(&["notes.md", "a.jsonl"]).is_err());
        // Positive control: one path still opens alone.
        let single = parse(&["notes.md"]).unwrap();
        assert!(single.extra_logs.is_empty());
    }

    #[test]
    fn build_info_is_a_standalone_command() {
        let command = |args: &[&str]| {
            parse_args(
                args.iter().map(std::ffi::OsString::from),
                super::super::Opts::default(),
            )
        };
        assert!(matches!(command(&["--build-info"]), Ok(Command::BuildInfo)));
        assert!(command(&["--build-info", "--vault", "/notes"]).is_err());
        // Positive control: after `--` it is a file name.
        assert!(matches!(
            command(&["--", "--build-info"]),
            Ok(Command::Launch(_))
        ));
    }

    #[test]
    fn uninstall_data_is_a_standalone_command() {
        let command = |args: &[&str]| {
            parse_args(
                args.iter().map(std::ffi::OsString::from),
                super::super::Opts::default(),
            )
        };
        assert!(matches!(
            command(&["--uninstall-data"]),
            Ok(Command::UninstallData { assume_yes: false })
        ));
        assert!(matches!(
            command(&["--uninstall-data", "--yes"]),
            Ok(Command::UninstallData { assume_yes: true })
        ));
        assert!(command(&["--uninstall-data", "--vault", "/notes"]).is_err());
        // After `--` it is an ordinary file name, not the command.
        assert!(matches!(
            command(&["--", "--uninstall-data"]),
            Ok(Command::Launch(_))
        ));
    }
    #[gpui::test]
    fn ordinary_startup_restores_local_document_and_first_run_opens_entry(cx: &mut TestAppContext) {
        use super::super::{reader_history::ReadingHistory, reader_startup, Opts};
        let fixture =
            std::env::temp_dir().join(format!("okilum-startup364-{}", uuid::Uuid::new_v4()));
        let state = fixture.join("state");
        let root = fixture.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("last.md"), "# Last document").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            reader_startup::launch(
                Opts {
                    session_directory: Some(state.clone()),
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1, "first run offers local entry");
        cx.update(|cx| assert!(cx.global::<Readers>().0.is_empty()));
        ReadingHistory::record_usable_document(&state, &root, "last.md").unwrap();
        cx.update(|cx| {
            reader_startup::launch(
                Opts {
                    session_directory: Some(state.clone()),
                    index_dir: Some(fixture.join("index")),
                    ..Default::default()
                },
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(
            cx.windows().len(),
            1,
            "publication removes the entry window"
        );
        cx.update(|cx| {
            let readers = &cx.global::<Readers>().0;
            assert_eq!(readers.len(), 1);
            let reader = readers[0].0.upgrade().unwrap();
            assert_eq!(reader.read(cx).current_rel, "last.md");
            assert_eq!(reader.read(cx).vault_root, root);
            assert!(reader.read(cx).document_ready());
        });
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[gpui::test]
    fn window_open_reports_platform_view_and_first_draw_inside_native_window_open(
        cx: &mut TestAppContext,
    ) {
        // #1008: the first launch's window cost must be attributable to a step.
        use super::super::{
            reader_diagnostics, reader_history::ReadingHistory, reader_startup, Opts,
        };
        let fixture =
            std::env::temp_dir().join(format!("okilum-window-phases-{}", uuid::Uuid::new_v4()));
        let state = fixture.join("state");
        let root = fixture.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("last.md"), "# Last document").unwrap();
        // A remembered document makes the launch open the Reader window, not the entry.
        ReadingHistory::record_usable_document(&state, &root, "last.md").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(reader_diagnostics::LaunchTrace(
                reader_diagnostics::Trace::new(Some(state.clone()), None),
            ));
            reader_startup::launch(
                Opts {
                    session_directory: Some(state.clone()),
                    index_dir: Some(fixture.join("index")),
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        let wanted = [
            "window_platform_create",
            "window_view_build",
            "window_first_draw",
            "native_window_open",
        ];
        let mut seen: Vec<(String, f64)> = Vec::new();
        // The diagnostic thread writes asynchronously; leave it ample time on a loaded runner.
        for _ in 0..2000 {
            let text =
                std::fs::read_to_string(state.join("reader-diagnostic.log")).unwrap_or_default();
            seen = text
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter_map(|event| {
                    let phase = event["phase"].as_str()?.to_string();
                    wanted.contains(&phase.as_str()).then(|| {
                        (
                            phase,
                            event["details"]["duration_ms"].as_f64().unwrap_or(-1.),
                        )
                    })
                })
                .collect();
            if seen.len() >= wanted.len() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, wanted, "each step reports once, the whole last");
        assert!(seen.iter().all(|(_, ms)| *ms >= 0.), "{seen:?}");
        let steps: f64 = seen[..3].iter().map(|(_, ms)| ms).sum();
        assert!(
            steps <= seen[3].1 + 1.,
            "the steps are parts of native_window_open: {seen:?}"
        );
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[gpui::test]
    fn restored_quick_session_does_not_index_its_folder(cx: &mut TestAppContext) {
        use super::super::{reader_history::ReadingHistory, reader_startup, Opts};
        let fixture =
            std::env::temp_dir().join(format!("okilum-quick-restore-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("notes");
        let state = fixture.join("state");
        let cache = fixture.join("cache");
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::write(root.join("last.md"), "# Restored quick document").unwrap();
        ReadingHistory::record_document_mode(&state, &root, "last.md", true).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            reader_startup::launch(
                Opts {
                    session_directory: Some(state),
                    index_dir: Some(cache.clone()),
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let readers = &cx.global::<Readers>().0;
            assert_eq!(readers.len(), 1);
            let reader = readers[0].0.upgrade().unwrap();
            assert!(reader.read(cx).single_file);
            assert!(reader.read(cx).document_ready());
            assert_eq!(reader.read(cx).current_rel, "last.md");
        });
        assert!(!cache.exists());
        std::fs::remove_dir_all(fixture).unwrap();
    }

    fn explicit_delivery_wins_startup(cx: &mut TestAppContext, entry_first: bool) {
        use super::super::{
            reader_history::{ReadingHistory, TestSessionDirectory},
            reader_startup, Opts,
        };
        let fixture =
            std::env::temp_dir().join(format!("okilum-startup-delivery-{}", uuid::Uuid::new_v4()));
        let state = fixture.join("state");
        let root = fixture.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("saved.md"), "# Saved").unwrap();
        std::fs::write(root.join("explicit.md"), "# Explicit").unwrap();
        if !entry_first {
            ReadingHistory::record_usable_document(&state, &root, "saved.md").unwrap();
        }
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            reader_startup::launch(
                Opts {
                    session_directory: Some(state.clone()),
                    index_dir: Some(fixture.join("index")),
                    ..Default::default()
                },
                cx,
            );
        });
        if entry_first {
            cx.run_until_parked();
            assert_eq!(cx.windows().len(), 1, "entry was actually shown");
            cx.update(|cx| assert!(cx.global::<Readers>().0.is_empty()));
        }
        cx.update(|cx| {
            dispatch_urls(
                vec![url::Url::from_file_path(root.join("explicit.md"))
                    .unwrap()
                    .into()],
                cx,
            );
            if entry_first {
                // #1110: the window is created after a background step; the
                // start window must stay until then or the app quits.
                assert_eq!(cx.windows().len(), 1, "start window kept meanwhile");
            }
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1, "no stale restore or entry remains");
        cx.update(|cx| {
            let readers = &cx.global::<Readers>().0;
            assert_eq!(readers.len(), 1);
            let reader = readers[0].0.upgrade().unwrap();
            assert_eq!(
                reader.read(cx).current_rel,
                "explicit.md",
                "positive delivery control"
            );
            assert!(reader.read(cx).document_ready());
        });
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[gpui::test]
    fn explicit_delivery_supersedes_pending_restore(cx: &mut TestAppContext) {
        explicit_delivery_wins_startup(cx, false);
    }

    #[gpui::test]
    fn explicit_delivery_replaces_first_run_entry(cx: &mut TestAppContext) {
        explicit_delivery_wins_startup(cx, true);
    }

    /// #1137: on macOS the app outlives its last window; a reopen (Dock
    /// click, `open -a`) with no window brings back the last vault, or the
    /// start screen when there is none. With a window it opens nothing new.
    #[gpui::test]
    fn reopen_without_windows_restores_the_last_vault_or_the_start_screen(cx: &mut TestAppContext) {
        use super::super::{
            reader_history::{ReadingHistory, TestSessionDirectory},
            reader_startup, Opts,
        };
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let root = fixture.path().join("Notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Plan.md"), "# Plan").unwrap();
        let root = root.canonicalize().unwrap();
        let base = Opts {
            session_directory: Some(state.clone()),
            index_dir: Some(fixture.path().join("index")),
            ..Default::default()
        };
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            cx.set_global(reader_startup::ReopenBase(base.clone()));
            // macOS keeps the app running without windows (#1137).
            cx.set_quit_mode(gpui::QuitMode::Explicit);
            // No history yet: the first launch shows the start screen.
            reader_startup::launch(base.clone(), cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1, "start screen");
        let close_all = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
            });
            cx.run_until_parked();
            assert!(
                cx.update(|cx| cx.windows().is_empty()),
                "all windows closed"
            );
        };
        // Reopen with no window and no history: the start screen again.
        close_all(cx);
        cx.update(reader_startup::reopen);
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(cx.windows().len(), 1);
            assert!(cx
                .global::<Readers>()
                .0
                .iter()
                .all(|(r, _)| r.upgrade().is_none()));
        });
        // With a remembered vault, reopen restores it.
        ReadingHistory::record_usable_document(&state, &root, "Plan.md").unwrap();
        close_all(cx);
        cx.update(reader_startup::reopen);
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(cx.windows().len(), 1, "one window, the vault");
            let reader = cx
                .global::<Readers>()
                .0
                .iter()
                .find_map(|(r, _)| r.upgrade())
                .expect("a Reader was opened");
            assert_eq!(reader.read(cx).vault_root, root);
            assert_eq!(reader.read(cx).current_rel, "Plan.md");
        });
        // Positive control: a reopen while a window exists opens nothing new.
        cx.update(reader_startup::reopen);
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
    }

    /// #1325: every open vault window comes back after a quit and a launch,
    /// each on its own last note; a vault that is gone is skipped and the
    /// others still open. The set is on disk before the quit (a crash keeps
    /// it), and windows closing as the app ends do not change it.
    #[gpui::test]
    fn launch_reopens_every_vault_window_and_skips_a_missing_one(cx: &mut TestAppContext) {
        use super::super::{
            reader_history::{ReadingHistory, TestSessionDirectory},
            reader_open_windows, reader_startup, Opts,
        };
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let roots: Vec<PathBuf> = ["Home", "Work", "Archive"]
            .iter()
            .map(|name| {
                let root = fixture.path().join(name);
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(root.join("Start.md"), "# Start").unwrap();
                std::fs::write(root.join(format!("{name}.md")), "# Note").unwrap();
                root.canonicalize().unwrap()
            })
            .collect();
        let base = Opts {
            session_directory: Some(state.clone()),
            index_dir: Some(fixture.path().join("index")),
            ..Default::default()
        };
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            reader_open_windows::install(state.clone(), cx);
            cx.set_global(reader_startup::ReopenBase(base.clone()));
            cx.set_quit_mode(gpui::QuitMode::Explicit);
        });
        for root in &roots {
            let name = root.file_name().unwrap().to_string_lossy().into_owned();
            cx.update(|cx| {
                open_window(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some(format!("{name}.md")),
                        ..base.clone()
                    },
                    cx,
                )
                .unwrap()
            });
            cx.run_until_parked();
        }
        let open_roots = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                let mut open: Vec<(PathBuf, String)> = readers(cx)
                    .iter()
                    .filter(|r| cx.windows().contains(&r.read(cx).reader_window))
                    .map(|r| {
                        (
                            r.read(cx).vault_root.clone(),
                            r.read(cx).current_rel.clone(),
                        )
                    })
                    .collect();
                open.sort();
                open
            })
        };
        // A restore opens one window after another, waiting on timers.
        let settle = |cx: &mut TestAppContext| {
            for _ in 0..60 {
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(100));
                cx.run_until_parked();
            }
        };
        // Every window shows its note and no notice.
        let all_shown = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                for reader in readers(cx) {
                    let reader = reader.read(cx);
                    if !cx.windows().contains(&reader.reader_window) {
                        continue;
                    }
                    let name = reader.vault_root.file_name().unwrap().to_string_lossy();
                    assert!(reader.document_ready(), "{name} shows its note");
                    assert!(
                        matches!(reader.content.read(cx).preparation_status(), Some(Ok(()))),
                        "{name} prepared its note"
                    );
                    assert!(reader.link_notice.is_none(), "{name} shows no notice");
                }
            })
        };
        let mut expected: Vec<(PathBuf, String)> = roots
            .iter()
            .map(|root| {
                let name = root.file_name().unwrap().to_string_lossy();
                (root.clone(), format!("{name}.md"))
            })
            .collect();
        expected.sort();
        assert_eq!(
            open_roots(cx),
            expected,
            "three vault windows before the quit"
        );
        // Recorded as they opened, before any quit: a crash keeps this.
        let mut recorded = reader_open_windows::load(&state);
        recorded.sort();
        let mut sorted_roots = roots.clone();
        sorted_roots.sort();
        assert_eq!(recorded, sorted_roots);

        // Focus decides the front window, not the opening order (#1329):
        // Archive opened last, then the focus goes to Work and on to Home.
        // A crash right after keeps Home in front.
        let window_of = |cx: &mut TestAppContext, root: &PathBuf| {
            cx.update(|cx| {
                readers(cx)
                    .into_iter()
                    .map(|r| r.read(cx))
                    .find(|r| r.vault_root == *root)
                    .map(|r| r.reader_window)
                    .unwrap()
            })
        };
        for root in [&roots[1], &roots[0]] {
            let window = window_of(cx, root);
            cx.update(|cx| {
                let _ = window.update(cx, |_, window, _| window.activate_window());
            });
            cx.run_until_parked();
        }
        settle(cx);
        assert_eq!(
            reader_open_windows::load(&state).last(),
            Some(&roots[0]),
            "the window focused last is the front one on disk"
        );

        // Quit, then the windows go away as the app ends.
        cx.update(reader_open_windows::quit);
        cx.update(|cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }
        });
        cx.run_until_parked();
        assert_eq!(
            reader_open_windows::load(&state).len(),
            3,
            "closing after quit keeps the set"
        );

        // Launch: all three come back, each on its own last note. The windows
        // record their notes as they come up; here another write holds the
        // history lock meanwhile (#1325, muninn gate): the writes wait for it
        // and no window reports it.
        let busy = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(state.join("reader-session.lock"))
            .unwrap();
        busy.try_lock().unwrap();
        cx.update(|cx| {
            cx.set_global(reader_startup::Startup::default());
            reader_startup::launch(base.clone(), cx);
        });
        // Held for 1.5 s, well inside the recorder's patience.
        for _ in 0..15 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
            cx.run_until_parked();
        }
        drop(busy);
        settle(cx);
        assert_eq!(open_roots(cx), expected, "every vault window reopens");
        assert_eq!(cx.windows().len(), 3, "no start screen next to them");
        all_shown(cx);
        assert_eq!(
            reader_open_windows::load(&state).last(),
            Some(&roots[0]),
            "the restored front window stays the front one"
        );
        for root in &roots {
            assert!(
                ReadingHistory::last_document(&state, root)
                    .unwrap()
                    .is_some(),
                "{} recorded its note once the lock was free",
                root.display()
            );
        }

        // Switching between windows re-records each one's note (the VM 150
        // gate saw the notice there too): with the lock held meanwhile, the
        // writes wait and no window reports it.
        let busy = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(state.join("reader-session.lock"))
            .unwrap();
        busy.try_lock().unwrap();
        for reader in cx.update(|cx| readers(cx)) {
            reader.update(cx, |reader, cx| {
                reader.last_recorded_document = None;
                reader.record_usable_document(cx);
            });
        }
        for _ in 0..15 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
            cx.run_until_parked();
        }
        drop(busy);
        settle(cx);
        all_shown(cx);

        // A vault folder that is gone is skipped; the other two still open.
        cx.update(reader_open_windows::quit);
        cx.update(|cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }
        });
        cx.run_until_parked();
        std::fs::remove_dir_all(&roots[1]).unwrap();
        cx.update(|cx| {
            cx.set_global(reader_startup::Startup::default());
            reader_startup::launch(base.clone(), cx);
        });
        settle(cx);
        expected.retain(|(root, _)| root != &roots[1]);
        assert_eq!(open_roots(cx), expected, "the missing vault is skipped");
        assert_eq!(cx.windows().len(), 2);
        all_shown(cx);

        // A damaged reading history does not cost the recorded windows.
        cx.update(reader_open_windows::quit);
        cx.update(|cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }
        });
        cx.run_until_parked();
        std::fs::write(state.join("update-session.json"), b"{ damaged").unwrap();
        cx.update(|cx| {
            cx.set_global(reader_startup::Startup::default());
            reader_startup::launch(base.clone(), cx);
        });
        settle(cx);
        let reopened: Vec<PathBuf> = open_roots(cx).into_iter().map(|(root, _)| root).collect();
        assert_eq!(
            reopened,
            expected
                .iter()
                .map(|(root, _)| root.clone())
                .collect::<Vec<_>>(),
            "the window set survives a damaged history"
        );
    }

    /// #1335: duplicate windows of a vault (New Window) and closing either the
    /// duplicate or the original keep that vault in the recorded set, and a
    /// launch brings back one window per vault.
    #[gpui::test]
    fn closing_a_duplicate_or_its_original_keeps_the_vault(cx: &mut TestAppContext) {
        use super::super::{
            reader_history::TestSessionDirectory, reader_open_windows, reader_startup, Opts,
        };
        for close_originals in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let state = fixture.path().join("state");
            let roots: Vec<PathBuf> = ["Gate1", "Gate2", "Gate3"]
                .iter()
                .map(|name| {
                    let root = fixture.path().join(name);
                    std::fs::create_dir_all(&root).unwrap();
                    std::fs::write(root.join(format!("{name}.md")), "# Note").unwrap();
                    root.canonicalize().unwrap()
                })
                .collect();
            let base = Opts {
                session_directory: Some(state.clone()),
                index_dir: Some(fixture.path().join("index")),
                ..Default::default()
            };
            cx.update(|cx| {
                gpui_component::init(cx);
                install(cx);
                cx.set_global(TestSessionDirectory(state.clone()));
                reader_open_windows::install(state.clone(), cx);
                cx.set_global(reader_startup::ReopenBase(base.clone()));
                cx.set_quit_mode(gpui::QuitMode::Explicit);
            });
            let window_of = |cx: &mut TestAppContext, root: &PathBuf| {
                cx.update(|cx| {
                    readers(cx)
                        .into_iter()
                        .filter(|r| cx.windows().contains(&r.read(cx).reader_window))
                        .find(|r| r.read(cx).vault_root == *root)
                        .map(|r| r.read(cx).reader_window)
                        .unwrap()
                })
            };
            // As the gate did: each vault from the picker, and New Window on
            // the first two before the next vault opens.
            for (i, root) in roots.iter().enumerate() {
                if i > 0 {
                    let previous = window_of(cx, &roots[i - 1]);
                    cx.update(|cx| {
                        let _ = previous.update(cx, |_, window, _| window.activate_window());
                        new_window(cx);
                    });
                    cx.run_until_parked();
                }
                cx.update(|cx| dispatch_path(root, cx));
                cx.run_until_parked();
            }
            assert_eq!(cx.windows().len(), 5, "three vaults and two duplicates");
            // Close one window of each duplicated vault.
            let doomed: Vec<gpui::AnyWindowHandle> = cx.update(|cx| {
                let open: Vec<_> = readers(cx)
                    .into_iter()
                    .filter(|r| cx.windows().contains(&r.read(cx).reader_window))
                    .map(|r| (r.read(cx).vault_root.clone(), r.read(cx).reader_window))
                    .collect();
                roots[..2]
                    .iter()
                    .map(|root| {
                        let mine: Vec<_> = open.iter().filter(|(r, _)| r == root).collect();
                        assert_eq!(mine.len(), 2, "{} has a duplicate", root.display());
                        mine[if close_originals { 0 } else { 1 }].1
                    })
                    .collect()
            });
            for window in doomed {
                cx.update(|cx| {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                });
                cx.run_until_parked();
            }
            assert_eq!(cx.windows().len(), 3);
            let mut recorded = reader_open_windows::load(&state);
            recorded.sort();
            assert_eq!(
                recorded, roots,
                "close_originals={close_originals}: recorded set"
            );
            cx.update(reader_open_windows::quit);
            cx.update(|cx| {
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
            });
            cx.run_until_parked();
            cx.update(|cx| {
                cx.set_global(reader_startup::Startup::default());
                reader_startup::launch(base.clone(), cx);
            });
            for _ in 0..60 {
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(100));
                cx.run_until_parked();
            }
            let mut reopened: Vec<PathBuf> = cx.update(|cx| {
                readers(cx)
                    .into_iter()
                    .filter(|r| cx.windows().contains(&r.read(cx).reader_window))
                    .map(|r| r.read(cx).vault_root.clone())
                    .collect()
            });
            reopened.sort();
            assert_eq!(
                reopened, roots,
                "close_originals={close_originals}: restored"
            );
            cx.update(|cx| {
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
            });
            cx.run_until_parked();
        }
    }

    /// #1338: each Reader stays bound to its own window. GPUI binds an entity
    /// to the window whose draw last read it, and runs its window callbacks
    /// (the quick-open Enter among them) there; a Reader read while another
    /// window draws took that window's title.
    #[gpui::test]
    fn every_reader_stays_bound_to_its_own_window(cx: &mut TestAppContext) {
        use super::super::{reader_history::TestSessionDirectory, Opts};
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let roots: Vec<PathBuf> = ["Amber", "Bravo", "Charlie"]
            .iter()
            .map(|name| {
                let root = fixture.path().join(name);
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(root.join(format!("{name}.md")), "# Note").unwrap();
                std::fs::write(root.join("Other.md"), "# Other").unwrap();
                root.canonicalize().unwrap()
            })
            .collect();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            super::super::reader_ui_state::install(&state, cx);
        });
        for root in &roots {
            cx.update(|cx| {
                open_window(
                    Opts {
                        vault: Some(root.clone()),
                        session_directory: Some(state.clone()),
                        index_dir: Some(fixture.path().join("index")),
                        ..Default::default()
                    },
                    cx,
                )
                .unwrap()
            });
            cx.run_until_parked();
        }
        let readers_now = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                readers(cx)
                    .into_iter()
                    .filter(|r| cx.windows().contains(&r.read(cx).reader_window))
                    .collect::<Vec<_>>()
            })
        };
        let all = readers_now(cx);
        assert_eq!(all.len(), 3);
        // Work in each window as a person does: focus it, open another note,
        // let every window draw.
        for reader in &all {
            let handle = cx.update(|cx| reader.read(cx).reader_window);
            cx.update(|cx| {
                handle
                    .update(cx, |_, window, cx| {
                        window.activate_window();
                        reader.update(cx, |r, cx| r.open_note("Other.md", None, window, cx));
                    })
                    .unwrap();
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| window.refresh());
                }
            });
            cx.run_until_parked();
        }
        cx.update(|cx| {
            for reader in &all {
                let own = reader.read(cx).reader_window;
                let name = reader
                    .read(cx)
                    .vault_root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let bound = cx.with_window(reader.entity_id(), |window, _| window.window_handle());
                assert_eq!(
                    bound,
                    Some(own),
                    "{name}'s Reader runs its window callbacks in its own window"
                );
            }
        });
    }

    /// #1327: a renamed vault is named by its display name in its window
    /// titles only; another vault's window keeps its own. Links still name
    /// the folder and open the renamed vault; a blank name is the folder.
    #[gpui::test]
    fn a_renamed_vault_titles_its_windows_and_links_still_open(cx: &mut TestAppContext) {
        use super::super::{reader_history::TestSessionDirectory, vault_name, Opts};
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let roots: Vec<PathBuf> = ["Gate1", "Gate2"]
            .iter()
            .map(|name| {
                let root = fixture.path().join(name);
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(root.join("Plan.md"), "# Plan").unwrap();
                root.canonicalize().unwrap()
            })
            .collect();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            super::super::reader_ui_state::install(&state, cx);
            super::super::reader_ui_state::trust_for_links(&roots[1], cx);
        });
        for root in &roots {
            cx.update(|cx| {
                open_window(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Plan.md".into()),
                        session_directory: Some(state.clone()),
                        index_dir: Some(fixture.path().join("index")),
                        ..Default::default()
                    },
                    cx,
                )
                .unwrap()
            });
            cx.run_until_parked();
        }
        let window_of = |cx: &mut TestAppContext, root: &PathBuf| {
            cx.update(|cx| {
                readers(cx)
                    .into_iter()
                    .find(|r| r.read(cx).vault_root == *root)
                    .map(|r| r.read(cx).reader_window)
                    .unwrap()
            })
        };
        let title = |cx: &mut TestAppContext, root: &PathBuf| {
            let window = window_of(cx, root);
            gpui::VisualTestContext::from_window(window, cx).window_title()
        };
        assert_eq!(
            title(cx, &roots[0]).as_deref(),
            Some("Okilum — Plan — Gate1")
        );
        assert_eq!(
            title(cx, &roots[1]).as_deref(),
            Some("Okilum — Plan — Gate2")
        );

        cx.update(|cx| vault_name::set(&roots[1], "  Work  ", cx));
        cx.run_until_parked();
        assert_eq!(
            title(cx, &roots[1]).as_deref(),
            Some("Okilum — Plan — Work"),
            "the renamed vault's window"
        );
        assert_eq!(
            title(cx, &roots[0]).as_deref(),
            Some("Okilum — Plan — Gate1"),
            "the other vault's window keeps its name"
        );
        cx.update(|cx| {
            assert_eq!(vault_name::shown(&roots[1], cx), "Work");
            // The link still names the folder, never the display name.
            let link =
                super::super::reader_files::okilum_link(&roots[1], "Plan.md", &Default::default());
            assert!(link.contains("Gate2") && !link.contains("Work"), "{link}");
        });
        // The link opens the renamed vault: its window comes forward, no
        // new window and no error.
        let link =
            super::super::reader_files::okilum_link(&roots[1], "Plan.md", &Default::default());
        cx.update(|cx| open_deep_link_among(&link, &roots, cx));
        cx.run_until_parked();
        assert_eq!(
            cx.windows().len(),
            2,
            "the link reuses the renamed vault's window"
        );

        // A blank name goes back to the folder name.
        cx.update(|cx| vault_name::set(&roots[1], "   ", cx));
        cx.run_until_parked();
        assert_eq!(
            title(cx, &roots[1]).as_deref(),
            Some("Okilum — Plan — Gate2")
        );
    }

    /// #1327: a rename made on another machine arrives as a changed
    /// `.okilum/vault.json` (Syncthing); the open window takes the new name
    /// without a restart. A broken file afterwards means the folder name.
    #[gpui::test]
    fn a_name_synced_from_another_machine_shows_without_restart(cx: &mut TestAppContext) {
        use super::super::{reader_history::TestSessionDirectory, vault_name, Opts};
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let root = fixture.path().join("Gate1");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Plan.md"), "# Plan").unwrap();
        let root = root.canonicalize().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            open_window(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Plan.md".into()),
                    session_directory: Some(state.clone()),
                    index_dir: Some(fixture.path().join("index")),
                    ..Default::default()
                },
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        let window = cx.update(|cx| cx.windows()[0]);
        let title = |cx: &mut TestAppContext| {
            gpui::VisualTestContext::from_window(window, cx).window_title()
        };
        let wait = |cx: &mut TestAppContext| {
            for _ in 0..30 {
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(100));
                cx.run_until_parked();
            }
        };
        assert_eq!(title(cx).as_deref(), Some("Okilum — Plan — Gate1"));
        // Machine A renamed the vault; Syncthing replaces the file here.
        let file = root.join(vault_name::DIRECTORY).join("vault.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"version":1,"name":"Work"}"#).unwrap();
        wait(cx);
        assert_eq!(
            title(cx).as_deref(),
            Some("Okilum — Plan — Work"),
            "picked up"
        );
        cx.update(|cx| assert_eq!(vault_name::shown(&root, cx), "Work"));
        // A conflict copy next to it is never read.
        std::fs::write(
            file.with_file_name("vault.sync-conflict-20261015-120000-ABCDEFG.json"),
            r#"{"version":1,"name":"Home"}"#,
        )
        .unwrap();
        wait(cx);
        assert_eq!(title(cx).as_deref(), Some("Okilum — Plan — Work"));
        // A broken file means the folder name, and is left as it is.
        std::fs::write(&file, b"{ broken").unwrap();
        wait(cx);
        assert_eq!(
            title(cx).as_deref(),
            Some("Okilum — Plan — Gate1"),
            "fallback"
        );
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"{ broken",
            "never rewritten"
        );
    }

    /// #1327: the name is read back from the vault after a restart, and the
    /// restored windows carry it.
    #[gpui::test]
    fn a_vault_name_survives_restart_and_restore(cx: &mut TestAppContext) {
        use super::super::{
            reader_history::TestSessionDirectory, reader_open_windows, reader_startup, vault_name,
            Opts,
        };
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let roots: Vec<PathBuf> = ["Gate1", "Gate2", "Gate3"]
            .iter()
            .map(|name| {
                let root = fixture.path().join(name);
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(root.join("Plan.md"), "# Plan").unwrap();
                root.canonicalize().unwrap()
            })
            .collect();
        let base = Opts {
            session_directory: Some(state.clone()),
            index_dir: Some(fixture.path().join("index")),
            ..Default::default()
        };
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            reader_open_windows::install(state.clone(), cx);
            cx.set_global(reader_startup::ReopenBase(base.clone()));
            cx.set_quit_mode(gpui::QuitMode::Explicit);
        });
        for root in &roots {
            cx.update(|cx| {
                open_window(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Plan.md".into()),
                        ..base.clone()
                    },
                    cx,
                )
                .unwrap()
            });
            cx.run_until_parked();
        }
        cx.update(|cx| vault_name::set(&roots[1], "Work", cx));
        cx.update(reader_open_windows::quit);
        cx.update(|cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }
            // The next process starts with nothing in memory.
            cx.set_global(vault_name::Names::default());
        });
        cx.run_until_parked();
        cx.update(|cx| {
            cx.set_global(reader_startup::Startup::default());
            reader_startup::launch(base.clone(), cx);
        });
        for _ in 0..60 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
            cx.run_until_parked();
        }
        let windows: Vec<gpui::AnyWindowHandle> = cx.update(|cx| {
            readers(cx)
                .into_iter()
                .filter(|r| cx.windows().contains(&r.read(cx).reader_window))
                .map(|r| r.read(cx).reader_window)
                .collect()
        });
        let mut titles: Vec<String> = windows
            .into_iter()
            .filter_map(|window| gpui::VisualTestContext::from_window(window, cx).window_title())
            .collect();
        titles.sort();
        assert_eq!(
            titles,
            [
                "Okilum — Plan — Gate1",
                "Okilum — Plan — Gate3",
                "Okilum — Plan — Work",
            ],
            "all three restored, the renamed one by its name"
        );
    }

    /// #1049: a link to a vault name that two known vaults share opens a
    /// chooser listing both (the one without the note cannot be chosen); a
    /// newer link replaces it; choosing opens that vault on the note.
    #[gpui::test]
    fn same_name_vaults_ask_which_one_and_open_the_choice(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home/Notes");
        let work = fixture.path().join("work/Notes");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(home.join("Plan.md"), "# Plan").unwrap();
        let home = home.canonicalize().unwrap();
        let work = work.canonicalize().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            super::super::reader_ui_state::install(&fixture.path().join("ui-state"), cx);
            open_deep_link_among(
                "okilum://v/Notes/Plan.md",
                &[home.clone(), work.clone()],
                cx,
            );
        });
        cx.run_until_parked();
        let chooser = cx.update(|cx| {
            assert_eq!(cx.windows().len(), 1, "the chooser, nothing opened yet");
            assert!(cx.global::<Readers>().0.is_empty());
            cx.global::<LinkChooser>().0.expect("a chooser is shown")
        });
        let choices = cx.update(|cx| {
            chooser
                .downcast::<Root>()
                .unwrap()
                .update(cx, |root, _, cx| {
                    root.view()
                        .clone()
                        .downcast::<LinkVaultChoice>()
                        .ok()
                        .unwrap()
                        .read(cx)
                        .choices
                        .iter()
                        .map(|(root, opts)| (root.clone(), opts.is_some()))
                        .collect::<Vec<_>>()
                })
                .unwrap()
        });
        assert_eq!(choices, vec![(home.clone(), true), (work.clone(), false)]);
        // A newer link replaces the chooser: still one window.
        cx.update(|cx| {
            open_deep_link_among(
                "okilum://v/Notes/Plan.md",
                &[home.clone(), work.clone()],
                cx,
            )
        });
        cx.run_until_parked();
        let chooser = cx.update(|cx| {
            assert_eq!(cx.windows().len(), 1, "one chooser at a time");
            cx.global::<LinkChooser>().0.unwrap()
        });
        // Choosing the home vault opens it on the note and trusts it.
        let _ = cx.update(|cx| {
            chooser.update(cx, |_, window, cx| {
                cx.default_global::<LinkChooser>().0 = None;
                open_from_prompt(
                    &home,
                    super::super::Opts {
                        vault: Some(home.clone()),
                        note: Some("Plan.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let readers = &cx.global::<Readers>().0;
            assert_eq!(readers.len(), 1);
            assert_eq!(readers[0].1, home);
            assert!(super::super::reader_ui_state::link_trusted(&home, cx));
        });
    }

    /// #1269: a link's line lands through the entry path an OS link takes,
    /// from a cold start (a trusted vault, no window) and into the window
    /// already showing that vault. Roots are canonical, as `known_roots`
    /// makes them (verbatim `\\?\` paths on Windows).
    #[gpui::test]
    fn line_links_land_on_the_line_cold_and_in_a_running_window(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("Notes");
        std::fs::create_dir_all(&root).unwrap();
        // Paragraph i is block i-1 on file line 2i-1 (line 81: paragraph 41, block 40).
        let body: String = (1..=60).map(|i| format!("Paragraph {i}.\n\n")).collect();
        std::fs::write(root.join("target.md"), &body).unwrap();
        std::fs::write(root.join("other.md"), &body).unwrap();
        let root = root.canonicalize().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            super::super::bind_keys(cx);
            install(cx);
            super::super::reader_ui_state::install(&fixture.path().join("ui-state"), cx);
            super::super::reader_ui_state::trust_for_links(&root, cx);
        });
        let settle = |cx: &mut TestAppContext| {
            for _ in 0..5 {
                cx.run_until_parked();
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(300));
            }
            cx.run_until_parked();
        };
        let landed = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                let readers = &cx.global::<Readers>().0;
                assert_eq!(readers.len(), 1, "one Reader for the vault");
                let reader = readers[0].0.upgrade().unwrap();
                let reader = reader.read(cx);
                let top = reader.content.read(cx).list_state().logical_scroll_top();
                (
                    reader.current_rel.clone(),
                    top.item_ix,
                    reader.link_notice.clone(),
                )
            })
        };
        let open = |link: &str, cx: &mut TestAppContext| {
            let root = root.clone();
            cx.update(|cx| open_deep_link_among(link, &[root], cx));
        };

        open("okilum://v/Notes/target.md?line=81", cx);
        settle(cx);
        assert_eq!(landed(cx), ("target.md".into(), 40, None), "cold start");

        // Positive control: without a line the running window opens a note at the top.
        open("okilum://v/Notes/other.md", cx);
        settle(cx);
        assert_eq!(landed(cx), ("other.md".into(), 0, None));

        open("okilum://v/Notes/target.md?line=81", cx);
        settle(cx);
        assert_eq!(landed(cx), ("target.md".into(), 40, None), "running window");
    }

    /// #1271: the link confirmation answers the keyboard like any dialog:
    /// Enter is Open (the vault opens and is trusted), Esc is Cancel
    /// (nothing opens). Another key leaves it waiting.
    #[gpui::test]
    fn link_confirmation_answers_enter_and_escape(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("Notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Plan.md"), "# Plan\n").unwrap();
        let root = root.canonicalize().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            super::super::bind_keys(cx);
            install(cx);
            super::super::reader_ui_state::install(&fixture.path().join("ui-state"), cx);
        });
        let prompt = |cx: &mut TestAppContext| {
            let known = [root.clone()];
            cx.update(|cx| open_deep_link_among("okilum://v/Notes/Plan.md", &known, cx));
            cx.run_until_parked();
            cx.update(|cx| {
                cx.global::<LinkPrompts>()
                    .0
                    .get(&root)
                    .map(|(handle, _)| *handle)
                    .expect("the link asks first")
            })
        };
        let state = |cx: &mut TestAppContext, window: AnyWindowHandle| {
            cx.update(|cx| {
                (
                    cx.windows().contains(&window),
                    cx.global::<Readers>().0.len(),
                    super::super::reader_ui_state::link_trusted(&root, cx),
                )
            })
        };

        let window = prompt(cx);
        let mut visual = gpui::VisualTestContext::from_window(window, cx);
        visual.simulate_keystrokes("a");
        cx.run_until_parked();
        assert_eq!(
            state(cx, window),
            (true, 0, false),
            "another key: still asking"
        );
        visual.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(state(cx, window), (false, 0, false), "Esc cancels");

        let window = prompt(cx);
        let mut visual = gpui::VisualTestContext::from_window(window, cx);
        visual.simulate_keystrokes("enter");
        cx.run_until_parked();
        // The prompt closes once the vault's window exists (a 50 ms poll).
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(500));
        cx.run_until_parked();
        assert_eq!(
            state(cx, window),
            (false, 1, true),
            "Enter opens and trusts"
        );
    }

    /// #1110: on a clean first run, choosing a vault folder on the start
    /// screen (the picker's `dispatch_path`) never leaves the app without a
    /// window, and the vault opens.
    #[gpui::test]
    fn first_run_choosing_a_vault_folder_keeps_a_window_until_it_opens(cx: &mut TestAppContext) {
        use super::super::{reader_history::TestSessionDirectory, reader_startup, Opts};
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let root = fixture.path().join("Brain");
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::write(root.join("Inbox/First.md"), "# First").unwrap();
        let root = root.canonicalize().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(TestSessionDirectory(state.clone()));
            reader_startup::launch(
                Opts {
                    session_directory: Some(state.clone()),
                    index_dir: Some(fixture.path().join("index")),
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(
            cx.windows().len(),
            1,
            "positive control: the start screen is shown"
        );
        let entry = cx.update(|cx| cx.windows()[0]);
        cx.update(|cx| {
            dispatch_path(&root, cx);
            assert_eq!(
                cx.windows(),
                vec![entry],
                "the start window stays while the vault window is prepared"
            );
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                cx.windows().len(),
                1,
                "the vault window replaced the start window"
            );
            assert!(!cx.windows().contains(&entry));
            let readers = &cx.global::<Readers>().0;
            assert_eq!(readers.len(), 1);
            assert_eq!(readers[0].1, root, "the chosen vault is open");
        });
    }

    #[cfg(all(unix, feature = "brain"))]
    #[test]
    fn managed_startup_requires_explicit_argument() {
        assert!(!parse(&[]).unwrap().managed_workspace);
        assert!(parse(&["--managed-workspace"]).unwrap().managed_workspace);
        assert!(parse(&["--managed-workspace", "notes.md"]).is_err());
        assert!(parse(&["--managed-workspace", "--vault", "/notes"]).is_err());
        assert!(parse(&["--brain-endpoint", "127.0.0.1:44171"])
            .unwrap()
            .brain_endpoint
            .is_some());
    }

    #[cfg(not(all(unix, feature = "brain")))]
    #[test]
    fn reader_build_rejects_managed_launch_with_positive_reader_control() {
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--managed-workspace"]).is_err());
        assert!(parse(&["--brain-endpoint", "127.0.0.1:99"]).is_err());
    }

    #[test]
    fn folder_picker_never_promises_read_only() {
        assert_eq!(picker_prompt(true), "Open folder");
        // Positive control: the file picker keeps its own wording.
        assert!(picker_prompt(false).starts_with("Open Markdown"));
        assert!(!picker_prompt(true).to_lowercase().contains("read-only"));
    }

    #[test]
    fn cli_picker_and_url_adapters_share_exact_intent() {
        let root =
            std::env::temp_dir().join(format!("okilum-327-adapters-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let parent =
            root.join("Library/Mobile Documents/iCloud~md~obsidian/Documents/Example Vault");
        std::fs::create_dir_all(&parent).unwrap();
        let file = parent.join("space # Ю%.md");
        std::fs::write(&file, "# Test\n").unwrap();
        let opts = parse(&[file.to_str().unwrap()]).unwrap();
        assert_eq!(opts.open_path.as_deref(), Some(file.as_path()));
        assert!(opts.vault.is_none());
        assert!(opts.index_dir.is_none());
        let intent = OpenIntent::validate(opts.open_path.as_deref().unwrap(), None, None).unwrap();
        assert_eq!(intent.note.as_deref(), Some("space # Ю%.md"));
        assert_eq!(intent.root, parent);
        let url = url::Url::from_file_path(&file).unwrap();
        assert_eq!(file_url_path(url.as_str()).unwrap(), file);
        let selected = picker_path(Ok(Some(vec![file.clone()])), false)
            .unwrap()
            .unwrap();
        assert_eq!(
            OpenIntent::validate(&selected, None, None).unwrap().note,
            intent.note
        );
        assert!(picker_path(Ok(None), false).unwrap().is_none());
        assert!(picker_path(Ok(Some(vec![])), false).is_err());
        assert!(picker_path(Ok(Some(vec![file.clone()])), true).is_err());
        assert!(picker_path(Err(anyhow::anyhow!("picker unavailable")), false).is_err());
        assert!(format!(
            "{:#}",
            picker_path(Ok(Some(vec![root.join("missing")])), true).unwrap_err()
        )
        .contains("missing or inaccessible"));
        for input in [
            "https://example.org/test.md",
            "file://remote-host/test.md",
            "file:///tmp/a.md#heading",
        ] {
            assert!(file_url_path(input).is_err(), "{input}");
        }
        for args in [
            vec!["--vault"],
            vec!["--note", "a.md"],
            vec![file.to_str().unwrap(), "--note", "different.md"],
            vec![file.to_str().unwrap(), "--brain-endpoint", "127.0.0.1:99"],
            vec![file.to_str().unwrap(), "--jump"],
            vec![file.to_str().unwrap(), file.to_str().unwrap()],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        #[cfg(all(unix, feature = "brain"))]
        {
            let managed = parse_args(
                ["--brain-endpoint", "127.0.0.1:99"].map(std::ffi::OsString::from),
                super::super::Opts {
                    vault: Some(root.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let Command::Launch(managed) = managed else {
                panic!("expected launch")
            };
            assert_eq!(managed.brain_endpoint.unwrap().port(), 99);
            assert!(managed.vault.is_none());
        }
        let old = parse(&[
            "--vault",
            parent.to_str().unwrap(),
            "--note",
            file.file_name().unwrap().to_str().unwrap(),
            "--query",
            "Test",
            "--copy-source",
            "--index-dir",
            "/explicit/cache",
        ])
        .unwrap();
        assert_eq!(old.note, intent.note);
        assert!(old.copy_source);
        assert_eq!(old.query.as_deref(), Some("Test"));
        assert_eq!(old.index_dir, Some(PathBuf::from("/explicit/cache")));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "# Test\n");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn vault_opens_coalesce_while_preparing_and_explicit_windows_share_resources(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let other = fixture.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.join("a.md"), "# Alpha\n").unwrap();
        std::fs::write(root.join("b.md"), "# Bravo\n").unwrap();
        std::fs::write(other.join("a.md"), "# Other\n").unwrap();
        let root = root.canonicalize().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            super::super::reader_ui_state::install(&fixture.path().join("ui-state"), cx);
            let opts = super::super::Opts {
                vault: Some(root.clone()),
                note: Some("a.md".into()),
                index_dir: Some(fixture.path().join("index")),
                ..Default::default()
            };
            // Both requests arrive before the first background preparation.
            open_window(opts.clone(), cx).unwrap();
            open_window(opts, cx).unwrap();
        });
        cx.run_until_parked();
        let first = cx.update(|cx| {
            assert_eq!(cx.windows().len(), 1);
            let first = cx.global::<Readers>().0[0].0.upgrade().unwrap();
            assert!(
                first.read(cx).searcher.is_some(),
                "full-vault positive control"
            );
            first
        });
        #[cfg(unix)]
        {
            let alias = fixture.path().join("alias");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            cx.update(|cx| dispatch_path(&alias, cx));
            cx.run_until_parked();
            assert_eq!(cx.windows().len(), 1, "symlink is the same vault");
        }
        cx.update(|cx| {
            let handle = first.read(cx).reader_window;
            handle
                .update(cx, |_, window, _| window.activate_window())
                .unwrap();
            first.update(cx, |reader, _| {
                reader.properties_open = true;
                reader.recent_expanded = true;
            });
            new_window(cx);
        });
        cx.run_until_parked();
        let second = cx.update(|cx| {
            assert_eq!(cx.windows().len(), 2, "explicit New Window bypasses reuse");
            let second = cx
                .global::<Readers>()
                .0
                .last()
                .unwrap()
                .0
                .upgrade()
                .unwrap();
            assert_ne!(first.entity_id(), second.entity_id());
            assert_eq!(second.read(cx).current_rel, "a.md");
            assert!(
                second.read(cx).properties_open,
                "snapshot reaches duplicate restore"
            );
            assert!(second.read(cx).recent_expanded);
            assert_eq!(
                second.read(cx).navigation.history,
                first.read(cx).navigation.history
            );

            assert!(std::sync::Arc::ptr_eq(
                first.read(cx).shared_session.as_ref().unwrap(),
                second.read(cx).shared_session.as_ref().unwrap()
            ));
            assert!(std::sync::Arc::ptr_eq(
                first.read(cx).searcher.as_ref().unwrap(),
                second.read(cx).searcher.as_ref().unwrap()
            ));
            let handle = second.read(cx).reader_window;
            handle
                .update(cx, |_, window, cx| {
                    second.update(cx, |reader, cx| reader.open_note("b.md", None, window, cx))
                })
                .unwrap();
            second
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(first.read(cx).current_rel, "a.md");
            assert_eq!(second.read(cx).current_rel, "b.md");
            assert_eq!(first.read(cx).navigation.history, vec!["a.md".to_string()]);
            assert_eq!(
                second.read(cx).navigation.history,
                vec!["a.md".to_string(), "b.md".to_string()]
            );
            second.update(cx, |reader, cx| reader.record_ui_state(true, cx));
            first.update(cx, |reader, cx| reader.record_ui_state(false, cx));
            let (saved, _) = super::super::reader_ui_state::layout(&root, cx).unwrap();
            assert_eq!(
                saved.note, "b.md",
                "background sibling cannot overwrite active state"
            );
            super::super::reader_ui_state::set_reading(20., 960., cx);
            assert_eq!(super::super::reader_ui_state::font_size(cx), 20.);
            assert_eq!(super::super::reader_ui_state::reading_width(cx), 960.);
        });
        let (release, hold) = async_channel::bounded(1);
        std::fs::write(root.join("b.md"), "# Changed after first window closed\n").unwrap();
        cx.update(|cx| {
            let shared = first.read(cx).shared_session.clone().unwrap();
            shared.lock().unwrap().hold = Some(hold);
            let handle = first.read(cx).reader_window;
            handle
                .update(cx, |_, window, cx| {
                    first.update(cx, |reader, cx| {
                        reader.apply_vault_changes(
                            okilum_core::Changes {
                                changed: ["b.md".to_string()].into(),
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                    })
                })
                .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let handle = second.read(cx).reader_window;
            handle
                .update(cx, |_, window, _| window.activate_window())
                .unwrap();
            new_window(cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(cx.windows().len(), 3);
            let third = cx
                .global::<Readers>()
                .0
                .last()
                .unwrap()
                .0
                .upgrade()
                .unwrap();
            assert_eq!(
                third.read(cx).current_rel,
                "b.md",
                "attachment during a held worker uses the last published snapshot"
            );
            assert!(third.read(cx).searcher.is_some());
            let handle = third.read(cx).reader_window;
            handle
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.update(|cx| {
            let handle = first.read(cx).reader_window;
            handle
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        release.try_send(()).unwrap();
        cx.run_until_parked();
        // Another sibling's poll can own the baseline while this window consumes
        // the completed publication. A refresh must survive that exact ordering.
        let shared = cx.update(|cx| second.read(cx).shared_session.clone().unwrap());
        super::super::reader_session::with_worker_baseline_detached(&shared, || {
            cx.update(|cx| {
                let handle = second.read(cx).reader_window;
                handle
                    .update(cx, |_, window, cx| {
                        second.update(cx, |reader, cx| reader.poll_shared_session(window, cx));
                    })
                    .unwrap();
            });
            cx.run_until_parked();
            cx.update(|cx| {
                assert!(
                    second
                        .read(cx)
                        .note_source
                        .contains("Changed after first window closed"),
                    "published document refresh cannot depend on worker-owned baseline"
                );
            });
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(second
                .read(cx)
                .note_source
                .contains("Changed after first window closed"));
            dispatch_path(&root, cx);
            open_window(
                super::super::Opts {
                    vault: Some(other),
                    index_dir: Some(fixture.path().join("other-index")),
                    ..Default::default()
                },
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        assert_eq!(
            cx.windows().len(),
            2,
            "existing vault focuses survivor; another vault opens independently"
        );
    }

    #[gpui::test]
    fn cold_and_warm_delivery_preserve_existing_reader(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!("okilum-327-events-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\n[Go](sub/Ю.md#Landing)\n").unwrap();
        std::fs::write(root.join("sub/Ю.md"), "# Target\n\n## Landing\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [root.join("start.md"), root.join("sub/Ю.md")] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
            }
            for path in [&root, &root.join("sub")] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
            }
        }
        // A folder's first discovered note follows filesystem enumeration order.
        // Seed the initial selection explicitly so the later file event proves
        // navigation to a different note on every filesystem.
        let state = root.with_extension("state");
        super::super::reader_history::ReadingHistory::record_usable_document(
            &state, &root, "start.md",
        )
        .unwrap();
        let before_tree = tree(&root);
        let (sender, receiver) = async_channel::unbounded();
        // Delivery before app initialization must not be lost.
        sender
            .try_send(vec![url::Url::from_file_path(&root).unwrap().into()])
            .unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            super::super::bind_keys(cx);
            install(cx);
            cx.set_global(super::super::reader_history::TestSessionDirectory(
                state.clone(),
            ));
            receive_events(receiver, cx);
        });
        cx.run_until_parked();
        let first = cx.update(|cx| {
            assert_eq!(cx.global::<Readers>().0.len(), 1);
            let first = cx.global::<Readers>().0[0].0.upgrade().unwrap();
            let reader = first.read(cx);
            assert_eq!(reader.current_rel, "start.md");
            assert_eq!(reader.vault_root, root);
            first.clone()
        });
        sender
            .try_send(vec![url::Url::from_file_path(root.join("sub/Ю.md"))
                .unwrap()
                .into()])
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(cx.global::<Readers>().0.len(), 1);
            assert_eq!(first.read(cx).current_rel, "sub/Ю.md");
            assert_eq!(first.read(cx).vault_root, root);
            assert!(!first.read(cx).single_file);
        });
        // Exercise the actual watcher batch handler with enough entries for a bulk rebuild.
        let window = cx.windows()[0];
        cx.update_window(window, |_, window, cx| {
            first.update(cx, |reader, cx| {
                assert!(reader.searcher.is_some(), "index positive control");
                let index = reader.index_dir.clone();
                assert!(!index.as_ref().unwrap().starts_with(&root));
                let changes = okilum_core::Changes {
                    changed: (0..okilum_core::watch::BULK_THRESHOLD)
                        .map(|i| format!("synthetic-{i}.md"))
                        .collect(),
                    ..Default::default()
                };
                assert!(changes.is_bulk());
                reader.apply_vault_changes(changes, window, cx);
                assert_eq!(reader.index_dir, index);
                assert!(reader.searcher.is_some());
                reader.apply_vault_changes(okilum_core::Changes::default(), window, cx);
            })
        })
        .unwrap();
        let selected = picker_path(Ok(Some(vec![root.clone()])), true)
            .unwrap()
            .unwrap();
        cx.update(|cx| dispatch_path(&selected, cx));
        cx.run_until_parked();
        assert_eq!(
            tree(&root),
            before_tree,
            "file/folder opens and bulk/refresh cannot create canonical entries"
        );
        let before = cx.windows().len();
        sender
            .try_send(vec![url::Url::from_file_path(root.join("missing.md"))
                .unwrap()
                .into()])
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), before + 1, "visible refusal window");
        cx.update(|cx| {
            assert_eq!(
                cx.global::<Readers>().0.len(),
                1,
                "refusal must not open a namesake"
            )
        });
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            "# Start\n\n[Go](sub/Ю.md#Landing)\n"
        );
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            2,
            "no derived files in canonical folder"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&root, &root.join("sub")] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(state).unwrap();
    }
}

#[cfg(all(test, unix, feature = "brain"))]
pub(crate) fn reader_locations(cx: &App) -> Vec<(PathBuf, String)> {
    cx.global::<Readers>()
        .0
        .iter()
        .filter_map(|(reader, root)| {
            reader
                .upgrade()
                .map(|reader| (root.clone(), reader.read(cx).current_rel.clone()))
        })
        .collect()
}

/// Reopen the actual current document, not the process's original CLI target.
#[cfg(any(windows, test))]
pub(crate) fn update_restart_args(cx: &App) -> anyhow::Result<Vec<std::ffi::OsString>> {
    let readers: Vec<_> = cx
        .try_global::<Readers>()
        .into_iter()
        .flat_map(|readers| readers.0.iter())
        .filter_map(|(reader, _)| reader.upgrade())
        .collect();
    if readers.len() > 1 {
        // Several vault windows reopen from the recorded set (#1325); a quick
        // viewer of one file is not part of it.
        anyhow::ensure!(
            readers.iter().all(|reader| !reader.read(cx).single_file),
            "Close other Okilum document windows before restarting to update."
        );
        return Ok(vec![]);
    }
    let Some(reader) = readers.first() else {
        return Ok(vec![]);
    };
    let reader = reader.read(cx);
    if reader.single_file {
        Ok(vec![
            "--".into(),
            reader
                .vault_root
                .join(reader.selected_file())
                .into_os_string(),
        ])
    } else {
        Ok(vec![
            "--vault".into(),
            reader.vault_root.clone().into_os_string(),
            "--note".into(),
            reader.current_rel.clone().into(),
        ])
    }
}

#[cfg(test)]
mod update_restart_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[cfg(test)]
    #[gpui::test]
    fn update_restart_without_document_does_not_reuse_old_cli(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| assert!(update_restart_args(cx).unwrap().is_empty()));
    }

    #[cfg(test)]
    #[gpui::test]
    fn update_restart_tracks_current_note_and_leaves_several_vaults_to_the_recorded_set(
        cx: &mut gpui::TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("A.md"), "# A").unwrap();
        std::fs::write(root.join("Б.md"), "# B").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(cx);
            cx.set_global(crate::reader_history::TestSessionDirectory(
                temp.path().join("state"),
            ));
            open_window(
                crate::Opts {
                    vault: Some(root.clone()),
                    note: Some("Б.md".into()),
                    ..Default::default()
                },
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let args = update_restart_args(cx).unwrap();
            assert_eq!(
                args,
                vec![
                    std::ffi::OsString::from("--vault"),
                    root.clone().into_os_string(),
                    "--note".into(),
                    "Б.md".into()
                ]
            );
            let second = temp.path().join("other-vault");
            std::fs::create_dir(&second).unwrap();
            std::fs::write(second.join("Other.md"), "# Other").unwrap();
            open_window(
                crate::Opts {
                    vault: Some(second),
                    ..Default::default()
                },
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        // Two vault windows: no arguments, so startup reopens both (#1325).
        cx.update(|cx| {
            assert_eq!(
                update_restart_args(cx).unwrap(),
                Vec::<std::ffi::OsString>::new()
            );
        });
    }
}
