//! Historical previews are separate from the live document and editor buffer.
use super::*;
use gpui_component::{
    input::{Editor, EditorState},
    notification::Notification,
};
use tessera_core::source_history::{self, Version};

pub(super) struct Timeline {
    pub(super) id: uuid::Uuid,
    root: PathBuf,
    rel: String,
    versions: Vec<Version>,
    pub(super) selected: Option<usize>,
    current: Option<String>,
    content: Option<Entity<TextViewState>>,
    source: Option<Entity<EditorState>>,
    states: prepared_links::States,
    source_mode: bool,
    changes: bool,
    loading: bool,
    pub(super) message: Option<String>,
    pub(super) selection: uuid::Uuid,
    _subscription: Option<Subscription>,
}

fn date(created: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp((created / 1_000_000) as i64)
        .map(|d| format!("{} {:02}:{:02} UTC", d.date(), d.hour(), d.minute()))
        .unwrap_or_else(|_| "Unknown date".into())
}

fn age(created: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = now.saturating_sub(created / 1_000_000);
    match seconds {
        0..60 => "Just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86400),
    }
}

fn delta(old: &str, current: &str) -> String {
    format!(
        "{:+} B · {:+} L",
        old.len() as i64 - current.len() as i64,
        old.lines().count() as i64 - current.lines().count() as i64
    )
}

// A bounded, exact comparison: retain the common prefix/suffix and show the
// replaced span. No quadratic diff matrix for large source files.
fn changes(old: &str, current: &str) -> String {
    let identical_bytes = old == current;
    let old: Vec<_> = old.lines().collect();
    let current: Vec<_> = current.lines().collect();
    let prefix = old.iter().zip(&current).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(current[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = String::from("--- Current file\n+++ Selected version\n");
    if old == current {
        out.push_str(if identical_bytes {
            "No changes.\n"
        } else {
            "Only line endings or final newline differ. Source shows the exact retained text.\n"
        });
        return out;
    }
    out.push_str(&format!("@@ From line {} @@\n", prefix + 1));
    for line in &current[prefix..current.len() - suffix] {
        out.push_str(&format!("- {line}\n"));
    }
    for line in &old[prefix..old.len() - suffix] {
        out.push_str(&format!("+ {line}\n"));
    }
    out
}

impl Reader {
    pub(super) fn active_timeline(&self) -> Option<&Timeline> {
        self.timeline.as_ref().filter(|t| {
            t.root == self.vault_root && t.rel == self.current_rel && !self.current_rel.is_empty()
        })
    }

    pub(super) fn open_timeline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_rel.is_empty() || self.file_preview.is_some() {
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        let id = uuid::Uuid::new_v4();
        self.timeline = Some(Timeline {
            id,
            root: root.clone(),
            rel: rel.clone(),
            versions: Vec::new(),
            selected: None,
            current: None,
            content: None,
            source: None,
            states: Arc::default(),
            source_mode: false,
            changes: false,
            loading: true,
            message: None,
            selection: uuid::Uuid::new_v4(),
            _subscription: None,
        });
        self.panels.open(reader_layout::Panel::Backlinks);
        self.focus_handle.focus(window, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let drafts = state.join("editor-drafts");
                    let mut listing = source_history::list(&drafts, &root)?;
                    let moves = source_history::move_versions(&state, &root)?;
                    listing.versions.extend(moves.versions);
                    listing.warnings.extend(moves.warnings);
                    for cleanup in [source_history::prune(&drafts), source_history::prune_moves(&state,&root)] {
                        if let Err(error) = cleanup { listing.warnings.push(format!("History cleanup could not finish: {error:#}. Recovery is retained.")); }
                    }
                    let path = root.canonicalize()?.join(&rel);
                    let path = path.canonicalize().unwrap_or(path);
                    listing.versions.retain(|v| v.note == path);
                    listing
                        .versions
                        .sort_by_key(|v| std::cmp::Reverse(v.created));
                    let current = std::fs::read_to_string(path).ok();
                    Ok::<_, anyhow::Error>((listing, current))
                })
                .await;
            let _ = this.update_in(cx, |r, _, cx| {
                if r.active_timeline().is_none_or(|t| t.id != id) {
                    return;
                }
                let t = r.timeline.as_mut().unwrap();
                t.loading = false;
                match loaded {
                    Ok((listing, current)) => {
                        t.versions = listing.versions;
                        t.current = current;
                        if !listing.warnings.is_empty() {
                            t.message = Some(listing.warnings.join("\n"));
                        }
                    }
                    Err(e) => t.message = Some(format!("Cannot open history: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn back_from_timeline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.timeline.as_mut() {
            t.selected = None;
            t.message = None;
            t.content = None;
            t.source = None;
            t.loading = false;
            t.selection = uuid::Uuid::new_v4();
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn select_timeline_version(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.active_timeline() else {
            return;
        };
        let Some(version) = t.versions.get(index).cloned() else {
            return;
        };
        let id = t.id;
        let rel = t.rel.clone();
        let vault = self.vault.clone();
        let selection = uuid::Uuid::new_v4();
        let t = self.timeline.as_mut().unwrap();
        t.selected = Some(index);
        t.selection = selection;
        t.loading = true;
        t.content = None;
        t.source = None;
        t.message = None;
        t.changes = false;
        self.clear_hover(cx);
        self.focus_handle.focus(window, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let current = std::fs::read_to_string(&version.note).ok();
                    let document = tessera_core::render::reader_document_from_source(
                        &vault,
                        &rel,
                        &version.text,
                    );
                    let mut links = tessera_core::document_links::prepared::LinkPreparation::new(
                        &vault,
                        &rel,
                        |path| {
                            let raw = std::fs::read_to_string(vault.root.join(path))
                                .map_err(|e| e.to_string())?;
                            let rendered =
                                tessera_core::render::reader_heading_source(&vault, path, &raw);
                            use sha2::{Digest, Sha256};
                            Ok(tessera_core::document_links::prepared::TargetSnapshot {
                                revision: format!("sha256:{:x}", Sha256::digest(raw.as_bytes())),
                                headings: tessera_core::document_links::HeadingInventory::new(
                                    &rendered,
                                ),
                                supports_setext: true,
                                managed: None,
                            })
                        },
                    );
                    let states = Arc::new(links.identities(&document.links));
                    Ok::<_, anyhow::Error>((version, current, document, states))
                })
                .await;
            let _ = this.update_in(cx, |r, window, cx| {
                if r.active_timeline()
                    .is_none_or(|t| t.id != id || t.selection != selection)
                {
                    return;
                }
                match loaded {
                    Err(e) => {
                        let t = r.timeline.as_mut().unwrap();
                        t.loading = false;
                        t.message = Some(format!("Cannot preview version: {e:#}"));
                    }
                    Ok((version, current, document, states)) => {
                        let configured = reader_plugins(
                            r.vault_root.clone(),
                            TextView::markdown("history-config", ""),
                            cx.entity().downgrade(),
                            r.sel_format,
                            states.clone(),
                            &r.link_identities,
                        );
                        let content = cx.new(|cx| {
                            let mut text = TextViewState::markdown("", cx)
                                .scrollable(true)
                                .selectable(true);
                            configured.prepare_state(&mut text, cx);
                            text.set_text_with_source(
                                &document.rendered,
                                Some(document.original_body.into()),
                                cx,
                            );
                            text
                        });
                        let source = cx.new(|cx| {
                            let mut source = EditorState::new(window, cx)
                                .language("markdown")
                                .soft_wrap(true);
                            source.set_readonly(true, cx);
                            source.set_value(version.text, window, cx);
                            source
                        });
                        let subscription = cx.observe(&content, |_, _, cx| cx.notify());
                        let t = r.timeline.as_mut().unwrap();
                        t.loading = false;
                        if current.is_none() { t.message = Some("Current file is unavailable. Save as a recovered note to keep this version.".into()); }
                        t.current = current;
                        t.content = Some(content);
                        t.source = Some(source);
                        t.states = states;
                        t._subscription = Some(subscription);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn step_timeline(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.active_timeline() else {
            return;
        };
        if t.versions.is_empty() {
            return;
        }
        let index = t
            .selected
            .map(|i| {
                if forward {
                    (i + 1).min(t.versions.len() - 1)
                } else {
                    i.saturating_sub(1)
                }
            })
            .unwrap_or(0);
        self.select_timeline_version(index, window, cx);
    }

    pub(super) fn toggle_timeline_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.timeline.as_mut() {
            t.source_mode = !t.source_mode;
            if t.changes {
                if let (Some(source), Some(index)) = (&t.source, t.selected) {
                    source.update(cx, |s, cx| {
                        s.set_value(t.versions[index].text.clone(), window, cx)
                    });
                }
            }
            t.changes = false;
        }
        cx.notify();
    }

    fn show_timeline_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.active_timeline() else {
            return;
        };
        let Some(index) = t.selected else {
            return;
        };
        if t.loading || t.source.is_none() || t.current.is_none() {
            return;
        }
        let enabled = !t.changes;
        let text = if enabled {
            changes(&t.versions[index].text, t.current.as_deref().unwrap_or(""))
        } else {
            t.versions[index].text.clone()
        };
        if let Some(source) = &t.source {
            source.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        let t = self.timeline.as_mut().unwrap();
        t.changes = enabled;
        t.source_mode = enabled;
        cx.notify();
    }

    fn restore_timeline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.active_timeline() else {
            return;
        };
        if t.loading {
            return;
        }
        let Some(index) = t.selected else {
            return;
        };
        let version = t.versions[index].clone();
        let Some(reviewed) = t.current.clone() else {
            return;
        };
        if version.link_move {
            return;
        }
        let root = t.root.clone();
        let rel = t.rel.clone();
        let result = if root.join(&rel).canonicalize().ok().as_ref() == Some(&version.note) {
            self.restore_source_version(&reviewed, &version.text, window, cx)
        } else {
            Err(anyhow::anyhow!("The note path changed; open history again"))
        };
        match result {
            Err(e) => {
                if let Some(t) = self.timeline.as_mut() {
                    t.message = Some(format!("Cannot restore: {e:#}"));
                }
                cx.notify();
            }
            Ok(()) => {
                self.timeline = None;
                let reader = cx.weak_entity();
                reader_toast::push(Notification::success("Version restored")
                    .action(move |_,_,cx| {
                        let reader = reader.clone(); let root = root.clone(); let rel = rel.clone();
                        let restored = version.text.clone(); let replaced = reviewed.clone(); let note = version.note.clone();
                        Button::new("history-undo").small().label("Undo")
                            .debug_selector(|| "history-undo".into())
                            .on_click(cx.listener(move |notice,_,window,cx| {
                                let result = reader.update(cx, |r,cx| {
                                    anyhow::ensure!(r.vault_root == root && r.current_rel == rel,
                                        "Open the restored note before undoing");
                                    anyhow::ensure!(root.join(&rel).canonicalize().ok().as_ref() == Some(&note), "The note path changed; open history again");
                                    r.restore_source_version(&restored, &replaced, window, cx)
                                });
                                match result.and_then(|result| result) {
                                    Ok(()) => { notice.dismiss(window,cx); reader_toast::transient("Restore undone",window,cx); }
                                    Err(error) => {
                                        let _ = reader.update(cx, |r,cx| {
                                            r.link_notice = Some(format!("Cannot undo restore: {error:#}. The previous text remains in Note history.").into());
                                            cx.notify();
                                        });
                                    }
                                }
                            }))
                    }), Some(Duration::from_secs(8)), window, cx);

                cx.notify();
            }
        }
    }

    pub(super) fn render_timeline(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.active_timeline().unwrap();
        let faint = brand::reader_palette(cx).text_faint;
        v_flex()
            .id("note-history-timeline")
            .key_context("ReaderHistory")
            .h_full()
            .min_h_0()
            .gap_2()
            .p_3()
            .child(
                reader_icon_button("history-close", IconName::ArrowLeft, "On this page", cx)
                    .on_click(cx.listener(|r, _, window, cx| {
                        r.timeline = None;
                        r.focus_handle.focus(window, cx);
                        cx.notify();
                    })),
            )
            .when(t.loading && t.selected.is_none(), |d| {
                d.child("Loading history…")
            })
            .when(!t.loading && t.versions.is_empty(), |d| {
                d.child("No retained versions.")
            })
            .when_some(t.message.clone(), |d, m| d.child(div().text_sm().child(m)))
            .child(
                v_flex()
                    .id("history-version-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(t.versions.iter().enumerate().map(|(index, v)| {
                        let details = format!(
                            "{} · {}{}",
                            date(v.created),
                            v.label,
                            if v.protected { " · Protected" } else { "" }
                        );
                        h_flex()
                            .id(("history-version", index))
                            .debug_selector(move || format!("history-version-{index}"))
                            .min_w_0()
                            .min_h(px(36.))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .rounded_md()
                            .bg(if t.selected == Some(index) {
                                cx.theme().accent
                            } else {
                                cx.theme().background
                            })
                            .hover(|d| d.bg(cx.theme().accent))
                            .tooltip(move |w, cx| {
                                gpui_component::tooltip::Tooltip::new(details.clone()).build(w, cx)
                            })
                            .on_click(cx.listener(move |r, _, window, cx| {
                                r.select_timeline_version(index, window, cx)
                            }))
                            .child(div().flex_1().min_w_0().text_sm().child(age(v.created)))
                            .when(v.protected, |d| {
                                d.child(Icon::new(IconName::Star).size(px(12.)).text_color(faint))
                            })
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .rounded(px(4.))
                                    .px_1()
                                    .py(px(2.))
                                    .text_xs()
                                    .text_color(faint)
                                    .bg(cx.theme().muted)
                                    .child(
                                        t.current
                                            .as_deref()
                                            .map(|current| delta(&v.text, current))
                                            .unwrap_or_else(|| {
                                                format!(
                                                    "{} B · {} L",
                                                    v.text.len(),
                                                    v.text.lines().count()
                                                )
                                            }),
                                    ),
                            )
                    })),
            )
            .into_any_element()
    }

    pub(super) fn render_timeline_preview(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = self.active_timeline()?;
        let version = t.versions.get(t.selected?)?;
        let mut style = reader_text_style(cx.theme());
        style.bottom_padding = reader_toast::bottom_space(window, cx);
        let timeline_id = t.id;
        let selected = t.selected.unwrap();
        Some(
            v_flex()
                .id("history-preview")
                .relative()
                .debug_selector(|| "history-preview".into())
                .key_context("ReaderHistory")
                .size_full()
                .min_w_0()
                .min_h_0()
                .when(t.loading, |d| d.child("Loading version…"))
                .when_some(t.source.clone().filter(|_| t.source_mode), |d, source| {
                    d.child(
                        Editor::new(&source)
                            .readonly(true)
                            .w_full()
                            .flex_1()
                            .min_h_0(),
                    )
                })
                .when_some(
                    t.content.clone().filter(|_| !t.source_mode),
                    |d, content| {
                        d.child(
                            h_flex().w_full().flex_1().min_h_0().justify_center().child(
                                reader_plugins(
                                    self.vault_root.clone(),
                                    TextView::new(&content)
                                        .scrollable(true)
                                        .selectable(true)
                                        .style(style)
                                        .text_size(px(BODY_FONT_SIZE))
                                        .px(px(READER_SIDE_PADDING))
                                        .py_4()
                                        .max_w(px(READER_MAX_WIDTH))
                                        .w_full()
                                        .flex_1()
                                        .min_h_0(),
                                    cx.weak_entity(),
                                    self.sel_format,
                                    t.states.clone(),
                                    &self.link_identities,
                                ),
                            ),
                        )
                    },
                )
                .child(
                    v_flex()
                        .id("history-actions-overlay")
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .absolute()
                        .bottom_3()
                        .left_3()
                        .max_w(px(440.))
                        .rounded_lg()
                        .shadow_md()
                        .bg(cx.theme().background)
                        .gap_1()
                        .px_4()
                        .py_2()
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(div().min_w_0().text_sm().truncate().child(format!(
                            "{} · {}",
                            self.current_title,
                            date(version.created)
                        )))
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    reader_icon_button(
                                        "history-restore",
                                        IconName::Undo2,
                                        "Restore this version",
                                        cx,
                                    )
                                    .disabled(t.loading || version.link_move || t.current.is_none())
                                    .on_click(cx.listener(|r, _, w, cx| r.restore_timeline(w, cx))),
                                )
                                .child(
                                    reader_icon_button(
                                        "history-changes",
                                        IconName::Replace,
                                        if t.changes {
                                            "Hide changes"
                                        } else {
                                            "Show changes"
                                        },
                                        cx,
                                    )
                                    .selected(t.changes)
                                    .disabled(
                                        t.loading || t.source.is_none() || t.current.is_none(),
                                    )
                                    .on_click(
                                        cx.listener(|r, _, w, cx| r.show_timeline_changes(w, cx)),
                                    ),
                                )
                                .child(
                                    reader_icon_button(
                                        "history-source",
                                        if t.source_mode {
                                            IconName::Eye
                                        } else {
                                            IconName::FileText
                                        },
                                        if t.source_mode { "Preview" } else { "Source" },
                                        cx,
                                    )
                                    .selected(t.source_mode)
                                    .on_click(
                                        cx.listener(|r, _, w, cx| r.toggle_timeline_source(w, cx)),
                                    ),
                                )
                                .child(
                                    reader_icon_button(
                                        "history-back",
                                        IconName::ArrowLeft,
                                        "Back to current (Esc)",
                                        cx,
                                    )
                                    .on_click(
                                        cx.listener(|r, _, w, cx| r.back_from_timeline(w, cx)),
                                    ),
                                )
                                .child(
                                    reader_icon_button(
                                        "history-save-copy",
                                        IconName::Copy,
                                        "Save as recovered note…",
                                        cx,
                                    )
                                    .on_click(cx.listener(
                                        move |r, _, w, cx| {
                                            let selected = r
                                                .active_timeline()
                                                .filter(|t| t.id == timeline_id)
                                                .and_then(|t| {
                                                    t.versions
                                                        .get(selected)
                                                        .map(|v| (v.clone(), t.root.clone()))
                                                });
                                            if let Some((version, root)) = selected {
                                                r.save_source_copy(version, root, w, cx);
                                            }
                                        },
                                    )),
                                )
                                .when(version.link_move, |d| {
                                    d.child(
                                        reader_icon_button(
                                            "history-recover-move",
                                            IconName::Network,
                                            "Recover whole link move…",
                                            cx,
                                        )
                                        .on_click(
                                            cx.listener(|r, _, w, cx| r.recover_link_moves(w, cx)),
                                        ),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::WindowExt;

    #[test]
    fn comparison_handles_insertions_deletions_and_empty_files() {
        assert_eq!(delta("a\nb\n", "a\n"), "+2 B · +1 L");
        assert!(changes("a\nb\nz", "a\nc\nz").contains("- c\n+ b\n"));
        assert!(changes("", "removed").contains("- removed"));
        assert!(changes("added", "").contains("+ added"));
        assert!(changes("same", "same").contains("No changes"));
    }

    #[cfg(unix)]
    #[gpui::test]
    fn timeline_preview_keyboard_restore_and_stale_guard(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-timeline-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let old = "# Previous Привет\r\n\r\nExact bytes e\u{301}\r\n";
        let middle = "# Middle\n";
        let current = "# Current\n";
        std::fs::write(root.join("note.md"), old).unwrap();
        std::fs::write(root.join("other.md"), "# Other").unwrap();
        {
            let mut editor = tessera_core::file_editor::FileEditor::open(
                &root.join("note.md"),
                &directory.join("state/editor-drafts"),
            )
            .unwrap();
            editor.set_text(middle.into()).unwrap();
            editor.save().unwrap();
            editor.set_text(current.into()).unwrap();
            editor.save().unwrap();
        }
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
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
        reader.update_in(visual, |r, w, cx| {
            assert_eq!(r.current_rel, "note.md");
            r.source_history(false, w, cx);
            assert!(!w.has_active_dialog(cx));
        });
        visual.run_until_parked();
        let index = reader.read_with(visual, |r, _| {
            let t = r.active_timeline().unwrap();
            assert!(!t.loading);
            assert!(t.versions.len() >= 2);
            t.versions.iter().position(|v| v.text == old).unwrap()
        });
        reader.update_in(visual, |r, w, cx| r.select_timeline_version(index, w, cx));
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let t = r.active_timeline().unwrap();
            assert!(!t.loading);
            assert!(t.content.is_some());
            assert_eq!(t.source.as_ref().unwrap().read(cx).value().as_ref(), old);
            assert!(r.note_source.contains("Current"));
            assert!(r.editing.is_none());
        });
        reader.update_in(visual, |r, _, cx| {
            r.document_header_hidden = px(21.);
            cx.notify();
        });
        visual.update(|w, cx| w.draw(cx).clear(cx));
        assert!(visual.debug_bounds("source-save").is_none());
        let preview = visual.debug_bounds("history-preview").unwrap();
        let live_offset = reader.read_with(visual, |r, cx| {
            r.content.read(cx).list_state().logical_scroll_top()
        });
        visual.simulate_event(ScrollWheelEvent {
            position: point(preview.center().x, preview.top() + px(160.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-120.))),
            ..Default::default()
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.document_header_hidden, px(21.));
            let offset = r.content.read(cx).list_state().logical_scroll_top();
            assert_eq!(
                (offset.item_ix, offset.offset_in_item),
                (live_offset.item_ix, live_offset.offset_in_item)
            );
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(
                r.active_timeline().unwrap().selected,
                Some(index.saturating_sub(1))
            )
        });
        visual.simulate_keystrokes("escape");
        reader.read_with(visual, |r, _| {
            assert!(r.active_timeline().unwrap().selected.is_none())
        });
        reader.update_in(visual, |r, w, cx| r.select_timeline_version(index, w, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, w, cx| {
            r.show_timeline_changes(w, cx);
            assert!(r.active_timeline().unwrap().changes);
            r.toggle_timeline_source(w, cx);
            r.toggle_timeline_source(w, cx);
            assert_eq!(
                r.active_timeline()
                    .unwrap()
                    .source
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                old
            );
            std::fs::write(root.join("note.md"), "external").unwrap();
            r.restore_timeline(w, cx);
            assert!(r
                .active_timeline()
                .unwrap()
                .message
                .as_ref()
                .unwrap()
                .contains("Cannot restore"));
            assert_eq!(
                std::fs::read_to_string(root.join("note.md")).unwrap(),
                "external"
            );
        });
        visual.run_until_parked();
        visual.update(|w, cx| assert_eq!(w.notifications(cx).len(), 1));
        visual.update(|w, cx| {
            reader_toast::dismiss(w, cx);
        });
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.active_timeline().unwrap().message.is_none())
        });
        // Retrying the identical failure must show it again after dismissal.
        reader.update_in(visual, |r, w, cx| r.restore_timeline(w, cx));
        visual.run_until_parked();
        visual.update(|w, cx| assert_eq!(w.notifications(cx).len(), 1));
        reader.update_in(visual, |r, w, cx| {
            std::fs::write(root.join("note.md"), current).unwrap();
            r.restore_timeline(w, cx);
            assert!(r.timeline.is_none());
            assert!(!w.has_active_dialog(cx));
        });
        visual.run_until_parked();
        assert_eq!(std::fs::read(root.join("note.md")).unwrap(), old.as_bytes());
        assert!(
            source_history::list(&directory.join("state/editor-drafts"), &root)
                .unwrap()
                .versions
                .iter()
                .any(|v| v.text == current)
        );
        // Exercise the actual eight-second Undo action, not a direct restore call.
        visual.update(|w, cx| w.draw(cx).clear(cx));
        let undo = visual
            .debug_bounds("history-undo")
            .expect("Restore must offer Undo");
        std::fs::write(root.join("note.md"), "external after restore").unwrap();
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "external after restore"
        );
        // The actionable error is the front toast. Dismiss it before retrying
        // the still-available Undo against the exact reviewed bytes.
        reader.read_with(visual, |r, _| {
            assert!(r
                .link_notice
                .as_ref()
                .unwrap()
                .contains("Cannot undo restore"))
        });
        visual.simulate_keystrokes("escape");
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.link_notice.is_none()));
        std::fs::write(root.join("note.md"), old).unwrap();
        visual.update(|w, cx| w.draw(cx).clear(cx));
        let undo = visual.debug_bounds("history-undo").unwrap();
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            current
        );
        // A dirty live editor stays intact while historical content is shown.
        reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
        visual.run_until_parked();
        visual.simulate_input("dirty ");
        reader.update_in(visual, |r, w, cx| r.open_timeline(w, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, w, cx| {
            let index = r
                .active_timeline()
                .unwrap()
                .versions
                .iter()
                .position(|v| v.text == old)
                .unwrap();
            r.select_timeline_version(index, w, cx);
        });
        visual.run_until_parked();
        visual.update(|w, cx| w.draw(cx).clear(cx));
        assert!(visual.debug_bounds("source-save").is_none());
        reader.update_in(visual, |r, _, cx| r.request_source_save(cx));
        visual.executor().advance_clock(Duration::from_millis(50));
        visual.run_until_parked();
        reader.update_in(visual, |r, w, cx| {
            assert_eq!(
                std::fs::read_to_string(root.join("note.md")).unwrap(),
                current
            );
            r.restore_timeline(w, cx);
            assert!(r
                .active_timeline()
                .unwrap()
                .message
                .as_ref()
                .unwrap()
                .contains("Cannot restore"));
            r.back_from_timeline(w, cx);
            assert!(r.save_source(cx));
            assert!(std::fs::read_to_string(root.join("note.md"))
                .unwrap()
                .contains("dirty "));
            r.toggle_source(w, cx);
        });
        visual.run_until_parked();
        std::fs::remove_file(root.join("note.md")).unwrap();
        reader.update_in(visual, |r, w, cx| r.open_timeline(w, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, w, cx| {
            let t = r.active_timeline().unwrap();
            assert!(!t.versions.is_empty());
            assert!(t.current.is_none());
            r.select_timeline_version(0, w, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, w, cx| {
            r.show_timeline_changes(w, cx);
            let t = r.active_timeline().unwrap();
            assert!(t.content.is_some());
            assert!(t.current.is_none());
            assert!(!t.changes);
        });
        reader.update_in(visual, |r, w, cx| {
            r.open_timeline(w, cx);
            r.show_empty_vault(w, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.current_rel.is_empty());
            assert!(r.active_timeline().is_none());
        });
        std::fs::remove_dir_all(directory).unwrap();
    }
}
