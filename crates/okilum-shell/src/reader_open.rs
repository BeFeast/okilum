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
        if positional.replace(PathBuf::from(arg)).is_some() {
            bail!("Open one file or folder per CLI invocation");
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

pub(crate) fn cache_base() -> Result<PathBuf> {
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
    h_flex, v_flex, ActiveTheme as _, Root, Sizable, TitleBar,
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
        readers.push((reader, root));
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
    use okilum_core::deep_link::{parse, resolve, Resolution};
    super::reader_startup::supersede(cx);
    let link = match parse(link) {
        Ok(link) => link,
        Err(refused) => return show_error(anyhow::anyhow!(refused.message()), cx),
    };
    match resolve(&link, &known_roots(cx), &|path| path.is_file()) {
        Resolution::Open {
            root,
            rel,
            position,
        } => {
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
        // A chooser follows; until then the user is told, never guessed for.
        Resolution::Choose { vault, roots, .. } => show_error(
            anyhow::anyhow!(
                "Several vaults are named \u{201c}{vault}\u{201d}:\n{}\nOpen the one you want first, then use the link again.",
                roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            cx,
        ),
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
pub(crate) fn open_window(mut opts: super::Opts, cx: &mut App) -> Result<()> {
    if opts.reusable_roots.is_empty() {
        opts.reusable_roots = reusable_roots(cx);
    }
    cx.spawn(async move |cx| {
        let resolved = cx
            .background_executor()
            .spawn(async move {
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
            .await;
        cx.update(|cx| match resolved {
            Ok(opts) => {
                if !focus_existing(&opts, cx) {
                    if let Err(error) = create_window(opts, None, None, cx) {
                        show_error(error, cx);
                    }
                }
            }
            Err(error) => show_error(error, cx),
        });
    })
    .detach();
    Ok(())
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
        window.set_window_title("Okilum — Opening document");
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
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
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
        let view = cx.new(|_| LinkConfirm {
            root: view_root,
            name,
        });
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

struct LinkConfirm {
    root: PathBuf,
    name: String,
}
impl Render for LinkConfirm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (open_root, cancel_root) = (self.root.clone(), self.root.clone());
        v_flex()
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
                    .child(self.root.display().to_string()),
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
    anyhow::ensure!(
        readers.len() <= 1,
        "Close other Okilum document windows before restarting to update."
    );
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
    fn update_restart_tracks_current_note_and_refuses_multiple_windows(
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
        cx.update(|cx| {
            assert!(update_restart_args(cx).is_err());
        });
    }
}
