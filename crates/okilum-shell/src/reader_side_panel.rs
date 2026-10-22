//! The side panels as cached views (#1290): the Notes tree and «On this page»
//! are reused unchanged on every scroll frame instead of being rebuilt, laid
//! out and painted again (about half of a frame's cost on the 24k-line note).
//!
//! A cached gpui view is redrawn only when its own entity is notified, so a
//! panel observes the Reader: every Reader `cx.notify()` redraws it, as every
//! frame did before. Scrolling the note does not notify the Reader; what the
//! panels show of the scroll position, the current section in the outline, is
//! checked on each Reader render and redraws the right panel, in the frame
//! after, when it changes.
use super::*;

pub(crate) struct SidePanel {
    reader: WeakEntity<Reader>,
    left: bool,
    _reader: Subscription,
    pub(super) file_outline: Option<Entity<reader_right_panel::file_outline::View>>,
    _outline: Option<Subscription>,
}

impl SidePanel {
    /// Both panels, made with the Reader and outside any draw: a view first
    /// created during a draw is not tracked by the window from the next frame
    /// on (it was already "accessed" before its first render), so a notify
    /// could never redraw it and the panel would stay frozen.
    pub(crate) fn pair(cx: &mut Context<Reader>) -> [Option<Entity<Self>>; 2] {
        let reader = cx.entity();
        [true, false].map(|left| Some(cx.new(|cx| Self::new(&reader, left, cx))))
    }

    fn new(reader: &Entity<Reader>, left: bool, cx: &mut Context<Self>) -> Self {
        let file_outline =
            (!left).then(|| cx.new(|_| reader_right_panel::file_outline::View::new()));
        let outline_observer = file_outline
            .as_ref()
            .map(|outline| cx.observe(outline, |_, _, cx| cx.notify()));
        Self {
            reader: reader.downgrade(),
            left,
            _reader: cx.observe(reader, |this, reader, cx| {
                let clear =
                    reader.read(cx).file_preview.is_none() || !reader.read(cx).panels.backlinks;
                if clear {
                    if let Some(outline) = &this.file_outline {
                        outline.update(cx, |outline, cx| outline.clear(cx));
                    }
                }
                cx.notify();
            }),
            file_outline,
            _outline: outline_observer,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Renders of the [left, right] panel, for the tests.
    static RENDERS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
}

impl Render for SidePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let left = self.left;
        let file_outline = self.file_outline.clone();
        #[cfg(test)]
        RENDERS.with(|renders| {
            let mut counts = renders.get();
            counts[usize::from(!left)] += 1;
            renders.set(counts);
        });
        let body = self
            .reader
            .update(cx, |reader, cx| {
                if left {
                    reader.render_sidebar(window, cx)
                } else if let Some(outline) = file_outline
                    .filter(|_| reader.file_preview.is_some() && reader.active_timeline().is_none())
                {
                    reader_right_panel::render_file(reader, outline, cx)
                } else {
                    if let Some(outline) = &self.file_outline {
                        reader_right_panel::file_outline::View::bind(outline, reader, cx);
                    }
                    reader.render_right_panel(cx)
                }
            })
            .unwrap_or_else(|_| div().into_any_element());
        div().size_full().child(body)
    }
}

// Tests draw the panels uncached unless they ask for the cache
// (`tests::CachedPanels`): gpui records `debug_selector` bounds only for
// subtrees it draws, not for a cached one it reuses, so a test looking up a
// panel row after a reused frame would find nothing, and an `is_none()`
// check would pass for the wrong reason.
#[cfg(test)]
thread_local! {
    static CACHE_IN_TESTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn cache_panels() -> bool {
    CACHE_IN_TESTS.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn cache_panels() -> bool {
    true
}

impl Reader {
    /// The panel's cached view (made with the Reader, see [`SidePanel::pair`]).
    pub(super) fn side_panel(&self, left: bool) -> AnyElement {
        let Some(panel) = &self.side_panels[usize::from(!left)] else {
            return div().into_any_element();
        };
        let view = AnyView::from(panel.clone());
        if cache_panels() {
            view.cached(StyleRefinement::default().size_full())
                .into_any_element()
        } else {
            view.into_any_element()
        }
    }

    /// Called on every Reader render: the outline marks the section at the
    /// top of the note, which moves without a Reader notify while scrolling.
    pub(super) fn sync_outline_section(&mut self, cx: &mut Context<Self>) {
        let section = self.current_section(cx);
        if self.outline_section == section {
            return;
        }
        self.outline_section = section;
        // A notify inside a draw only takes effect at the next draw, and none
        // would be scheduled: after this one, it asks for its own frame.
        if let Some(panel) = self.side_panels[1].clone() {
            cx.defer(move |cx| panel.update(cx, |_, cx| cx.notify()));
        }
    }

    /// The document notifies on every scroll frame. The Reader redraws with the
    /// window anyway; it notifies (and so redraws the cached panels) only when
    /// the document itself changed: a parse landed or failed.
    pub(super) fn content_changed(&mut self, cx: &mut Context<Self>) {
        let status = self
            .content
            .read(cx)
            .preparation_status()
            .map(|status| status.is_ok());
        if status != self.content_status {
            self.content_status = status;
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn renders() -> [usize; 2] {
        RENDERS.with(|renders| renders.get())
    }

    /// Panels drawn through the cache, as in the app, for one test.
    pub(super) struct CachedPanels;
    impl CachedPanels {
        pub(super) fn on() -> Self {
            CACHE_IN_TESTS.with(|cache| cache.set(true));
            Self
        }
    }
    impl Drop for CachedPanels {
        fn drop(&mut self) {
            CACHE_IN_TESTS.with(|cache| cache.set(false));
        }
    }

    /// #1290: scrolling inside a section redraws neither panel; reaching the
    /// next section redraws «On this page» (its current row moves); a Reader
    /// change (another note) redraws both.
    #[gpui::test]
    fn scrolling_reuses_the_side_panels_until_what_they_show_changes(cx: &mut TestAppContext) {
        let _cache = CachedPanels::on();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = tempfile::tempdir().unwrap();
        let body: String = (1..=40)
            .map(|i| format!("## Section {i}\n\n{}\n\n", "A line of text. ".repeat(40)))
            .collect();
        std::fs::write(root.path().join("long.md"), body).unwrap();
        std::fs::write(root.path().join("other.md"), "# Other\n\nShort.\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.path().to_path_buf()),
                        note: Some("long.md".into()),
                        index_dir: Some(root.path().join(".index")),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(800.)));
        visual.run_until_parked();
        view.update(visual, |v, cx| {
            assert_eq!(v.outline.len(), 40, "outline positive control");
            v.panels.open(reader_layout::Panel::Notes);
            v.panels.open(reader_layout::Panel::Backlinks);
            cx.notify();
        });
        visual.run_until_parked();
        // The test platform does not draw a dirty window by itself; the app
        // gets the next frame from the display. Draw it, so a deferred redraw
        // shows up (and a missing one would be caught).
        let frame = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        };
        let section = |visual: &mut VisualTestContext| {
            view.read_with(visual, |v, cx| (v.current_section(cx), v.outline_section))
        };
        assert_eq!(section(visual), (Some(0), Some(0)));
        frame(visual);
        let opened = renders();
        assert!(
            opened[0] > 0 && opened[1] > 0,
            "both panels drawn: {opened:?}"
        );
        let document = visual
            .debug_bounds("reader-document")
            .expect("the document is drawn");
        let wheel = |visual: &mut VisualTestContext, pixels: f32| {
            visual.simulate_event(ScrollWheelEvent {
                position: point(document.center().x, document.top() + px(200.)),
                delta: ScrollDelta::Pixels(point(px(0.), px(-pixels))),
                ..Default::default()
            });
            visual.run_until_parked();
            frame(visual);
        };

        // Inside the first section: the note moves, the panels are reused.
        let top = |visual: &mut VisualTestContext| {
            view.read_with(visual, |v, cx| {
                let top = v.content.read(cx).list_state().logical_scroll_top();
                (top.item_ix, top.offset_in_item)
            })
        };
        let before = top(visual);
        wheel(visual, 30.);
        assert_ne!(top(visual), before, "positive control: the note scrolled");
        assert_eq!(section(visual), (Some(0), Some(0)));
        assert_eq!(
            renders(),
            opened,
            "a scroll inside a section redraws no panel"
        );

        // Into a later section: only «On this page» is drawn again, with its new current row.
        for _ in 0..40 {
            if section(visual).0 != Some(0) {
                break;
            }
            wheel(visual, 200.);
        }
        // The frame that met the new section asked for the next one.
        frame(visual);
        let (current, drawn) = section(visual);
        assert!(
            current.is_some_and(|s| s > 0),
            "scrolled into a later section: {current:?}"
        );
        assert_eq!(drawn, current, "the outline marks the section at the top");
        let moved = renders();
        assert_eq!(
            moved[0], opened[0],
            "the Notes tree does not depend on the scroll"
        );
        assert!(moved[1] > opened[1], "«On this page» was drawn again");

        // A Reader change redraws both panels.
        view.update_in(visual, |v, window, cx| {
            v.open_note("other.md", None, window, cx)
        });
        visual.run_until_parked();
        frame(visual);
        view.read_with(visual, |v, _| assert_eq!(v.current_rel, "other.md"));
        let switched = renders();
        assert!(
            switched[0] > moved[0] && switched[1] > moved[1],
            "{moved:?} -> {switched:?}"
        );
    }
}
