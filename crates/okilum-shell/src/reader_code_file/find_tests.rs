//! Observable contracts for the shared header Find and semantic Tree rows.
use super::view_tests::{fixture, settle};
use super::*;
use ::core::prelude::v1::test;

fn find(code: &Entity<CodePreview>, query: &str, v: &mut VisualTestContext) {
    let target = code.read_with(v, |s, cx| s.find_target(code.clone(), cx));
    v.update(|w, cx| target.set_query(query, w, cx));
    v.run_until_parked();
    v.executor()
        .advance_clock(std::time::Duration::from_millis(70));
    settle(v);
}

#[test]
fn code2810_semantic_accessors_hidden_root_and_ordered_children() {
    crate::source_presentation::code_languages::install();
    let doc = Document::new("{\"components\":{\"schemas\":{\"title\":\"Account\"}},\"paths\":[1,true,null],\"empty\":{}}".into(), "json");
    assert_eq!(doc.children(0).collect::<Vec<_>>(), [1, 4, 8]);
    assert_eq!(doc.key_text(1), Some("components"));
    assert_eq!(doc.container(2), Some((document::ContainerKind::Object, 1)));
    assert_eq!(doc.parent(3), Some(2));
    assert_eq!(doc.array_ordinal(5), Some(0));
    assert_eq!(
        doc.scalar(3),
        Some((document::ScalarKind::String, "\"Account\"".into()))
    );
    assert_eq!(doc.scalar(6).unwrap().0, document::ScalarKind::Boolean);
    assert_eq!(doc.scalar(7).unwrap().0, document::ScalarKind::Null);
    assert_eq!(doc.row_label(1), "components 1 key");
    assert_eq!(doc.row_label(3), "title \"Account\"");
    assert_eq!(doc.row_label(4), "paths 3 items");
    assert_eq!(doc.row_label(8), "empty { }");
    let mut tree = structure::Structure::default();
    tree.reset(&doc);
    assert_eq!(tree.visible, [1, 4, 8]);
}

#[test]
fn code2810_sorted_lookup_matches_linear_reference_on_nested_random_ranges() {
    crate::source_presentation::code_languages::install();
    let text = format!(
        "{{\"rows\":[{}],\"tail\":42}}",
        (0..2000)
            .map(|i| format!("{{\"key{i}\":{{\"n\":{i}}}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let doc = Document::new(text, "json");
    let mut seed = 76543_u64;
    for _ in 0..10_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let start = seed as usize % doc.text.len();
        let end = (start + (seed >> 32) as usize % 50).min(doc.text.len());
        let expected = doc
            .structure
            .iter()
            .rposition(|r| r.range.start <= start && r.range.end >= end);
        assert_eq!(doc.row_at(&(start..end)), expected);
    }
}

#[test]
fn code2810_scrollbar_marks_are_evenly_sampled_and_capped() {
    crate::source_presentation::code_languages::install();
    let text = format!(
        "[{}]",
        (0..5000)
            .map(|_| "\"target\"")
            .collect::<Vec<_>>()
            .join(",")
    );
    let doc = Document::new(text, "json");
    let hits = doc.find("target");
    let marks = super::find::sample_marks(&hits, &doc);
    assert_eq!(hits.len(), 5000);
    assert_eq!(marks.len(), 1000);
    assert_eq!(marks.first(), Some(&1));
    assert_eq!(marks.last(), Some(&5000));
    assert!(marks.windows(2).all(|p| (4..=6).contains(&(p[1] - p[0]))));
}

#[gpui::test]
fn code2810_tree_find_case_step_escape_and_fold_back(cx: &mut TestAppContext) {
    let text = "{\"a\":{\"inner\":{\"value\":\"needle\"}},\"b\":{\"value\":\"NEEDLE\"},\"c\":{\"value\":\"needle\"}}";
    let (code, v, _temp) = fixture(cx, "data.json", text);
    code.update_in(v, |s, w, cx| {
        s.set_view(View::Tree, w, cx);
        s.open_find(w, cx);
    });
    settle(v);
    find(&code, "needle", v);
    code.update_in(v, |s, w, cx| {
        assert!(
            s.find.focus_handle(cx).is_focused(w),
            "shared field retains focus after query"
        )
    });
    code.read_with(v, |s, _| {
        assert_eq!(s.matches.len(), 3);
        assert_eq!(s.structure.selected, Some(3));
    });
    v.simulate_keystrokes("enter");
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.match_index, 1));
    v.simulate_keystrokes("enter");
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.match_index, 2));
    v.simulate_keystrokes("shift-enter");
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.match_index, 1));
    let case = v
        .debug_bounds("find-case-sensitive")
        .expect("real case button");
    v.simulate_click(case.center(), Modifiers::default());
    settle(v);
    code.read_with(v, |s, _| {
        assert_eq!(s.matches.len(), 2);
        assert!(s.case_sensitive.get());
    });
    v.simulate_keystrokes("enter");
    settle(v);
    let selected = code.read_with(v, |s, _| s.structure.selected);
    v.simulate_keystrokes("escape");
    settle(v);
    code.read_with(v, |s, cx| {
        assert!(s.find.read(cx).value().is_empty());
        assert!(s.matches.is_empty());
        assert!(s.scrollbar_marks.is_empty());
        assert!(s.structure.match_node.is_none());
        assert_eq!(s.structure.selected, selected);
        assert!(s.structure.visible.contains(&selected.unwrap()));
        assert!(s.structure.folded(1), "Find-only path a folded back");
        assert!(!s.structure.folded(6), "current c path remains open");
    });
    assert_eq!(
        std::fs::read_to_string(_temp.path().join("data.json")).unwrap(),
        text
    );
}

#[gpui::test]
fn code2810_case_sensitive_whole_file_hit_crosses_part_boundary(cx: &mut TestAppContext) {
    let text = format!("{}Needle{}needle", "x".repeat(32765), "x".repeat(70_000));
    let (code, v, _temp) = fixture(cx, "data.txt", &text);
    code.update_in(v, |s, w, cx| s.open_find(w, cx));
    find(&code, "Needle", v);
    code.read_with(v, |s, _| {
        assert_eq!(s.matches.len(), 2);
        let doc = s.document.as_ref().unwrap();
        assert!(s.matches[0].start < doc.pages[0].end && s.matches[0].end > doc.pages[0].end);
    });
    let target = code.read_with(v, |s, cx| s.find_target(code.clone(), cx));
    v.update(|w, cx| (target.match_case.unwrap().1)(w, cx));
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.matches.len(), 1));
    code.update_in(v, |s, w, cx| {
        s.show_match(w, cx);
        assert_eq!(s.page, 0);
    });
}

#[gpui::test]
fn code2810_every_read_format_uses_header_field_and_unfolds_first_hit(cx: &mut TestAppContext) {
    for (name, text) in [
        ("code.rs", "fn first() {\n    target();\n}\n"),
        ("data.yaml", "root:\n  child: target\n"),
        ("data.toml", "[root]\nchild = \"target\"\n"),
        ("data.xml", "<root>\n  <child>target</child>\n</root>\n"),
        ("plain.txt", "one target two target three TARGET"),
        ("data.json", "{\"root\":{\"value\":\"target\"}}"),
    ] {
        let (code, v, _temp) = fixture(cx, name, text);
        let input = code.read_with(v, |s, _| s.input().unwrap());
        input.update(v, |s, cx| s.set_all_folded(true, cx));
        code.update_in(v, |s, w, cx| s.open_find(w, cx));
        find(&code, "target", v);
        assert!(
            v.debug_bounds("viewer-find-field").is_some(),
            "{name}: shared field"
        );
        assert!(
            v.debug_bounds("search-panel").is_none(),
            "{name}: no toolkit strip"
        );
        code.read_with(v, |s, cx| {
            assert_eq!(
                s.find.read(cx).presentation().placeholder().as_ref(),
                "Find"
            )
        });
        input.read_with(v, |s, _| {
            assert!(!s.search_session().open);
            assert!(s.search_session().external_presentation);
            assert!(!s.search_session().matcher.matched_ranges().is_empty());
            assert_eq!(&s.value()[s.selected_range()], "target");
        });
        v.simulate_keystrokes("escape");
        settle(v);
        input.read_with(v, |s, _| {
            assert!(s.search_session().matcher.matched_ranges().is_empty());
            assert_eq!(&s.value()[s.selected_range()], "target");
        });
    }
}

#[gpui::test]
fn code2810_tree_keyboard_rows_context_menu_hover_and_raw_jump(cx: &mut TestAppContext) {
    let source = format!(
        "{{\"components\":{{\"long\":\"{}\",\"leaf\":42}},\"tail\":true}}",
        "x".repeat(100_000)
    );
    let (code, v, _temp) = fixture(cx, "data.json", &source);
    code.update_in(v, |s, w, cx| {
        s.set_view(View::Tree, w, cx);
        s.focus.focus(w, cx);
    });
    settle(v);
    assert!(v.debug_bounds("data-structure-row-0").is_none());
    let label = v.debug_bounds("data-node-label-1").unwrap();
    assert!(
        v.debug_bounds("data-node-copy-1").is_some(),
        "selected action is painted"
    );
    v.simulate_keystrokes("down");
    settle(v);
    assert!(
        v.debug_bounds("data-node-copy-1").is_none(),
        "unselected actions must not paint transparent widgets"
    );
    v.simulate_mouse_move(label.center(), None, Modifiers::default());
    settle(v);
    assert!(
        v.debug_bounds("data-node-copy-1").is_some(),
        "hover paints the reserved action slot"
    );
    v.simulate_mouse_move(point(px(1.), px(1.)), None, Modifiers::default());
    settle(v);
    assert!(v.debug_bounds("data-node-copy-1").is_none());
    assert_eq!(
        v.debug_bounds("data-node-label-1").unwrap(),
        label,
        "unselected row keeps its action slot"
    );
    v.simulate_keystrokes("up");
    settle(v);
    v.simulate_keystrokes("right");
    settle(v);
    code.read_with(v, |s, _| assert!(!s.structure.folded(1)));
    v.simulate_keystrokes("right");
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.structure.selected, Some(2)));
    v.simulate_keystrokes("down");
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.structure.selected, Some(3)));
    v.simulate_keystrokes("up");
    v.simulate_keystrokes("left");
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.structure.selected, Some(1)));
    v.simulate_keystrokes("left");
    settle(v);
    code.read_with(v, |s, _| assert!(s.structure.folded(1)));
    assert_eq!(
        v.debug_bounds("data-node-label-1").unwrap(),
        label,
        "fixed action slot prevents text shift"
    );
    v.simulate_keystrokes("down");
    settle(v);
    v.simulate_keystrokes("ctrl-shift-c");
    v.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "\"tail\":true"
        )
    });
    v.simulate_keystrokes("enter");
    settle(v);
    code.read_with(v, |s, cx| {
        assert_eq!(s.view, View::Raw);
        assert!(s.page > 0);
        assert_eq!(
            &s.input().unwrap().read(cx).value()[s.input().unwrap().read(cx).selected_range()],
            "\"tail\":true"
        );
    });
}

#[gpui::test]
fn code2810_tree_only_builds_actions_for_the_hovered_or_selected_row(cx: &mut TestAppContext) {
    let rows = (0..414)
        .map(|i| format!("\"route-{i:04}\":{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let (code, v, _temp) = fixture(
        cx,
        "data.json",
        &format!("{{\"paths\":{{{rows}}},\"tail\":false}}"),
    );
    code.update_in(v, |s, w, cx| {
        s.set_view(View::Tree, w, cx);
        s.structure.toggle(1, s.document.as_ref().unwrap());
        // The first measurement row and the visible range are both unselected.
        s.structure.selected = Some(416);
        s.structure
            .scroll
            .scroll_to_item_strict(180, ScrollStrategy::Top);
        cx.notify();
    });
    v.simulate_mouse_move(point(px(1.), px(1.)), None, Modifiers::default());
    settle(v);
    let label = v
        .debug_bounds("data-node-label-181")
        .expect("positive control: scrolled rows mounted");
    let before = structure::action_builds();
    v.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear(cx);
    });
    assert_eq!(
        structure::action_builds(),
        before,
        "unselected rows must not construct hidden action controls"
    );
    assert!(v.debug_bounds("data-node-copy-181").is_none());
    v.simulate_mouse_move(label.center(), None, Modifiers::default());
    settle(v);
    let button = v
        .debug_bounds("data-node-copy-181")
        .expect("hover mounts the action controls");
    assert!(
        structure::action_builds() > before,
        "positive control: visible actions really built"
    );
    assert_eq!(v.debug_bounds("data-node-label-181").unwrap(), label);
    let expected = code.read_with(v, |s, _| {
        let doc = s.document.as_ref().unwrap();
        doc.text[doc.structure[181].range.clone()].to_owned()
    });
    v.simulate_click(button.center(), Modifiers::default());
    settle(v);
    v.update(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), expected));
}

#[gpui::test]
fn code2810_header_typing_renders_zero_slots_and_notifies_zero_panels(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _cached = reader_side_panel::tests::CachedPanels::on();
    let (reader, v, _temp) = reader_viewer_chrome::tests::fixture_with_files(
        cx,
        "guard.json",
        &[(
            "guard.json",
            "{\"a\":{\"value\":\"target\"},\"b\":\"TARGET\"}",
        )],
    );
    settle(v);
    let code = reader.read_with(v, |r, _| {
        r.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    struct Watch {
        _subs: Vec<Subscription>,
    }
    let counts = std::rc::Rc::new(std::cell::Cell::new([0; 2]));
    let panels = reader.read_with(v, |r, _| r.side_panels.clone());
    let _watch = v.update(|_, cx| {
        cx.new(|cx: &mut Context<Watch>| Watch {
            _subs: panels
                .into_iter()
                .enumerate()
                .map(|(i, p)| {
                    let counts = counts.clone();
                    cx.observe(&p.unwrap(), move |_, _, _| {
                        let mut c = counts.get();
                        c[i] += 1;
                        counts.set(c);
                    })
                })
                .collect(),
        })
    });
    for view in [View::Tree, View::Formatted, View::Raw] {
        code.update_in(v, |s, w, cx| {
            reader_ui_state::remember_find_case_sensitive(false, cx);
            s.set_view(view, w, cx);
            s.open_find(w, cx);
        });
        settle(v);
        let target = reader.read_with(v, |r, cx| r.viewer_chrome(cx).find.unwrap());
        v.update(|w, cx| target.set_query("", w, cx));
        settle(v);
        v.simulate_keystrokes("home");
        settle(v);
        v.update(|w, cx| {
            assert!(
                target.input.focus_handle(cx).is_focused(w),
                "{view:?}: field focused before typing"
            )
        });
        let before = reader_viewer_chrome::header_renders(reader.entity_id());
        let cached_header = reader.read_with(v, |r, _| {
            r.viewer_header.borrow().as_ref().unwrap().entity_id()
        });
        assert!(before > 0);
        counts.set([0, 0]);
        for ch in "target".chars() {
            v.simulate_input(&ch.to_string());
            v.run_until_parked();
            v.executor()
                .advance_clock(std::time::Duration::from_millis(70));
            settle(v);
            v.update(|w, cx| {
                assert!(
                    target.input.focus_handle(cx).is_focused(w),
                    "{view:?}: Find keeps focus through worker completion"
                )
            });
            let current_header = reader.read_with(v, |r, _| {
                r.viewer_header.borrow().as_ref().unwrap().entity_id()
            });
            assert_eq!(
                current_header, cached_header,
                "{view:?}: typing does not change HeaderKey"
            );
            assert_eq!(
                reader_viewer_chrome::header_renders(reader.entity_id()),
                before,
                "{view:?}: no slot renders on Find typing"
            );
            assert_eq!(counts.get(), [0, 0]);
        }
        code.read_with(v, |s, cx| {
            let t = s.find_target(code.clone(), cx);
            assert_eq!(
                t.count.unwrap().as_ref(),
                "1 of 2",
                "positive control: actual query completed"
            );
        });
        let target = code.read_with(v, |s, cx| s.find_target(code.clone(), cx));
        v.update(|w, cx| (target.match_case.unwrap().1)(w, cx));
        v.run_until_parked();
        v.executor()
            .advance_clock(std::time::Duration::from_millis(70));
        settle(v);
        assert_eq!(
            reader_viewer_chrome::header_renders(reader.entity_id()),
            before,
            "case click does not render slots"
        );
        assert_eq!(counts.get(), [0, 0]);
        code.read_with(v, |s, cx| {
            assert_eq!(
                s.find_target(code.clone(), cx).count.unwrap().as_ref(),
                "1 of 1"
            )
        });
    }
}

#[gpui::test]
fn code2810_editor_find_debounces_mutations_and_refreshes_source(cx: &mut TestAppContext) {
    let (code, v, _temp) = fixture(cx, "code.rs", "needle");
    let input = code.read_with(v, |s, _| s.input().unwrap());
    let adapter = code.read_with(v, |s, _| s.editor_find.clone().unwrap());
    adapter.update_in(v, |s, w, cx| s.set_query("needle", w, cx));
    assert_eq!(
        adapter.read_with(v, |s, cx| s.count(cx).1),
        0,
        "no matching on query callback"
    );
    v.run_until_parked();
    v.executor()
        .advance_clock(std::time::Duration::from_millis(70));
    settle(v);
    assert_eq!(adapter.read_with(v, |s, cx| s.count(cx).1), 1);
    input.update_in(v, |s, w, cx| {
        s.set_readonly(false, cx);
        s.replace_text_in_range(Some(6..6), " needle", w, cx);
    });
    // A real SourceMutation refreshes the adapter, but never computes the new
    // two-hit result inside that event (the editor may retire stale marks).
    assert!(
        adapter.read_with(v, |s, cx| s.count(cx).1) < 2,
        "source edit callback never scans"
    );
    v.run_until_parked();
    v.executor()
        .advance_clock(std::time::Duration::from_millis(70));
    settle(v);
    assert_eq!(
        adapter.read_with(v, |s, cx| s.count(cx).1),
        2,
        "worker refreshed the actual mutated source"
    );
    input.update_in(v, |s, w, cx| s.set_value("different text", w, cx));
    v.run_until_parked();
    v.executor()
        .advance_clock(std::time::Duration::from_millis(70));
    settle(v);
    assert_eq!(
        adapter.read_with(v, |s, cx| s.count(cx).1),
        0,
        "a completed refresh reflects the replacement source"
    );
    input.read_with(v, |s, _| assert_eq!(s.value().as_ref(), "different text"));
}

#[gpui::test]
fn code2810_header_find_keeps_switch_buttons_content_and_navigation_bounds(
    cx: &mut TestAppContext,
) {
    use reader_viewer_chrome::tests::{fixture_with_files, measured_bounds};
    let source = format!(
        "{{\"components\":{{\"description\":\"{}\"}},\"paths\":{{}}}}",
        "x".repeat(100_000)
    );
    let (reader, v, _temp) = fixture_with_files(cx, "file.json", &[("file.json", &source)]);
    for width in [1440., 700., 600.] {
        v.simulate_resize(size(px(width), px(800.)));
        settle(v);
        let header = measured_bounds(v, "document-header").unwrap();
        let ids = [
            "code-view-tree",
            "code-view-formatted",
            "code-view-raw",
            "document-more",
            "reader-code-file",
        ];
        let before = ids.map(|id| {
            v.debug_bounds(id)
                .unwrap_or_else(|| panic!("required {id}"))
        });
        reader.update_in(v, |r, w, cx| r.open_find(w, cx));
        settle(v);
        let field = measured_bounds(v, "viewer-find-field").unwrap();
        assert!(field.left() >= header.left() && field.right() <= header.right());
        for (id, before) in ids.into_iter().zip(before) {
            assert_eq!(
                v.debug_bounds(id).unwrap(),
                before,
                "{id} moves on Find focus at {width}"
            );
        }
        for id in ["reader-history-back", "reader-history-forward"] {
            let nav = v.debug_bounds(id).expect("navigation remains mounted");
            assert!(
                nav.right() <= field.left() || nav.left() >= field.right(),
                "Find covers navigation at {width}"
            );
        }
        v.simulate_keystrokes("escape");
        settle(v);
    }
}

#[gpui::test]
fn code2810_tree_context_menu_has_live_actions_and_selected_node(cx: &mut TestAppContext) {
    let (code, v, _temp) = fixture(cx, "data.json", "{\"first\":1,\"second\":2}");
    code.update_in(v, |s, w, cx| s.set_view(View::Tree, w, cx));
    settle(v);
    let row = v.debug_bounds("data-structure-row-2").unwrap();
    v.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::default());
    v.simulate_mouse_up(row.center(), MouseButton::Right, Modifiers::default());
    settle(v);
    code.read_with(v, |s, _| assert_eq!(s.structure.selected, Some(2)));
    v.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear(cx);
        assert!(
            !gpui_base::GlobalState::native_overlays(w, cx).is_empty(),
            "actual Tree context menu mounts"
        );
        structure::TREE_MENU.with(|record| {
            let record = record.borrow();
            let (menu, items) = record.as_ref().expect("real row menu built");
            let menu = menu.upgrade().expect("mounted menu alive");
            assert!(menu.read(cx).focus_handle(cx).is_focused(w));
            assert_eq!(items.len(), 4, "all context menu actions are mounted");
            for (item, (name, action)) in items.iter().zip([
                ("Copy node", Box::new(CopyNode) as Box<dyn Action>),
                ("Show in raw text", Box::new(ShowNodeRaw)),
                ("Fold all", Box::new(FoldAll)),
                ("Unfold all", Box::new(UnfoldAll)),
            ]) {
                let (label, actual) = item;
                assert_eq!(label.as_ref(), name);
                assert!(actual.partial_eq(action.as_ref()));
                assert!(
                    reader_shortcuts::shortcut(action.as_ref(), cx).is_some(),
                    "live binding for {name}"
                );
            }
        });
    });
    v.simulate_keystrokes("down enter");
    settle(v);
    v.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "\"second\":2",
            "context menu copies the right-clicked node"
        )
    });
}

#[gpui::test]
fn code2810_bare_host_shares_case_between_whole_file_and_editor_adapters(cx: &mut TestAppContext) {
    let (code, v, _temp) = fixture(
        cx,
        "data.json",
        "{\"first\":\"needle\",\"second\":\"NEEDLE\"}",
    );
    v.update(|_, cx| {
        assert!(
            !reader_ui_state::installed(cx),
            "bare compact host has no preference store"
        )
    });
    code.update_in(v, |s, w, cx| {
        s.set_view(View::Tree, w, cx);
        s.open_find(w, cx);
    });
    find(&code, "needle", v);
    let target = code.read_with(v, |s, cx| s.find_target(code.clone(), cx));
    v.update(|w, cx| (target.match_case.unwrap().1)(w, cx));
    settle(v);
    code.read_with(v, |s, cx| {
        assert_eq!(
            s.find_target(code.clone(), cx).count.unwrap().as_ref(),
            "1 of 1"
        )
    });
    for view in [View::Formatted, View::Raw, View::Tree] {
        code.update_in(v, |s, w, cx| s.set_view(view, w, cx));
        settle(v);
        code.read_with(v, |s, cx| {
            let target = s.find_target(code.clone(), cx);
            assert!(
                target.match_case.as_ref().unwrap().0,
                "{view:?}: same field retains Match case"
            );
            assert_eq!(target.count.unwrap().as_ref(), "1 of 1");
        });
    }
    // Exercise the reverse handoff too: a bare host has no persisted state
    // through which independently allocated adapter cells could synchronize.
    code.update_in(v, |s, w, cx| s.set_view(View::Formatted, w, cx));
    settle(v);
    let target = code.read_with(v, |s, cx| s.find_target(code.clone(), cx));
    v.update(|w, cx| (target.match_case.unwrap().1)(w, cx));
    settle(v);
    code.update_in(v, |s, w, cx| s.set_view(View::Tree, w, cx));
    settle(v);
    code.read_with(v, |s, cx| {
        let target = s.find_target(code.clone(), cx);
        assert!(!target.match_case.as_ref().unwrap().0);
        assert_eq!(target.count.unwrap().as_ref(), "1 of 2");
    });
}

#[gpui::test]
fn code2810_tree_keys_paint_with_the_mock_regular_code_face(cx: &mut TestAppContext) {
    let (code, v, _temp) = fixture(cx, "data.json", "{\"paths\":{\"route\":42},\"tail\":true}");
    code.update_in(v, |s, w, cx| s.set_view(View::Tree, w, cx));
    settle(v);
    structure::LABEL_WEIGHTS.with(|weights| weights.borrow_mut().clear());
    v.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear(cx);
    });
    assert!(v.debug_bounds("data-node-label-1").is_some());
    structure::LABEL_WEIGHTS.with(|weights| {
        let weights = weights.borrow();
        assert!(
            !weights.is_empty(),
            "positive control: label paint style was captured"
        );
        assert!(
            weights.iter().all(|weight| *weight == FontWeight::NORMAL),
            "Tree keys must use the mock's regular code face"
        );
    });
}

#[gpui::test]
fn code2810_tree_prepares_numeric_glyph_phases_once_before_scrolling(cx: &mut TestAppContext) {
    use structure::digit_glyphs::REQUESTS;
    REQUESTS.with(|requests| requests.borrow_mut().clear());
    let text = format!(
        "{{\"paths\":{{{}}}}}",
        (0..414)
            .map(|i| format!("\"/v1/path-{i:04}\":{{}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let (code, v, temp) = fixture(cx, "data.json", &text);
    v.update(|_, cx| reader_ui_state::install(&temp.path().join("state"), cx));
    code.update_in(v, |s, w, cx| {
        s.set_view(View::Tree, w, cx);
        s.structure.toggle(1, s.document.as_ref().unwrap());
        cx.notify();
    });
    settle(v);
    let phases = usize::from(SUBPIXEL_VARIANTS_X) * usize::from(SUBPIXEL_VARIANTS_Y);
    let initial = REQUESTS.with(|requests| {
        let requests = requests.borrow();
        assert_eq!(
            requests.len(),
            structure::digit_glyphs::LABEL_GLYPHS.chars().count()
                * phases
                * requests
                    .iter()
                    .map(|r| r.4)
                    .fold(Vec::<Hsla>::new(), |mut colors, color| {
                        if !colors.contains(&color) {
                            colors.push(color);
                        }
                        colors
                    })
                    .len(),
            "all printable label phases and colours prepared before scroll"
        );
        let glyphs: std::collections::HashSet<_> = requests.iter().map(|r| r.0).collect();
        assert_eq!(
            glyphs.len(),
            structure::digit_glyphs::LABEL_GLYPHS.chars().count(),
            "positive control: all source glyphs are prepared (TestPlatform aliases glyph IDs)"
        );
        for glyph in ['…', '·', ',', '.', '-'] {
            assert!(
                glyphs.contains(&glyph),
                "generated labels must prepare {glyph}"
            );
        }
        for glyph in glyphs {
            let variants: std::collections::HashSet<_> = requests
                .iter()
                .filter(|r| r.0 == glyph)
                .map(|r| (r.1, r.2))
                .collect();
            assert_eq!(
                variants.len(),
                phases,
                "each printable label glyph has every subpixel phase"
            );
        }
        requests.len()
    });
    v.update(|_, cx| {
        let faint = brand::reader_palette(cx).text_faint;
        let number = cx
            .theme()
            .highlight_theme
            .style
            .syntax
            .style("number")
            .and_then(|style| style.color)
            .unwrap();
        REQUESTS.with(|requests| {
            for color in [faint, number] {
                for glyph in structure::digit_glyphs::LABEL_GLYPHS.chars() {
                    assert_eq!(
                        requests
                            .borrow()
                            .iter()
                            .filter(|r| r.0 == glyph && r.4 == color)
                            .count(),
                        phases,
                        "all actual Tree colours include every glyph phase"
                    );
                }
            }
        });
    });
    let label = v.debug_bounds("data-node-label-2").unwrap();
    let row = v.debug_bounds("data-structure-row-2").unwrap();
    assert_eq!(row.size.height, px(32.));
    assert!(label.top() >= row.top() && label.bottom() <= row.bottom());
    for sample in 0..48 {
        code.update_in(v, |s, _, cx| {
            s.structure
                .scroll
                .scroll_to_item_strict(1 + (sample * 7) % 390, ScrollStrategy::Top);
            cx.notify();
        });
        settle(v);
        REQUESTS.with(|requests| {
            assert_eq!(
                requests.borrow().len(),
                initial,
                "scrolling must not prepare glyphs again"
            )
        });
    }
    v.update(|_, cx| reader_ui_state::set_text_zoom(1.25, cx));
    settle(v);
    REQUESTS.with(|requests| {
        let requests = requests.borrow();
        assert_eq!(
            requests.len(),
            initial * 2,
            "a new text size needs its own glyph phases"
        );
        assert_ne!(requests[0].3, requests[initial].3);
    });
    code.update_in(v, |_, _, cx| cx.notify());
    settle(v);
    REQUESTS.with(|requests| assert_eq!(requests.borrow().len(), initial * 2));
}
