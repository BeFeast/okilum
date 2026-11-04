//! A frozen MRU gesture: highlight is not navigation; Control release commits once.
use super::*;

pub(super) mod gallery;
mod thumb_code;
mod thumb_markdown;

#[derive(Default)]
pub(super) struct Switcher {
    paths: Vec<String>,
    selected: usize,
    root: PathBuf,
    scroll: ScrollHandle,
    release_generation: u64,
    gallery_open: bool,
    gallery: Option<GallerySession>,
}
impl Switcher {
    fn begin(
        &mut self,
        current: &str,
        recent: &[String],
        available: &[okilum_core::vault::Note],
        root: &Path,
    ) {
        self.cancel();
        let mut paths = vec![];
        for path in std::iter::once(current).chain(recent.iter().rev().map(String::as_str)) {
            if path.to_lowercase().ends_with(".md")
                && !okilum_core::excalidraw::is_drawing(path)
                && available.iter().any(|note| note.path == path)
                && !paths.iter().any(|existing| existing == path)
            {
                paths.push(path.to_string());
            }
        }
        self.paths = paths;
        self.selected = 0;
        self.root = root.to_path_buf();
    }
    pub fn open(&self) -> bool {
        self.gallery_open || self.paths.len() > 1
    }
    #[cfg(test)]
    pub(super) fn gallery_selection(&self, cx: &App) -> Option<String> {
        self.gallery
            .as_ref()?
            .view
            .read(cx)
            .selected_path()
            .map(str::to_owned)
    }

    #[cfg(test)]
    pub(super) fn gallery_paths(&self) -> Vec<&str> {
        self.gallery.as_ref().map_or_else(Vec::new, |session| {
            session
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect()
        })
    }

    fn step(&mut self, delta: isize) {
        self.invalidate_release();
        if self.paths.len() > 1 && !self.gallery_open {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.paths.len() as isize) as usize;
            self.scroll.scroll_to_item(self.selected);
        }
    }
    fn take(&mut self) -> Option<(PathBuf, String)> {
        let result = (self.paths.len() > 1 && !self.gallery_open)
            .then(|| (self.root.clone(), self.paths[self.selected].clone()));
        self.paths.clear();
        result
    }
    pub fn cancel(&mut self) {
        self.invalidate_release();
        self.paths.clear();
        self.gallery_open = false;
    }
    pub fn invalidate_release(&mut self) {
        self.release_generation = self.release_generation.wrapping_add(1);
    }
}

/// Window-owned UI/cache only; inventory and files remain owned by Reader.
struct GallerySession {
    root: PathBuf,
    view: Entity<gallery::Gallery>,
    input: Entity<InputState>,
    inventory: Option<Arc<Vec<okilum_core::vault::Note>>>,
    entries: Vec<reader_title_nav::Entry>,
    appearance: String,
    focused: bool,
    navigation: u64,
    _subscriptions: Vec<Subscription>,
}

mod lifecycle {
    use std::sync::Arc;
    pub(super) fn same_inventory<T>(left: Option<&Arc<T>>, right: Option<&Arc<T>>) -> bool {
        match (left, right) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }
    }
    pub(super) fn columns(viewport: f32) -> usize {
        (((viewport.min(800.) - 32. + 12.) / 252.).floor() as usize).max(1)
    }
    pub(super) fn step(key: &str, columns: usize) -> Option<isize> {
        match key {
            "left" => Some(-1),
            "right" => Some(1),
            "up" => Some(-(columns.max(1) as isize)),
            "down" => Some(columns.max(1) as isize),
            _ => None,
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn accepted_inventory_identity_includes_same_path_source_updates() {
            let first = Arc::new(vec!["a.md"]);
            let retained = first.clone();
            let accepted = Arc::new(vec!["a.md"]);
            assert!(same_inventory(Some(&first), Some(&retained)));
            assert!(!same_inventory(Some(&first), Some(&accepted)));
            assert!(!same_inventory(Some(&first), None));
            assert!(same_inventory::<Vec<&str>>(None, None));
        }
        #[test]
        fn arrows_follow_actual_gallery_columns_without_capturing_text() {
            assert_eq!((columns(1600.), columns(600.), columns(300.)), (3, 2, 1));
            assert_eq!(step("down", columns(600.)), Some(2));
            assert_eq!(step("up", columns(300.)), Some(-1));
            assert_eq!(step("left", 3), Some(-1));
            assert_eq!(step("a", 3), None);
        }
    }
}

impl Reader {
    /// Parent's existing Recent button/menu route calls this instead of the list
    /// popover. No main field or new inventory is needed; render_recent owns it.
    pub(super) fn toggle_recent_gallery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recent_switcher.gallery_open {
            self.close_recent_gallery(window, cx);
            return;
        }
        self.close_quick_open(window, cx);
        self.close_nav_menu(window, cx);
        self.close_project_switcher(window, cx);
        self.recent_switcher.cancel();
        if self
            .recent_switcher
            .gallery
            .as_ref()
            .is_some_and(|session| session.root != self.vault_root)
        {
            self.recent_switcher.gallery = None;
        }
        if self.recent_switcher.gallery.is_none() {
            let entries = self.recent_gallery_entries();
            let root = self.vault_root.clone();
            let view = cx.new(|cx| gallery::Gallery::new(root.clone(), entries.clone(), cx));
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Search recent files…"));
            let input_events = cx.subscribe_in(&input, window, |this, input, event, _, cx| {
                let Some(view) = this
                    .recent_switcher
                    .gallery
                    .as_ref()
                    .map(|session| session.view.clone())
                else {
                    return;
                };
                if !this.recent_switcher.gallery_open {
                    return;
                }
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    view.update(cx, |view, cx| view.set_query(query, cx));
                } else if matches!(event, InputEvent::PressEnter { .. }) {
                    view.update(cx, |view, cx| view.accept(cx));
                }
            });
            let opened = cx.subscribe_in(
                &view,
                window,
                |this, _, target: &gallery::OpenFile, window, cx| {
                    if !this.recent_switcher.gallery_open || target.root != this.vault_root {
                        return;
                    }
                    // Revalidate against the accepted current scope, not a stale card index.
                    if !this
                        .recent_gallery_entries()
                        .iter()
                        .any(|entry| entry.path == target.relative)
                    {
                        this.link_notice =
                            Some("This recent file is no longer in the current inventory.".into());
                        this.refresh_recent_gallery(cx);
                        cx.notify();
                        return;
                    }
                    this.close_recent_gallery(window, cx);
                    if this.selected_file() != target.relative {
                        this.open_note(&target.relative, None, window, cx);
                    }
                },
            );
            let observed = cx.entity();
            let lifecycle = cx.observe_in(&observed, window, |this, _, window, cx| {
                if !this.recent_switcher.gallery_open {
                    this.finish_recent_gallery_close(window, cx);
                } else {
                    this.refresh_recent_gallery(cx);
                }
            });
            let appearance = cx.observe_global_in::<Theme>(window, |this, _, cx| {
                let appearance = this.recent_gallery_appearance(cx);
                if let Some(session) = &mut this.recent_switcher.gallery {
                    session.appearance = appearance;
                    session
                        .view
                        .update(cx, |view, cx| view.appearance_changed(cx));
                }
                cx.notify();
            });
            self.recent_switcher.gallery = Some(GallerySession {
                root,
                view,
                input,
                entries,
                inventory: self.quick_open.inventory.clone(),
                appearance: self.recent_gallery_appearance(cx),
                focused: false,
                navigation: self.navigation.generation,
                _subscriptions: vec![input_events, opened, lifecycle, appearance],
            });
        }
        self.recent_switcher.gallery_open = true;
        if let Some(session) = &mut self.recent_switcher.gallery {
            session.navigation = self.navigation.generation;
        }
        self.refresh_recent_gallery(cx);
        if let Some(session) = &mut self.recent_switcher.gallery {
            session.focused = true;
            let view = session.view.clone();
            view.update(cx, |view, cx| view.set_query(String::new(), cx));
            // Reopening validates every retained source revision in the same worker.
            if session.inventory.is_some() {
                view.update(cx, |view, cx| {
                    view.refresh(session.entries.clone(), &[], cx)
                });
            }
            session.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
        }
        cx.notify();
    }

    fn recent_gallery_entries(&self) -> Vec<reader_title_nav::Entry> {
        let Some(inventory) = &self.quick_open.inventory else {
            return Vec::new();
        };
        self.nav_entries(reader_title_nav::Menu::Recent, reader_sidebar::now())
            .into_iter()
            .filter(|entry| !entry.folder && inventory.iter().any(|file| file.path == entry.path))
            .take(40)
            .collect()
    }

    fn recent_gallery_appearance(&self, cx: &App) -> String {
        let palette = brand::palette(cx);
        format!(
            "{:?}:{:?}:{:?}:{:?}:{}:{:p}:{}:{}:{}",
            palette.surface,
            palette.text,
            palette.selected,
            palette.border,
            cx.theme().is_dark(),
            Arc::as_ptr(&cx.theme().highlight_theme),
            reader_fonts::note(cx),
            reader_fonts::code(cx),
            reader_ui_state::font_size(cx).to_bits()
        )
    }

    /// Refresh only accepted inventory publications; no watcher polling or disk I/O.
    fn refresh_recent_gallery(&mut self, cx: &mut Context<Self>) {
        if !self.recent_switcher.gallery_open {
            return;
        }
        if self
            .recent_switcher
            .gallery
            .as_ref()
            .is_some_and(|session| {
                session.root != self.vault_root || session.navigation != self.navigation.generation
            })
        {
            self.recent_switcher.cancel();
            cx.notify();
            return;
        }
        let entries = self.recent_gallery_entries();
        let inventory = self.quick_open.inventory.clone();
        let appearance = self.recent_gallery_appearance(cx);
        let Some(session) = self.recent_switcher.gallery.as_mut() else {
            return;
        };
        if session.appearance != appearance {
            session.appearance = appearance;
            session
                .view
                .update(cx, |view, cx| view.appearance_changed(cx));
        }
        // Missing inventory is a preparation state, not evidence of deletion.
        if inventory.is_none() {
            if session.inventory.take().is_some() {
                session.view.update(cx, |view, cx| view.suspend(cx));
            }
            return;
        }
        if !lifecycle::same_inventory(session.inventory.as_ref(), inventory.as_ref())
            || session.entries != entries
        {
            session.inventory = inventory;
            session.entries = entries.clone();
            session
                .view
                .update(cx, |view, cx| view.refresh(entries, &[], cx));
        }
    }

    /// An accepted rename publication may supply verified final old/new mappings.
    /// External renames without such evidence remain deletion/addition, never guesses.
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub(super) fn refresh_recent_gallery_renames(
        &mut self,
        renames: &[(String, String)],
        cx: &mut Context<Self>,
    ) {
        if !self.recent_switcher.gallery_open || self.quick_open.inventory.is_none() {
            return;
        }
        let entries = self.recent_gallery_entries();
        let inventory = self.quick_open.inventory.clone();
        if let Some(session) = &mut self.recent_switcher.gallery {
            if session.root != self.vault_root {
                return;
            }
            session.inventory = inventory;
            session.entries = entries.clone();
            session
                .view
                .update(cx, |view, cx| view.refresh(entries, renames, cx));
        }
    }

    fn finish_recent_gallery_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut restore_focus = false;
        if let Some(session) = &mut self.recent_switcher.gallery {
            if session.focused {
                session.focused = false;
                restore_focus = session.input.read(cx).focus_handle(cx).is_focused(window);
                session.view.update(cx, |view, cx| view.suspend(cx));
            }
        }
        if self
            .recent_switcher
            .gallery
            .as_ref()
            .is_some_and(|session| session.root != self.vault_root)
        {
            self.recent_switcher.gallery = None;
        }
        if restore_focus {
            self.restore_document_focus(window, cx);
        }
    }
    pub(super) fn close_recent_gallery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recent_switcher.cancel();
        self.finish_recent_gallery_close(window, cx);
        cx.notify();
    }

    fn render_recent_gallery(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = &self.recent_switcher.gallery else {
            return div().into_any_element();
        };
        let palette = brand::palette(cx);
        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(64.))
            .child(
                v_flex()
                    .id("recent-gallery-overlay")
                    .debug_selector(|| "recent-gallery-overlay".into())
                    .occlude()
                    .w(px(800.))
                    .max_w_full()
                    .rounded(px(12.))
                    .shadow_lg()
                    .bg(palette.surface)
                    .on_mouse_down_out(
                        cx.listener(|this, _, window, cx| this.close_recent_gallery(window, cx)),
                    )
                    .capture_action(gallery::capture_arrow::<gpui_component::input::MoveLeft>(
                        session.view.clone(),
                        "left",
                    ))
                    .capture_action(gallery::capture_arrow::<gpui_component::input::MoveRight>(
                        session.view.clone(),
                        "right",
                    ))
                    .capture_action(gallery::capture_arrow::<gpui_component::input::MoveUp>(
                        session.view.clone(),
                        "up",
                    ))
                    .capture_action(gallery::capture_arrow::<gpui_component::input::MoveDown>(
                        session.view.clone(),
                        "down",
                    ))
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        let key = event.keystroke.key.as_str();
                        let modifiers = event.keystroke.modifiers;
                        if modifiers.control || modifiers.alt || modifiers.platform {
                            return;
                        }
                        let Some(view) = this
                            .recent_switcher
                            .gallery
                            .as_ref()
                            .map(|session| session.view.clone())
                        else {
                            return;
                        };
                        if let Some(delta) = lifecycle::step(
                            key,
                            lifecycle::columns(f32::from(window.viewport_size().width)),
                        ) {
                            view.update(cx, |view, cx| view.step(delta, cx));
                            cx.stop_propagation();
                        } else if key == "enter" {
                            view.update(cx, |view, cx| view.accept(cx));
                            cx.stop_propagation();
                        } else if key == "escape" {
                            this.close_recent_gallery(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(div().p_4().child(
                        Input::new(&session.input).prefix(Icon::new(IconName::Search).small()),
                    ))
                    .when(self.quick_open.inventory.is_none(), |view| {
                        view.child(div().p_4().child("Recent files are loading…"))
                    })
                    .when(self.quick_open.inventory.is_some(), |view| {
                        view.child(session.view.clone())
                    }),
            )
            .into_any_element()
    }

    pub(super) fn cycle_recent(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.quick_open.open || self.shortcut_sheet.open || self.recent_switcher.gallery_open {
            return;
        }
        if !self.recent_switcher.open() {
            let Some(inventory) = &self.quick_open.inventory else {
                return;
            };
            self.recent_switcher.begin(
                &self.current_rel,
                &self.quick_open.recent,
                inventory,
                &self.vault_root,
            );
        }
        self.recent_switcher.step(delta);
        cx.notify();
    }
    /// A compositor can clear modifiers before reporting keyboard focus loss.
    /// Do not navigate inside that event: allow activation cancellation to win.
    pub(super) fn queue_recent_release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.recent_switcher.open() || self.recent_switcher.gallery_open {
            return;
        }
        self.recent_switcher.invalidate_release();
        let generation = self.recent_switcher.release_generation;
        let navigation = self.navigation.generation;
        cx.spawn_in(window, async move |reader, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let _ = reader.update_in_own_window(cx, |reader, window, cx| {
                if reader.recent_switcher.release_generation != generation {
                    return;
                }
                if reader.navigation.generation != navigation {
                    reader.recent_switcher.cancel();
                    cx.notify();
                    return;
                }
                if window.modifiers().control {
                    return;
                }
                reader.release_recent(window, cx);
            });
        })
        .detach();
    }

    pub(super) fn release_recent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recent_switcher.gallery_open {
            return;
        }
        if !window.is_window_active() {
            self.recent_switcher.cancel();
            cx.notify();
            return;
        }
        let Some((root, path)) = self.recent_switcher.take() else {
            return;
        };
        if root == self.vault_root && path != self.current_rel {
            self.open_note(&path, None, window, cx);
        }
        cx.notify();
    }
    pub(super) fn render_recent(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.recent_switcher.gallery_open {
            self.refresh_recent_gallery(cx);
            return if self.recent_switcher.gallery_open {
                self.render_recent_gallery(cx)
            } else {
                div().into_any_element()
            };
        }
        let palette = brand::palette(cx);
        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(72.))
            .child(
                v_flex()
                    .id("recent-switcher")
                    .debug_selector(|| "recent-switcher".into())
                    .w(px(420.))
                    .max_w_full()
                    .max_h(px(360.))
                    .p_2()
                    .rounded(px(12.))
                    .bg(palette.surface)
                    .text_color(palette.text)
                    .shadow_lg()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_size(px(12.))
                            .text_color(palette.text_muted)
                            .child("Recent notes"),
                    )
                    .child(
                        v_flex()
                            .id("recent-switcher-list")
                            .max_h(px(280.))
                            .overflow_y_scroll()
                            .track_scroll(&self.recent_switcher.scroll)
                            .children(self.recent_switcher.paths.iter().enumerate().map(
                                |(index, path)| {
                                    let title = self.note_label(path);
                                    let duplicate = self
                                        .recent_switcher
                                        .paths
                                        .iter()
                                        .filter(|p| self.note_label(p) == title)
                                        .count()
                                        > 1;
                                    v_flex()
                                        .id(("recent-switcher-row", index))
                                        .tooltip({
                                            let path = path.clone();
                                            move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(path.clone())
                                                    .build(window, cx)
                                            }
                                        })
                                        .px_2()
                                        .py_1()
                                        .rounded(px(6.))
                                        .when(index == self.recent_switcher.selected, |row| {
                                            row.bg(palette.selected)
                                        })
                                        .child(div().text_sm().truncate().child(title))
                                        .when(duplicate, |row| {
                                            row.child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(palette.text_muted)
                                                    .truncate()
                                                    .child(
                                                        Path::new(path)
                                                            .parent()
                                                            .filter(|p| !p.as_os_str().is_empty())
                                                            .map(|p| {
                                                                p.to_string_lossy().into_owned()
                                                            })
                                                            .unwrap_or_else(|| "Vault".into()),
                                                    ),
                                            )
                                        })
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    fn notes(paths: &[&str]) -> Vec<okilum_core::vault::Note> {
        paths
            .iter()
            .map(|p| okilum_core::vault::Note {
                path: (*p).into(),
                title: (*p).into(),
            })
            .collect()
    }
    #[test]
    fn frozen_mru_wraps_both_ways_and_commits_once() {
        let mut s = Switcher::default();
        s.begin(
            "C.md",
            &[
                "A.md".into(),
                "B.md".into(),
                "C.md".into(),
                "missing.md".into(),
                "B.md".into(),
            ],
            &notes(&["A.md", "B.md", "C.md"]),
            Path::new("/vault"),
        );
        s.step(1);
        assert_eq!(s.paths[s.selected], "B.md");
        s.step(1);
        assert_eq!(s.paths[s.selected], "A.md");
        s.step(1);
        assert_eq!(s.paths[s.selected], "C.md");
        s.step(-1);
        assert_eq!(s.take().unwrap().1, "A.md");
        assert!(s.take().is_none());
    }
    #[test]
    fn cancellation_and_single_candidate_never_navigate() {
        let mut s = Switcher::default();
        s.begin("A.md", &[], &notes(&["A.md"]), Path::new("/vault"));
        s.step(1);
        assert!(s.take().is_none());
        s.begin(
            "A.md",
            &["B.md".into()],
            &notes(&["A.md", "B.md"]),
            Path::new("/vault"),
        );
        s.step(1);
        s.cancel();
        assert!(s.take().is_none());
    }
    #[test]
    fn gallery_never_commits_through_mru_control_release() {
        let mut switcher = Switcher {
            gallery_open: true,
            ..Default::default()
        };
        assert!(switcher.open());
        switcher.step(1);
        assert!(switcher.take().is_none());
        switcher.cancel();
        assert!(!switcher.open());
    }

    #[gpui::test]
    fn gallery_search_inventory_rename_accept_and_reopen_use_one_cached_view(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("A.md"), "# Root").unwrap();
        std::fs::write(root.join("nested/A.md"), "# Nested").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("A.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        session_directory: Some(fixture.path().join("state")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let cached = reader.update_in(visual, |reader, window, cx| {
            reader.sidebar.recent = vec![("A.md".into(), 0), ("nested/A.md".into(), 0)];
            reader.quick_open.inventory = Some(Arc::new(notes(&["A.md", "nested/A.md"])));
            reader.toggle_recent_gallery(window, cx);
            reader
                .recent_switcher
                .gallery
                .as_ref()
                .unwrap()
                .view
                .clone()
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("recent-gallery-overlay").is_some(),
            "positive control: real overlay is mounted"
        );
        visual.simulate_input("A");
        visual.simulate_keystrokes("right");
        visual.run_until_parked();
        cached.read_with(visual, |gallery, _| {
            assert_eq!(gallery.selected_path(), Some("nested/A.md"))
        });
        // Control release belongs to MRU only, never to this focused overlay.
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.recent_switcher.gallery_open);
            assert_eq!(reader.current_rel, "A.md");
        });
        // Publish an accepted inventory with a deleted neighbour. The Reader's
        // own notification observer must preserve the selected exact path.
        reader.update(visual, |reader, cx| {
            reader.quick_open.inventory = Some(Arc::new(notes(&["nested/A.md"])));
            cx.notify();
        });
        visual.run_until_parked();
        cached.read_with(visual, |gallery, _| {
            assert_eq!(gallery.selected_path(), Some("nested/A.md"))
        });
        std::fs::rename(root.join("nested/A.md"), root.join("nested/renamed.md")).unwrap();
        reader.update_in(visual, |reader, window, cx| {
            // Clearing search permits the renamed filename to remain visible.
            let input = reader
                .recent_switcher
                .gallery
                .as_ref()
                .unwrap()
                .input
                .clone();
            input.update(cx, |input, cx| input.set_value("", window, cx));
            cached.update(cx, |gallery, cx| gallery.set_query(String::new(), cx));
            reader.sidebar.recent = vec![("nested/renamed.md".into(), 0)];
            reader.quick_open.inventory = Some(Arc::new(notes(&["nested/renamed.md"])));
            reader.refresh_recent_gallery_renames(
                &[("nested/A.md".into(), "nested/renamed.md".into())],
                cx,
            );
            cx.notify();
        });
        visual.run_until_parked();
        cached.read_with(visual, |gallery, _| {
            assert_eq!(gallery.selected_path(), Some("nested/renamed.md"))
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "nested/renamed.md");
            assert!(!reader.recent_switcher.gallery_open);
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_recent_gallery(window, cx);
            assert_eq!(
                reader.recent_switcher.gallery.as_ref().unwrap().view,
                cached
            );
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(!reader.recent_switcher.open()));
        assert!(visual.debug_bounds("recent-gallery-overlay").is_none());
    }

    #[gpui::test]
    fn native_release_and_escape_route_from_reader_and_editor(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("A.md"), "Alpha").unwrap();
        std::fs::write(root.join("B.md"), "Beta").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("A.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        session_directory: Some(fixture.path().join("state")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.quick_open.remember("B.md");
            r.quick_open.remember("A.md");
            r.focus_handle.focus(window, cx);
            window.activate_window();
        });
        visual.run_until_parked();
        // Escape must work with the physical Control/Shift modifiers held.
        for escape in ["ctrl-escape", "ctrl-shift-escape"] {
            visual.simulate_keystrokes("ctrl-tab");
            visual.run_until_parked();
            assert!(visual.debug_bounds("recent-switcher").is_some());
            visual.simulate_keystrokes(escape);
            visual.run_until_parked();
            assert!(visual.debug_bounds("recent-switcher").is_none());
            visual.simulate_event(gpui::ModifiersChangedEvent::default());
            visual.run_until_parked();
            reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "A.md"));
        }
        visual.simulate_keystrokes("ctrl-tab");
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-switcher").is_some());
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "A.md"));
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "B.md"));
        let previous_input = reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.editing.as_ref().unwrap().test_input()
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("ctrl-tab");
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-switcher").is_some());
        visual.simulate_keystrokes("ctrl-escape");
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "B.md");
            assert!(r.editing.is_some());
        });
        // Wayland can clear Control while the window still reports active,
        // then deliver deactivation. No navigation may happen in between.
        visual.simulate_keystrokes("ctrl-tab");
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-switcher").is_some());
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "B.md"));
        visual.deactivate_window();
        visual.run_until_parked();
        reader.update_in(visual, |_, window, _| window.activate_window());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(
                r.current_rel, "B.md",
                "focus loss cancels the queued release"
            );
            assert!(!r.recent_switcher.open());
        });
        // A compositor may deliver the release only after the window returns.
        reader.update_in(visual, |_, window, _| window.activate_window());
        visual.run_until_parked();
        visual.simulate_keystrokes("ctrl-tab");
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-switcher").is_some());
        visual.deactivate_window();
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(!r.recent_switcher.open()));
        reader.update_in(visual, |_, window, _| window.activate_window());
        visual.run_until_parked();
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "B.md"));
        visual.simulate_keystrokes("ctrl-tab");
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        let current_input = reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "A.md", "real release accepted the target");
            let editing = r
                .editing
                .as_ref()
                .expect("file selection restored the chosen editor");
            assert_ne!(
                editing.test_input(),
                previous_input,
                "the target owns a new editor"
            );
            assert_eq!(r.selected_file(), "A.md", "actual selected editor path");
            assert!(
                !r.source_live_preview(),
                "the chosen Source mode was retained"
            );
            assert_eq!(editing.test_input().read(cx).value().as_ref(), "Alpha");
            editing.test_input()
        });
        visual.simulate_input("mine");
        visual.run_until_parked();
        let dirty_source = reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert_eq!(editing.test_input(), current_input);
            let source = editing.test_input().read(cx).value().to_string();
            assert!(
                source.contains("mine") && source != "Alpha",
                "real keystrokes reached the editor"
            );
            assert!(
                r.source_is_dirty(cx),
                "real edited buffer is dirty before external change"
            );
            source
        });
        std::fs::write(root.join("A.md"), "external").unwrap();
        visual.simulate_keystrokes("ctrl-tab");
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.current_rel, "A.md",
                "external conflict refuses real navigation"
            );
            let editing = r.editing.as_ref().unwrap();
            assert_eq!(
                editing.test_input(),
                current_input,
                "refusal keeps the owned editor"
            );
            assert!(
                r.link_notice
                    .as_deref()
                    .is_some_and(|notice| notice.starts_with("This file changed outside Okilum.")),
                "actual save reports the external conflict"
            );
            assert_eq!(editing.test_input().read(cx).value().as_ref(), dirty_source);
        });
        assert_eq!(
            std::fs::read_to_string(root.join("A.md")).unwrap(),
            "external"
        );
    }
}
