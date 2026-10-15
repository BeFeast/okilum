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

/// Where display names are kept. Whether a name travels with the vault or
/// stays on this machine is still open; either keeps this shape: names by
/// canonical vault root, the existing vault identity.
pub(crate) trait Storage {
    fn save(&self, names: &BTreeMap<PathBuf, String>) -> anyhow::Result<()>;
}

/// Display names by canonical vault root.
#[derive(Default)]
pub(crate) struct Names {
    names: BTreeMap<PathBuf, String>,
    storage: Option<Box<dyn Storage>>,
}
impl Global for Names {}

/// Start with the names a storage holds; later changes are saved to it.
pub(crate) fn install(names: BTreeMap<PathBuf, String>, storage: Box<dyn Storage>, cx: &mut App) {
    cx.set_global(Names {
        names,
        storage: Some(storage),
    });
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
/// clears it. Every window repaints with the new name.
pub(crate) fn set(root: &Path, name: &str, cx: &mut App) {
    let name = clean(name).filter(|name| *name != folder_name(root));
    let state = cx.default_global::<Names>();
    if state.names.get(root) == name.as_ref() {
        return;
    }
    match name {
        Some(name) => state.names.insert(root.to_owned(), name),
        None => state.names.remove(root),
    };
    if let Some(storage) = &state.storage {
        if let Err(error) = storage.save(&state.names) {
            eprintln!("Cannot save the vault name: {error:#}");
        }
    }
    retitle(root, cx);
    cx.refresh_windows();
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
    input.update(cx, |input, cx| {
        input.focus(window, cx);
        input.select_all(window, cx);
    });
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
    fn the_shown_name_falls_back_to_the_folder(cx: &mut TestAppContext) {
        let root = PathBuf::from("/vaults/Gate1");
        cx.update(|cx| {
            assert_eq!(shown(&root, cx), "Gate1", "no name yet: the folder");
            set(&root, "  Work  ", cx);
            assert_eq!(shown(&root, cx), "Work", "trimmed");
            assert_eq!(stored(&root, cx).as_deref(), Some("Work"));
            set(&root, "   ", cx);
            assert_eq!(shown(&root, cx), "Gate1", "blank goes back to the folder");
            assert_eq!(stored(&root, cx), None);
            set(&root, "Gate1", cx);
            assert_eq!(stored(&root, cx), None, "the folder name stores nothing");
            // Another vault is not affected.
            set(&root, "Work", cx);
            assert_eq!(shown(Path::new("/vaults/Gate2"), cx), "Gate2");
        });
    }
}
