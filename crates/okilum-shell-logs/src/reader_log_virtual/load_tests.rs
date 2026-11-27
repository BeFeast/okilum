use super::*;
use ::core::prelude::v1::test;
use gpui::TestAppContext;

#[gpui::test]
async fn failed_definition_load_retains_draft_save_lease_and_results_then_valid_reload_replaces_target(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(vault.join("logs")).unwrap();
    std::fs::write(vault.join("logs/a.log"), "first\n").unwrap();
    let original = vault.join("original.okilog");
    let original_bytes = r#"{"version":1,"name":"Original","vault_root":true,"glob":"logs/*.log"}"#;
    std::fs::write(&original, original_bytes).unwrap();
    let broken = vault.join("broken.okilog");
    std::fs::write(&broken, "{invalid definition").unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            let mut panel = Panel::new(vault.clone(), window, cx);
            panel.state = Some(state.clone());
            let mut loaded = model::load(&vault, &original).unwrap();
            let selection = loaded
                .definition
                .plan(&vault, &original, 0)
                .unwrap()
                .select(|_, _| {});
            loaded.session = Some(save::Session::open(&loaded, &state).unwrap());
            panel.configure(loaded, window, cx);
            panel.pending = Some(Pending {
                files: selection.files,
                elapsed: Duration::ZERO,
            });
            panel.watch = false;
            panel.name.update(cx, |input, cx| {
                input.set_value("Unsaved criteria", window, cx)
            });
            panel
        });
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1250.), px(950.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let view_id = panel.update(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap();
        view.update(cx, |view, _| {
            assert_eq!(
                view.total(),
                1,
                "positive control: prepared result is visible"
            );
            view.selected = Some(0);
            view.anchor = Some(0);
            view.marks.insert((0, 0));
        });
        view.entity_id()
    });

    for (index, candidate) in [vault.join("missing.okilog"), broken.clone()]
        .into_iter()
        .enumerate()
    {
        panel.update_in(visual, |panel, window, cx| {
            panel.path.update(cx, |input, cx| {
                input.set_value(candidate.to_string_lossy().into_owned(), window, cx)
            });
            if index == 0 {
                panel.load_definition(candidate.clone(), cx);
                let generation = panel.load_generation;
                assert!(panel.loading);
                // A queued second Load cannot drop the task holding the old
                // editor lease or replace the first request's rollback state.
                panel.load_definition(broken.clone(), cx);
                assert_eq!(panel.load_generation, generation);
            }
        });
        visual.run_until_parked();
        if index == 1 {
            let load = visual.debug_bounds("virtual-load").unwrap();
            visual.simulate_click(load.center(), Modifiers::default());
        }
        visual
            .condition(&panel, |panel, _| {
                !panel.loading || panel.pending_load.is_some()
            })
            .await;
        visual.run_until_parked();
        panel.read_with(visual, |panel, cx| {
            assert!(panel.error);
            assert!(panel.message.contains("Cannot load definition"));
            assert!(panel.message.contains("Still using"));
            assert_eq!(panel.loaded.as_ref(), Some(&original));
            assert!(
                panel.session.is_some(),
                "failed replacement returns the original Save lease"
            );
            assert!(panel.save_error.is_none());
            assert_eq!(panel.definition(cx).unwrap().name, "Unsaved criteria");
            assert!(panel.worker.is_none() && !panel.active);
            let view = panel.view.as_ref().unwrap();
            assert_eq!(view.entity_id(), view_id);
            let view = view.read(cx);
            assert_eq!(view.total(), 1);
            assert_eq!(view.selected, Some(0));
            assert!(view.marks.contains(&(0, 0)) && view.source_locked);
            assert_eq!(view.source(0).unwrap().raw(0).unwrap(), b"first");
        });
        assert_eq!(std::fs::read_to_string(&original).unwrap(), original_bytes);
        assert_eq!(
            std::fs::read_to_string(&broken).unwrap(),
            "{invalid definition"
        );
    }

    // The retained session still saves to the original document, never to the
    // failed candidate left in the path input. No implicit rescan is permitted.
    let save = visual.debug_bounds("virtual-save").unwrap();
    visual.simulate_click(save.center(), Modifiers::default());
    visual.condition(&panel, |panel, _| !panel.saving).await;
    assert_eq!(
        model::load(&vault, &original).unwrap().definition.name,
        "Unsaved criteria"
    );
    assert_eq!(
        std::fs::read_to_string(&broken).unwrap(),
        "{invalid definition"
    );
    panel.read_with(visual, |panel, _| {
        assert!(!panel.error && panel.worker.is_none());
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), view_id);
    });

    // Positive control for same-file reload: the old editor lock must be
    // released only after successful parsing, before acquiring the new lease.
    panel.update(visual, |panel, cx| {
        panel.load_definition(original.clone(), cx)
    });
    visual
        .condition(&panel, |panel, _| {
            !panel.loading || panel.pending_load.is_some()
        })
        .await;
    visual.run_until_parked();
    panel.read_with(visual, |panel, _| {
        assert!(!panel.error && panel.save_error.is_none());
        assert!(panel.session.is_some());
        assert_eq!(panel.loaded.as_ref(), Some(&original));
    });

    let replacement = vault.join("replacement.okilog");
    std::fs::write(
        &replacement,
        r#"{"version":1,"name":"Replacement","vault_root":true,"glob":"logs/a.log"}"#,
    )
    .unwrap();
    panel.update_in(visual, |panel, window, cx| {
        panel.path.update(cx, |input, cx| {
            input.set_value(replacement.to_string_lossy().into_owned(), window, cx)
        });
    });
    visual.run_until_parked();
    let load = visual.debug_bounds("virtual-load").unwrap();
    visual.simulate_click(load.center(), Modifiers::default());
    visual
        .condition(&panel, |panel, _| {
            !panel.loading || panel.pending_load.is_some()
        })
        .await;
    visual.run_until_parked();
    panel.update_in(visual, |panel, window, cx| {
        assert_eq!(panel.loaded.as_ref(), Some(&replacement));
        assert_eq!(panel.definition(cx).unwrap().name, "Replacement");
        assert!(panel.session.is_some() && panel.save_error.is_none());
        assert!(panel.worker.is_none() && !panel.active);
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), view_id);
        panel.name.update(cx, |input, cx| {
            input.set_value("Saved replacement", window, cx)
        });
    });
    visual.run_until_parked();
    let save = visual.debug_bounds("virtual-save").unwrap();
    visual.simulate_click(save.center(), Modifiers::default());
    visual.condition(&panel, |panel, _| !panel.saving).await;
    assert_eq!(
        model::load(&vault, &replacement).unwrap().definition.name,
        "Saved replacement"
    );
    assert_eq!(
        model::load(&vault, &original).unwrap().definition.name,
        "Unsaved criteria"
    );
}
