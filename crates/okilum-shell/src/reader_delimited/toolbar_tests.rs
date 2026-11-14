use super::*;
use ::core::prelude::v1::test;
use gpui_component::{ThemeMode, WindowExt as _};
use reader_code_file::{Availability, CodePreview, View};
use reader_viewer_chrome::tests::{fixture_with_files, measured_bounds};

fn settle(v: &mut VisualTestContext) {
    // Drain the focus-chain registration scheduled by painting the sibling
    // field, as the existing CodePreview header guards do. Never refresh.
    for _ in 0..3 {
        v.run_until_parked();
        v.executor().advance_clock(Duration::from_millis(70));
        v.update(|w, cx| w.draw(cx).clear(cx));
    }
    v.run_until_parked();
}

fn code(reader: &Entity<Reader>, v: &VisualTestContext) -> Entity<CodePreview> {
    reader.read_with(v, |r, _| {
        r.file_preview
            .as_ref()
            .unwrap()
            .code
            .clone()
            .expect("CSV uses CodePreview")
    })
}
fn table(code: &Entity<CodePreview>, v: &VisualTestContext) -> Entity<TablePreview> {
    code.read_with(v, |p, _| p.table_view().unwrap())
}
fn query(table: &Entity<TablePreview>, value: &str, v: &mut VisualTestContext) {
    let target = table.read_with(v, |p, cx| p.find_target(table.clone(), true, cx));
    v.update(|w, cx| target.set_query(value, w, cx));
    settle(v);
}

#[gpui::test]
fn table2812_csv_is_lazy_table_then_readonly_raw_and_real_edit(cx: &mut TestAppContext) {
    let source = "name,value\r\none,1\r\ntwo,2\r\n";
    let (reader, v, temp) = fixture_with_files(cx, "first.csv", &[("first.csv", source)]);
    v.simulate_resize(size(px(1680.), px(900.)));
    settle(v);
    let c = code(&reader, v);
    c.read_with(v, |p, _| {
        assert_eq!(p.availability(View::Table), Availability::Available);
        assert_eq!(p.availability(View::Raw), Availability::Available);
        assert!(p.input().is_none(), "opening Table does not load Raw text");
        assert!(p.editable(), "metadata admits the ordinary source editor");
    });
    assert!(measured_bounds(v, "delimited-viewport").is_some());
    assert!(
        measured_bounds(v, "code-view-table").is_some(),
        "Table segment"
    );
    reader.update_in(v, |r, w, cx| r.toggle_source(w, cx));
    settle(v);
    reader.read_with(v, |r, cx| {
        let input = r
            .editing
            .as_ref()
            .expect("CSV Edit opens source")
            .test_input()
            .read(cx);
        assert_eq!(input.value().as_ref(), source);
        assert!(input.is_editable());
    });
    c.read_with(v, |p, _| {
        assert!(
            p.input().is_none(),
            "Edit from Table does not eagerly load the read-only Raw buffer"
        )
    });
    reader.update_in(v, |r, w, cx| r.toggle_source(w, cx));
    settle(v);
    let c = code(&reader, v);
    // Keyboard round trip from a focused grid: leaving Table must not strand
    // focus on the unmounted grid, or the next view key reaches nothing.
    let view_key = |n: u8| {
        if cfg!(target_os = "macos") {
            format!("ctrl-{n}")
        } else {
            format!("alt-{n}")
        }
    };
    let cell = measured_bounds(v, "delimited-cell-1-0").expect("grid mounted");
    v.simulate_click(cell.center(), Modifiers::default());
    settle(v);
    v.simulate_keystrokes(&view_key(4));
    settle(v);
    c.read_with(v, |p, _| {
        assert_eq!(p.active_view(), View::Raw, "view key reaches Raw")
    });
    v.simulate_keystrokes(&view_key(1));
    settle(v);
    c.read_with(v, |p, _| {
        assert_eq!(
            p.active_view(),
            View::Table,
            "focus survived leaving the grid"
        )
    });
    v.simulate_keystrokes(&view_key(4));
    settle(v);
    c.read_with(v, |p, cx| {
        let input = p.input().unwrap().read(cx);
        assert_eq!(input.value().as_ref(), source);
        assert!(!input.is_editable());
        assert_eq!(p.find_target(c.clone(), cx).scope.as_ref(), "Find");
    });
    assert_eq!(
        std::fs::read(temp.path().join("vault/first.csv")).unwrap(),
        source.as_bytes()
    );
}

#[gpui::test]
fn table2812_matching_cells_case_steps_and_escape_keep_selection(cx: &mut TestAppContext) {
    let (r, v, _temp) =fixture_with_files(cx,"cells.csv",&[("cells.csv","name,a,b,c,d,e\nneedle,0,0,0,0,needle\nNEEDLE,0,0,0,0,0\nneedle,0,0,0,0,0\nother,0,0,0,0,0\n")]);
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    query(&t, "needle", v);
    t.read_with(v, |p, cx| {
        assert_eq!(p.order, [1, 2, 3]);
        assert_eq!(p.hits.len(), 4);
        assert_eq!(
            p.find_target(t.clone(), true, cx).count.unwrap().as_ref(),
            "1 of 4"
        );
    });
    let target = t.read_with(v, |p, cx| p.find_target(t.clone(), true, cx));
    v.update(|w, cx| (target.step.as_ref().unwrap())(1, w, cx));
    settle(v);
    t.read_with(v, |p, _| {
        assert_eq!(p.end, Some(Cell { row: 1, column: 5 }));
        assert!(
            p.horizontal.offset().x < px(0.),
            "next hit scrolls horizontally"
        );
    });
    let cell = measured_bounds(v, "delimited-cell-1-5").expect("matching far cell is mounted");
    let pane = measured_bounds(v, "delimited-viewport").unwrap();
    assert!(
        cell.left() >= pane.left() && cell.right() <= pane.right(),
        "matching cell fully visible"
    );
    v.update(|w, cx| {
        (target.step.as_ref().unwrap())(1, w, cx);
        (target.step.as_ref().unwrap())(-1, w, cx);
    });
    settle(v);
    t.read_with(v, |p, _| {
        assert_eq!(
            p.end,
            Some(Cell { row: 1, column: 5 }),
            "Previous must reverse Next among four hits"
        )
    });
    v.update(|w, cx| (target.match_case.as_ref().unwrap().1)(w, cx));
    settle(v);
    t.read_with(v, |p, _| assert_eq!(p.hits.len(), 3));
    v.update(|w, cx| target.focus(w, cx));
    settle(v);
    v.simulate_keystrokes("escape");
    settle(v);
    t.read_with(v, |p, _| {
        assert!(p.query.is_empty());
        assert_eq!(p.order, [1, 2, 3, 4]);
        assert_eq!(
            p.end,
            Some(Cell { row: 1, column: 0 }),
            "Escape retains the source selection"
        );
    });
}

#[gpui::test]
fn table2812_sort_header_toggle_status_and_copy_toast(cx: &mut TestAppContext) {
    let source = format!(
        "name,amount\n{}",
        (0..1200)
            .map(|i| format!("item{i},{}\n", 1200 - i))
            .collect::<String>()
    );
    let (r, v, temp) = fixture_with_files(cx, "status.csv", &[("status.csv", &source)]);
    v.simulate_resize(size(px(1680.), px(900.)));
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    let reset = t.read_with(v, |p, cx| {
        p.chrome(t.clone(), true, cx)
            .actions
            .into_iter()
            .find(|a| a.id == "table-reset-sort")
            .unwrap()
    });
    assert!(!reset.enabled);
    assert_eq!(reset.disabled_reason.unwrap().as_ref(), "Nothing is sorted");
    let header = measured_bounds(v, "delimited-cell-0-1").unwrap();
    v.simulate_click(header.center(), Modifiers::default());
    settle(v);
    t.read_with(v, |p, cx| {
        let s = p.status(t.clone(), cx);
        assert!(s.text.contains("First 1,000 rows"));
        assert!(s
            .text
            .contains(" KB file · 2 columns · sorted by amount, smallest first"));
        assert_eq!(s.actions.len(), 1);
        assert!(
            p.chrome(t.clone(), true, cx)
                .actions
                .iter()
                .find(|a| a.id == "table-reset-sort")
                .unwrap()
                .enabled
        );
    });
    let cell = measured_bounds(v, "delimited-cell-1-0").unwrap();
    v.simulate_click(cell.center(), Modifiers::default());
    v.simulate_keystrokes("secondary-c");
    settle(v);
    v.update(|w, cx| {
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "item999");
        assert!(!w.notifications(cx).is_empty(), "Copy cells shows feedback");
    });
    // Toasts enter and stack on wall-clock springs (400 ms), not the test
    // clock, and reduced motion snaps them above their resting place. Let the
    // copy toast come to rest, so the click below meets it where a user does
    // on every host, instead of only on hosts fast enough to click before the
    // slide ends (#2840: the resting toast covered Load more rows).
    std::thread::sleep(Duration::from_secs(1));
    let more = measured_bounds(v, "table-load-more").unwrap();
    v.update(|w, cx| {
        assert!(
            !w.notifications(cx).is_empty(),
            "the copy toast is still shown when Load more rows is clicked"
        )
    });
    v.simulate_click(more.center(), Modifiers::default());
    settle(v);
    t.read_with(v, |p, cx| {
        assert_eq!(
            p.order.len(),
            1200,
            "Load more rows takes the click while the copy toast is shown"
        );
        assert!(p.status(t.clone(), cx).actions.is_empty());
    });
    t.update(v, |p, cx| p.reset_sort(cx));
    settle(v);
    t.read_with(v, |p, _| assert_eq!(p.order[0], 1));
    let toggle = measured_bounds(v, "table-first-row-header").unwrap();
    v.simulate_click(toggle.center(), Modifiers::default());
    settle(v);
    t.read_with(v, |p, cx| {
        assert!(!p.header);
        assert!(!reader_ui_state::delimited_header(&p.path, cx));
        assert_eq!(p.order.len(), 1201);
    });
    // The preference is keyed by the path the Reader opened (canonical, e.g.
    // /private/var on macOS), not by a re-joined temporary path.
    let path = t.read_with(v, |p, _| p.path.clone());
    v.update(|_, cx| {
        reader_ui_state::flush(cx);
        reader_ui_state::install(&temp.path().join("state"), cx);
        assert!(!reader_ui_state::delimited_header(&path, cx));
    });
    assert_eq!(
        std::fs::read_to_string(temp.path().join("vault/status.csv")).unwrap(),
        source
    );
}

#[gpui::test]
fn table2812_malformed_badge_reveals_filtered_row_and_failed_table_opens_raw(
    cx: &mut TestAppContext,
) {
    let (r, v, temp) = fixture_with_files(
        cx,
        "bad.csv",
        &[("bad.csv", "name,value\ngood,1\nshort\nlast,3\n")],
    );
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    query(&t, "good", v);
    let badge = t.read_with(v, |p, cx| {
        assert!(p
            .status(t.clone(), cx)
            .text
            .contains("different number of columns"));
        let b = p.chrome(t.clone(), true, cx).state.unwrap();
        assert!(b.enabled);
        b
    });
    v.update(|w, cx| (badge.invoke)(w, cx));
    settle(v);
    t.read_with(v, |p, _| {
        assert!(p.query.is_empty());
        assert_eq!(p.end, Some(Cell { row: 2, column: 0 }));
    });
    let lookahead = format!("name,value\n{}bad\nlast,2\n", "good,1\n".repeat(1000));
    std::fs::write(temp.path().join("vault/lookahead.csv"), lookahead).unwrap();
    r.update_in(v, |r, w, cx| r.preview_file("lookahead.csv", w, cx));
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    let badge = t.read_with(v, |p, cx| {
        assert!(!p.all);
        assert_eq!(p.table.as_ref().unwrap().first_bad_row, Some(1001));
        p.chrome(t.clone(), true, cx).state.unwrap()
    });
    v.update(|w, cx| (badge.invoke)(w, cx));
    settle(v);
    t.read_with(v, |p, _| {
        assert!(
            p.all,
            "badge expands scope when the known bad row is in lookahead"
        );
        assert_eq!(
            p.end,
            Some(Cell {
                row: 1001,
                column: 0
            })
        );
    });
    std::fs::write(
        temp.path().join("vault/not-utf8.csv"),
        [0xff, 0xfe, b',', b'1'],
    )
    .unwrap();
    r.update_in(v, |r, w, cx| r.preview_file("not-utf8.csv", w, cx));
    settle(v);
    let c = code(&r, v);
    c.read_with(v,|p,cx|{
assert!(matches!(p.availability(View::Table),Availability::Disabled(reason) if reason.contains("UTF-8")));
assert_eq!(p.chrome(c.clone(),cx).views.iter().find(|view|view.active).unwrap().action.id,"code-view-raw","failed table falls back to Raw");
});
    assert!(measured_bounds(v, "delimited-viewport").is_none());
    assert!(measured_bounds(v, "code-status-line").is_some());
    assert!(
        measured_bounds(v, "table-unavailable").is_some(),
        "the unreadable table shows its badge by the name"
    );
}

#[gpui::test]
fn table2812_json_has_four_views_and_no_header_toggle(cx: &mut TestAppContext) {
    let (r, v, _temp) = fixture_with_files(
        cx,
        "array.json",
        &[(
            "array.json",
            "[{\"name\":\"needle\",\"n\":1},{\"name\":\"needle\",\"n\":2}]",
        )],
    );
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    c.read_with(v, |p, cx| {
        let chrome = p.chrome(c.clone(), cx);
        assert_eq!(chrome.views.len(), 4);
        assert!(chrome
            .actions
            .iter()
            .all(|a| a.id != "table-first-row-header"));
        assert_eq!(chrome.find.unwrap().scope.as_ref(), "Find in loaded rows");
    });
    query(&t, "needle", v);
    t.read_with(v, |p, _| assert_eq!(p.hits.len(), 2));
}

#[gpui::test]
fn table2812_find_debounces_and_notifies_no_header_or_side_panels(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _cached = reader_side_panel::tests::CachedPanels::on();
    let (r, v, _temp) = fixture_with_files(
        cx,
        "guard.csv",
        &[("guard.csv", "name,value\nneedle,1\nNEEDLE,2\nother,3\n")],
    );
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    r.update_in(v, |r, w, cx| r.open_find(w, cx));
    settle(v);
    v.simulate_keystrokes("home");
    settle(v);
    t.update_in(v, |p, w, cx| {
        assert!(
            p.filter.as_ref().unwrap().focus_handle(cx).is_focused(w),
            "Find is focused before measuring typing"
        )
    });
    struct Watch {
        _subscriptions: Vec<Subscription>,
    }
    let counts = Rc::new(std::cell::Cell::new([0usize; 2]));
    let panels = r.read_with(v, |r, _| r.side_panels.clone());
    let _watch = v.update(|_, cx| {
        cx.new(|cx: &mut Context<Watch>| Watch {
            _subscriptions: panels
                .into_iter()
                .enumerate()
                .map(|(i, p)| {
                    let counts = counts.clone();
                    cx.observe(&p.unwrap(), move |_, _, _| {
                        let mut n = counts.get();
                        n[i] += 1;
                        counts.set(n);
                    })
                })
                .collect(),
        })
    });
    let cached = r.read_with(v, |r, _| {
        r.viewer_header.borrow().as_ref().unwrap().entity_id()
    });
    let before = reader_viewer_chrome::header_renders(r.entity_id());
    let projections = t.read_with(v, |p, _| p.projections);
    for ch in "needle".chars() {
        v.simulate_input(&ch.to_string());
        v.run_until_parked();
    }
    settle(v);
    t.read_with(v, |p, _| {
        assert_eq!(p.hits.len(), 2, "positive: actual query completed");
        assert_eq!(
            p.projections - projections,
            1,
            "one worker for a burst of keys"
        );
    });
    assert_eq!(
        r.read_with(v, |r, _| r
            .viewer_header
            .borrow()
            .as_ref()
            .unwrap()
            .entity_id()),
        cached,
        "Find does not change HeaderKey"
    );
    assert_eq!(
        reader_viewer_chrome::header_renders(r.entity_id()),
        before,
        "Find typing renders zero header slots"
    );
    assert_eq!(counts.get(), [0, 0], "Find notifies zero SidePanels");
}

#[gpui::test]
fn table2812_duckdb_running_menu_and_missing_cli_toast(cx: &mut TestAppContext) {
    let (r, v, temp) = fixture_with_files(cx, "duck.csv", &[("duck.csv", "name,value\nitem,1\n")]);
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    t.update_in(v, |p, w, cx| {
        p.duckdb_override = Some(None);
        p.open_duckdb(w, cx);
    });
    settle(v);
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(5));
        v.executor().advance_clock(Duration::from_millis(100));
        settle(v);
        if !t.read_with(v, |p, _| p.external_pending) {
            break;
        }
    }
    t.read_with(v, |p, _| {
        assert!(!p.external_pending);
        assert_eq!(
            p.external_error.as_deref(),
            Some(controls::MISSING_DUCKDB),
            "frame 7 «DuckDB is not installed» text"
        );
    });
    assert!(
        measured_bounds(v, "table-get-duckdb").is_some(),
        "missing CLI offers an actionable toast"
    );
    t.update(v, |p, cx| {
        p.external_pending = true;
        cx.notify();
    });
    let running = t.read_with(v, |p, cx| {
        p.chrome(t.clone(), true, cx).open_actions.pop().unwrap()
    });
    assert_eq!(running.label.as_ref(), "DuckDB is open");
    assert!(
        !running.enabled && running.toggled,
        "running menu is checked and disabled"
    );
    t.update(v, |p, cx| {
        p.external_pending = false;
        cx.notify();
    });
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = temp.path().join("fake-duckdb");
        let exit = temp.path().join("duckdb-exit");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nwhile [ ! -f '{}' ]; do sleep 0.05; done\n",
                exit.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        t.update_in(v, |p, w, cx| {
            p.duckdb_override = Some(Some(executable));
            p.open_duckdb(w, cx);
        });
        let action = t.read_with(v, |p, cx| {
            p.chrome(t.clone(), true, cx).open_actions.pop().unwrap()
        });
        assert_eq!(action.label.as_ref(), "DuckDB is open");
        assert!(!action.enabled && action.toggled);
        std::fs::write(exit, "exit").unwrap();
        // Wait for the actual reaped process on a worker, not a state stub.
        for _ in 0..30 {
            v.executor().advance_clock(Duration::from_millis(100));
            settle(v);
            if !t.read_with(v, |p, _| p.external_pending) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        t.read_with(v, |p, _| {
            assert!(!p.external_pending);
            assert!(p.external_error.is_none());
        });
    }
}

#[gpui::test]
fn table2812_csv_tsv_memory_is_separate_from_json_array(cx: &mut TestAppContext) {
    let (r, v, _temp) = fixture_with_files(
        cx,
        "one.csv",
        &[
            ("one.csv", "name,n\na,1\n"),
            ("two.csv", "name,n\nb,2\n"),
            ("one.tsv", "name\tn\nc\t3\n"),
            ("array.json", "[{\"n\":1}]"),
        ],
    );
    settle(v);
    let c = code(&r, v);
    c.update_in(v, |p, w, cx| p.set_view(View::Raw, w, cx));
    settle(v);
    for name in ["two.csv", "one.tsv", "array.json"] {
        r.update_in(v, |r, w, cx| r.preview_file(name, w, cx));
        settle(v);
        let c = code(&r, v);
        c.read_with(v, |p, cx| {
            let has_grid = p
                .chrome(c.clone(), cx)
                .views
                .iter()
                .find(|i| i.active)
                .unwrap()
                .action
                .id
                == "code-view-table";
            assert_eq!(
                has_grid,
                name != "two.csv",
                "remembered Raw affects CSV only"
            );
            assert_eq!(
                p.input().is_none(),
                name == "one.tsv",
                "TSV remains lazy; JSON keeps its existing text path"
            );
        });
    }
}

#[gpui::test]
fn table2812_paged_raw_finds_whole_file_and_keeps_bytes(cx: &mut TestAppContext) {
    let source = format!("name,payload\nitem,{}needle\n", "x".repeat(120_000));
    let (r, v, temp) = fixture_with_files(cx, "paged.csv", &[("paged.csv", &source)]);
    settle(v);
    let c = code(&r, v);
    c.update_in(v, |p, w, cx| p.set_view(View::Raw, w, cx));
    settle(v);
    let target = c.read_with(v, |p, cx| p.find_target(c.clone(), cx));
    assert_eq!(target.scope.as_ref(), "Find in whole file");
    v.update(|w, cx| target.set_query("needle", w, cx));
    settle(v);
    c.read_with(v, |p, cx| {
        let input = p.input().unwrap().read(cx);
        let selected = input.selected_range();
        assert_eq!(&input.value().as_ref()[selected], "needle");
        assert_eq!(
            p.find_target(c.clone(), cx).count.unwrap().as_ref(),
            "1 of 1"
        );
    });
    assert_eq!(
        std::fs::read(temp.path().join("vault/paged.csv")).unwrap(),
        source.as_bytes()
    );
}

#[gpui::test]
fn table2812_raw_cap_does_not_eagerly_read_a_large_csv(cx: &mut TestAppContext) {
    let (r, v, temp) = fixture_with_files(cx, "small.csv", &[("small.csv", "a,b\n1,2\n")]);
    settle(v);
    let path = temp.path().join("vault/large.csv");
    std::fs::write(&path, "a,b\n1,2\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(reader_code_file::MAX_BYTES + 1)
        .unwrap();
    r.update_in(v, |r, w, cx| r.preview_file("large.csv", w, cx));
    settle(v);
    let c = code(&r, v);
    c.read_with(v, |p, _| {
        assert!(p.input().is_none());
        assert!(
            p.editable(),
            "CSV Edit keeps the source editor without the code viewer's caps"
        );
        assert_eq!(p.availability(View::Table), Availability::Available);
        assert!(
            matches!(p.availability(View::Raw), Availability::Disabled(reason) if reason.contains("64 MB"))
        );
    });
}

fn sparse_csv(path: &std::path::Path, bytes: u64) {
    std::fs::write(path, "a,b\n1,2\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(bytes)
        .unwrap();
}

#[gpui::test]
fn table2812_remembered_raw_yields_to_table_when_raw_is_over_its_budget(cx: &mut TestAppContext) {
    let (r, v, temp) = fixture_with_files(cx, "small.csv", &[("small.csv", "a,b\n1,2\n")]);
    settle(v);
    let c = code(&r, v);
    c.update_in(v, |p, w, cx| p.set_view(View::Raw, w, cx));
    settle(v);
    v.update(|_, cx| assert_eq!(reader_ui_state::code_view("csv", cx), Some(View::Raw)));
    sparse_csv(
        &temp.path().join("vault/large.csv"),
        reader_code_file::MAX_BYTES + 1,
    );
    r.update_in(v, |r, w, cx| r.preview_file("large.csv", w, cx));
    settle(v);
    let c = code(&r, v);
    c.read_with(v, |p, _| {
        assert_eq!(
            p.active_view(),
            View::Table,
            "Raw is unavailable here, so the bounded table opens"
        );
        assert!(
            matches!(p.availability(View::Raw), Availability::Disabled(reason) if reason.contains("64 MB")),
            "Raw keeps its reason"
        );
    });
    assert!(measured_bounds(v, "delimited-viewport").is_some());
    v.update(|_, cx| {
        assert_eq!(
            reader_ui_state::code_view("csv", cx),
            Some(View::Raw),
            "the remembered per-type choice is not overwritten"
        )
    });
}

#[gpui::test]
fn table2812_five_mib_csv_still_opens_the_source_editor(cx: &mut TestAppContext) {
    let row = "item,1234567890,abcdefghijklmnopqrstuvwxyz\n";
    let source = format!(
        "name,n,text\n{}",
        row.repeat(5 * 1024 * 1024 / row.len() + 1)
    );
    assert!(source.len() > 5 * 1024 * 1024);
    let (r, v, _temp) = fixture_with_files(cx, "big.csv", &[("big.csv", &source)]);
    settle(v);
    let c = code(&r, v);
    c.read_with(v, |p, _| assert!(p.editable(), "no 4 MiB cap for CSV Edit"));
    assert!(
        measured_bounds(v, "code-readonly").is_none(),
        "no size lock by the name"
    );
    r.update_in(v, |r, w, cx| r.toggle_source(w, cx));
    settle(v);
    r.read_with(v, |r, cx| {
        let editing = r.editing.as_ref().expect("Edit opens the source editor");
        assert_eq!(editing.test_input().read(cx).value().len(), source.len());
    });
}

#[gpui::test]
fn table2812_read_only_lock_stays_by_the_name_in_table(cx: &mut TestAppContext) {
    // One item per line: a long single line would open in Tree, not Table.
    let item = "{\"name\":\"needle\",\"value\":12345678901234567890},\n";
    let body = item.repeat(4 * 1024 * 1024 / item.len() + 2);
    let source = format!("[\n{}]", body.trim_end_matches(",\n"));
    let (r, v, _temp) = fixture_with_files(cx, "array.json", &[("array.json", &source)]);
    settle(v);
    let c = code(&r, v);
    c.read_with(v, |p, _| {
        assert_eq!(p.active_view(), View::Table, "positive: Table is active");
        assert!(!p.editable(), "positive: over the JSON editing limit");
    });
    assert!(
        measured_bounds(v, "code-readonly").is_some(),
        "the file's read-only lock stays by the name while Table is active"
    );
}

#[gpui::test]
fn table2812_find_scan_stops_when_table_is_hidden_or_closed(cx: &mut TestAppContext) {
    let (r, v, _temp) = fixture_with_files(
        cx,
        "scan.csv",
        &[
            ("scan.csv", "name,value\nneedle,1\nother,2\n"),
            ("next.csv", "a\n1\n"),
        ],
    );
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    // In flight: the worker has not run yet when the view changes.
    let flag = t.update(v, |p, cx| {
        p.query = "needle".into();
        p.refilter(cx);
        p.projection_cancel.clone()
    });
    c.update_in(v, |p, w, cx| p.set_view(View::Raw, w, cx));
    assert!(
        flag.load(std::sync::atomic::Ordering::Relaxed),
        "hiding Table stops the scan"
    );
    settle(v);
    c.update_in(v, |p, w, cx| p.set_view(View::Table, w, cx));
    settle(v);
    t.read_with(v, |p, _| {
        assert!(!p.updating);
        assert_eq!(p.order, [1], "the latest query reruns when Table returns");
    });
    let flag = t.update(v, |p, cx| {
        p.query = "other".into();
        p.refilter(cx);
        p.projection_cancel.clone()
    });
    drop(t);
    drop(c);
    r.update_in(v, |r, w, cx| r.preview_file("next.csv", w, cx));
    settle(v);
    assert!(
        flag.load(std::sync::atomic::Ordering::Relaxed),
        "closing the file releases the table and stops its scan"
    );
}

#[gpui::test]
fn table2812_narrow_header_and_measured_compact_pane_keep_controls_inside(cx: &mut TestAppContext) {
    let (r, v, temp) = fixture_with_files(
        cx,
        "narrow.csv",
        &[("narrow.csv", "name,value\nneedle,1\n")],
    );
    for (theme, appearance) in [
        (brand::ThemeId::Okilum, ThemeMode::Light),
        (brand::ThemeId::Okilum, ThemeMode::Dark),
        (brand::ThemeId::HighContrast, ThemeMode::Dark),
    ] {
        v.update(|_, cx| {
            reader_ui_state::set_theme(theme, cx);
            reader_ui_state::set_appearance(Some(appearance), cx);
            brand::apply_theme(cx);
        });
        v.simulate_resize(size(px(700.), px(800.)));
        settle(v);
        let header = measured_bounds(v, "document-header").unwrap();
        for id in ["code-view-table", "code-view-raw", "document-more"] {
            let bounds =
                measured_bounds(v, id).unwrap_or_else(|| panic!("required narrow control {id}"));
            assert!(
                bounds.left() >= header.left() && bounds.right() <= header.right(),
                "{id} stays inside the header"
            );
        }
        assert!(measured_bounds(v, "table-first-row-header").is_none());
        assert!(measured_bounds(v, "table-reset-sort").is_none());
        let c = code(&r, v);
        let t = table(&c, v);
        t.update_in(v, |p, w, cx| {
            p.focus_filter(w, cx);
        });
        settle(v);
        let find = measured_bounds(v, "viewer-find-field").unwrap();
        assert!(find.left() >= header.left() && find.right() <= header.right());
        let column = measured_bounds(v, "delimited-row-0").unwrap();
        assert!(
            (column.top() - header.bottom()).abs() <= px(2.),
            "no table row between toolbar and column headings"
        );
        assert!(measured_bounds(v, "table-status-line").is_some());
    }
    struct Pane(Entity<CodePreview>);
    impl Render for Pane {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(v_flex().w(px(420.)).h_full().child(self.0.clone()))
        }
    }
    let (_, v) = cx.add_window_view(|w, cx| {
        let c = cx.new(|cx| {
            CodePreview::new(
                temp.path().join("vault"),
                "narrow.csv".into(),
                temp.path().join("vault/narrow.csv"),
                w,
                cx,
            )
        });
        Root::new(cx.new(|_| Pane(c)), w, cx)
    });
    v.simulate_resize(size(px(1100.), px(800.)));
    settle(v);
    let pane = measured_bounds(v, "viewer-compact-header").unwrap();
    assert_eq!(pane.size.width, px(420.));
    for id in ["code-view-menu", "viewer-find", "code-compact-more"] {
        let b = measured_bounds(v, id).unwrap_or_else(|| panic!("required compact control {id}"));
        assert!(
            b.left() >= pane.left() && b.right() <= pane.right(),
            "compact {id} planned against the pane"
        );
    }
}

#[gpui::test]
fn table2812_edit_stops_table_find_and_read_runs_it_once(cx: &mut TestAppContext) {
    for (name, source) in [
        ("edit.csv", "name,value\nneedle,1\nother,2\n".to_owned()),
        (
            "edit.json",
            "[{\"name\":\"needle\",\"value\":1},{\"name\":\"other\",\"value\":2}]".to_owned(),
        ),
    ] {
        let (r, v, _temp) = fixture_with_files(cx, name, &[(name, &source)]);
        settle(v);
        let c = code(&r, v);
        let t = table(&c, v);
        c.read_with(v, |p, _| assert_eq!(p.active_view(), View::Table, "{name}"));
        let (flag, before) = t.update(v, |p, cx| {
            let before = p.projections;
            p.query = "needle".into();
            p.refilter(cx);
            (p.projection_cancel.clone(), before)
        });
        r.update_in(v, |r, w, cx| r.toggle_source(w, cx));
        assert!(
            flag.load(std::sync::atomic::Ordering::Relaxed),
            "{name}: Edit stops the table scan"
        );
        settle(v);
        assert!(
            r.read_with(v, |r, _| r.editing.is_some()),
            "{name}: editing"
        );
        t.read_with(v, |p, _| {
            assert_eq!(
                p.projections,
                before + 1,
                "{name}: no scan behind the editor"
            )
        });
        r.update_in(v, |r, w, cx| r.toggle_source(w, cx));
        settle(v);
        let shown = table(&code(&r, v), v);
        shown.read_with(v, |p, _| {
            assert!(!p.updating, "{name}");
            assert_eq!(p.query, "needle", "{name}: the query survives Edit");
            assert_eq!(p.order.len(), 1, "{name}: the latest query ran on return");
            if shown == t {
                assert_eq!(p.projections, before + 2, "{name}: resumed exactly once");
            }
        });
    }
}

#[gpui::test]
fn table2812_hidden_load_more_waits_and_runs_the_query_once_on_return(cx: &mut TestAppContext) {
    let source = format!(
        "name,n\n{}",
        (0..1500)
            .map(|i| format!("{}{i},{i}\n", if i % 2 == 0 { "needle" } else { "x" }))
            .collect::<String>()
    );
    let (r, v, _temp) = fixture_with_files(cx, "more.csv", &[("more.csv", &source)]);
    settle(v);
    let c = code(&r, v);
    let t = table(&c, v);
    query(&t, "needle", v);
    let before = t.read_with(v, |p, _| {
        assert!(!p.all);
        assert_eq!(
            p.order.len(),
            500,
            "positive: the first 1,000 rows were searched"
        );
        p.projections
    });
    t.update(v, |p, cx| p.load(true, cx));
    c.update_in(v, |p, w, cx| p.set_view(View::Raw, w, cx));
    settle(v);
    t.read_with(v, |p, _| {
        assert!(p.all, "Load more rows finished while hidden");
        assert_eq!(p.projections, before, "no scan runs while Table is hidden");
    });
    c.update_in(v, |p, w, cx| p.set_view(View::Table, w, cx));
    settle(v);
    t.read_with(v, |p, _| {
        assert_eq!(
            p.projections,
            before + 1,
            "the pending query runs exactly once"
        );
        assert_eq!(p.order.len(), 750, "over all loaded rows");
    });
}
