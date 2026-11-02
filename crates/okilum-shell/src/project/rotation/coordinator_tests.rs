use super::*;
use crate::project::rotation::policy_owner;
use okilum_t3::rotation::TzifResolver;

#[gpui::test]
fn rotation_policy_1473_actual_coordinator_ack_and_replaced_authority_guard(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    for replace in [false, true] {
        let state = tempfile::tempdir().unwrap();
        let mut owner = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let project =
                cx.new(|cx| discuss_handoff::native_tests::view(state.path(), window, cx));
            let composer = project.read(cx).composer.clone();
            owner = Some(project);
            Root::new(composer, window, cx)
        });
        let project = owner.unwrap();
        visual.run_until_parked();
        let checkpoint = project.update_in(visual, |view, window, cx| {
            view.draft_handoff_surface_visible(true, cx);
            let policy = policy_owner::tests::seed(view, cx);
            let canonical = policy_owner::canonical_owner(view, cx).unwrap();
            let before = policy.load().unwrap().unwrap();
            let checkpoint = policy
                .claim(
                    before.revision,
                    &canonical,
                    before.through + 22 * 3600,
                    &TzifResolver::new("/usr/share/zoneinfo"),
                    None,
                )
                .unwrap();
            view.mount_pending_rotation(
                policy,
                Some("Reviewed source\n".into()),
                policy_owner::canonical_owner,
                window,
                cx,
            );
            checkpoint
        });
        visual.run_until_parked();
        let coordinator =
            project.update_in(visual, |view, _, _| view.rotation.as_ref().unwrap().clone());
        let authority =
            coordinator.update_in(visual, |coordinator, _, _| coordinator.authority.clone());
        if replace {
            authority
                .with_current(&checkpoint.owner, |policy| {
                    policy.acknowledge(checkpoint.revision, &checkpoint.owner)
                })
                .unwrap();
            let prior = authority.load().unwrap().unwrap();
            authority
                .configure(
                    prior.record.sequence,
                    prior.record.configuration.clone(),
                    20759 * 86400,
                    &TzifResolver::new("/usr/share/zoneinfo"),
                    || true,
                )
                .unwrap();
        }
        coordinator.update_in(visual, |coordinator, window, cx| {
            coordinator.request(
                Requested {
                    action: Action::Resume,
                    expected: checkpoint.clone(),
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        project.update_in(visual, |view, _, cx| {
            let current = authority.load().unwrap().unwrap();
            assert!(current.checkpoint.pending.is_none());
            if replace {
                assert_eq!(
                    current.record.lifecycle_revision,
                    checkpoint.owner.revision + 1
                );
                assert!(
                    view.composer.read(cx).typed_source_text(cx).is_empty(),
                    "old UI cannot apply a draft after authority replacement"
                );
            } else {
                assert_eq!(current.record.lifecycle_revision, checkpoint.owner.revision);
                assert_eq!(
                    view.composer.read(cx).typed_source_text(cx),
                    "Reviewed source\n"
                );
                let environment = view.environment.as_ref().unwrap();
                let durable =
                    okilum_t3::chat_drafts::Store::<okilum_t3::chat_drafts::Attachment>::new(
                        state.path(),
                        environment,
                        "project",
                        "thread",
                    );
                assert_eq!(
                    durable.load_with_metadata().unwrap().0.draft.text,
                    "Reviewed source\n",
                    "ack follows the real durable writer barrier"
                );
            }
            assert!(
                view.records.is_empty(),
                "Resume never prepares a transport message"
            );
        });
    }
}
