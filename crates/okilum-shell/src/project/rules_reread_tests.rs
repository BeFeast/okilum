//! #1592: rules scope from the Rules stack, readiness without a connection,
//! and the connected send path through the fake T3 gateway.
use super::*;
use ::core::prelude::v1::test;
use std::collections::BTreeSet;

const BINDING: &str = "t3:\n  schema_version: 1\n  environment: example\n  manager: {project_id: fixture-managers, thread_id: 'mcp:fixture-manager', workspace_mode: local}\n  executor: {project_id: fixture-executors}\n  default_models:\n    manager: {instanceId: codex, model: gpt-6.1-sol}\n    executor: {instanceId: codex, model: gpt-6.1-sol}\n";

#[test]
fn scope_is_the_rules_stack_plus_agent_files_inside_the_project() {
    let vault = Path::new("/vault");
    let area = Path::new("/vault/Dev/Areas/demo");
    let mut layers = BTreeSet::new();
    layers.insert(PathBuf::from("Dev/shared/style.md"));
    let yes = [
        "/vault/Dev/Areas/demo/AGENTS.md",
        "/vault/Dev/Areas/demo/CLAUDE.md",
        "/vault/Dev/Areas/demo/GEMINI.md",
        "/vault/Dev/Areas/demo/.claude/skills/x/SKILL.md",
        "/vault/Dev/AGENTS.md",
        "/vault/AGENTS.md",
        "/vault/Dev/CLAUDE.md",
        "/vault/_AgentContract.md",
        "/vault/@AgentContract.md",
        // Imported by a layer; reached only through the Rules stack.
        "/vault/Dev/shared/style.md",
    ];
    for path in yes {
        assert!(
            in_rules_scope(vault, area, &layers, Path::new(path)),
            "{path}"
        );
    }
    let no = [
        "/vault/Dev/Areas/demo/note.md",
        "/vault/Dev/Areas/other/AGENTS.md",
        "/vault/Dev/Areas/other/CLAUDE.md",
        "/vault/Dev/SKILL.md",
        "/vault/Dev/_AgentContract.md",
        "/vault/Dev/shared/other.md",
        "/elsewhere/AGENTS.md",
        "/vault/Dev/Areas/demo/../other/AGENTS.md",
    ];
    for path in no {
        assert!(
            !in_rules_scope(vault, area, &layers, Path::new(path)),
            "{path}"
        );
    }
    // Control: without the discovered layer the import is out of scope.
    assert!(!in_rules_scope(
        vault,
        area,
        &BTreeSet::new(),
        Path::new("/vault/Dev/shared/style.md")
    ));
}

#[test]
fn notice_says_rules_load_at_start_and_never_claims_delivery_offline() {
    use crate::reader_agents_offer::rules_notice;
    let connected = rules_notice("Dev/Areas/demo/CLAUDE.md", None, false);
    assert!(connected.starts_with("CLAUDE.md saved."));
    assert!(connected.contains("reads its rules only when it starts"));
    assert!(connected.contains("Ask it to re-read"));
    let offline = rules_notice("AGENTS.md", Some(RulesUnready::Offline), false);
    assert!(offline.contains("only when it starts"));
    assert!(offline.contains("not connected, so it was not told"));
    let none = rules_notice("AGENTS.md", Some(RulesUnready::NoManager), false);
    assert!(none.contains("no Manager set up"));
    assert!(rules_notice("AGENTS.md", None, true).contains("Retry it"));
}

fn project_files(root: &Path) -> (PathBuf, PathBuf) {
    let vault = root.join("vault");
    let area = vault.join("Dev/Areas/demo");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::write(
        area.join("ops/project.yaml"),
        format!("name: Synthetic rules\nrepo:\n  full_name: Example/demo\n{BINDING}"),
    )
    .unwrap();
    std::fs::write(
        area.join("ops/thread-ids.md"),
        "| Role | Thread |\n|---|---|\n| Manager | `mcp:fixture-manager` |\n",
    )
    .unwrap();
    std::fs::write(
        vault.join("AGENTS.md"),
        "Root rules\n@Dev/shared/style.md\n",
    )
    .unwrap();
    std::fs::create_dir_all(vault.join("Dev/shared")).unwrap();
    std::fs::write(vault.join("Dev/shared/style.md"), "Style\n").unwrap();
    std::fs::write(area.join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
    (vault, area)
}

#[gpui::test]
fn offline_save_is_explained_and_connected_send_is_sent_once_with_one_retry(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let files = fixture::canonical_tempdir();
    let (vault, area) = project_files(files.path());
    let gateway = Arc::new(fixture::Fake {
        fail_first: true,
        ..Default::default()
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture::view(files.path().join("state"), gateway.clone(), window, cx);
            view.area = Some(area.clone());
            view.resource_vault = Some(vault.clone());
            view.sync_rules_scope(true, cx);
            view
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    visual.run_until_parked();

    // The imported file is in scope once the Rules stack was discovered.
    let import = view.read_with(visual, |view, _| {
        view.rules_source(&vault, &vault.join("Dev/shared/style.md"), "sha256:a")
    });
    assert_eq!(import.unwrap().path, "Dev/shared/style.md");
    let outside = view.read_with(visual, |view, _| {
        view.rules_source(&vault, &area.join("note.md"), "sha256:a")
    });
    assert!(outside.is_none(), "an ordinary note gets no notice");

    let saved = view
        .read_with(visual, |view, _| {
            view.rules_source(&vault, &area.join("CLAUDE.md"), "sha256:b")
        })
        .expect("CLAUDE.md save is in scope");
    let offer = view
        .read_with(visual, |view, _| view.rules_offer_for(&saved))
        .expect("confirmed Manager identity");

    // Offline: the same save is explained, never offered as delivered.
    let chat = view.update(visual, |view, _| view.chat.take());
    assert_eq!(
        view.read_with(visual, |view, _| view.rules_offer_for(&saved)),
        Err(RulesUnready::Offline)
    );
    let binding = view.update(visual, |view, _| view.binding.take());
    assert_eq!(
        view.read_with(visual, |view, _| view.rules_offer_for(&saved)),
        Err(RulesUnready::NoManager)
    );
    view.update(visual, |view, _| {
        view.binding = binding;
        view.chat = chat;
    });

    let mut refresh = RulesRefresh::default();
    assert!(refresh.saved(offer.clone()).unwrap());
    let run = |refresh: &RulesRefresh, visual: &mut gpui::VisualTestContext| {
        let job = view
            .read_with(visual, |view, cx| view.rules_send_job(refresh, &offer, cx))
            .expect("connected send job");
        job()
    };
    // The transport drops the first attempt: the request is retained.
    refresh.begin_delivery(&offer).unwrap();
    let first = run(&refresh, visual);
    assert!(
        first.error.is_some(),
        "first send fails in the fake transport"
    );
    let receipt = first.receipt.clone().expect("durable request identity");
    assert!(refresh.record_delivery(&receipt, first.acknowledged));
    refresh.end_delivery(&offer);
    assert_eq!(refresh.pending(), Some(&receipt));

    // Retry sends the same request, then a third click cannot send again.
    refresh.begin_delivery(&offer).unwrap();
    let second = run(&refresh, visual);
    assert!(second.error.is_none(), "{:?}", second.error);
    assert_eq!(second.receipt.as_ref(), Some(&receipt));
    assert!(refresh.record_delivery(&receipt, second.acknowledged));
    refresh.end_delivery(&offer);
    assert!(refresh.pending().is_none());
    assert!(view
        .read_with(visual, |view, cx| view.rules_send_job(&refresh, &offer, cx))
        .is_err());

    let sends = gateway.sends.lock().unwrap();
    assert_eq!(sends.len(), 2, "one message and one retry");
    assert_eq!(sends[0], sends[1], "retry reuses the original request");
    assert!(format!("{:?}", sends[0]).contains("Dev/Areas/demo/CLAUDE.md"));
}
