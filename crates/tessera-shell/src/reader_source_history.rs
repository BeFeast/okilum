//! Explicit recovery previews. Merely opening history never mutates a note.
use super::*;
use gpui_component::{
    input::{Editor, EditorState},
    WindowExt,
};
use tessera_core::source_history::{self, Version};

impl Reader {
    pub(super) fn source_history(
        &mut self,
        drafts_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !drafts_only {
            self.open_timeline(window, cx);
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            self.link_notice = Some("No recovery storage is available.".into());
            cx.notify();
            return;
        };
        let root = self.vault_root.clone();
        let result = (|| -> anyhow::Result<_> {
            let drafts = state.join("editor-drafts");
            let mut listing = if drafts_only {
                let mut listing = source_history::drafts(&drafts, &root)?;
                let history = source_history::list(&drafts, &root)?;
                listing
                    .versions
                    .extend(history.versions.into_iter().filter(|v| !v.note.exists()));
                listing.warnings.extend(history.warnings);
                let legacy = source_history::legacy_preimages(&root)?;
                listing.versions.extend(legacy.versions);
                listing.warnings.extend(legacy.warnings);
                listing
            } else {
                let mut listing = source_history::list(&drafts, &root)?;
                let moves = source_history::move_versions(&state, &root)?;
                listing.versions.extend(moves.versions);
                listing.warnings.extend(moves.warnings);
                let note = root.canonicalize()?.join(&self.current_rel);
                let note = note.canonicalize().unwrap_or(note);
                listing.versions.retain(|version| version.note == note);
                listing
            };
            // Cleanup failures in one store must not block the others.
            for cleanup in [
                source_history::prune(&drafts),
                source_history::prune_moves(&state, &root),
            ] {
                if let Err(error) = cleanup {
                    listing.warnings.push(format!(
                        "History cleanup could not finish: {error:#}. Recovery is retained."
                    ));
                }
            }
            listing
                .versions
                .sort_by_key(|v| std::cmp::Reverse(v.created));
            Ok(listing)
        })();
        let listing = match result {
            Ok(listing) => listing,
            Err(error) => {
                self.link_notice = Some(format!("Cannot open history: {error:#}"));
                cx.notify();
                return;
            }
        };
        let reader = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let reader = reader.clone(); let root = root.clone();
            dialog.title(if drafts_only { "Recover notes" } else { "Note history" }).width(px(760.))
                .child("Completed history: 20 versions per note, 30 days, 128 MiB. Unsaved drafts and interrupted recovery are protected. Restore checks the current file; Save as recovered note never replaces an existing file.")
                .children(listing.warnings.iter().map(|text| div().child(text.clone())))
                .child(if listing.versions.is_empty() { "No retained versions found." } else { "Choose a version to preview:" })
                .child(v_flex().id("source-history-list").gap_2().max_h(px(360.)).overflow_y_scroll()
                    .children(listing.versions.iter().enumerate().map(|(index, version)| {
                        let version = version.clone(); let reader = reader.clone(); let root = root.clone();
                        let date = time::OffsetDateTime::from_unix_timestamp((version.created / 1_000_000) as i64).map(|d| d.to_string()).unwrap_or_default();
                        let name = version.note.strip_prefix(&root).unwrap_or(&version.note).display();
                        Button::new(("history-version", index))
                            .label(format!("{name} · {} · {date}{}", version.label, if version.protected { " · protected" } else { "" }))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let _ = reader.update(cx, |r, cx| r.preview_source_version(version.clone(), root.clone(), drafts_only, window, cx));
                            })
                    })))
        });
    }

    fn preview_source_version(
        &mut self,
        version: Version,
        root: PathBuf,
        copy_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reviewed = std::fs::read_to_string(&version.note).ok();
        let input = cx.new(|cx| {
            let mut input = EditorState::new(window, cx)
                .language("markdown")
                .soft_wrap(true);
            input.set_readonly(true, cx);
            input.set_value(version.text.clone(), window, cx);
            input
        });
        let reader = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let move_reader = reader.clone();
            let save_reader = reader.clone();
            let restore_reader = reader.clone();
            let save_version = version.clone();
            let restore_version = version.clone();
            let save_root = root.clone();
            let restore_root = root.clone();
            let reviewed = reviewed.clone();
            dialog
                .title("Preview recovered source")
                .width(px(760.))
                .child(version.label.clone())
                .child(
                    Editor::new(&input)
                        .readonly(true)
                        .font_family("Cascadia Code")
                        .h(px(320.)),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .when(
                            !copy_only && !version.link_move && reviewed.is_some(),
                            |row| {
                                row.child(
                                    Button::new("restore-source-version")
                                        .label("Restore this version…")
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = restore_reader.update(cx, |r, cx| {
                                                r.confirm_source_restore(
                                                    restore_version.clone(),
                                                    restore_root.clone(),
                                                    reviewed.clone().unwrap(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }),
                                )
                            },
                        )
                        .when(version.link_move && !copy_only, |row| {
                            row.child(
                                Button::new("recover-history-move")
                                    .label("Recover whole link move…")
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let _ = move_reader
                                            .update(cx, |r, cx| r.recover_link_moves(window, cx));
                                    }),
                            )
                        })
                        .child(
                            Button::new("recover-source-copy")
                                .primary()
                                .label("Save as recovered note…")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = save_reader.update(cx, |r, cx| {
                                        r.save_source_copy(
                                            save_version.clone(),
                                            save_root.clone(),
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        ),
                )
        });
    }

    fn confirm_source_restore(
        &mut self,
        version: Version,
        root: PathBuf,
        reviewed: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(PromptLevel::Warning, "Restore this version?",
            Some("The current source will become a history version. Unsaved edits or a file changed since preview prevent restoration."), &["Cancel", "Restore"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(1) {
                return;
            }
            let _ = this.update_in(cx, |r, window, cx| {
                let result = if r.vault_root == root
                    && r.vault_root
                        .join(&r.current_rel)
                        .canonicalize()
                        .ok()
                        .as_ref()
                        == Some(&version.note)
                {
                    r.restore_source_version(&reviewed, &version.text, window, cx)
                } else {
                    Err(anyhow::anyhow!("The open note changed; open history again"))
                };
                match result {
                    Ok(()) => reader_toast::transient(
                        "Previous version restored. The replaced source is in Note history.",
                        window,
                        cx,
                    ),
                    Err(error) => r.link_notice = Some(format!("Cannot restore: {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn save_source_copy(
        &mut self,
        version: Version,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picker = cx.prompt_for_new_path(&root, Some("Recovered note.md"));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut path))) = picker.await else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("md");
            }
            let _ = this.update_in(cx, |r, window, cx| {
                let result = (|| -> anyhow::Result<_> {
                    anyhow::ensure!(
                        r.vault_root == root,
                        "The open folder changed; choose recovery again"
                    );
                    let root = root.canonicalize()?;
                    let path = path
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("Missing destination folder"))?
                        .canonicalize()?
                        .join(
                            path.file_name()
                                .ok_or_else(|| anyhow::anyhow!("Missing filename"))?,
                        );
                    let relative = path
                        .strip_prefix(&root)
                        .map_err(|_| anyhow::anyhow!("Choose a location inside the open folder"))?;
                    let state = r
                        .session_directory
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("No recovery storage"))?;
                    source_history::save_copy(
                        &root,
                        relative,
                        &state.join("editor-drafts"),
                        &version.text,
                    )?;
                    Ok(relative.display().to_string())
                })();
                match result {
                    Ok(path) => reader_toast::transient(
                        format!(
                            "Recovered source saved as {path}. The original recovery is retained."
                        ),
                        window,
                        cx,
                    ),
                    Err(error) => r.link_notice = Some(format!("Cannot recover a copy: {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
