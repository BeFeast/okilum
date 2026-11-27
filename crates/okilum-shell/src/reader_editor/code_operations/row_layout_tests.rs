//! Actual editor frames; deterministic source-work and geometry controls only.
use super::*;
use ::core::prelude::v1::test;
use gpui::{px, Bounds, Pixels};
use gpui_component::input::{Redo, Undo};

fn source() -> String {
    // Keep the proven two-long-rows + short-row scenario, including a complete
    // Unicode-correct shape and a TAB path rather than an ASCII-only fixture.
    format!(
        "//{{ {}שלום🙂\n// {}\tend\nshort\n",
        "x".repeat(300 * 1024),
        "y".repeat(300 * 1024)
    )
}

fn draw(v: &mut VisualTestContext) {
    v.update(|w, cx| {
        w.simulate_next_frame(cx);
        w.draw(cx).clear(cx);
    });
}

fn prepare(f: &tests::Fixture, v: &mut VisualTestContext) {
    f.input.update_in(v, |i, w, cx| {
        i.set_soft_wrap(false, w, cx);
        i.enable_layout_diagnostics();
        i.focus(w, cx);
    });
    tests::settle(v);
    draw(v);
    // The production syntax adapter debounces for 150 ms; establish its
    // published styles before comparing cold and reused geometry.
    v.executor().advance_clock(Duration::from_millis(151));
    v.run_until_parked();
    draw(v);
}

#[gpui::test]
fn edit2803_paragraph_direction_does_not_copy_reused_long_note_rows(cx: &mut TestAppContext) {
    // Markdown enables per-paragraph direction; plain-code fixtures do not.
    // Open a small real note first, then insert the full fixture atomically
    // after selecting unwrapped mode. Initial wrapped opening is a separate
    // residual, not the paragraph-direction/reused-row path measured here.
    let seed = "a{\r\nא\r\nshort\r\n";
    let original = format!(
        "a{{{}\r\nא{}\r\nshort\r\n",
        "x".repeat(300 * 1024),
        "y".repeat(300 * 1024)
    );
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=open-small-reader-note");
    let (f, v) = tests::open_file(cx, seed, "directions.md");
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=prepare-unwrapped-editor");
    prepare(&f, v);
    assert!(f.input.update_in(v, |i, w, cx| {
        i.replace_source_range(0..seed.len(), &original, w, cx)
    }));
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=large-source-inserted");
    prepare(&f, v);
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=large-source-prepared");
    let at = original.rfind("short").unwrap();
    f.input.update(v, |i, cx| i.set_selected_range(at..at, cx));
    draw(v);
    let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    f.input
        .update_in(v, |i, w, cx| i.replace_source_range(at..at, "!", w, cx));
    draw(v);
    let after = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=short-edit-painted");
    let queries = after.paragraph_direction_queries - before.paragraph_direction_queries;
    let source_bytes =
        after.paragraph_direction_source_bytes - before.paragraph_direction_source_bytes;
    assert!(
        after.source_row_cache_hits >= before.source_row_cache_hits + 2,
        "both long rows must actually reuse layouts"
    );
    assert!(queries >= 2, "actual paragraph-direction path must fire");
    assert!(
        source_bytes <= queries * 8,
        "paragraph direction copied long rows: {source_bytes} source bytes for {queries} queries"
    );

    // Direction classification must keep exact caret/hit geometry, including
    // the right-aligned Hebrew paragraph, across a cold effective-style pass.
    let hebrew = original.find('א').unwrap() + 'א'.len_utf8();
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=check-rtl-geometry");
    let warm = geometry(&f, v, hebrew);
    tint(&f, v, gpui::rgb(0x2233aa).into());
    assert_eq!(geometry(&f, v, hebrew), warm);
    let expected = format!("{}!{}", &original[..at], &original[at..]);
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), expected);
    v.dispatch_action(Undo);
    v.run_until_parked();
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), original);
    v.dispatch_action(Redo);
    v.run_until_parked();
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), expected);
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=save-on-edit-exit");
    f.reader.update(v, |r, cx| {
        // toggle_source uses this same guarded save/exit before starting a
        // separate Read preparation. Verify the required canonical-save
        // boundary directly; do not drain unrelated long-note Read rendering.
        assert!(r.leave_source(cx), "the normal source-exit guard must save");
        assert!(r.pane.editing.is_none(), "source exit must complete");
        assert_eq!(
            std::fs::read_to_string(f.directory.path().join("vault/directions.md")).unwrap(),
            expected
        );
        diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=guarded-source-exit-complete");
    });
    diagnostic_error!("PARAGRAPH_DIRECTION_STAGE phase=source-and-save-preserved");
}

#[gpui::test]
fn edit2803_short_edit_reuses_unchanged_unicode_and_tab_rows(cx: &mut TestAppContext) {
    let original = source();
    let (f, v) = tests::open_file(cx, &original, "rows.rs");
    prepare(&f, v);
    let at = original.rfind("short").unwrap();
    f.input.update(v, |i, cx| i.set_selected_range(at..at, cx));
    draw(v);
    let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    // A retained frame must fire the existing complete-frame cache first.
    f.input.update(v, |_, cx| cx.notify());
    draw(v);
    let cached = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(cached.source_shape_cache_hits > before.source_shape_cache_hits);
    assert_eq!(cached.layout_calls, before.layout_calls);

    f.input
        .update_in(v, |i, w, cx| i.replace_source_range(at..at, "!", w, cx));
    draw(v);
    let edited = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(
        edited.layout_calls > cached.layout_calls,
        "changed-row path must fire"
    );
    assert!(edited.source_row_materialized_bytes > cached.source_row_materialized_bytes);
    assert_eq!(
        edited.source_row_materialized_bytes - cached.source_row_materialized_bytes,
        "!short".len() as u64,
        "only changed row bytes may materialize; unchanged large rows must reuse layouts"
    );
    assert!(edited.source_row_cache_hits >= cached.source_row_cache_hits + 2);
    let expected = format!("{}!{}", &original[..at], &original[at..]);
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), expected);
    v.dispatch_action(Undo);
    v.run_until_parked();
    draw(v);
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), original);
    v.dispatch_action(Redo);
    v.run_until_parked();
    draw(v);
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), expected);

    // Use the real Edit -> Read -> Edit boundary, not a forced cache release.
    f.reader.update_in(v, |r, w, cx| r.toggle_source(w, cx));
    tests::settle(v);
    f.reader.update_in(v, |r, w, cx| r.toggle_source(w, cx));
    tests::settle(v);
    let reopened = f.reader.read_with(v, |r, cx| {
        r.pane
            .editing
            .as_ref()
            .unwrap()
            .input
            .read(cx)
            .value()
            .to_string()
    });
    assert_eq!(reopened, expected);
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/rows.rs")).unwrap(),
        expected
    );
}

fn geometry(
    f: &tests::Fixture,
    v: &mut VisualTestContext,
    at: usize,
) -> (Bounds<Pixels>, Pixels, Vec<Option<usize>>) {
    f.input.update(v, |i, cx| i.set_selected_range(at..at, cx));
    draw(v);
    f.input.read_with(v, |i, _| {
        assert_eq!(i.selected_range(), at..at);
        let (bounds, height) = i.cursor_layout().expect("actual painted caret");
        let points =
            [-2., -0.5, 0., 0.5, 2.].map(|dx| bounds.origin + gpui::point(px(dx), height / 2.));
        let hits = points.map(|p| i.source_offset_for_point(p)).to_vec();
        assert!(
            hits.iter().any(Option::is_some),
            "real hit testing must fire"
        );
        (bounds, height, hits)
    })
}

// Exercise the real TextRun composition seam deterministically. Changing a
// language name alone need not change effective styles (and is not a control).
struct Tint {
    language: SharedString,
    color: gpui::Hsla,
    styles_seen: Rc<std::cell::Cell<usize>>,
}
impl gpui_component::input::InputHighlighter for Tint {
    fn language(&self) -> SharedString {
        self.language.clone()
    }
    fn update(
        &mut self,
        _: Option<gpui_component::input::InputEdit>,
        _: &gpui_component::input::Rope,
        _: bool,
        _: &mut Window,
        _: &mut Context<EditorState>,
    ) {
    }
    fn styles(
        &self,
        range: &Range<usize>,
        _: &dyn gpui_component::input::HighlightStyleResolver,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        self.styles_seen.set(self.styles_seen.get() + 1);
        vec![(
            range.clone(),
            gpui::HighlightStyle {
                color: Some(self.color),
                ..Default::default()
            },
        )]
    }
    fn fold_ranges(
        &self,
        _: &gpui_component::input::Rope,
    ) -> Vec<gpui_component::input::FoldRange> {
        Vec::new()
    }
}
fn tint(f: &tests::Fixture, v: &mut VisualTestContext, color: gpui::Hsla) {
    let seen = Rc::new(std::cell::Cell::new(0));
    let observed = seen.clone();
    f.input.update(v, |i, cx| {
        i.set_highlighter_factory(
            Rc::new(move |language| {
                Some(Box::new(Tint {
                    language: language.to_owned().into(),
                    color,
                    styles_seen: observed.clone(),
                }))
            }),
            cx,
        );
    });
    draw(v);
    assert!(seen.get() > 0, "actual style composition must fire");
}

#[gpui::test]
fn edit2803_reused_row_geometry_matches_cold_layout_and_styles_invalidate(cx: &mut TestAppContext) {
    let original = source();
    let (f, v) = tests::open_file(cx, &original, "geometry.rs");
    prepare(&f, v);
    let ink: gpui::Hsla = gpui::rgb(0x2233aa).into();
    let alternate: gpui::Hsla = gpui::rgb(0xaa3322).into();
    tint(&f, v, ink);
    let before_edit = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    let short = original.rfind("short").unwrap();
    f.input.update_in(v, |i, w, cx| {
        i.replace_source_range(short..short, "!", w, cx)
    });
    draw(v);
    let reused = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(
        reused.source_row_cache_hits >= before_edit.source_row_cache_hits + 2,
        "geometry oracle must start from actual reused rows"
    );
    let expected = format!("{}!{}", &original[..short], &original[short..]);
    for at in [
        original.find("שלום").unwrap() + 2,
        original.find("\tend").unwrap() + 1,
    ] {
        let warm = geometry(&f, v, at);
        let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
        // Effective color changes force cold decorated layouts, with the same
        // font, viewport and scroll state as the actual reused-row snapshot.
        tint(&f, v, alternate);
        tint(&f, v, ink);
        let after = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
        assert!(after.source_row_materialized_bytes > before.source_row_materialized_bytes);
        assert_eq!(
            geometry(&f, v, at),
            warm,
            "strict far-right caret and hit geometry"
        );
    }
    // Resize is a separate admission control: a viewport round trip changes
    // horizontal scroll state on main too, so it is not the geometry oracle.
    let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    let viewport = v.update(|w, _| w.viewport_size());
    v.simulate_resize(gpui::size(viewport.width + px(80.), viewport.height));
    draw(v);
    v.simulate_resize(viewport);
    draw(v);
    let after = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(
        after.source_row_materialized_bytes > before.source_row_materialized_bytes,
        "viewport changes must invalidate row layouts"
    );
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), expected);
    v.dispatch_action(Undo);
    v.run_until_parked();
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), original);
}

#[gpui::test]
fn edit2803_small_note_typing_and_autosave_keep_chrome_cached(cx: &mut TestAppContext) {
    let _cached = crate::reader_side_panel::tests::CachedPanels::on();
    let mut note = String::from(
        "# Save latency\n\nSmall note remains responsive.\n\n## Below caret\n\n```json\n{\"a\": 1}\n```\n\n",
    );
    for _ in 0..18 {
        note.push_str("A short paragraph keeps the document close to the reported size.\n\n");
    }
    let (f, v) = tests::open_file(cx, &note, "note.md");
    f.reader.update_in(v, |r, w, cx| {
        r.panels.open(crate::reader_layout::Panel::Notes);
        r.panels.open(crate::reader_layout::Panel::Backlinks);
        r.set_live_preview(true, w, cx);
        cx.notify();
    });
    tests::settle(v);
    draw(v);
    let at = note.find('\n').unwrap();
    f.input.update(v, |i, cx| i.set_selected_range(at..at, cx));
    // Let initial vault indexing, one-time startup timers (workbench 500 ms,
    // open-note watch) and layout bounds settle before measuring steady-state
    // typing + autosave chrome invalidations.
    for _ in 0..40 {
        v.run_until_parked();
        draw(v);
        v.run_until_parked();
        if f.reader.read_with(v, |r, _| {
            r.incremental_state.is_some() && !r.incremental_initializing
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    v.executor().advance_clock(Duration::from_millis(1100));
    v.run_until_parked();
    draw(v);
    v.run_until_parked();
    draw(v);

    let panels = f.reader.read_with(v, |r, _| r.side_panels.clone());
    let side_notifies = std::rc::Rc::new(std::cell::Cell::new([0usize; 2]));
    let _subs: Vec<_> = panels
        .iter()
        .enumerate()
        .filter_map(|(ix, panel)| {
            let panel = panel.as_ref()?;
            let counts = side_notifies.clone();
            Some(v.update(|_, cx| {
                cx.observe(panel, move |_, _| {
                    let mut c = counts.get();
                    c[ix] += 1;
                    counts.set(c);
                })
            }))
        })
        .collect();
    let header = f.reader.read_with(v, |r, _| r.header_cache.clone());
    let header_before = header.read_with(v, |h, _| h.render_calls.get());

    let started = Instant::now();
    for ch in ["\n", "s", "a", "v", "e", "d"] {
        v.simulate_input(ch);
        v.run_until_parked();
        draw(v);
    }
    let after_type_side = side_notifies.get();
    let after_type_header = header.read_with(v, |h, _| h.render_calls.get());
    // Wait past Live Preview classification, recovery write, and autosave pause.
    v.executor().advance_clock(Duration::from_millis(1100));
    for _ in 0..40 {
        v.run_until_parked();
        draw(v);
        if std::fs::read_to_string(f.directory.path().join("vault/note.md"))
            .unwrap()
            .contains("\nsaved\n")
            && f.reader.read_with(v, |r, _| {
                !r.incremental_active && r.incremental_state.is_some()
            })
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    v.run_until_parked();
    draw(v);
    let elapsed = started.elapsed();

    let header_after = header.read_with(v, |h, _| h.render_calls.get());
    let side = side_notifies.get();
    diagnostic_error!(
        "EDIT2803_SMALL_NOTE_TYPING_AND_SAVE typing_side={after_type_side:?} save_side={side:?} header_renders={}->{}->{} elapsed_ms={:.3}",
        header_before,
        after_type_header,
        header_after,
        elapsed.as_secs_f64() * 1000.
    );
    assert_eq!(
        after_type_side,
        [0, 0],
        "typing in Live Preview above headings must not invalidate cached SidePanels"
    );
    assert!(
        after_type_header <= header_before + 2,
        "typing in Live Preview must keep Header cached (was {header_before} -> {after_type_header})"
    );
    assert!(
        side[0] <= 3 && side[1] <= 3,
        "clean autosave must avoid cascading SidePanel invalidations (saw {side:?})"
    );
    assert!(f
        .input
        .read_with(v, |i, _| i.value().to_string())
        .contains("\nsaved\n"));
    assert!(
        std::fs::read_to_string(f.directory.path().join("vault/note.md"))
            .unwrap()
            .contains("\nsaved\n"),
        "autosave must persist the typed edit"
    );
}

#[gpui::test]
fn edit2803_popup_frames_reuse_editor_shapes_on_small_notes(cx: &mut TestAppContext) {
    let note = "# Small note\n\n```\n{\"a\": 1}\n```\n\nSome body text.\n".repeat(4);
    let (f, v) = tests::open_file(cx, &note, "note.md");
    f.input.update_in(v, |i, _, _| {
        i.enable_layout_diagnostics();
    });
    tests::settle(v);
    draw(v);
    let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    let started = Instant::now();
    for _ in 0..16 {
        // Same prepaint path triggered on every PopupMenu hover/Down frame.
        f.input.update(v, |_, cx| cx.notify());
        draw(v);
    }
    let elapsed = started.elapsed();
    let after = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    diagnostic_error!(
        "EDIT2803_SMALL_NOTE_POPUP_FRAMES layout_calls={}->{} cache_hits={}->{} elapsed_ms={:.3}",
        before.layout_calls,
        after.layout_calls,
        before.source_shape_cache_hits,
        after.source_shape_cache_hits,
        elapsed.as_secs_f64() * 1000.
    );
    assert!(
        after.source_shape_cache_hits >= before.source_shape_cache_hits + 16,
        "unchanged popup frames on small/wrapped notes must hit shape cache: before={before:?}, after={after:?}"
    );
    assert_eq!(
        after.layout_calls, before.layout_calls,
        "unchanged popup frames must not re-shape visible lines"
    );
    // Positive control: a real edit invalidates the cache.
    f.input
        .update_in(v, |i, w, cx| i.replace_source_range(0..0, "x", w, cx));
    draw(v);
    let edited = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(
        edited.layout_calls > after.layout_calls,
        "a real edit must invalidate the frame shape cache"
    );
}

#[gpui::test]
fn edit2803_4mib_single_line_json_avoids_full_row_materialization(cx: &mut TestAppContext) {
    let mut json = String::with_capacity(4_200_000);
    json.push('[');
    let rec = "{\"id\":1234,\"name\":\"record-0000001\",\"ok\":true},";
    while json.len() + rec.len() + 20 < 4_194_411 {
        json.push_str(rec);
    }
    json.push_str("{\"pad\":\"");
    let pad = 4_194_411usize.saturating_sub(json.len() + 3);
    for _ in 0..pad {
        json.push('x');
    }
    json.push_str("\"}]");
    assert_eq!(json.len(), 4_194_411);

    let (f, v) = tests::open_file(cx, &json, "big.json");
    f.input.update_in(v, |i, _, _| {
        i.enable_layout_diagnostics();
    });
    let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    let started = Instant::now();
    f.input.update_in(v, |i, w, cx| {
        i.replace_source_range(1..1, " ", w, cx);
    });
    draw(v);
    let edit_ms = started.elapsed().as_secs_f64() * 1000.;
    let after = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    let materialized = after
        .source_row_materialized_bytes
        .saturating_sub(before.source_row_materialized_bytes);
    diagnostic_error!(
        "EDIT2803_4MIB_JSON_SINGLE_LINE materialized_bytes={materialized} edit_draw_ms={edit_ms:.3}"
    );
    assert_eq!(
        materialized, 0,
        "4.2 MB monospace ASCII single-line JSON must not materialize the full row (materialized {materialized} bytes)"
    );
    let far = json.len() - 5;
    let _ = geometry(&f, v, 2);
    let _ = geometry(&f, v, far);
    v.dispatch_action(Undo);
    v.run_until_parked();
    draw(v);
    assert_eq!(f.input.read_with(v, |i, _| i.value().len()), 4_194_411);
}
