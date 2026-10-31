use super::*;
use ::core::prelude::v1::test;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use okilum_t3::projects::{
    icon::{Avatar, IconColor, ProjectIcon},
    Kind,
};
use std::sync::Mutex;

type GatewayResult<T> = std::result::Result<T, okilum_t3::GatewayError>;

/// The paired T3 connection, faked: never Oleg's T3. Records every
/// `project.create` and icon write; `refuse` answers create as T3 does
/// for a workspace root that does not exist.
#[derive(Default)]
struct FakeT3 {
    refuse: bool,
    projects: Mutex<Vec<okilum_t3::Project>>,
    creates: Mutex<Vec<(String, String)>>,
    icons: Mutex<Vec<(String, Option<ProjectIcon>)>>,
}
impl okilum_t3::ManagerGateway for FakeT3 {
    fn status(&self) -> okilum_t3::GatewayStatus {
        okilum_t3::GatewayStatus::Connected {
            expires_at: u64::MAX,
        }
    }
    fn server_identity(&self) -> GatewayResult<okilum_t3::ServerIdentity> {
        panic!("unused identity")
    }
    fn list_projects(&self) -> GatewayResult<Vec<okilum_t3::Project>> {
        Ok(self.projects.lock().unwrap().clone())
    }
    fn create_project(&self, title: &str, root: &str) -> GatewayResult<okilum_t3::Project> {
        self.creates
            .lock()
            .unwrap()
            .push((title.into(), root.into()));
        if self.refuse {
            return Err(okilum_t3::GatewayError::Tool {
                code: "workspace_root_not_found".into(),
                message: format!("Workspace root {root} does not exist"),
            });
        }
        let project = okilum_t3::Project {
            id: "new-1".into(),
            title: title.into(),
            workspace_root: root.into(),
            ..Default::default()
        };
        self.projects.lock().unwrap().push(project.clone());
        Ok(project)
    }
    fn set_project_icon(
        &self,
        id: &str,
        icon: Option<&ProjectIcon>,
    ) -> GatewayResult<okilum_t3::Project> {
        self.icons.lock().unwrap().push((id.into(), icon.cloned()));
        let mut projects = self.projects.lock().unwrap();
        let project = projects.iter_mut().find(|p| p.id == id).unwrap();
        project.project_icon = icon.cloned();
        Ok(project.clone())
    }
    fn list_threads(&self, _: &str) -> GatewayResult<Vec<okilum_t3::ThreadSummary>> {
        panic!("unused threads")
    }
    fn read_thread(&self, _: &okilum_t3::ThreadQuery) -> GatewayResult<okilum_t3::ThreadPage> {
        panic!("unused read")
    }
    fn send_message(
        &self,
        _: &okilum_t3::OutgoingMessage,
    ) -> GatewayResult<okilum_t3::SendReceipt> {
        panic!("unused send")
    }
    fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
        panic!("unused questions")
    }
    fn capabilities(&self) -> GatewayResult<okilum_t3::Capabilities> {
        panic!("unused catalog")
    }
    fn thread_configuration(&self, _: &str) -> GatewayResult<okilum_t3::ThreadConfiguration> {
        panic!("unused configuration")
    }
    fn configure_model(
        &self,
        _: &str,
        _: &okilum_t3::ModelSelection,
    ) -> GatewayResult<okilum_t3::ThreadConfiguration> {
        panic!("unused configure")
    }
}

fn connect(reader: &mut Reader, t3: &Arc<FakeT3>) {
    let gateway: Arc<dyn okilum_t3::ManagerGateway> = t3.clone();
    reader.project_navigation.t3_gateways = Gateways::from([("test".to_owned(), gateway)]);
}

/// A temporary vault: `HomeLab` (a folder), `Dev/Areas/app` (a project)
/// and `Dev/Areas/tool` (a folder with its own index).
fn open(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    PathBuf,
    Entity<Reader>,
    &mut VisualTestContext,
) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("HomeLab")).unwrap();
    std::fs::write(root.join("HomeLab/mesh.md"), "# Mesh\n").unwrap();
    std::fs::create_dir_all(root.join("Dev/Areas/app/ops")).unwrap();
    std::fs::write(
        root.join("Dev/Areas/app/ops/project.yaml"),
        "name: App\nkind: code\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("Dev/Areas/tool")).unwrap();
    std::fs::write(root.join("Dev/Areas/tool/_index.md"), "# Tool notes\n").unwrap();
    std::fs::write(root.join("start.md"), "# Start\n").unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    open_path: Some(root.join("start.md")),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    (temp, root, entity.unwrap(), visual)
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    visual.simulate_click(bounds.center(), Modifiers::none());
    visual.run_until_parked();
}

fn drawn(visual: &mut VisualTestContext, selector: &'static str) -> bool {
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.debug_bounds(selector).is_some()
}

/// #1475: «Make this a project…» is offered for a folder that is not a
/// project, and hidden for one that is (its manifest, or a project the
/// Projects list knows).
#[gpui::test]
fn make_this_a_project_is_hidden_for_folders_that_are_projects(cx: &mut TestAppContext) {
    let (_temp, root, reader, visual) = open(cx);
    reader.read_with(visual, |r, _| {
        assert!(r.can_make_project("HomeLab"), "positive control");
        assert!(r.can_make_project("Dev/Areas/tool"));
        assert!(
            !r.can_make_project("Dev/Areas/app"),
            "it has ops/project.yaml"
        );
        assert!(
            r.can_make_project(""),
            "the vault root can be a local project"
        );
        assert!(!r.can_make_project(".obsidian"));
    });
    // A project the Projects list knows, even before its manifest is seen.
    reader.update(visual, |r, _| {
        r.project_navigation
            .discovery
            .projects
            .push(okilum_t3::projects::Project {
                folder: root.join("HomeLab"),
                name: "HomeLab".into(),
                kind: Kind::Knowledge,
                environment: None,
                manager_project: None,
                manager: okilum_t3::projects::Manager::NotStarted,
                appearance: Default::default(),
            })
    });
    reader.read_with(visual, |r, _| assert!(!r.can_make_project("HomeLab")));
}

/// The folder's menu → the dialog → Create project: project.yaml and
/// `_index.md` are written, the dialog closes, and the project is in the
/// Projects list at once. Nothing reaches T3 for a knowledge project.
#[gpui::test]
fn make_this_a_project_writes_the_manifest_and_lists_the_project(cx: &mut TestAppContext) {
    let (_temp, root, reader, visual) = open(cx);
    let t3 = Arc::new(FakeT3::default());
    reader.update_in(visual, |r, window, cx| {
        connect(r, &t3);
        r.open_new_project_dialog(Some("HomeLab".into()), window, cx);
    });
    visual.run_until_parked();
    assert!(drawn(visual, "new-project-dialog"));
    assert!(drawn(visual, "new-project-folder"), "the folder is fixed");
    click(visual, "new-project-create");
    assert!(!drawn(visual, "new-project-dialog"), "the dialog closes");

    let folder = root.join("HomeLab");
    let manifest = std::fs::read_to_string(folder.join("ops/project.yaml")).unwrap();
    assert!(manifest.contains("name: \"HomeLab\"\n"), "{manifest}");
    assert!(manifest.contains("kind: knowledge"));
    assert!(!manifest.contains("t3:"), "T3 is off for knowledge");
    assert_eq!(
        std::fs::read_to_string(folder.join("_index.md")).unwrap(),
        "# HomeLab\n"
    );
    reader.read_with(visual, |r, _| {
        let projects = &r.project_navigation.discovery.projects;
        assert!(projects
            .iter()
            .any(|p| p.folder == folder && p.name == "HomeLab"));
        assert!(!r.can_make_project("HomeLab"), "the menu item is gone");
    });
    assert!(t3.creates.lock().unwrap().is_empty());
    assert!(!drawn(visual, "reader-error-toast"));
}

/// An existing `_index.md` is kept, and «+ New project…» starts with the
/// folder selected in the tree.
#[gpui::test]
fn new_project_starts_with_the_selected_folder_and_keeps_its_index(cx: &mut TestAppContext) {
    let (_temp, root, reader, visual) = open(cx);
    reader.update_in(visual, |r, window, cx| {
        r.tree.cursor = Some("Dev/Areas/tool".into());
        assert_eq!(r.default_new_project_folder(), "Dev/Areas/tool");
        r.open_new_project_dialog(None, window, cx);
    });
    visual.run_until_parked();
    assert!(
        drawn(visual, "new-project-folder-input"),
        "the folder is asked for"
    );
    click(visual, "new-project-create");
    assert_eq!(
        std::fs::read_to_string(root.join("Dev/Areas/tool/_index.md")).unwrap(),
        "# Tool notes\n"
    );
    let manifest = std::fs::read_to_string(root.join("Dev/Areas/tool/ops/project.yaml")).unwrap();
    assert!(manifest.contains("name: \"tool\"\n"), "{manifest}");
    assert!(manifest.contains("domain: \"Dev\"\n"));
}

fn code_draft(folder: &Path, icon: Option<ProjectIcon>) -> Draft {
    let mut draft = Draft::for_folder(folder);
    draft.name = "Tool".into();
    draft.set_kind(Kind::Code);
    draft.set_create_in_t3(true); // This fixture deliberately exercises the explicit T3 path.
    draft.source = "/srv/src/tool".into();
    draft.icon = icon;
    draft
}

/// A code project with «Create the project in T3»: T3's create at the
/// checkout with the chosen icon over the paired connection, then the
/// `t3:` binding in project.yaml and T3's icon on the project.
#[gpui::test]
fn t3_creates_the_project_with_its_icon_and_the_manifest_binds_it(cx: &mut TestAppContext) {
    let (_temp, root, reader, visual) = open(cx);
    let t3 = Arc::new(FakeT3::default());
    let folder = root.join("Dev/Areas/tool");
    let icon = ProjectIcon::Lucide {
        name: "wrench".into(),
        color: IconColor::Orange,
    };
    reader.update_in(visual, |r, window, cx| {
        connect(r, &t3);
        let vault = r.vault_root.clone();
        let dialog = cx.new(|cx| NewProjectDialog::new(&vault, "Dev/Areas/tool", true, window, cx));
        r.create_project(
            dialog.downgrade(),
            folder.clone(),
            code_draft(&folder, Some(icon.clone())),
            window,
            cx,
        );
    });
    visual.run_until_parked();
    assert_eq!(
        *t3.creates.lock().unwrap(),
        [("Tool".to_owned(), "/srv/src/tool".to_owned())]
    );
    assert_eq!(
        *t3.icons.lock().unwrap(),
        [("new-1".to_owned(), Some(icon.clone()))]
    );
    let manifest = std::fs::read_to_string(folder.join("ops/project.yaml")).unwrap();
    assert!(manifest.contains("execution:\n  source_checkout: \"/srv/src/tool\"\n"));
    assert!(
        manifest.contains("  environment: \"test\"\n  executor_project_id: \"new-1\"\n"),
        "{manifest}"
    );
    reader.read_with(visual, |r, cx| {
        let project = r
            .project_navigation
            .discovery
            .projects
            .iter()
            .find(|p| p.folder == folder)
            .expect("listed")
            .clone();
        assert_eq!(project.environment.as_deref(), Some("test"));
        assert_eq!(
            project.appearance.executor_project.as_deref(),
            Some("new-1")
        );
        assert_eq!(
            okilum_shell::project_avatar::avatar(&project, cx),
            Avatar::Icon(icon.clone()),
            "T3's icon"
        );
    });
    assert!(!drawn(visual, "reader-error-toast"));
}

/// T3 refuses: a clear error toast, and project.yaml stays written,
/// without a T3 binding.
#[gpui::test]
fn a_t3_failure_is_a_toast_and_the_manifest_stays(cx: &mut TestAppContext) {
    let (_temp, root, reader, visual) = open(cx);
    let t3 = Arc::new(FakeT3 {
        refuse: true,
        ..Default::default()
    });
    let folder = root.join("Dev/Areas/tool");
    reader.update_in(visual, |r, window, cx| {
        connect(r, &t3);
        let vault = r.vault_root.clone();
        let dialog = cx.new(|cx| NewProjectDialog::new(&vault, "Dev/Areas/tool", true, window, cx));
        r.create_project(
            dialog.downgrade(),
            folder.clone(),
            code_draft(&folder, None),
            window,
            cx,
        );
    });
    visual.run_until_parked();
    assert_eq!(t3.creates.lock().unwrap().len(), 1);
    let manifest = std::fs::read_to_string(folder.join("ops/project.yaml")).unwrap();
    assert!(manifest.contains("kind: code"));
    assert!(!manifest.contains("t3:"), "{manifest}");
    assert!(
        drawn(visual, "reader-error-toast"),
        "the refusal is a toast"
    );
    reader.read_with(visual, |r, _| {
        assert!(r
            .project_navigation
            .discovery
            .projects
            .iter()
            .any(|p| p.folder == folder && p.environment.is_none()));
    });
}

#[gpui::test]
fn root_code_project_starts_locally_and_does_not_call_a_saved_remote_gateway(
    cx: &mut TestAppContext,
) {
    let (_temp, root, reader, visual) = open(cx);
    let t3 = Arc::new(FakeT3::default());
    reader.update_in(visual, |r, window, cx| {
        connect(r, &t3);
        let dialog = cx.new(|cx| NewProjectDialog::new(&root, "", true, window, cx));
        let mut draft = Draft::for_folder(&root);
        draft.set_kind(Kind::Code);
        assert!(!draft.create_in_t3());
        r.create_project(dialog.downgrade(), root.clone(), draft, window, cx);
    });
    visual.run_until_parked();
    assert!(root.join("ops/project.yaml").is_file());
    assert!(
        t3.creates.lock().unwrap().is_empty(),
        "local creation must not dispatch remote project.create"
    );
    let manifest = std::fs::read_to_string(root.join("ops/project.yaml")).unwrap();
    assert!(!manifest.contains("t3:"));
    reader.read_with(visual, |r, _| {
        assert!(r
            .project_navigation
            .discovery
            .projects
            .iter()
            .any(|p| p.folder == root));
        assert!(
            !r.can_make_project(""),
            "positive control: successful creation removes the duplicate-create action"
        );
    });
}
