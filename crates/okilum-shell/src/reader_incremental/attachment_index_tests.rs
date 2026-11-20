//! #2803: own saves and UI renders must not rebuild the vault-wide attachment
//! index. Dmitry's diagnostics showed `link_index_build` at 205–265 ms per
//! render in a project folder with thousands of non-note files.
use super::*;
use ::core::prelude::v1::test;
use okilum_core::vault::attachment_index_builds as builds;

const NOTE: &str = "# Message\n\n![](diagram.png)\n\n[spec](file3_4.json)\n";

fn ui_render(vault: &okilum_core::Vault, source: &str) -> String {
    okilum_core::render::reader_document_from_source(vault, "message.md", source).rendered
}

#[gpui::test]
fn edit2803_saves_and_ui_renders_reuse_the_background_attachment_index(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("assets")).unwrap();
    // A software project used as a vault: most entries are non-note files.
    for module in 0..20 {
        let dir = root.join(format!("integrations/module{module}/src"));
        std::fs::create_dir_all(&dir).unwrap();
        for file in 0..20 {
            std::fs::write(dir.join(format!("file{module}_{file}.json")), "{}\n").unwrap();
        }
    }
    std::fs::write(root.join("assets/diagram.png"), b"png").unwrap();
    std::fs::write(root.join("message.md"), NOTE).unwrap();
    // Unresolved until `assets/new.png` appears: that batch must reparse it.
    std::fs::write(root.join("refs.md"), "Later: ![](new.png)\n").unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("message.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
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
    let index = reader.read_with(visual, |r, _| {
        assert!(r.incremental_state.is_some());
        assert!(
            r.vault.attachment_index_ready(),
            "background publication warms the index before the UI renders"
        );
        let before = builds();
        let rendered = ui_render(&r.vault, NOTE);
        assert_eq!(
            builds() - before,
            0,
            "a UI render builds no attachment index"
        );
        assert!(rendered.contains("assets/diagram.png"), "{rendered}");
        assert!(rendered.contains("file3_4.json"), "{rendered}");
        // Fired positive control: the same inventory without its warm index
        // builds it on this caller, exactly as every render did before.
        let mut cold = (*r.vault).clone();
        cold.invalidate_attachment_index();
        let before = builds();
        assert_eq!(ui_render(&cold, NOTE), rendered);
        assert_eq!(builds() - before, 1, "positive control fired");
        r.vault.attachment_index_identity().unwrap()
    });

    for round in 0..3 {
        let before = builds();
        reader.update_in(visual, |r, window, cx| {
            if round == 0 {
                r.toggle_source(window, cx);
            }
            r.pane.editing.as_ref().unwrap().set_value(
                &format!("{NOTE}\nedit {round} ![](diagram.png)\n"),
                window,
                cx,
            );
            assert!(r.save_source(cx));
        });
        visual.run_until_parked();
        // GPUI's test executor runs the incremental worker on this thread, so
        // this counts the worker and the UI publication together.
        assert_eq!(
            builds() - before,
            0,
            "own save {round} rebuilt the attachment index"
        );
        reader.read_with(visual, |r, _| {
            assert!(!r.incremental_active, "the save batch was published");
            assert_eq!(
                std::fs::read_to_string(root.join("message.md")).unwrap(),
                format!("{NOTE}\nedit {round} ![](diagram.png)\n")
            );
            assert_eq!(
                r.vault.attachment_index_identity(),
                Some(index),
                "a body-only save keeps the inventory index"
            );
        });
    }

    // An inventory change is the one case that rebuilds: once, off the UI.
    std::fs::write(root.join("assets/new.png"), b"png").unwrap();
    let before = builds();
    reader.update_in(visual, |r, _, cx| {
        r.queue_vault_mutation(
            okilum_core::Changes {
                directories: std::collections::BTreeSet::from(["assets".to_owned()]),
                ..Default::default()
            },
            cx,
        );
    });
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert!(!r.incremental_active);
        // The backlink pass and the publication warm share one build.
        assert_eq!(
            builds() - before,
            1,
            "the new attachment rebuilt the index exactly once, in the worker"
        );
        assert_eq!(
            r.vault.backlinks("assets/new.png").len(),
            1,
            "positive control: the affected file-link referrer was reparsed"
        );
        assert!(r.vault.attachment_index_ready());
        assert_ne!(r.vault.attachment_index_identity(), Some(index));
        let published = builds();
        let rendered = ui_render(&r.vault, "![](new.png)");
        assert_eq!(builds(), published, "the UI render after it builds nothing");
        assert!(rendered.contains("assets/new.png"), "{rendered}");
    });
}
