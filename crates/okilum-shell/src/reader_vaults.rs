//! Known vaults live in application state, never in a vault or derived index.
use super::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Availability {
    Checking,
    Available,
    Unavailable,
}

impl Availability {
    /// Runs only on a worker: a disconnected mount must not block the UI.
    fn probe(root: &Path) -> Self {
        if std::fs::metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
            Self::Available
        } else {
            Self::Unavailable
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Checking => " — Checking…",
            Self::Available => "",
            Self::Unavailable => " — Unavailable",
        }
    }
}

#[derive(Default)]
struct Entries {
    rows: Vec<(PathBuf, Availability)>,
    inline_actions: Vec<SidebarAction>,
    show_hidden: bool,
    loaded: bool,
    removing: std::collections::BTreeSet<PathBuf>,
    error: Option<String>,
}

type Model = Rc<RefCell<Entries>>;

/// Read history afresh on every opening. The menu appears immediately, and
/// each path is checked independently so one unresponsive mount holds no
/// other row back. A closed menu drops its pending results.
pub(super) fn menu(
    menu: PopupMenu,
    reader: WeakEntity<Reader>,
    inline_actions: &[SidebarAction],
    show_hidden: bool,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let model = Rc::new(RefCell::new(Entries {
        inline_actions: inline_actions.to_vec(),
        show_hidden,
        ..Entries::default()
    }));
    let directory = reader
        .upgrade()
        .and_then(|reader| reader.read(cx).session_directory.clone());
    let loading = model.clone();
    let view = reader.clone();
    cx.spawn_in(window, async move |menu, cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                let directory =
                    directory.ok_or_else(|| anyhow::anyhow!("Application state is unavailable"))?;
                reader_history::ReadingHistory::known_vault_roots(&directory)
            })
            .await;
        let _ = menu.update_in(cx, |menu, window, cx| {
            let roots = {
                let mut state = loading.borrow_mut();
                state.loaded = true;
                match result {
                    Ok(roots) => {
                        state.rows = roots
                            .iter()
                            .cloned()
                            .map(|root| (root, Availability::Checking))
                            .collect();
                        roots
                    }
                    Err(error) => {
                        state.error = Some(format!("Known vaults could not be loaded: {error:#}"));
                        Vec::new()
                    }
                }
            };
            rebuild(menu, view.clone(), loading.clone(), window, cx);
            for root in roots {
                let model = loading.clone();
                let reader = view.clone();
                cx.spawn_in(window, async move |menu, cx| {
                    let checked_root = root.clone();
                    let availability = cx
                        .background_executor()
                        .spawn(async move { Availability::probe(&checked_root) })
                        .await;
                    let _ = menu.update_in(cx, |menu, window, cx| {
                        if let Some((_, status)) = model
                            .borrow_mut()
                            .rows
                            .iter_mut()
                            .find(|(path, _)| *path == root)
                        {
                            *status = availability;
                        }
                        rebuild(menu, reader, model, window, cx);
                    });
                })
                .detach();
            }
        });
    })
    .detach();
    populate(menu, reader, model, cx)
}

fn rebuild(
    menu: &mut PopupMenu,
    reader: WeakEntity<Reader>,
    model: Model,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) {
    menu.rebuild(window, cx, |menu, _, cx| populate(menu, reader, model, cx));
}

fn populate(
    menu: PopupMenu,
    reader: WeakEntity<Reader>,
    model: Model,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(view) = reader.upgrade() else {
        return menu;
    };
    let current = view.read(cx).vault_root.clone();
    let single_file = view.read(cx).single_file;
    let state = model.borrow();
    let mut menu = menu
        .min_w(px(340.))
        .max_w(px(620.))
        .max_h(px(420.))
        .scrollable(true)
        .label("Known vaults");
    if !state.loaded {
        menu = menu.label("Loading…");
    } else if state.rows.is_empty() && state.error.is_none() {
        menu = menu.label("No known vaults");
    }
    if let Some(error) = &state.error {
        menu = menu.label(error.clone());
    }
    for (index, (root, availability)) in state.rows.iter().enumerate() {
        let root = root.clone();
        let availability = *availability;
        let available = availability == Availability::Available;
        let label = format!("{}{}", vault_name::shown(&root, cx), availability.suffix());
        let tooltip = vault_name::path_tooltip(&root);
        let removing = state.removing.contains(&root);
        let checked = !single_file && root == current;
        let click_root = root.clone();
        let click_reader = reader.clone();
        let remove_root = root.clone();
        let remove_reader = reader.clone();
        let remove_model = model.clone();
        let popup = cx.entity().downgrade();
        menu = menu.item(
            PopupMenuItem::element(move |_, _| {
                let remove_root = remove_root.clone();
                let remove_reader = remove_reader.clone();
                let remove_model = remove_model.clone();
                let popup = popup.clone();
                let tooltip = tooltip.clone();
                h_flex()
                    .id(("known-vault", index))
                    .debug_selector(move || format!("known-vault-{index}"))
                    .flex_1()
                    .gap_3()
                    .min_w_0()
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                    .child(
                        div()
                            .debug_selector(move || {
                                format!("known-vault-status-{index}-{availability:?}")
                            })
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(label.clone()),
                    )
                    .child(
                        Button::new(("remove-known-vault", index))
                            .debug_selector(move || format!("remove-known-vault-{index}"))
                            .small()
                            .ghost()
                            .label(if removing {
                                "Removing…"
                            } else {
                                "Remove from list"
                            })
                            .disabled(removing)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let _ = popup.update(cx, |menu, cx| {
                                    remove(
                                        menu,
                                        remove_reader.clone(),
                                        remove_model.clone(),
                                        remove_root.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    )
            })
            .checked(checked)
            .on_click(move |_, _, cx| {
                if available && !removing {
                    open(&click_reader, &click_root, cx);
                }
            }),
        );
    }
    let inline_actions = state.inline_actions.clone();
    let show_hidden = state.show_hidden;
    drop(state);
    let reveal = current.clone();
    let settings_reader = reader.clone();
    menu = menu
        .separator()
        .menu(
            SidebarNameItem::SwitchVault.label(),
            Box::new(reader_open::OpenFolder),
        )
        .item(
            PopupMenuItem::new(SidebarNameItem::RevealVault.label())
                .on_click(move |_, window, cx| reader_files::reveal(&reveal, window, cx)),
        );
    if !single_file {
        menu = menu.item(
            PopupMenuItem::new(SidebarNameItem::VaultSettings.label()).on_click(move |_, _, cx| {
                reader_settings::show_vault(Some(settings_reader.clone()), cx)
            }),
        );
    }
    // Keep the header actions when asynchronous vault availability/removal
    // rebuilds this menu; inline sidebar modes occupy their former slots.
    if !inline_actions.is_empty() {
        menu = inline_actions
            .iter()
            .fold(menu.separator(), |menu, &action| {
                menu.item(
                    PopupMenuItem::new(action.label())
                        .checked(action == SidebarAction::ShowHidden && show_hidden)
                        .on_click(sidebar_action_click(reader.clone(), action)),
                )
            });
    }
    menu
}

fn remove(
    menu: &mut PopupMenu,
    reader: WeakEntity<Reader>,
    model: Model,
    root: PathBuf,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) {
    let Some(directory) = reader
        .upgrade()
        .and_then(|reader| reader.read(cx).session_directory.clone())
    else {
        return;
    };
    if !model.borrow_mut().removing.insert(root.clone()) {
        return;
    }
    rebuild(menu, reader.clone(), model.clone(), window, cx);
    cx.spawn_in(window, async move |menu, cx| {
        let removed_root = root.clone();
        let result = cx
            .background_executor()
            .spawn(async move {
                reader_history::ReadingHistory::remove_from_list(&directory, &removed_root)
            })
            .await;
        let _ = menu.update_in(cx, |menu, window, cx| {
            {
                let mut state = model.borrow_mut();
                state.removing.remove(&root);
                match result {
                    Ok(()) => {
                        state.rows.retain(|(path, _)| *path != root);
                        state.error = None;
                    }
                    Err(error) => {
                        state.error = Some(format!(
                            "Vault could not be removed from the list: {error:#}"
                        ))
                    }
                }
            }
            rebuild(menu, reader, model, window, cx);
        });
    })
    .detach();
}

/// Reuse the normal opener, including existing-window focus, the configured
/// destination prompt, and the source save/conflict guard. Never inherit the
/// old vault's note, landing, index path or quick-viewer mode.
fn open(reader: &WeakEntity<Reader>, root: &Path, cx: &mut App) {
    let Some(view) = reader.upgrade() else { return };
    let state = view.read(cx);
    let opts = Opts {
        vault: Some(root.to_path_buf()),
        session_directory: state.session_directory.clone(),
        #[cfg(test)]
        cache_base_override: state
            .session_directory
            .as_ref()
            .map(|state| state.join("cache")),
        ..Default::default()
    };
    reader_open::focused(reader.clone(), cx);
    reader_startup::supersede(cx);
    if let Err(error) = reader_open::open_window(opts, cx) {
        eprintln!("Cannot open known vault: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn missing_paths_are_marked_without_creating_or_forgetting_them() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let root = fixture.path().join("unmounted-vault");
        reader_history::ReadingHistory::record_usable_document(&state, &root, "note.md").unwrap();
        let roots = reader_history::ReadingHistory::known_vault_roots(&state).unwrap();
        assert_eq!(roots, std::slice::from_ref(&root));
        assert_eq!(Availability::probe(&root), Availability::Unavailable);
        assert_eq!(Availability::Unavailable.suffix(), " — Unavailable");
        assert!(!root.exists());
        std::fs::create_dir(&root).unwrap();
        assert_eq!(
            Availability::probe(&root),
            Availability::Available,
            "positive control: a remounted directory becomes usable"
        );
        assert_eq!(
            reader_history::ReadingHistory::known_vault_roots(&state).unwrap(),
            roots
        );
        let file = fixture.path().join("not-a-vault");
        std::fs::write(&file, b"file").unwrap();
        assert_eq!(Availability::probe(&file), Availability::Unavailable);
    }

    #[gpui::test]
    fn name_menu_loads_missing_vault_and_remove_button_does_not_switch_or_touch_files(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let current = fixture.path().join("current");
        let missing = fixture.path().join("missing");
        std::fs::create_dir(&current).unwrap();
        let bytes = b"# Current\r\n";
        std::fs::write(current.join("note.md"), bytes).unwrap();
        reader_history::ReadingHistory::record_usable_document(&state, &missing, "offline.md")
            .unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(current.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(state.clone()),
                        cache_base_override: Some(state.join("cache")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update(visual, |reader, cx| {
            reader.panels.open(reader_layout::Panel::Notes);
            cx.notify();
        });
        visual.run_until_parked();
        let title = visual.debug_bounds("sidebar-vault-title").unwrap();
        visual.simulate_click(title.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("known-vault-0").is_some(),
            "positive control: current row loaded"
        );
        assert!(
            visual
                .debug_bounds("known-vault-status-1-Unavailable")
                .is_some(),
            "the absent vault is marked and remains listed"
        );
        let remove = visual.debug_bounds("remove-known-vault-1").unwrap();
        visual.simulate_click(remove.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("known-vault-1").is_none(),
            "successful removal updates the still-open menu"
        );
        assert!(
            visual.debug_bounds("known-vault-0").is_some(),
            "removal did not dismiss or switch the menu"
        );
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.vault_root, current.canonicalize().unwrap());
            assert_eq!(reader.current_rel, "note.md");
        });
        assert_eq!(
            reader_history::ReadingHistory::known_vault_roots(&state).unwrap(),
            [current.canonicalize().unwrap()]
        );
        assert!(!missing.exists());
        assert_eq!(std::fs::read(current.join("note.md")).unwrap(), bytes);
    }

    #[gpui::test]
    fn known_vault_click_reuses_destination_policy_and_keeps_vault_state_separate(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let roots: Vec<_> = ["one", "two", "three"]
            .into_iter()
            .map(|name| {
                let root = fixture.path().join(name);
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join(format!("{name}.md")), format!("# {name}")).unwrap();
                root.canonicalize().unwrap()
            })
            .collect();
        cx.update(|cx| {
            gpui_component::init(cx);
            reader_open::install(cx);
            cx.set_global(reader_history::TestSessionDirectory(state.clone()));
            reader_ui_state::install(&state, cx);
            reader_ui_state::set_vault_open("current", cx);
            reader_open::finish_open(
                Ok(Opts {
                    vault: Some(roots[0].clone()),
                    session_directory: Some(state.clone()),
                    cache_base_override: Some(state.join("cache")),
                    note: Some("one.md".into()),
                    ..Default::default()
                }),
                cx,
            );
        });
        cx.run_until_parked();
        let reader = cx.update(|cx| reader_open::readers(cx)[0].downgrade());
        let window = cx.update(|cx| reader.upgrade().unwrap().read(cx).reader_window);
        cx.update(|cx| open(&reader, &roots[1], cx));
        cx.run_until_parked();
        cx.update(|cx| {
            let reader = reader.upgrade().unwrap();
            let view = reader.read(cx);
            assert_eq!(
                cx.windows().len(),
                1,
                "current-window preference is respected"
            );
            assert_eq!(view.reader_window, window);
            assert_eq!(view.vault_root, roots[1]);
            assert_eq!(
                view.current_rel, "two.md",
                "old vault note is not carried across"
            );
            assert!(view.document_ready());
            assert!(!view.single_file);
            assert!(view
                .index_dir
                .as_ref()
                .unwrap()
                .starts_with(state.join("cache")));
        });
        cx.update(|cx| {
            reader_ui_state::set_vault_open("new", cx);
            open(&reader, &roots[2], cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 2, "new-window preference is respected");
        cx.update(|cx| {
            assert_eq!(reader.upgrade().unwrap().read(cx).vault_root, roots[1]);
            let other = reader_open::readers(cx)
                .into_iter()
                .find(|reader| reader.read(cx).vault_root == roots[2])
                .unwrap();
            assert_eq!(other.read(cx).current_rel, "three.md");
            // An already-open vault focuses it instead of creating another Reader.
            open(&reader, &roots[2], cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 2);
        cx.update(|cx| {
            reader_ui_state::set_vault_open("current", cx);
            reader_open::open_vault_in_new_window(roots[1].clone(), state.clone(), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            cx.windows().len(),
            3,
            "Settings explicitly opens another window despite the current-window preference"
        );
        cx.update(|cx| {
            assert_eq!(
                reader_open::readers(cx)
                    .iter()
                    .filter(|reader| reader.read(cx).vault_root == roots[1])
                    .count(),
                2,
                "an existing vault is duplicated rather than just focused"
            );
        });
        for (root, name) in roots.iter().zip(["one", "two", "three"]) {
            assert_eq!(
                std::fs::read_to_string(root.join(format!("{name}.md"))).unwrap(),
                format!("# {name}")
            );
        }
    }
}
