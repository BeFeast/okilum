use super::*;
use ::core::prelude::v1::test;
use okilum_t3::{
    chat_drafts::{Draft as ChatDraft, Store},
    ManagerGateway,
};
use std::sync::atomic::Ordering;

fn unbound(area: &Path) -> Project {
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::write(
        area.join("ops/project.yaml"),
        "# retained\nname: Local\nkind: code\ncustom: retained\n",
    )
    .unwrap();
    Project::read(area).unwrap()
}
fn gateway(checkout: &Path) -> Arc<FakeT3> {
    Arc::new(FakeT3 {
        verified_identity: Some("verified-local".into()),
        projects: Mutex::new(vec![okilum_t3::Project {
            id: "explicit-owner".into(),
            title: "My checkout".into(),
            workspace_root: checkout.canonicalize().unwrap().to_str().unwrap().into(),
            ..Default::default()
        }]),
        ..Default::default()
    })
}
fn environment() -> okilum_t3::Environment {
    okilum_t3::Environment::new("explicit-local", "http://localhost:3773").unwrap()
}

#[test]
fn local_binding_1801_verified_choices_reject_ambiguous_checkout_and_remote_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let area = temp.path().join("area");
    unbound(&area);
    let binding = Binding::load(&area).unwrap();
    let t3 = gateway(&area);
    let (choices, errors) =
        verified_local_owner_choices(&binding, environment(), t3.clone()).unwrap();
    assert_eq!(choices.len(), 1);
    assert!(errors.is_empty());
    assert_eq!(choices[0].selection.project_id, "explicit-owner");
    assert_eq!(
        t3.identity_reads.load(Ordering::SeqCst),
        2,
        "positive control: identity verified on both sides of the complete listing"
    );
    let mut duplicate = t3.projects.lock().unwrap()[0].clone();
    duplicate.id = "second-owner".into();
    t3.projects.lock().unwrap().push(duplicate);
    let (choices, errors) =
        verified_local_owner_choices(&binding, environment(), t3.clone()).unwrap();
    assert!(choices.is_empty());
    assert_eq!(errors.len(), 2);
    let before = t3.identity_reads.load(Ordering::SeqCst);
    let remote = okilum_t3::Environment::new("remote", "https://remote.invalid").unwrap();
    assert!(verified_local_owner_choices(&binding, remote, t3.clone()).is_err());
    assert_eq!(
        t3.identity_reads.load(Ordering::SeqCst),
        before,
        "nonlocal endpoints are refused before contacting a gateway"
    );
    assert!(t3.creates.lock().unwrap().is_empty());
}

#[gpui::test]
fn local_binding_1801_mounted_explicit_picker_saves_existing_owner_and_reconnects_without_rewrite(
    cx: &mut TestAppContext,
) {
    let (temp, root, reader, visual) = open(cx);
    visual.update(|_, cx| reader_features::enable_test_modules(cx));
    let area = root.join("HomeLab");
    let project = unbound(&area);
    let t3 = gateway(&area);
    let env = environment();
    let draft_store = Store::<serde_json::Value>::new(
        &temp.path().join("retained"),
        &env,
        "explicit-owner",
        "mcp:existing",
    );
    let metadata = serde_json::json!({"version":1,"selection":{"kind":"Bug","provenance":"ExplicitSelection","author":"User"},"conversion_suppressed":false,"token_pill":false});
    draft_store
        .save_with_metadata(
            0,
            ChatDraft {
                text: "BUG: unsent full source\r\n🧠  ".into(),
                attachments: vec![],
            },
            Some(metadata.clone()),
        )
        .unwrap();
    let before_draft = draft_store.load_with_metadata().unwrap();
    let binding = Binding::load(&area).unwrap();
    let (choices, errors) =
        verified_local_owner_choices(&binding, env.clone(), t3.clone()).unwrap();
    let original = std::fs::read(area.join("ops/project.yaml")).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.show_new_project(project.clone(), cx);
        reader.show_local_owner_picker(project.clone(), binding, choices, errors, window, cx);
    });
    visual.run_until_parked();
    assert!(drawn(visual, "local-owner-picker"));
    assert_eq!(
        std::fs::read(area.join("ops/project.yaml")).unwrap(),
        original,
        "showing verified choices does not bind automatically"
    );
    assert_eq!(t3.project_reads.load(Ordering::SeqCst), 0);
    click(visual, "local-owner-0");
    assert!(!drawn(visual, "local-owner-picker"));
    let saved = Project::read(&area).unwrap();
    assert_eq!(saved.manager_project.as_deref(), Some("explicit-owner"));
    assert_eq!(saved.manager, okilum_t3::projects::Manager::NotStarted);
    assert_eq!(
        t3.project_reads.load(Ordering::SeqCst),
        1,
        "save revalidates the exact selected owner"
    );
    assert_eq!(draft_store.load_with_metadata().unwrap(), before_draft);
    assert!(t3.creates.lock().unwrap().is_empty());
    let gateway: Arc<dyn ManagerGateway> = t3.clone();
    reader.read_with(visual, |reader, _| {
        assert!(Arc::ptr_eq(
            &reader.project_navigation.t3_gateways[&env.alias],
            &gateway
        ));
        assert!(
            reader.project.is_none(),
            "binding does not select or create a Manager conversation"
        );
    });
    std::fs::write(
        area.join("ops/thread-ids.md"),
        "| Manager | `mcp:existing` |\n",
    )
    .unwrap();
    let reconnect = Project::read(&area).unwrap();
    let binding = Binding::load(&area).unwrap();
    let bytes = std::fs::read(area.join("ops/project.yaml")).unwrap();
    let (choices, errors) =
        verified_local_owner_choices(&binding, env.clone(), t3.clone()).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.show_new_project(reconnect.clone(), cx);
        reader.show_local_owner_picker(reconnect, binding, choices, errors, window, cx);
    });
    visual.run_until_parked();
    click(visual, "local-owner-0");
    assert_eq!(
        std::fs::read(area.join("ops/project.yaml")).unwrap(),
        bytes,
        "reconnect retains exact manifest bytes and thread registry identity"
    );
    assert_eq!(
        Project::read(&area).unwrap().manager,
        okilum_t3::projects::Manager::Thread("mcp:existing".into())
    );
    assert_eq!(draft_store.load_with_metadata().unwrap(), before_draft);
    assert_eq!(t3.project_reads.load(Ordering::SeqCst), 2);
}

#[gpui::test]
fn local_binding_1801_stale_missing_owner_keeps_draft_manifest_and_cached_connection(
    cx: &mut TestAppContext,
) {
    let (_temp, root, reader, visual) = open(cx);
    visual.update(|_, cx| reader_features::enable_test_modules(cx));
    let area = root.join("HomeLab");
    let project = unbound(&area);
    let t3 = gateway(&area);
    let env = environment();
    let old: Arc<dyn ManagerGateway> = Arc::new(FakeT3::default());
    let binding = Binding::load(&area).unwrap();
    let (choices, errors) =
        verified_local_owner_choices(&binding, env.clone(), t3.clone()).unwrap();
    let original = std::fs::read(area.join("ops/project.yaml")).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.show_new_project(project.clone(), cx);
        reader
            .project_navigation
            .t3_gateways
            .insert(env.alias.clone(), old.clone());
        reader.show_local_owner_picker(project, binding, choices, errors, window, cx);
    });
    visual.run_until_parked();
    t3.projects.lock().unwrap().clear();
    click(visual, "local-owner-0");
    assert!(drawn(visual, "local-owner-picker"));
    assert!(drawn(visual, "local-owner-error"));
    assert_eq!(
        std::fs::read(area.join("ops/project.yaml")).unwrap(),
        original
    );
    reader.read_with(visual, |reader, _| {
        assert!(Arc::ptr_eq(
            &reader.project_navigation.t3_gateways[&env.alias],
            &old
        ));
        assert!(reader.project.is_none());
    });
    assert_eq!(
        t3.project_reads.load(Ordering::SeqCst),
        0,
        "missing owner fails at complete listing verification"
    );
    assert!(t3.creates.lock().unwrap().is_empty());
}

#[gpui::test]
fn local_binding_1801_held_project_identity_and_ordinary_create_preserve_local_authority(
    cx: &mut TestAppContext,
) {
    let (_temp, root, reader, visual) = open(cx);
    visual.update(|_, cx| reader_features::enable_test_modules(cx));
    let area = root.join("HomeLab");
    let project = unbound(&area);
    reader.update(visual, |reader, cx| {
        reader.show_new_project(project.clone(), cx);
        assert!(reader.local_binding_target_current(&project));
        let mut replacement = project.clone();
        replacement.manager = okilum_t3::projects::Manager::Thread("mcp:replacement".into());
        reader.show_new_project(replacement, cx);
        assert!(
            !reader.local_binding_target_current(&project),
            "held menu cannot retarget a replaced owner"
        );
    });
    let remote = Arc::new(FakeT3::default());
    reader.update_in(visual, |reader, window, cx| {
        connect(reader, &remote);
        let folder = root.join("Dev/Areas/tool");
        let dialog = cx.new(|cx| NewProjectDialog::new(&root, "Dev/Areas/tool", true, window, cx));
        let mut draft = Draft::for_folder(&folder);
        draft.set_kind(Kind::Code);
        reader.create_project(dialog.downgrade(), folder, draft, window, cx);
    });
    visual.run_until_parked();
    assert_eq!(remote.identity_reads.load(Ordering::SeqCst), 0);
    assert!(remote.creates.lock().unwrap().is_empty());
    assert!(!drawn(visual, "local-owner-picker"));
    assert!(!Project::read(&root.join("Dev/Areas/tool"))
        .unwrap()
        .manager_project
        .is_some());
}
