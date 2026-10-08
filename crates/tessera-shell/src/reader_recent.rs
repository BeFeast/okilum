//! A frozen MRU gesture: highlight is not navigation; Control release commits once.
use super::*;

#[derive(Default)]
pub(super) struct Switcher {
    paths: Vec<String>,
    selected: usize,
    root: PathBuf,
    scroll: ScrollHandle,
}
impl Switcher {
    fn begin(
        &mut self,
        current: &str,
        recent: &[String],
        available: &[tessera_core::vault::Note],
        root: &Path,
    ) {
        let mut paths = vec![];
        for path in std::iter::once(current).chain(recent.iter().rev().map(String::as_str)) {
            if path.to_lowercase().ends_with(".md")
                && !tessera_core::excalidraw::is_drawing(path)
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
        self.paths.len() > 1
    }
    fn step(&mut self, delta: isize) {
        if self.open() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.paths.len() as isize) as usize;
            self.scroll.scroll_to_item(self.selected);
        }
    }
    fn take(&mut self) -> Option<(PathBuf, String)> {
        let result = self
            .open()
            .then(|| (self.root.clone(), self.paths[self.selected].clone()));
        self.paths.clear();
        result
    }
    pub fn cancel(&mut self) {
        self.paths.clear();
    }
}

impl Reader {
    pub(super) fn cycle_recent(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.quick_open.open || self.shortcut_sheet.open {
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
    pub(super) fn release_recent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((root, path)) = self.recent_switcher.take() else {
            return;
        };
        if root == self.vault_root && path != self.current_rel {
            self.open_note(&path, None, window, cx);
        }
        cx.notify();
    }
    pub(super) fn render_recent(&self, cx: &mut Context<Self>) -> AnyElement {
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
                                    let title = Path::new(path)
                                        .file_stem()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned();
                                    let duplicate = self
                                        .recent_switcher
                                        .paths
                                        .iter()
                                        .filter(|p| {
                                            Path::new(p).file_stem() == Path::new(path).file_stem()
                                        })
                                        .count()
                                        > 1;
                                    v_flex()
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
    fn notes(paths: &[&str]) -> Vec<tessera_core::vault::Note> {
        paths
            .iter()
            .map(|p| tessera_core::vault::Note {
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
    #[gpui::test]
    fn native_release_and_escape_route_from_reader_and_editor(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
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
        });
        visual.simulate_keystrokes("ctrl-tab");
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-switcher").is_some());
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "A.md"));
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "B.md"));
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        visual.simulate_keystrokes("ctrl-tab");
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-switcher").is_some());
        visual.simulate_keystrokes("escape");
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "B.md");
            assert!(r.editing.is_some());
        });
        visual.simulate_keystrokes("ctrl-tab");
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "A.md"));
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        visual.simulate_input("mine");
        visual.run_until_parked();
        std::fs::write(root.join("A.md"), "external").unwrap();
        visual.simulate_keystrokes("ctrl-tab");
        visual.simulate_event(gpui::ModifiersChangedEvent::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "A.md");
            assert!(r.editing.is_some());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("A.md")).unwrap(),
            "external"
        );
    }
}
