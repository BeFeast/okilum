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
        let path = path
            .canonicalize()
            .context("The requested path is missing or inaccessible")?;
        let explicit_root = explicit_root
            .map(|root| {
                let root = root
                    .canonicalize()
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
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            bail!("Choose a local Markdown (.md) file or a directory");
        }
        // Fail before constructing a Reader rather than falling back to its first note.
        if read_primary {
            std::fs::read_to_string(&path).context("The Markdown file cannot be read as UTF-8")?;
        }
        let parent = path
            .parent()
            .context("The document has no containing directory")?;
        let reusable = reusable_root
            .and_then(|root| root.canonicalize().ok())
            .filter(|root| root.is_dir() && path.starts_with(root));
        let single_file = explicit_root.is_none() && reusable.is_none();
        let root = explicit_root
            .or(reusable)
            .unwrap_or_else(|| parent.to_path_buf());
        let relative = path.strip_prefix(&root)?;
        relative
            .to_str()
            .context("The document path is not valid UTF-8")?;
        let note = tessera_core::vault::note_path(relative);
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
            .join("tessera")
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
            let p = std::env::temp_dir().join(format!("tessera-open-{}", uuid::Uuid::new_v4()));
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
        assert!(!f.0.join(".tessera-index").exists());
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
        assert!(OpenIntent::validate(&f.0.join("other.txt"), None, None).is_err());
        assert!(OpenIntent::validate(&f.0.join("notes space"), Some(&f.0), None).is_err());
        assert_eq!(OpenIntent::validate(&f.0, None, None).unwrap().root, f.0);
    }
}

pub(crate) enum Command {
    Help,
    Launch(Box<super::Opts>),
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
                    opts.note = Some(tessera_core::vault::note_path(Path::new(&note)));
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

fn cache_base() -> Result<PathBuf> {
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
    h_flex, v_flex, Root, Sizable, TitleBar,
};

#[derive(Default)]
pub(crate) struct Readers(Vec<(WeakEntity<super::Reader>, PathBuf)>);
impl Global for Readers {}

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
                .filter(|(reader, _)| reader.upgrade().is_some_and(|r| !r.read(cx).single_file))
                .map(|(_, root)| root.clone())
                .collect()
        })
        .unwrap_or_default()
}

actions!(reader_open, [OpenFile, OpenFolder]);

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
    cx.on_action(|_: &OpenFile, cx| pick(false, cx));
    cx.on_action(|_: &OpenFolder, cx| pick(true, cx));
}

pub(crate) fn file_menu() -> Menu {
    Menu {
        name: "File".into(),
        disabled: false,
        items: vec![
            MenuItem::action("Open Markdown File…", OpenFile),
            MenuItem::action("Open Folder…", OpenFolder),
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

fn pick(folder: bool, cx: &mut App) {
    let result = cx.prompt_for_paths(PathPromptOptions {
        files: !folder,
        directories: folder,
        multiple: false,
        prompt: Some(
            if folder {
                "Open read-only folder"
            } else {
                "Open Markdown file"
            }
            .into(),
        ),
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
            Err(error) => show_error(format!("{error:#}"), cx),
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
                "a Markdown file"
            }
        );
    }
    Ok(Some(path))
}

pub(crate) fn dispatch_urls(urls: Vec<String>, cx: &mut App) {
    for url in urls {
        match file_url_path(&url) {
            Ok(path) => dispatch_path(&path, cx),
            Err(error) => show_error(format!("{error:#}"), cx),
        }
    }
}

fn dispatch_path(path: &Path, cx: &mut App) {
    super::reader_startup::supersede(cx);
    // Canonicalization is deliberately off the UI thread, including network paths.
    let path = path.to_owned();
    cx.spawn(async move |cx| {
        let (canonical, is_file) = cx
            .background_executor()
            .spawn(async move {
                let canonical = path.canonicalize().unwrap_or(path);
                let is_file = canonical.is_file();
                (canonical, is_file)
            })
            .await;
        cx.update(|cx| dispatch_canonical_path(&canonical, is_file, cx));
    })
    .detach();
}

fn dispatch_canonical_path(path: &Path, is_file: bool, cx: &mut App) {
    let existing = cx.try_global::<Readers>().and_then(|readers| {
        readers.0.iter().rev().find_map(|(weak, root)| {
            let reader = weak.upgrade()?;
            let state = reader.read(cx);
            if !is_file
                || !path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                || state.single_file
                || !path.starts_with(root)
            {
                return None;
            }
            let relative = path.strip_prefix(root).ok()?;
            relative.to_str()?;
            let rel = tessera_core::vault::note_path(relative);
            Some((reader.clone(), state.reader_window, rel))
        })
    });
    if let Some((reader, window, rel)) = existing {
        let _ = window.update(cx, |_, window, cx| {
            reader.update(cx, |reader, cx| reader.open_note(&rel, None, window, cx));
            window.activate_window();
        });
        cx.activate(true);
        return;
    }
    let opts = super::Opts {
        open_path: Some(path.to_path_buf()),
        reusable_roots: reusable_roots(cx),
        ..Default::default()
    };
    if let Err(error) = open_window(opts, cx) {
        show_error(format!("{error:#}"), cx);
    }
}

pub(crate) fn open_window(opts: super::Opts, cx: &mut App) -> Result<()> {
    let _phase = super::reader_diagnostics::phase(cx, "native_window_open");
    let _key_phase = super::reader_diagnostics::phase(cx, "window_key_and_geometry");
    let key = super::window_state::reader_key(&opts);
    super::reader_ui_state::guard_window_root(&key, cx);
    let (options, frame_key) = super::window_state::prepare(window_options(cx), &key, cx);
    drop(_key_phase);
    cx.open_window(options, |window, cx| {
        super::sync_appearance(window, cx);
        window
            .observe_window_appearance(|window, cx| {
                super::sync_appearance(window, cx);
                window.refresh();
            })
            .detach();
        window.set_window_title("Tessera — Opening document");
        let reader = cx.new(|cx| super::Reader::new(opts, window, cx));
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
        super::window_state::track(&root, frame_key, window, cx);
        root
    })?;
    super::reader_startup::supersede(cx);
    cx.activate(true);
    Ok(())
}

pub(crate) fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1500.), px(1000.)),
            cx,
        ))),
        window_min_size: Some(size(px(600.), px(400.))),
        app_id: Some("tessera".into()),
        #[cfg(target_os = "linux")]
        window_background: WindowBackgroundAppearance::Opaque,
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        titlebar: Some(super::reader_titlebar_options()),
        ..TitleBar::window_options()
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
fn show_error(message: String, cx: &mut App) {
    eprintln!("Cannot open document: {message}");
    let (options, frame_key) = super::window_state::prepare(window_options(cx), "open-error", cx);
    let result = cx.open_window(options, |window, cx| {
        window.set_window_title("Tessera — Cannot open document");
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
        }
    }
    #[gpui::test]
    fn ordinary_startup_restores_local_document_and_first_run_opens_entry(cx: &mut TestAppContext) {
        use super::super::{reader_history::ReadingHistory, reader_startup, Opts};
        let fixture =
            std::env::temp_dir().join(format!("tessera-startup364-{}", uuid::Uuid::new_v4()));
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
    fn restored_quick_session_does_not_index_its_folder(cx: &mut TestAppContext) {
        use super::super::{reader_history::ReadingHistory, reader_startup, Opts};
        let fixture =
            std::env::temp_dir().join(format!("tessera-quick-restore-{}", uuid::Uuid::new_v4()));
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
            std::env::temp_dir().join(format!("tessera-startup-delivery-{}", uuid::Uuid::new_v4()));
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
            )
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
    fn cli_picker_and_url_adapters_share_exact_intent() {
        let root =
            std::env::temp_dir().join(format!("tessera-327-adapters-{}", uuid::Uuid::new_v4()));
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
    fn cold_and_warm_delivery_preserve_existing_reader(cx: &mut TestAppContext) {
        let root =
            std::env::temp_dir().join(format!("tessera-327-events-{}", uuid::Uuid::new_v4()));
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
                let changes = tessera_core::Changes {
                    changed: (0..tessera_core::watch::BULK_THRESHOLD)
                        .map(|i| format!("synthetic-{i}.md"))
                        .collect(),
                    ..Default::default()
                };
                assert!(changes.is_bulk());
                reader.apply_vault_changes(changes, window, cx);
                assert_eq!(reader.index_dir, index);
                assert!(reader.searcher.is_some());
                reader.apply_vault_changes(tessera_core::Changes::default(), window, cx);
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
                2,
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

#[cfg(test)]
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
