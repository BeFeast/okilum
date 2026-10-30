//! Settings management of the same app-owned list as the vault-name menu.
use super::*;
use reader_history::{KnownVault, ReadingHistory};

#[derive(Clone)]
struct Row {
    vault: KnownVault,
    available: Option<bool>,
}

pub(super) struct Vaults {
    directory: Option<PathBuf>,
    reader: Option<WeakEntity<Reader>>,
    rows: Vec<Row>,
    loading: bool,
    pending: bool,
    generation: u64,
    error: Option<String>,
}

enum Change {
    Remove(PathBuf),
    Default(Option<PathBuf>),
}

impl Vaults {
    pub fn new(
        directory: anyhow::Result<PathBuf>,
        reader: Option<WeakEntity<Reader>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (directory, error) = match directory {
            Ok(directory) => (Some(directory), None),
            Err(error) => (None, Some(format!("Vault list is unavailable: {error:#}"))),
        };
        let mut this = Self {
            directory,
            reader,
            rows: Vec::new(),
            loading: false,
            pending: false,
            generation: 0,
            error,
        };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(directory) = self.directory.clone() else {
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ReadingHistory::known_vaults(&directory) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(vaults) => {
                        this.error = None;
                        this.rows = vaults
                            .into_iter()
                            .map(|vault| Row {
                                vault,
                                available: None,
                            })
                            .collect();
                        for row in &this.rows {
                            let root = row.vault.root.clone();
                            cx.spawn(async move |this, cx| {
                                let checked = root.clone();
                                let available = cx
                                    .background_executor()
                                    .spawn(async move {
                                        std::fs::metadata(checked)
                                            .is_ok_and(|metadata| metadata.is_dir())
                                    })
                                    .await;
                                let _ = this.update(cx, |this, cx| {
                                    if this.generation != generation {
                                        return;
                                    }
                                    if let Some(row) =
                                        this.rows.iter_mut().find(|row| row.vault.root == root)
                                    {
                                        row.available = Some(available);
                                        cx.notify();
                                    }
                                });
                            })
                            .detach();
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!("Vault list could not be loaded: {error:#}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn change(&mut self, change: Change, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(directory) = self.directory.clone() else {
            return;
        };
        self.pending = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match change {
                        Change::Remove(root) => ReadingHistory::remove_from_list(&directory, &root),
                        Change::Default(root) => {
                            ReadingHistory::set_default_vault(&directory, root.as_deref())
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                match result {
                    Ok(()) => this.reload(cx),
                    Err(error) => {
                        this.error = Some(format!("Vault list could not be saved: {error:#}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }
}

fn opened_label(seconds: Option<u64>) -> String {
    seconds
        .and_then(|seconds| i64::try_from(seconds).ok())
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M %Z")
                .to_string()
        })
        .unwrap_or_else(|| "Not recorded".into())
}

impl Render for Vaults {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self
            .reader
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .filter(|reader| !reader.read(cx).single_file)
            .map(|reader| reader.read(cx).vault_root.clone());
        let palette = brand::palette(cx);
        v_flex().gap_3()
            .debug_selector(|| "settings-vaults-list".into())
            .child("Removing a vault from this list keeps its files and last reading position.")
            .child(div().text_sm().text_color(palette.text_muted).child("The default opens when no previous window can be restored. Explicit file and folder opens take priority."))
            .child(h_flex().gap_2()
                .child(Button::new("vaults-open-folder").label("Open folder…").on_click(|_, window, cx| window.dispatch_action(Box::new(reader_open::OpenFolder), cx)))
                .child(Button::new("vaults-refresh").label("Refresh").disabled(self.pending || self.loading).on_click(cx.listener(|this, _, _, cx| this.reload(cx)))))
            .when(self.loading, |view| view.child("Loading vaults…"))
            .when(!self.loading && self.rows.is_empty(), |view| view.child("No known vaults. Open a folder to add one."))
            .children(self.error.as_ref().map(|error| div().text_color(palette.danger).child(error.clone())))
            .children(self.rows.iter().enumerate().map(|(index, row)| {
                let root = row.vault.root.clone();
                let open_root = root.clone();
                let remove_root = root.clone();
                let default_root = (!row.vault.is_default).then_some(root.clone());
                let is_current = current.as_ref() == Some(&root);
                let status = match row.available { Some(true) => None, Some(false) => Some("Unavailable"), None => Some("Checking…") };
                let directory = self.directory.clone();
                v_flex().gap_2().p_3().border_1().border_color(palette.border_subtle).rounded(px(6.))
                    .debug_selector(move || format!("settings-vault-row-{index}").into())
                    .child(h_flex().gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(vault_name::shown(&root, cx)))
                        .when(is_current, |view| view.child("Current"))
                        .when(row.vault.is_default, |view| view.child("Default"))
                        .children(status.map(|status| div().debug_selector(move || format!("settings-vault-status-{index}").into()).text_color(palette.text_muted).child(status))))
                    .child(div().text_sm().text_color(palette.text_muted).child(vault_name::path_tooltip(&root)))
                    .child(div().text_sm().text_color(palette.text_muted).child(format!("Last opened: {}", opened_label(row.vault.last_opened))))
                    .child(h_flex().flex_wrap().gap_2()
                        .child(Button::new(("vaults-new-window", index)).debug_selector(move || format!("vaults-new-window-{index}").into()).label("Open in new window").disabled(self.pending || row.available != Some(true))
                            .on_click(move |_, _, cx| {
                                if let Some(directory) = &directory {
                                    reader_open::open_vault_in_new_window(open_root.clone(), directory.clone(), cx);
                                }
                            }))
                        .child(Button::new(("vaults-default", index)).debug_selector(move || format!("vaults-default-{index}").into()).label(if row.vault.is_default { "Clear default" } else { "Make default" }).disabled(self.pending)
                            .on_click(cx.listener(move |this, _, _, cx| this.change(Change::Default(default_root.clone()), cx))))
                        .child(Button::new(("vaults-remove", index)).debug_selector(move || format!("vaults-remove-{index}").into()).label("Remove from list").disabled(self.pending)
                            .on_click(cx.listener(move |this, _, _, cx| this.change(Change::Remove(remove_root.clone()), cx)))))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn unknown_opening_dates_are_not_fabricated() {
        assert_eq!(opened_label(None), "Not recorded");
        assert_eq!(opened_label(Some(u64::MAX)), "Not recorded");
        assert_ne!(opened_label(Some(1_700_000_000)), "Not recorded");
    }

    #[gpui::test]
    fn settings_actions_persist_default_and_remove_without_touching_vault_bytes(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let vault = fixture.path().join("vault");
        let absent = fixture.path().join("unmounted");
        std::fs::create_dir(&vault).unwrap();
        let bytes = b"# Exact bytes\r\n";
        std::fs::write(vault.join("note.md"), bytes).unwrap();
        ReadingHistory::record_document_mode(&state, &absent, "offline.md", false).unwrap();
        ReadingHistory::record_document_opened(
            &state,
            &vault,
            "note.md",
            false,
            Some(1_700_000_000),
        )
        .unwrap();
        cx.update(gpui_component::init);
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Vaults::new(Ok(state.clone()), None, cx));
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.rows.len(), 2);
            assert_eq!(view.rows[0].available, Some(true));
            assert_eq!(view.rows[1].available, Some(false));
        });
        let default = visual.debug_bounds("vaults-default-0").unwrap();
        visual.simulate_click(default.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            ReadingHistory::default_vault(&state).unwrap(),
            Some(vault.clone())
        );
        view.read_with(visual, |view, _| assert!(view.rows[0].vault.is_default));
        let remove = visual.debug_bounds("vaults-remove-0").unwrap();
        visual.simulate_click(remove.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(ReadingHistory::default_vault(&state).unwrap(), None);
        assert_eq!(
            ReadingHistory::known_vault_roots(&state).unwrap(),
            [absent.clone()]
        );
        view.read_with(visual, |view, _| {
            assert_eq!(view.rows.len(), 1);
            assert_eq!(view.rows[0].vault.root, absent);
            assert_eq!(view.rows[0].available, Some(false));
        });
        assert_eq!(std::fs::read(vault.join("note.md")).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&vault).unwrap().count(), 1);
        assert!(!absent.exists());
    }
}
