//! A vault's display name (#1327): what Okilum calls the vault everywhere it
//! names one, without renaming its folder. An empty or blank name means the
//! folder name. Links and every other lookup keep using the folder (and its
//! canonical root); the display name is never an identity.
use anyhow::Context as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, Disableable as _, WindowExt,
};
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
};

/// The name lives in the vault, so it travels with it (Syncthing): one small
/// file in a service folder that every inventory skips, so it never shows in
/// the tree, search or quick open. Syncthing conflict copies sit next to it
/// and are never read; the last write wins.
pub(crate) use okilum_core::vault_descriptor::DIRECTORY;
use okilum_core::vault_descriptor::{parse, path as file, read_bytes};
/// How often an open vault's name file is looked at for a change made on
/// another machine.
const POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Display names by canonical vault root, as last read from each vault.
#[derive(Default)]
pub(crate) struct Names {
    names: BTreeMap<PathBuf, String>,
    sync_names: BTreeMap<PathBuf, String>,
    watched: std::collections::BTreeSet<PathBuf>,
}
impl Global for Names {}

/// The name in the vault's file; `None` when it is missing, empty, blank or
/// not readable as JSON. Reading never writes.
pub(crate) fn read(root: &Path) -> Option<String> {
    let value = okilum_core::vault_descriptor::read(root)?;
    clean(value.get("name")?.as_str()?)
}

/// Write the name into the vault's file, keeping anything else in it; `None`
/// removes the name. Written to a temporary file in the same folder and
/// renamed, so another machine never syncs half a file.
fn write(root: &Path, name: Option<&str>) -> anyhow::Result<()> {
    let original = read_bytes(root)?;
    let mut object = match original.as_deref() {
        Some(bytes) => match parse(bytes)? {
            serde_json::Value::Object(object) => object,
            _ => anyhow::bail!("The vault descriptor must be a JSON object"),
        },
        None => serde_json::Map::new(),
    };
    if object.get("name").and_then(serde_json::Value::as_str) == name
        && (name.is_some() || !object.contains_key("name"))
    {
        return Ok(());
    }
    match name {
        Some(name) => object.insert("name".into(), name.into()),
        None => object.remove("name"),
    };

    let _admission = okilum_write_authority::operation(root)?;
    let directory = root.join(DIRECTORY);
    okilum_core::note_files::create_folders(root, Path::new(DIRECTORY), false)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".vault-name-")
        .suffix(".tmp")
        .tempfile_in(&directory)?;
    serde_json::to_writer_pretty(&mut temporary, &object)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    publish(root, original.as_deref(), temporary)?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn publish(
    root: &Path,
    original: Option<&[u8]>,
    temporary: tempfile::NamedTempFile,
) -> anyhow::Result<()> {
    let _admission = okilum_write_authority::operation(root)?;
    anyhow::ensure!(
        read_bytes(root)?.as_deref() == original,
        "The vault descriptor changed while saving the name. Try again."
    );
    if original.is_some() {
        temporary.persist(file(root))
    } else {
        temporary.persist_noclobber(file(root))
    }
    .context("Cannot publish the vault name")?;
    Ok(())
}

fn stamp(root: &Path) -> Option<(std::time::SystemTime, u64)> {
    let metadata = std::fs::metadata(file(root)).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Keep this vault's name current while a window shows it: read it now and
/// again whenever the file changes, e.g. after Syncthing brings a rename
/// from another machine. Off the UI thread; stops when no window shows it.
pub(crate) fn watch(root: PathBuf, cx: &mut App) {
    if !cx.default_global::<Names>().watched.insert(root.clone()) {
        return;
    }
    cx.spawn(async move |cx| {
        let mut seen = None;
        let mut first = true;
        loop {
            let probe = root.clone();
            let last = seen;
            let changed = cx
                .background_executor()
                .spawn(async move {
                    let now = stamp(&probe);
                    (first || now != last).then(|| (now, read(&probe)))
                })
                .await;
            first = false;
            if let Some((now, name)) = changed {
                seen = now;
                let root = root.clone();
                cx.update(|cx| adopt(&root, name, cx));
            }
            let open = cx.update(|cx| {
                crate::reader_open::readers(cx)
                    .iter()
                    .any(|reader| reader.read(cx).vault_root == root)
            });
            if !open {
                cx.update(|cx| cx.default_global::<Names>().watched.remove(&root));
                return;
            }
            cx.background_executor().timer(POLL).await;
        }
    })
    .detach();
}

/// Take a name read from the vault: shown at once in every window of it.
fn adopt(root: &Path, name: Option<String>, cx: &mut App) {
    let names = &mut cx.default_global::<Names>().names;
    if names.get(root) == name.as_ref() {
        return;
    }
    match name {
        Some(name) => names.insert(root.to_owned(), name),
        None => names.remove(root),
    };
    // After the current update: a rename made in a vault window runs inside
    // that window's update, where the window cannot be updated again (#1364).
    let root = root.to_owned();
    cx.defer(move |cx| retitle(&root, cx));
    cx.refresh_windows();
}

/// Names read for vaults no window shows yet (the start screen's list).
pub(crate) fn remember(known: Vec<(PathBuf, String)>, cx: &mut App) {
    let names = &mut cx.default_global::<Names>().names;
    for (root, name) in known {
        names.entry(root).or_insert(name);
    }
}

/// Local Sync display names are a fallback, independent of the synced name file.
/// No vault file is written just to display an authorized catalog name.
pub(crate) fn remember_sync(known: Vec<(PathBuf, String)>, cx: &mut App) {
    let mut changed = false;
    for (root, name) in known {
        let names = &mut cx.default_global::<Names>().sync_names;
        if names.get(&root) != Some(&name) {
            names.insert(root.clone(), name);
            changed = true;
            cx.defer(move |cx| retitle(&root, cx));
        }
    }
    if changed {
        cx.refresh_windows();
    }
}

/// Read only the local durable display projection; never enables Sync.
pub(crate) fn read_sync_names(profile: &Path) -> Vec<(PathBuf, String)> {
    #[cfg(target_os = "linux")]
    {
        okilum_sync_controller::direct::received_vault_names(&profile.join("sync/direct"))
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = profile;
        vec![]
    }
}

/// The folder's own name, which links and lookups use.
pub(crate) fn folder_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| okilum_core::vault::display_path(root))
}

/// A vault tooltip identifies its folder without abbreviating the home
/// directory or truncating long paths. Vault roots are already absolute.
pub(crate) fn path_tooltip(root: &Path) -> String {
    okilum_core::vault::display_path(root)
}

/// The name a person gave, trimmed; `None` for an empty or blank one.
pub(crate) fn clean(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// What Okilum calls this vault: its display name, else its folder name.
pub(crate) fn shown(root: &Path, cx: &App) -> String {
    stored(root, cx).unwrap_or_else(|| folder_name(root))
}

/// The display name stored for this vault, if any.
pub(crate) fn stored(root: &Path, cx: &App) -> Option<String> {
    cx.try_global::<Names>().and_then(|names| {
        names
            .names
            .get(root)
            .or_else(|| names.sync_names.get(root))
            .cloned()
    })
}

/// Why this vault's name cannot be saved here, before trying: the vault is
/// an archive (its lifecycle, a top-level `read_only: true`) or the folder
/// that holds the name file cannot be written. `None` when a write may work;
/// the write itself still reports any other failure.
pub(crate) fn read_only(root: &Path) -> Option<&'static str> {
    use okilum_core::vault_descriptor::Lifecycle;
    let flagged = Lifecycle::load(root) == Lifecycle::Archive
        || okilum_core::vault_descriptor::read(root)
            .is_some_and(|value| value.get("read_only") == Some(&serde_json::Value::Bool(true)));
    let directory = root.join(DIRECTORY);
    let folder = if directory.is_dir() {
        directory.as_path()
    } else {
        root
    };
    let locked = |path: &Path| {
        std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
    };
    (flagged || locked(folder) || locked(&file(root)))
        .then_some("This vault is read-only (archive), so its name can\u{2019}t be changed.")
}

const NOT_CHANGED: &str = "This vault is read-only (archive). The name was not changed.";

/// Set or clear the display name. A blank name, or the folder name itself,
/// clears it. It is written into the vault, and every window repaints.
#[cfg(test)]
pub(crate) fn set(root: &Path, name: &str, cx: &mut App) {
    if let Err(message) = try_set(root, name, cx) {
        diagnostic_error!("{message}");
    }
}

/// [`set`], returning what to show when the name was not changed.
fn try_set(root: &Path, name: &str, cx: &mut App) -> Result<(), String> {
    // Every attempt in an archive says so, even one that names no change.
    if read_only(root).is_some() {
        return Err(NOT_CHANGED.into());
    }
    let has_sync_name = cx
        .try_global::<Names>()
        .is_some_and(|names| names.sync_names.contains_key(root));
    // Explicit reset still means the local folder name, even with a catalog fallback.
    let name = clean(name)
        .filter(|name| has_sync_name || *name != folder_name(root))
        .or_else(|| has_sync_name.then(|| folder_name(root)));
    if stored(root, cx) == name {
        return Ok(());
    }
    write(root, name.as_deref()).map_err(|error| format!("The name was not changed: {error:#}"))?;
    adopt(root, name, cx);
    Ok(())
}

/// The native window title: `Okilum — Note — Vault`, `Okilum — Vault`
/// before a note opens, and no vault part for a quick viewer of one file.
/// With several vault windows the title is what tells them apart in the
/// Window menu, the taskbar and the window switcher.
pub(crate) fn window_title(note: Option<&str>, vault: Option<&str>) -> String {
    match (note.filter(|note| !note.is_empty()), vault) {
        (Some(note), Some(vault)) => format!("Okilum — {note} — {vault}"),
        (Some(note), None) => format!("Okilum — {note}"),
        (None, Some(vault)) => format!("Okilum — {vault}"),
        (None, None) => "Okilum".into(),
    }
}

impl crate::Reader {
    /// Title this window for `note`, with its vault's current name.
    pub(crate) fn show_window_title(
        &mut self,
        note: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.title_note = note;
        let vault = (!self.single_file).then(|| shown(&self.vault_root, cx));
        window.set_window_title(&window_title(self.title_note.as_deref(), vault.as_deref()));
    }
}

/// Retitle every window of this vault, each through its own window handle.
fn retitle(root: &Path, cx: &mut App) {
    for reader in crate::reader_open::readers(cx) {
        let (handle, mine) = {
            let r = reader.read(cx);
            (r.reader_window, r.vault_root == root && !r.single_file)
        };
        if !mine {
            continue;
        }
        let _ = handle.update(cx, |_, window, cx| {
            reader.update(cx, |r, cx| {
                let note = r.title_note.clone();
                r.show_window_title(note, window, cx);
            })
        });
    }
}

/// «Rename vault…»: a dialog with the current name; Enter or Rename saves,
/// an empty field goes back to the folder name.
pub(crate) fn open_rename(root: PathBuf, window: &mut Window, cx: &mut App) {
    open_rename_input(root, window, cx);
}

fn open_rename_input(root: PathBuf, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let current = shown(&root, cx);
    let folder = folder_name(&root);
    // Explained before any attempt; the controls stay off for an archive.
    let locked = read_only(&root);
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(folder.clone());
        input.set_value(current, window, cx);
        input
    });
    // A failed save keeps the dialog open with the reason in it: the name
    // was not changed, and the person sees why.
    let failure = cx.new(|_| None::<SharedString>);
    let save = {
        let root = root.clone();
        let input = input.clone();
        let failure = failure.clone();
        move |window: &mut Window, cx: &mut App| {
            let value = input.read(cx).value().to_string();
            match try_set(&root, &value, cx) {
                Ok(()) => window.close_dialog(cx),
                Err(message) => {
                    diagnostic_error!("Cannot save the vault name: {message}");
                    failure.update(cx, |failure, cx| {
                        *failure = Some(message.into());
                        cx.notify();
                    });
                    window.refresh();
                }
            }
        }
    };
    let enter = save.clone();
    window
        .subscribe(&input, cx, move |_, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                enter(window, cx);
            }
        })
        .detach();
    let opened = input.clone();
    let field = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let save = save.clone();
        let reset_input = input.clone();
        let reset_folder = folder.clone();
        let failed = failure.read(cx).clone();
        let palette = crate::brand::palette(cx);
        let refused = failure.clone();
        dialog
            .title("Rename vault")
            .width(px(420.))
            // Enter with the field off reaches the dialog itself: in an archive
            // it reports the refusal instead of closing as if it had saved.
            .on_ok(move |_, window, cx| {
                if locked.is_none() {
                    return true;
                }
                refused.update(cx, |failure, cx| {
                    *failure = Some(NOT_CHANGED.into());
                    cx.notify();
                });
                window.refresh();
                false
            })
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Input::new(&input).disabled(locked.is_some()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.text_muted)
                            .child(format!(
                                "Only Okilum shows this name. The folder stays \u{201c}{folder}\u{201d}, and links keep working."
                            )),
                    )
                    .children(locked.filter(|_| failed.is_none()).map(|notice| {
                        div()
                            .debug_selector(|| "vault-rename-read-only".into())
                            .text_sm()
                            .text_color(palette.text)
                            .child(notice)
                    }))
                    .children(failed.map(|message| {
                        div()
                            .debug_selector(|| "vault-rename-error".into())
                            .text_sm()
                            .text_color(palette.danger)
                            .child(message)
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("vault-rename-reset")
                                    .ghost()
                                    .label("Use folder name")
                                    .disabled(locked.is_some())
                                    .on_click(move |_, window, cx| {
                                        reset_input.update(cx, |input, cx| {
                                            input.set_value(reset_folder.clone(), window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("vault-rename-save")
                                    .primary()
                                    .label("Rename")
                                    .disabled(locked.is_some())
                                    .on_click(move |_, window, cx| save(window, cx)),
                            ),
                    ),
            )
    });
    // The dialog takes the focus as it opens; the field gets it afterwards,
    // so typing replaces the shown name at once (muninn check, #1327).
    window.defer(cx, move |window, cx| {
        opened.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    });
    field
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_window_title_names_note_and_vault() {
        assert_eq!(
            window_title(Some("Plan"), Some("Work")),
            "Okilum — Plan — Work"
        );
        assert_eq!(window_title(None, Some("Work")), "Okilum — Work");
        assert_eq!(window_title(Some(""), Some("Work")), "Okilum — Work");
        assert_eq!(window_title(Some("log.txt"), None), "Okilum — log.txt");
        assert_eq!(window_title(None, None), "Okilum");
    }

    #[gpui::test]
    fn vault_tooltip_keeps_the_full_absolute_path_and_name_separate(cx: &mut TestAppContext) {
        let home = PathBuf::from(
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .expect("absolute home directory"),
        );
        let root = home
            .join("Library")
            .join("Mobile Documents")
            .join("Long folder with spaces and Заметки 🧠".repeat(12))
            .join("Obsidian Vault");
        assert!(root.is_absolute());
        let expected = okilum_core::vault::plain_path(&root)
            .to_string_lossy()
            .into_owned();
        assert!(expected.chars().count() > 300, "long-path control");
        cx.update(|cx| {
            remember(vec![(root.clone(), "Main Brain".into())], cx);
            assert_eq!(
                shown(&root, cx),
                "Main Brain",
                "the switcher label stays a name"
            );
            assert_eq!(
                path_tooltip(&root),
                expected,
                "no home alias or middle truncation"
            );
        });
    }

    #[test]
    fn a_blank_name_is_no_name() {
        assert_eq!(clean("  Work notes  ").as_deref(), Some("Work notes"));
        assert_eq!(clean("   "), None);
        assert_eq!(clean(""), None);
        assert_eq!(clean("\t\n"), None);
    }

    #[gpui::test]
    fn the_name_lives_in_the_vault_and_falls_back_to_the_folder(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("Gate1");
        std::fs::create_dir_all(&root).unwrap();
        cx.update(|cx| {
            assert_eq!(shown(&root, cx), "Gate1", "no name yet: the folder");
            set(&root, "  Work  ", cx);
            assert_eq!(shown(&root, cx), "Work", "trimmed");
        });
        assert_eq!(
            read(&root).as_deref(),
            Some("Work"),
            "written into the vault"
        );
        cx.update(|cx| set(&root, "   ", cx));
        assert_eq!(read(&root), None, "blank removes the name");
        cx.update(|cx| assert_eq!(shown(&root, cx), "Gate1"));
    }

    #[test]
    fn a_missing_empty_or_broken_file_is_no_name_and_is_left_alone() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        assert_eq!(read(root), None, "missing");
        std::fs::create_dir_all(root.join(DIRECTORY)).unwrap();
        for broken in [
            &b""[..],
            b"{ not json",
            b"[1,2]",
            b"{\"name\": 7}",
            b"{\"name\": \"  \"}",
        ] {
            std::fs::write(file(root), broken).unwrap();
            assert_eq!(read(root), None, "{:?}", String::from_utf8_lossy(broken));
            assert_eq!(
                std::fs::read(file(root)).unwrap(),
                broken,
                "reading never rewrites"
            );
        }
        // Positive control: a good file reads.
        std::fs::write(file(root), br#"{"version":1,"name":"Work"}"#).unwrap();
        assert_eq!(read(root).as_deref(), Some("Work"));
    }

    /// Typing goes straight into the field: the dialog must not keep the
    /// focus (found on muninn, where typing went nowhere).
    #[gpui::test]
    fn the_rename_field_has_the_focus_and_typing_replaces_the_name(cx: &mut TestAppContext) {
        struct Blank;
        impl Render for Blank {
            // Like the Reader and Settings: the dialog layer is drawn, so the
            // field gets its input handler.
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .children(gpui_component::Root::render_dialog_layer(window, cx))
            }
        }
        cx.update(crate::test_init);
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("Gate2");
        std::fs::create_dir_all(&root).unwrap();
        let (_, visual) = cx
            .add_window_view(|window, cx| gpui_component::Root::new(cx.new(|_| Blank), window, cx));
        let input = visual.update(|window, cx| open_rename_input(root.clone(), window, cx));
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(
                input.read(cx).focus_handle(cx).is_focused(window),
                "the name field has the focus"
            )
        });
        visual.simulate_input("Work");
        visual.run_until_parked();
        // Typing replaces the shown name, which was selected; Enter and the
        // Rename button save it (checked natively on muninn).
        assert_eq!(
            visual.update(|_, cx| input.read(cx).value().to_string()),
            "Work",
            "typed over the shown name"
        );
    }

    #[test]
    fn a_failed_write_leaves_no_temporary_file() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir_all(root.join(DIRECTORY)).unwrap();
        // The name file is a folder, so the rename fails.
        std::fs::create_dir_all(file(root)).unwrap();
        std::fs::write(file(root).join("keep"), b"").unwrap();
        assert!(write(root, Some("Work")).is_err());
        let left: Vec<_> = std::fs::read_dir(root.join(DIRECTORY))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            left,
            [std::ffi::OsString::from(
                okilum_core::vault_descriptor::FILE
            )],
            "only the original entry"
        );
    }

    #[test]
    fn rename_preserves_damaged_descriptor_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir_all(root.join(DIRECTORY)).unwrap();
        let damaged = b"{\"name\":";
        std::fs::write(file(root), damaged).unwrap();
        assert!(write(root, Some("Work")).is_err());
        assert_eq!(std::fs::read(file(root)).unwrap(), damaged);
    }

    #[test]
    fn migration_review_vault_name_rejects_duplicate_keys_without_changing_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir(root.join(DIRECTORY)).unwrap();
        for bytes in [
            br#"{"name":"First","name":"Second"}"#.as_slice(),
            br#"{"name":"Work","extra":[{"x":1,"\u0078":2}]}"#.as_slice(),
        ] {
            std::fs::write(file(root), bytes).unwrap();
            assert!(write(root, Some("New name")).is_err());
            assert_eq!(std::fs::read(file(root)).unwrap(), bytes);
        }
    }

    #[test]
    fn migration_review_vault_name_noop_leaves_metadata_directory_absent() {
        let fixture = tempfile::tempdir().unwrap();
        write(fixture.path(), None).unwrap();
        assert!(!fixture.path().join(DIRECTORY).exists());
    }

    #[test]
    fn name_publication_refuses_a_gardener_update_after_the_original_read() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let metadata = root.join(DIRECTORY);
        std::fs::create_dir(&metadata).unwrap();
        let old = br#"{"name":"Old"}"#;
        let gardener = br#"{"name":"Old","new_field":[1,2,3]}"#;
        for original in [Some(old.as_slice()), None] {
            let mut temporary = tempfile::NamedTempFile::new_in(&metadata).unwrap();
            temporary.write_all(br#"{"name":"New"}"#).unwrap();
            temporary.as_file().sync_all().unwrap();
            std::fs::write(file(root), gardener).unwrap();
            let error = publish(root, original, temporary).unwrap_err();
            assert!(error.to_string().contains("changed"));
            assert_eq!(std::fs::read(file(root)).unwrap(), gardener);
            assert_eq!(std::fs::read_dir(&metadata).unwrap().count(), 1);
        }
    }

    /// An archive (lifecycle or the top-level flag) and a folder without
    /// write permission refuse the rename with a reason, and leave the
    /// descriptor's bytes alone. A writable vault is the positive control.
    #[gpui::test]
    fn a_read_only_vault_reports_the_name_was_not_changed(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let cases: [(&str, &[u8], bool); 4] = [
            (
                "Archive",
                br#"{"name":"Old","lifecycle":{"state":"archive","read_only":true}}"#,
                false,
            ),
            ("Flagged", br#"{"name":"Old","read_only":true}"#, false),
            ("Locked", br#"{"name":"Old"}"#, true),
            ("Writable", br#"{"name":"Old"}"#, false),
        ];
        for (folder, bytes, chmod) in cases {
            let root = fixture.path().join(folder);
            std::fs::create_dir_all(root.join(DIRECTORY)).unwrap();
            std::fs::write(file(&root), bytes).unwrap();
            #[cfg(unix)]
            if chmod {
                use std::os::unix::fs::PermissionsExt as _;
                for path in [file(&root), root.join(DIRECTORY), root.clone()] {
                    let mode = if path.is_dir() { 0o555 } else { 0o444 };
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
                }
            }
            #[cfg(not(unix))]
            if chmod {
                continue;
            }
            let result = cx.update(|cx| try_set(&root, "New", cx));
            if folder == "Writable" {
                assert_eq!(result, Ok(()));
                assert_eq!(read(&root).as_deref(), Some("New"));
                assert_eq!(read_only(&root), None);
                continue;
            }
            assert!(read_only(&root).is_some(), "{folder}: explained up front");
            let message = result.unwrap_err();
            assert!(
                message.contains("read-only") && message.contains("not changed"),
                "{folder}: {message}"
            );
            assert_eq!(std::fs::read(file(&root)).unwrap(), bytes, "{folder}");
            #[cfg(unix)]
            if chmod {
                use std::os::unix::fs::PermissionsExt as _;
                for path in [root.clone(), root.join(DIRECTORY)] {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
            }
        }
    }

    #[test]
    fn writing_keeps_other_fields() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir_all(root.join(DIRECTORY)).unwrap();
        let descriptor = serde_json::json!({"version": 4, "color": "teal", "layout": {"extension": {"kept": true}}});
        std::fs::write(file(root), serde_json::to_vec(&descriptor).unwrap()).unwrap();
        write(root, Some("Work")).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file(root)).unwrap()).unwrap();
        assert_eq!(value["name"], "Work");
        assert_eq!(value["color"], "teal");
        assert_eq!(value["version"], descriptor["version"]);
        assert_eq!(value["layout"], descriptor["layout"]);
        write(root, None).unwrap();
        assert_eq!(read(root), None);
    }
}
