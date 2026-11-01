use super::*;
use gpui::TestAppContext;
use okilum_core::log::stream::Assembler;

#[gpui::test]
fn explicit_target_validation_and_stale_completion_preserve_current_session(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Panel::new("fixture-vault".into(), window, cx));
        panel = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let panel = panel.unwrap();
    panel.update_in(visual, |panel, window, cx| {
        assert!(!panel.running);
        assert!(
            panel.worker.is_none(),
            "opening the UI must not start kubectl"
        );
        let mut assembler = Assembler::new("kept.log".into(), "session-2".into()).unwrap();
        let snapshot = assembler.append(b"kept\n").unwrap().unwrap();
        panel.pending = Some(snapshot.clone());
        panel.start(cx); // Empty namespace/pod/container: validation only.
        assert!(panel.worker.is_none());
        assert!(panel.error.as_ref().unwrap().contains("namespace"));
        assert!(Arc::ptr_eq(panel.pending.as_ref().unwrap(), &snapshot));
        for (input, value) in [
            (&panel.namespace, "prod"),
            (&panel.pod, "api-7"),
            (&panel.container, "server"),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        panel
            .context
            .update(cx, |input, cx| input.set_value("work cluster", window, cx));
        let args = panel.request(cx).unwrap().args().unwrap();
        assert!(
            args.contains(&"--context=work cluster".to_string()),
            "context stays one literal argument"
        );
        assert!(args.contains(&"pod/api-7".to_string()));
        panel
            .tail
            .update(cx, |input, cx| input.set_value("100001", window, cx));
        panel.start(cx);
        assert!(
            panel.worker.is_none(),
            "over-limit history cannot spawn a process"
        );
        assert!(Arc::ptr_eq(panel.pending.as_ref().unwrap(), &snapshot));
        panel.error = None;
        panel.generation = 2;
        panel.running = true;
        panel.receive(
            1,
            worker::Update {
                snapshot: None,
                status: worker::Status::Failed("stale failure".into()),
                diagnostic: "stale diagnostic".into(),
                pending: 0,
            },
            cx,
        );
        assert!(
            panel.running,
            "old completion cannot finish the current session"
        );
        assert!(panel.error.is_none());
        assert!(panel.diagnostic.is_empty());
        assert!(Arc::ptr_eq(panel.pending.as_ref().unwrap(), &snapshot));
        panel.receive(
            2,
            worker::Update {
                snapshot: Some(snapshot),
                status: worker::Status::Complete,
                diagnostic: String::new(),
                pending: 3,
            },
            cx,
        );
        assert!(
            !panel.running,
            "positive control: current completion is accepted"
        );
        assert!(panel.message.contains("3 trailing bytes"));
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("kubernetes-start").is_some());
    assert!(visual.debug_bounds("kubernetes-stop").is_some());
}
