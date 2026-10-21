use super::*;

const MANIFEST: &str = include_str!("../../tests/fixtures/area/ops/project.yaml");
struct Area(std::path::PathBuf);
impl Area {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("okilum-launch-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("ops")).unwrap();
        std::fs::write(path.join("ops/project.yaml"), MANIFEST).unwrap();
        std::fs::write(path.join("brief.md"), "# Role\nשלום · Привет · 🦀\n").unwrap();
        Self(path)
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn setup() -> (Area, AreaBinding, LaunchContext, Draft) {
    let area = Area::new();
    let binding = AreaBinding::parse(MANIFEST).unwrap().unwrap();
    let model = binding.default_models.executor.clone();
    let context = LaunchContext {
        project: Project { id: "poc-executors".into(), title: "Repo".into(), workspace_root: "/server/repo".into(), default_model_selection: Some(model) },
        catalog: serde_json::from_value(serde_json::json!({"runtimeMode":"full-access","providers":[
            {"providerInstanceId":"codex","driverKind":"codex","models":[{"id":"gpt-6.1-sol"},{"id":"alternative"}]}]})).unwrap(),
    };
    let draft = Draft {
        role: "UI".into(),
        brief_path: "brief.md".into(),
        worktree_path: "/server/worktrees/ui".into(),
        branch: "feature/ui".into(),
        model_override: None,
    };
    (area, binding, context, draft)
}

#[test]
fn captures_exact_unicode_brief_and_project_default_or_explicit_live_override() {
    let (area, binding, context, mut draft) = setup();
    let request = Request::prepare(&area.0, &binding, &context, &draft).unwrap();
    assert_eq!(
        request.brief_source,
        std::fs::read_to_string(area.0.join("brief.md")).unwrap()
    );
    assert_eq!(
        request.model_selection,
        context.project.default_model_selection.clone().unwrap()
    );
    assert_eq!(
        Request::from_message(&request.message().unwrap()).unwrap(),
        Some(request.clone())
    );
    draft.model_override = Some(ModelSelection {
        instance_id: "codex".into(),
        model: "alternative".into(),
        options: vec![],
    });
    let second = Request::prepare(&area.0, &binding, &context, &draft).unwrap();
    assert_eq!(second.model_selection, draft.model_override.unwrap());
    assert_ne!(
        request.request_id, second.request_id,
        "separate explicit requests are separate operations"
    );
    assert_eq!(
        second.worktree_path, "/server/worktrees/ui",
        "a remote Unix path is preserved on every client platform"
    );
}

#[test]
fn refuses_binding_rotation_missing_brief_oversize_and_unknown_model_without_truncation() {
    let (area, binding, context, mut draft) = setup();
    std::fs::write(
        area.0.join("ops/project.yaml"),
        MANIFEST.replace("mcp:poc-manager", "mcp:rotated"),
    )
    .unwrap();
    assert!(Request::prepare(&area.0, &binding, &context, &draft)
        .unwrap_err()
        .to_string()
        .contains("binding changed"));
    std::fs::write(area.0.join("ops/project.yaml"), MANIFEST).unwrap();
    draft.brief_path = "../outside.md".into();
    assert!(Request::prepare(&area.0, &binding, &context, &draft).is_err());
    draft.brief_path = "absent.md".into();
    assert!(Request::prepare(&area.0, &binding, &context, &draft).is_err());
    draft.brief_path = "brief.md".into();
    std::fs::write(area.0.join("brief.md"), "x".repeat(MAX_BRIEF_BYTES + 1)).unwrap();
    assert!(Request::prepare(&area.0, &binding, &context, &draft)
        .unwrap_err()
        .to_string()
        .contains("64 KiB"));
    // The bounded read stops one byte into an emoji. Size rejection must
    // precede decoding so a valid large Unicode file is not called corrupt.
    std::fs::write(
        area.0.join("brief.md"),
        "🦀".repeat(MAX_BRIEF_BYTES / 4 + 1),
    )
    .unwrap();
    assert!(Request::prepare(&area.0, &binding, &context, &draft)
        .unwrap_err()
        .to_string()
        .contains("64 KiB"));
    let exact = "א".repeat(MAX_BRIEF_BYTES / 2);
    std::fs::write(area.0.join("brief.md"), &exact).unwrap();
    assert_eq!(
        Request::prepare(&area.0, &binding, &context, &draft)
            .unwrap()
            .brief_source,
        exact
    );
    std::fs::write(area.0.join("brief.md"), [0xff, 0xfe]).unwrap();
    assert!(Request::prepare(&area.0, &binding, &context, &draft)
        .unwrap_err()
        .to_string()
        .contains("UTF-8"));
    std::fs::write(area.0.join("brief.md"), "brief").unwrap();
    draft.model_override = Some(ModelSelection {
        instance_id: "codex".into(),
        model: "removed".into(),
        options: vec![],
    });
    assert!(Request::prepare(&area.0, &binding, &context, &draft)
        .unwrap_err()
        .to_string()
        .contains("live catalog"));
}

#[cfg(unix)]
#[test]
fn refuses_a_brief_symlink_outside_the_selected_area() {
    let (area, binding, context, mut draft) = setup();
    let outside = Area::new();
    std::os::unix::fs::symlink(outside.0.join("brief.md"), area.0.join("escape.md")).unwrap();
    draft.brief_path = "escape.md".into();
    assert!(Request::prepare(&area.0, &binding, &context, &draft)
        .unwrap_err()
        .to_string()
        .contains("outside this vault area"));
}
