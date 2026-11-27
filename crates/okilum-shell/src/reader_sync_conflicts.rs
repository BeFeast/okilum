//! App-level `Reader` glue and Reader-integration tests for
//! `okilum_shell_sync::reader_sync_conflicts` (#3240, epic #3241).
//! Standalone conflict panel, inventory view, and unit tests live in
//! `crates/okilum-shell-sync`.

use gpui::{App, WeakEntity, Window};
use std::path::{Path, PathBuf};

pub(crate) use okilum_shell_sync::reader_sync_conflicts::{
    append_history, show_startup_notice, InventoryView, StartupNotice,
};

fn owner_dirty_fn(
    owner: Option<WeakEntity<crate::Reader>>,
) -> Option<Box<dyn Fn(&Path, &App) -> bool>> {
    owner.map(|owner| {
        Box::new(move |root: &Path, cx: &App| {
            owner.upgrade().is_some_and(|reader| {
                let reader = reader.read(cx);
                reader.vault_root == root && reader.source_is_dirty(cx)
            })
        }) as Box<dyn Fn(&Path, &App) -> bool>
    })
}

pub(crate) fn open(
    root: PathBuf,
    state: PathBuf,
    owner: Option<WeakEntity<crate::Reader>>,
    preferred: Option<PathBuf>,
    window: &mut Window,
    cx: &mut App,
) {
    okilum_shell_sync::reader_sync_conflicts::open(
        root,
        state,
        owner_dirty_fn(owner),
        preferred,
        window,
        cx,
    );
}

pub(crate) fn recover_before_open(opts: &crate::Opts) -> Vec<String> {
    okilum_shell_sync::reader_sync_conflicts::recover_before_open(opts.session_directory.clone())
}

impl crate::Reader {
    pub(crate) fn render_sync_conflict_banner(
        &self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui::*;
        use gpui_component::{button::Button, h_flex, Sizable};
        let (root, rel, copies, complete) = self.sync_note_conflicts.as_ref()?;
        if *root != self.vault_root
            || *rel != self.pane.current_rel
            || (copies.is_empty() && *complete)
        {
            return None;
        }
        let label = if copies.is_empty() {
            "Conflict check unavailable. Review Sync conflicts.".into()
        } else {
            format!("This note has {} conflicting version(s).", copies.len())
        };
        let root = root.clone();
        let state = self.session_directory.clone()?;
        let owner = cx.entity().downgrade();
        let preferred = copies.first().cloned();
        Some(
            h_flex()
                .id("sync-conflict-banner")
                .debug_selector(|| "sync-conflict-banner".into())
                .px_4()
                .py_1()
                .gap_2()
                .text_sm()
                .child(label)
                .child(
                    Button::new("sync-conflict-review")
                        .debug_selector(|| "sync-conflict-review".into())
                        .small()
                        .label("Review versions")
                        .on_click(move |_, window, cx| {
                            open(
                                root.clone(),
                                state.clone(),
                                Some(owner.clone()),
                                preferred.clone(),
                                window,
                                cx,
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod view {
    mod tests {
        use super::super::*;
        use crate::Reader;
        use ::core::prelude::v1::test;
        use gpui::{AppContext, EntityInputHandler as _, TestAppContext};
        use gpui_component::Root;
        use okilum_shell_sync::reader_sync_conflicts::State;
        use okilum_sync_controller::conflicts::Choice;

        #[gpui::test]
        fn issue2044_symlink_inventory_blocks_a_dirty_canonical_reader(cx: &mut TestAppContext) {
            cx.update(crate::test_init);
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("vault");
            let alias = directory.path().join("vault-alias");
            let state = directory.path().join("state");
            std::fs::create_dir(&root).unwrap();
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            let copy = PathBuf::from("note.sync-conflict-20261104-123456-ABC2345.md");
            std::fs::write(root.join("note.md"), b"this").unwrap();
            std::fs::write(root.join(&copy), b"that").unwrap();
            let mut reader = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        crate::Opts {
                            vault: Some(root.clone()),
                            note: Some("note.md".into()),
                            session_directory: Some(state.clone()),
                            index_dir: Some(directory.path().join("index")),
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
            let inventory = visual.update(|_, cx| {
                cx.new(|cx| {
                    InventoryView::new(
                        alias.clone(),
                        state.clone(),
                        owner_dirty_fn(Some(reader.downgrade())),
                        cx,
                    )
                })
            });
            visual.run_until_parked();
            inventory.read_with(visual, |view, _| {
                assert_eq!(view.test_root(), root.canonicalize().unwrap());
                assert_eq!(view.test_copies(), std::slice::from_ref(&copy));
            });
            inventory.update(visual, |view, cx| view.test_preview(copy.clone(), cx));
            visual.run_until_parked();
            reader.update_in(visual, |reader, window, cx| {
                reader.toggle_source(window, cx);
                reader.source_input().unwrap().update(cx, |input, cx| {
                    input.replace_text_in_range(Some(0..0), "unsaved edit", window, cx);
                });
                assert!(reader.source_is_dirty(cx), "positive dirty control");
            });
            inventory.update(visual, |view, cx| {
                assert!(
                    view.test_dirty(cx),
                    "symlink inventory must see the canonical editor"
                );
                view.test_apply(Choice::KeepThat, cx);
                let panel = view.test_panel().unwrap();
                assert!(matches!(panel.state, State::Ready));
                assert!(panel.message.contains("unsaved draft"));
            });
            assert_eq!(std::fs::read(root.join("note.md")).unwrap(), b"this");
            assert_eq!(std::fs::read(root.join(copy)).unwrap(), b"that");
        }
    }
}
