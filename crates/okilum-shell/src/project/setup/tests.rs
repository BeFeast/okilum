use super::*;
use ::core::prelude::v1::test;

fn fixture() -> tempfile::TempDir {
    let area = tempfile::tempdir().unwrap();
    std::fs::create_dir(area.path().join("ops")).unwrap();
    std::fs::create_dir(area.path().join("checkout")).unwrap();
    std::fs::write(
        area.path().join("ops/project.yaml"),
        format!(
            "# Local work\nname: Pulse\nkind: code\nexecution:\n  source_checkout: {}\n",
            serde_json::to_string(&area.path().join("checkout")).unwrap()
        ),
    )
    .unwrap();
    area
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    area: &Path,
    gateway: Arc<super::super::fixture::Fake>,
) -> (Entity<ProjectView>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(reader_t3_settings::Connection {
            origin: Some("https://remote.example".into()),
        });
        reader_t3_settings::fixture_connection(
            &model::environment("localhost:3778").unwrap(),
            gateway,
            cx,
        );
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = ProjectView::empty(Some(area.join("app-state")), window, cx);
            view.area = Some(area.into());
            view.reason = Some("This area is not connected to T3.".into());
            view
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(900.)));
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    (view, visual)
}
fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

#[gpui::test]
fn local_setup_uses_real_controls_and_only_explicit_start_launches_at_the_checked_workspace(
    cx: &mut TestAppContext,
) {
    let area = fixture();
    let g = Arc::new(super::super::fixture::Fake::default());
    *g.local_projects.lock().unwrap() = Some(vec![]);
    let before = std::fs::read(area.path().join("ops/project.yaml")).unwrap();
    let (view, visual) = open(cx, area.path(), g.clone());
    click(visual, "project-local-setup");
    view.read_with(visual, |view, cx| {
        let f = view.local_setup.as_ref().unwrap();
        assert!(
            f.origin.read(cx).value().is_empty(),
            "saved remote origin is never the local default"
        );
        assert_eq!(
            Path::new(&f.workspace.read(cx).value().to_string()),
            area.path().join("checkout")
        );
    });
    view.update_in(visual, |view, window, cx| {
        view.local_setup
            .as_ref()
            .unwrap()
            .origin
            .update(cx, |input, cx| {
                input.set_value("https://remote.example", window, cx)
            })
    });
    click(visual, "project-local-save");
    assert!(visual.debug_bounds("project-local-error").is_some());
    assert!(g.local_creates.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read(area.path().join("ops/project.yaml")).unwrap(),
        before
    );
    view.update_in(visual, |view, window, cx| {
        view.local_setup
            .as_ref()
            .unwrap()
            .origin
            .update(cx, |input, cx| {
                input.set_value("localhost:3778", window, cx)
            })
    });
    click(visual, "project-local-save");
    assert!(visual.debug_bounds("project-local-form").is_none());
    assert!(visual.debug_bounds("start-manager").is_some());
    assert_eq!(
        g.local_creates.lock().unwrap().as_slice(),
        [area
            .path()
            .join("checkout")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()]
    );
    assert!(
        g.local_starts.lock().unwrap().is_empty(),
        "setup never launches"
    );
    assert!(!area.path().join("ops/thread-ids.md").exists());
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.binding.as_ref().unwrap().manager.project_id.as_deref(),
            Some("fixture-local")
        )
    });
    click(visual, "start-manager");
    assert_eq!(g.local_starts.lock().unwrap().as_slice(), ["fixture-local"]);
    assert_eq!(
        okilum_t3::projects::Project::read(area.path())
            .unwrap()
            .manager,
        okilum_t3::projects::Manager::Thread("mcp:fixture-local-manager".into())
    );
    let intents: Vec<_> = std::fs::read_dir(area.path().join("app-state/manager-starts"))
        .unwrap()
        .collect();
    assert_eq!(intents.len(), 2, "intent and created receipt retained");
    assert!(
        g.sends.lock().unwrap().is_empty(),
        "no extra message is sent after launch"
    );
}

#[gpui::test]
fn local_setup_cancel_is_read_only_and_conflict_keeps_the_form_without_a_remote_create(
    cx: &mut TestAppContext,
) {
    let area = fixture();
    let g = Arc::new(super::super::fixture::Fake::default());
    *g.local_projects.lock().unwrap() = Some(vec![]);
    let path = area.path().join("ops/project.yaml");
    let before = std::fs::read(&path).unwrap();
    let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
    let (view, visual) = open(cx, area.path(), g.clone());
    click(visual, "project-local-setup");
    click(visual, "project-local-cancel");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp);
    assert!(g.local_creates.lock().unwrap().is_empty());
    click(visual, "project-local-setup");
    view.update_in(visual, |view, window, cx| {
        view.local_setup
            .as_ref()
            .unwrap()
            .origin
            .update(cx, |input, cx| {
                input.set_value("localhost:3778", window, cx)
            })
    });
    let mut external = before;
    external.extend_from_slice(b"# External edit\n");
    std::fs::write(&path, &external).unwrap();
    click(visual, "project-local-save");
    assert!(visual.debug_bounds("project-local-form").is_some());
    assert!(visual.debug_bounds("project-local-error").is_some());
    view.read_with(visual, |v, cx| {
        assert_eq!(
            v.local_setup
                .as_ref()
                .unwrap()
                .origin
                .read(cx)
                .value()
                .as_ref(),
            "localhost:3778"
        )
    });
    assert_eq!(std::fs::read(&path).unwrap(), external);
    assert!(g.local_creates.lock().unwrap().is_empty());
    assert!(g.local_starts.lock().unwrap().is_empty());
}
