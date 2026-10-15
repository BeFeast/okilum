//! A vault's display name (#1327): what Okilum calls the vault everywhere it
//! names one, without renaming its folder. An empty or blank name means the
//! folder name. Links and every other lookup keep using the folder (and its
//! canonical root); the display name is never an identity.
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, WindowExt,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The name lives in the vault, so it travels with it (Syncthing): one small
/// file in a service folder that every inventory skips, so it never shows in
/// the tree, search or quick open. Syncthing conflict copies sit next to it
/// and are never read; the last write wins.
pub(crate) const DIRECTORY: &str = ".okilum";
const FILE: &str = "vault.json";
/// How often an open vault's name file is looked at for a change made on
/// another machine.
const POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Display names by canonical vault root, as last read from each vault.
#[derive(Default)]
pub(crate) struct Names {
    names: BTreeMap<PathBuf, String>,
    watched: std::collections::BTreeSet<PathBuf>,
}
impl Global for Names {}

fn file(root: &Path) -> PathBuf {
    root.join(DIRECTORY).join(FILE)
}

/// The name in the vault's file; `None` when it is missing, empty, blank or
/// not readable as JSON. Reading never writes.
pub(crate) fn read(root: &Path) -> Option<String> {
    let bytes = std::fs::read(file(root)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    clean(value.get("name")?.as_str()?)
}

/// Write the name into the vault's file, keeping anything else in it; `None`
/// removes the name. Written to a temporary file in the same folder and
/// renamed, so another machine never syncs half a file.
fn write(root: &Path, name: Option<&str>) -> anyhow::Result<()> {
    let directory = root.join(DIRECTORY);
    std::fs::create_dir_all(&directory)?;
    let mut value = std::fs::read(file(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let object = value.as_object_mut().expect("an object");
    object.insert("version".into(), 1.into());
    match name {
        Some(name) => object.insert("name".into(), name.into()),
        None => object.remove("name"),
    };
    let temporary = directory.join(format!(".{FILE}.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    std::fs::rename(&temporary, file(root)).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
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
    retitle(root, cx);
    cx.refresh_windows();
}

/// The folder's own name, which links and lookups use.
pub(crate) fn folder_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| okilum_core::vault::display_path(root))
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
    cx.try_global::<Names>()
        .and_then(|names| names.names.get(root).cloned())
}

/// Set or clear the display name. A blank name, or the folder name itself,
/// clears it. It is written into the vault, and every window repaints.
pub(crate) fn set(root: &Path, name: &str, cx: &mut App) {
    let name = clean(name).filter(|name| *name != folder_name(root));
    if stored(root, cx) == name {
        return;
    }
    if let Err(error) = write(root, name.as_deref()) {
        let message = format!("Cannot save the vault name: {error:#}");
        eprintln!("{message}");
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, cx| {
                crate::reader_toast::error(message, window, cx)
            });
        }
        return;
    }
    adopt(root, name, cx);
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
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(folder.clone());
        input.set_value(current, window, cx);
        input
    });
    let save = {
        let root = root.clone();
        let input = input.clone();
        move |window: &mut Window, cx: &mut App| {
            let value = input.read(cx).value().to_string();
            set(&root, &value, cx);
            window.close_dialog(cx);
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
        dialog
            .title("Rename vault")
            .width(px(420.))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Input::new(&input),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(crate::brand::palette(cx).text_muted)
                            .child(format!(
                                "Only Okilum shows this name. The folder stays \u{201c}{folder}\u{201d}, and links keep working."
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("vault-rename-reset")
                                    .ghost()
                                    .label("Use folder name")
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
        cx.update(gpui_component::init);
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
    fn writing_keeps_other_fields() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir_all(root.join(DIRECTORY)).unwrap();
        std::fs::write(file(root), br#"{"version":1,"color":"teal"}"#).unwrap();
        write(root, Some("Work")).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file(root)).unwrap()).unwrap();
        assert_eq!(value["name"], "Work");
        assert_eq!(value["color"], "teal");
        write(root, None).unwrap();
        assert_eq!(read(root), None);
    }
}
