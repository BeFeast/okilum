//! The offer to keep Okilum's per-machine files out of Syncthing (#1248).
//!
//! A vault whose root has `.stfolder` is a Syncthing folder. When its `.stignore` lacks the
//! Okilum lines (index, safe-save temp files) Okilum offers once per launch, in a toast with one
//! button, to append a block to it. Nothing is written without that click. «Don't ask for this
//! vault» is kept in the app state (never in the vault folder, which Syncthing shares). Settings →
//! Files shows the same state and the same button. The check and the edit run off the UI thread.
use super::*;
use gpui_component::notification::Notification;
use okilum_core::syncthing_ignore::{self, Added, Status};
use std::sync::Mutex;

impl Reader {
    /// Look at the vault's `.stignore` and, when Okilum's lines are missing and the offer was not
    /// declined for this vault or shown in this launch, offer to add them.
    pub(super) fn check_stignore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_stignore(true, window, cx);
    }

    fn refresh_stignore(&mut self, offer: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.single_file || self.vault_root.as_os_str().is_empty() {
            return;
        }
        let root = self.vault_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let checked = root.clone();
            let status = cx
                .background_executor()
                .spawn(async move { syncthing_ignore::status(&checked) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.vault_root != root {
                    return;
                }
                this.stignore = Some((root, status));
                cx.notify();
                if offer {
                    this.offer_stignore(window, cx);
                }
            });
        })
        .detach();
    }

    fn offer_stignore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The decline lives with the sidebar state of this root, which may not be loaded yet.
        self.load_sidebar_state();
        let Some((root, Status::Missing(_))) = self.stignore.clone() else {
            return;
        };
        if self.single_file
            || self.sidebar.stignore_declined
            || !self.stignore_offered.insert(root)
        {
            return;
        }
        let reader = cx.weak_entity();
        // The buttons close the toast they sit in; its key is known only after it is pushed.
        let key: Arc<Mutex<Option<reader_toast::ToastKey>>> = Arc::new(Mutex::new(None));
        let shown = key.clone();
        let pushed = reader_toast::push(
            Notification::new()
                .message(
                    "This vault is a Syncthing folder. Add Okilum's index and temporary files \
                     to .stignore so they are not shared with your other devices.",
                )
                .content(move |_, _, _| {
                    let close = |key: &Arc<Mutex<Option<reader_toast::ToastKey>>>,
                                 window: &mut Window,
                                 cx: &mut App| {
                        if let Some(key) = key.lock().ok().and_then(|key| *key) {
                            reader_toast::remove(key, window, cx);
                        }
                    };
                    h_flex()
                        .gap_2()
                        .child({
                            let (reader, key) = (reader.clone(), shown.clone());
                            Button::new("stignore-add")
                                .debug_selector(|| "stignore-add".into())
                                .small()
                                .label("Add to .stignore")
                                .on_click(move |_, window, cx| {
                                    close(&key, window, cx);
                                    let _ = reader.update(cx, |this, cx| {
                                        this.add_stignore_block(window, cx)
                                    });
                                })
                        })
                        .child({
                            let (reader, key) = (reader.clone(), shown.clone());
                            Button::new("stignore-decline")
                                .debug_selector(|| "stignore-decline".into())
                                .small()
                                .ghost()
                                .label("Don't ask for this vault")
                                .on_click(move |_, window, cx| {
                                    close(&key, window, cx);
                                    let _ = reader.update(cx, |this, cx| this.decline_stignore(cx));
                                })
                        })
                        .into_any_element()
                }),
            Some(Duration::from_secs(20)),
            window,
            cx,
        );
        if let Ok(mut slot) = key.lock() {
            *slot = Some(pushed);
        }
    }

    /// Append the Okilum block to `.stignore` (the click on «Add to .stignore»), off the UI thread.
    pub(super) fn add_stignore_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stignore_busy || self.single_file {
            return;
        }
        self.stignore_busy = true;
        cx.notify();
        let root = self.vault_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let target = root.clone();
            let added = cx
                .background_executor()
                .spawn(async move { syncthing_ignore::add_block(&target) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.stignore_busy = false;
                match added {
                    Ok(Added::Appended(_)) => reader_toast::transient(
                        "Added Okilum's lines to .stignore",
                        window,
                        cx,
                    ),
                    Ok(Added::AlreadyCovered) => {}
                    Err(error) => reader_toast::error(
                        format!("Could not update .stignore: {error:#}"),
                        window,
                        cx,
                    ),
                }
                // Whatever happened, the state shown comes from the file.
                if this.vault_root == root {
                    this.refresh_stignore(false, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// «Don't ask for this vault»: remembered with the vault's other per-root state.
    pub(super) fn decline_stignore(&mut self, cx: &mut Context<Self>) {
        self.load_sidebar_state();
        self.sidebar.stignore_declined = true;
        self.save_sidebar(cx);
        cx.notify();
    }

    /// Ask again at the next launch (Settings → Files).
    fn undo_decline_stignore(&mut self, cx: &mut Context<Self>) {
        self.load_sidebar_state();
        self.sidebar.stignore_declined = false;
        self.save_sidebar(cx);
        cx.notify();
    }
}

/// The «Syncthing» row of Settings → Files: nothing for a vault that is not a Syncthing folder.
pub(super) fn settings_row(reader: &Entity<Reader>, cx: &App) -> Option<AnyElement> {
    let this = reader.read(cx);
    let (root, status) = this.stignore.as_ref()?;
    if *root != this.vault_root || this.single_file {
        return None;
    }
    let p = brand::palette(cx);
    let declined = this.sidebar.stignore_declined;
    let busy = this.stignore_busy;
    let (help, can_add): (String, bool) = match status {
        Status::NotSyncthing => return None,
        Status::Covered => (
            "Syncthing ignores Okilum's index and temporary files in this vault.".into(),
            false,
        ),
        Status::Missing(missing) => (
            format!(
                "This vault is a Syncthing folder and its .stignore does not list {}. \
                 Syncthing would share them with your other devices. Adding appends one block; \
                 the rest of the file is not touched.",
                missing.join(" and ")
            ),
            true,
        ),
        Status::Include => (
            format!(
                "This vault's .stignore includes other files, which Okilum does not read. Add {} \
                 to your ignore file by hand.",
                syncthing_ignore::PATTERNS.join(" and ")
            ),
            false,
        ),
        Status::Unreadable(error) => (format!("Could not check .stignore: {error}"), false),
    };
    let weak = reader.downgrade();
    let again = weak.clone();
    Some(
        v_flex()
            .id("settings-stignore")
            .debug_selector(|| "settings-stignore".into())
            .w_full()
            .gap_1()
            .py_2()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child("Syncthing")
                            .child(
                                div()
                                    .debug_selector(|| "settings-stignore-help".into())
                                    .text_sm()
                                    .text_color(p.text_muted)
                                    .child(help),
                            ),
                    )
                    .when(can_add, |row| {
                        row.child(
                            div().flex_none().child(
                                Button::new("settings-stignore-add")
                                    .debug_selector(|| "settings-stignore-add".into())
                                    .small()
                                    .label("Add to .stignore")
                                    .disabled(busy)
                                    .on_click(move |_, window, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.add_stignore_block(window, cx)
                                        });
                                    }),
                            ),
                        )
                    }),
            )
            .when(can_add && declined, |column| {
                column.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .text_color(p.text_muted)
                                .child("Not offered when this vault opens."),
                        )
                        .child(
                            Button::new("settings-stignore-ask")
                                .debug_selector(|| "settings-stignore-ask".into())
                                .small()
                                .ghost()
                                .label("Offer again")
                                .on_click(move |_, _, cx| {
                                    let _ =
                                        again.update(cx, |this, cx| this.undo_decline_stignore(cx));
                                }),
                        ),
                )
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_core::syncthing_ignore::{BLOCK_COMMENT, MARKER};

    const BEFORE: &[u8] = b".tessera-index\n// kept as it is\n/.claude/worktrees\n";

    /// A vault in a temporary directory; `ignore` is the `.stignore` it starts with.
    fn vault(syncthing: bool, ignore: Option<&[u8]>) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("Note.md"), "# Note\n").unwrap();
        if syncthing {
            std::fs::create_dir(root.join(MARKER)).unwrap();
        }
        if let Some(ignore) = ignore {
            std::fs::write(root.join(".stignore"), ignore).unwrap();
        }
        (temp, root)
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        temp: &tempfile::TempDir,
        root: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.to_owned()),
                        note: Some("Note.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        (reader.unwrap(), visual)
    }

    fn shown(visual: &mut VisualTestContext, id: &'static str) -> bool {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.debug_bounds(id).is_some()
    }

    fn click(visual: &mut VisualTestContext, id: &'static str) {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = visual
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("{id} is shown"));
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
    }

    fn ignore(root: &Path) -> Vec<u8> {
        std::fs::read(root.join(".stignore")).unwrap()
    }

    fn status(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> Option<Status> {
        reader.read_with(visual, |r, _| r.stignore.as_ref().map(|(_, s)| s.clone()))
    }

    fn setup(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
    }

    #[gpui::test]
    fn a_syncthing_vault_without_the_block_is_offered_and_nothing_is_written_before_the_click(
        cx: &mut TestAppContext,
    ) {
        setup(cx);
        let (temp, root) = vault(true, Some(BEFORE));
        let (reader, visual) = open(cx, &temp, &root);
        assert!(
            matches!(status(&reader, visual), Some(Status::Missing(_))),
            "positive control: the lines are found missing"
        );
        assert!(shown(visual, "stignore-add"), "the offer is shown");
        assert_eq!(ignore(&root), BEFORE, "an offer writes nothing");
        // Time passes, the offer goes away on its own: still nothing written.
        visual.executor().advance_clock(Duration::from_secs(25));
        visual.run_until_parked();
        assert!(!shown(visual, "stignore-add"), "the offer is not sticky");
        assert_eq!(ignore(&root), BEFORE, "an ignored offer writes nothing");
        // It is not offered again in this launch, even when the vault is checked again.
        reader.update_in(visual, |r, window, cx| r.check_stignore(window, cx));
        visual.run_until_parked();
        assert!(!shown(visual, "stignore-add"), "once per launch and vault");
        assert_eq!(ignore(&root), BEFORE);
    }

    #[gpui::test]
    fn the_button_appends_the_block_and_the_vault_is_not_offered_again(cx: &mut TestAppContext) {
        setup(cx);
        let (temp, root) = vault(true, Some(BEFORE));
        {
            let (reader, visual) = open(cx, &temp, &root);
            assert!(shown(visual, "stignore-add"), "positive control");
            click(visual, "stignore-add");
            let after = ignore(&root);
            assert!(after.starts_with(BEFORE), "every other byte stays");
            let appended = String::from_utf8(after[BEFORE.len()..].to_vec()).unwrap();
            assert_eq!(
                appended,
                format!("\n{BLOCK_COMMENT}\n.okilum-index\n.okilum-save-*\n")
            );
            assert_eq!(status(&reader, visual), Some(Status::Covered));
            assert!(!shown(visual, "stignore-add"), "the offer is closed");
        }
        let written = ignore(&root);
        // A new launch finds the lines and offers nothing.
        let (reader, visual) = open(cx, &temp, &root);
        assert_eq!(status(&reader, visual), Some(Status::Covered));
        assert!(!shown(visual, "stignore-add"));
        assert_eq!(ignore(&root), written);
    }

    #[gpui::test]
    fn not_asking_for_this_vault_is_remembered_and_writes_nothing(cx: &mut TestAppContext) {
        setup(cx);
        let (temp, root) = vault(true, Some(BEFORE));
        {
            let (reader, visual) = open(cx, &temp, &root);
            assert!(shown(visual, "stignore-decline"), "positive control");
            click(visual, "stignore-decline");
            assert!(!shown(visual, "stignore-decline"), "the offer is closed");
            reader.read_with(visual, |r, _| assert!(r.sidebar.stignore_declined));
            assert_eq!(ignore(&root), BEFORE, "declining writes nothing");
            visual.run_until_parked();
        }
        // The decline lives in the app state: a new launch does not offer, and still sees it missing.
        let (reader, visual) = open(cx, &temp, &root);
        assert!(matches!(status(&reader, visual), Some(Status::Missing(_))));
        assert!(!shown(visual, "stignore-add"), "not offered after a decline");
        reader.read_with(visual, |r, _| assert!(r.sidebar.stignore_declined));
        assert_eq!(ignore(&root), BEFORE);
    }

    #[gpui::test]
    fn a_vault_that_is_not_a_syncthing_folder_or_already_covers_the_lines_is_not_offered(
        cx: &mut TestAppContext,
    ) {
        setup(cx);
        // No `.stfolder`: an `.stignore` without the lines is none of Okilum's business.
        let (temp, root) = vault(false, Some(BEFORE));
        {
            let (reader, visual) = open(cx, &temp, &root);
            assert_eq!(status(&reader, visual), Some(Status::NotSyncthing));
            assert!(!shown(visual, "stignore-add"));
            assert_eq!(ignore(&root), BEFORE);
        }
        // A Syncthing folder that has them (the owner's own block).
        let covered = format!("{BLOCK_COMMENT}\n.okilum-index\n.okilum-save-*\n");
        let (temp, root) = vault(true, Some(covered.as_bytes()));
        let (reader, visual) = open(cx, &temp, &root);
        assert_eq!(status(&reader, visual), Some(Status::Covered));
        assert!(!shown(visual, "stignore-add"));
        assert_eq!(ignore(&root), covered.as_bytes());
    }
}
