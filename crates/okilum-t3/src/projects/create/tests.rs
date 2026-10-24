use super::*;
use crate::gateway::{GatewayError, GatewayResult};
use crate::projects::icon::IconColor;
use crate::{
    Capabilities, GatewayStatus, ModelSelection, OutgoingMessage, SendReceipt, ServerIdentity,
    ThreadConfiguration, ThreadPage, ThreadQuery, ThreadSummary,
};
use std::path::PathBuf;
use std::sync::Mutex;

/// A temporary vault; never Oleg's.
struct Vault(PathBuf);
impl Vault {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("okilum-create-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn folder(&self, relative: &str) -> PathBuf {
        let folder = self.0.join(relative);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }
}
impl Drop for Vault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn server() -> ProjectIcon {
    ProjectIcon::Lucide {
        name: "server".into(),
        color: IconColor::Blue,
    }
}

fn t3(id: &str, root: &str) -> crate::Project {
    crate::Project {
        id: id.into(),
        title: id.into(),
        workspace_root: root.into(),
        ..Default::default()
    }
}

#[test]
fn the_dialog_starts_with_the_folder_name_and_t3_follows_the_kind() {
    let draft = Draft::for_folder(Path::new("/vault/HomeLab"));
    assert_eq!(draft.name, "HomeLab");
    assert_eq!(draft.kind, Kind::Knowledge);
    assert_eq!(draft.icon, None);
    assert!(!draft.create_in_t3(), "off for knowledge");

    let mut draft = draft;
    draft.set_kind(Kind::Code);
    assert!(draft.create_in_t3(), "on for code");
    draft.set_kind(Kind::Hybrid);
    assert!(!draft.create_in_t3(), "off for hybrid");

    // Once the user sets the box, a kind change leaves it alone.
    draft.set_create_in_t3(true);
    draft.set_kind(Kind::Knowledge);
    assert!(draft.create_in_t3());
    draft.set_create_in_t3(false);
    draft.set_kind(Kind::Code);
    assert!(!draft.create_in_t3());
}

#[test]
fn create_says_what_is_missing() {
    let mut draft = Draft::for_folder(Path::new("/vault/App"));
    assert_eq!(draft.problem(), None);
    draft.name = "  ".into();
    assert_eq!(draft.problem().as_deref(), Some("Enter a name."));
    draft.name = "App".into();
    draft.set_kind(Kind::Code);
    assert!(draft
        .problem()
        .is_some_and(|p| p.contains("checkout path on the T3 host")));
    draft.source = "https://git.example/me/app.git".into();
    assert!(
        draft.problem().is_some_and(|p| p.contains("checkout path")),
        "T3 needs a checkout, not a URL"
    );
    draft.set_create_in_t3(false);
    assert_eq!(draft.problem(), None, "a repo URL is fine without T3");
    draft.source = "src/app".into();
    assert!(draft
        .problem()
        .is_some_and(|p| p.contains("/home/me/src/app")));
    draft.source = "/home/me/src/app/".into();
    draft.set_create_in_t3(true);
    assert_eq!(draft.problem(), None);
    assert_eq!(
        draft.source(),
        Some(Source::Checkout("/home/me/src/app".into()))
    );
    // A knowledge project has no code location, whatever the field holds.
    draft.set_kind(Kind::Knowledge);
    assert_eq!(draft.source(), None);
}

#[test]
fn sources_and_slugs() {
    assert_eq!(parse_source(" "), None);
    assert_eq!(
        parse_source("git@git.example:me/app.git"),
        Some(Source::Repo("git@git.example:me/app.git".into()))
    );
    assert_eq!(
        parse_source("ssh://git@host:2222/me/app.git"),
        Some(Source::Repo("ssh://git@host:2222/me/app.git".into()))
    );
    assert_eq!(slug("HomeLab"), "homelab");
    assert_eq!(slug("  My Mesh — v2 "), "my-mesh-v2");
    assert_eq!(slug("Студия"), "project");
}

#[test]
fn a_knowledge_project_yaml_holds_what_the_projects_list_reads() {
    let mut draft = Draft::for_folder(Path::new("/vault/HomeLab"));
    draft.icon = Some(server());
    assert_eq!(
        manifest(&draft, "HomeLab"),
        "# Okilum project manifest, created in Okilum (okilum#1475).\n\
         schema_version: 1\n\
         slug: homelab\n\
         name: \"HomeLab\"\n\
         kind: knowledge   # code | knowledge | hybrid\n\
         domain: \"HomeLab\"\n\
         index: \"HomeLab/_index.md\"\n\
         icon: {kind: lucide, name: \"server\", color: blue}\n"
    );
}

#[test]
fn a_code_project_yaml_names_its_checkout_or_its_repo() {
    let mut draft = Draft::for_folder(Path::new("/vault/Dev/Areas/app"));
    draft.name = "App \"two\"".into();
    draft.set_kind(Kind::Code);
    draft.source = "/home/me/src/app".into();
    let text = manifest(&draft, "Dev/Areas/app");
    assert!(text.contains("name: \"App \\\"two\\\"\"\n"), "{text}");
    assert!(text.contains("kind: code"));
    assert!(text.contains("domain: \"Dev\"\n"));
    assert!(text.contains("execution:\n  source_checkout: \"/home/me/src/app\"\n"));
    assert!(!text.contains("icon:"), "no icon: the automatic one");
    draft.source = "https://git.example/me/app.git".into();
    let text = manifest(&draft, "Dev/Areas/app");
    assert!(text.contains("repo:\n  clone_url: \"https://git.example/me/app.git\"\n"));
    assert!(!text.contains("execution:"));
}

#[test]
fn create_writes_the_manifest_and_an_index_only_when_there_is_none() {
    let vault = Vault::new();
    let folder = vault.folder("HomeLab");
    let mut draft = Draft::for_folder(&folder);
    draft.icon = Some(server());
    let created = create(&vault.0, &folder, &draft).unwrap();
    assert!(created.wrote_index);
    assert_eq!(
        std::fs::read_to_string(folder.join("_index.md")).unwrap(),
        "# HomeLab\n"
    );
    let project = created.project;
    assert_eq!(project.name, "HomeLab");
    assert_eq!(project.kind, Kind::Knowledge);
    assert_eq!(project.appearance.icon, Some(server()));
    assert_eq!(project.environment, None, "no T3 block without T3");
    assert!(is_project(&folder));

    // The Projects list finds it: the same discovery the picker runs.
    let discovery = crate::projects::discover(&vault.0);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    assert_eq!(discovery.projects.len(), 1);
    assert_eq!(discovery.projects[0].folder, folder);

    // A second time is refused and changes nothing.
    let before = std::fs::read_to_string(folder.join(MANIFEST)).unwrap();
    let mut other = draft.clone();
    other.name = "Other".into();
    let error = create(&vault.0, &folder, &other).unwrap_err();
    assert!(
        format!("{error:#}").contains("already a project"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join(MANIFEST)).unwrap(),
        before
    );

    // An existing index is kept byte for byte.
    let notes = vault.folder("Music/Areas/Studio");
    std::fs::write(
        notes.join("_index.md"),
        "---\ntype: Note\n---\n\n# Studio notes\n",
    )
    .unwrap();
    let created = create(&vault.0, &notes, &Draft::for_folder(&notes)).unwrap();
    assert!(!created.wrote_index);
    assert_eq!(
        std::fs::read_to_string(notes.join("_index.md")).unwrap(),
        "---\ntype: Note\n---\n\n# Studio notes\n"
    );
    assert!(std::fs::read_to_string(notes.join(MANIFEST))
        .unwrap()
        .contains("domain: \"Music\"\nindex: \"Music/Areas/Studio/_index.md\"\n"));
}

#[test]
fn the_vault_root_and_missing_folders_are_refused() {
    let vault = Vault::new();
    let draft = Draft::for_folder(&vault.0);
    assert!(create(&vault.0, &vault.0, &draft).is_err());
    let missing = vault.0.join("Nope");
    assert!(create(&vault.0, &missing, &Draft::for_folder(&missing)).is_err());
    assert!(!missing.exists());
    let hidden = vault.folder(".obsidian");
    assert!(create(&vault.0, &hidden, &Draft::for_folder(&hidden)).is_err());
}

#[test]
fn the_t3_block_is_appended_and_binds_the_icon_to_t3() {
    let vault = Vault::new();
    let folder = vault.folder("HomeLab");
    let mut draft = Draft::for_folder(&folder);
    draft.icon = Some(server());
    create(&vault.0, &folder, &draft).unwrap();
    let before = std::fs::read_to_string(folder.join(MANIFEST)).unwrap();
    let link = T3Link {
        environment: "maestro".into(),
        project_id: "p-1".into(),
    };
    let project = link_t3(&folder, &link).unwrap();
    assert_eq!(project.environment.as_deref(), Some("maestro"));
    assert_eq!(project.appearance.executor_project.as_deref(), Some("p-1"));
    let after = std::fs::read_to_string(folder.join(MANIFEST)).unwrap();
    assert!(after.starts_with(&before), "every earlier byte stays");
    assert!(after.ends_with(
        "t3:\n  schema_version: 1\n  environment: \"maestro\"\n  executor_project_id: \"p-1\"\n  manager: {project_id: null, thread_id: null, workspace_mode: local}\n"
    ));
    // The Project view's binding reads: no manager yet.
    let binding = project.native_binding().unwrap().unwrap();
    assert_eq!(binding.environment, "maestro");
    assert_eq!(binding.manager.project_id, None);
    assert_eq!(binding.manager.thread_id, None);
    // Never a second block.
    assert!(link_t3(&folder, &link).is_err());
}

#[test]
fn the_vault_on_the_t3_host_is_found_from_t3_projects_rooted_in_it() {
    let vault = Vault::new();
    vault.folder("HomeLab");
    vault.folder("Dev/Areas/maestro");
    let listing = vec![
        t3("brain", "/home/god/Obsidian Vault"),
        t3("homelab", "/home/god/Obsidian Vault/HomeLab"),
        t3("maestro", "/home/god/Obsidian Vault/Dev/Areas/maestro/"),
        t3("app", "/home/god/src/app"),
    ];
    assert_eq!(
        server_vault_root(&vault.0, &listing).as_deref(),
        Some("/home/god/Obsidian Vault")
    );
    assert_eq!(server_vault_root(&vault.0, &listing[3..]), None);

    let folder = vault.folder("Music/Areas/Studio");
    let draft = Draft::for_folder(&folder);
    assert_eq!(
        t3_workspace_root(&draft, &vault.0, &folder, &listing).unwrap(),
        "/home/god/Obsidian Vault/Music/Areas/Studio"
    );
    // No T3 project in the vault: the folder's own path (T3 on this machine).
    assert_eq!(
        t3_workspace_root(&draft, &vault.0, &folder, &[]).unwrap(),
        folder.to_string_lossy().replace('\\', "/")
    );
    // A code project's root is its checkout.
    let mut code = draft.clone();
    code.set_kind(Kind::Code);
    code.source = "/home/god/src/studio".into();
    assert_eq!(
        t3_workspace_root(&code, &vault.0, &folder, &listing).unwrap(),
        "/home/god/src/studio"
    );
}

/// T3 as the paired connection answers it: a listing, `project.create`,
/// `project.update` for the icon. Records every call.
#[derive(Default)]
struct FakeT3 {
    projects: Mutex<Vec<crate::Project>>,
    creates: Mutex<Vec<(String, String)>>,
    icons: Mutex<Vec<(String, Option<ProjectIcon>)>>,
    refuse_create: bool,
    refuse_icon: bool,
}

impl crate::ManagerGateway for FakeT3 {
    fn status(&self) -> GatewayStatus {
        GatewayStatus::Connected { expires_at: 0 }
    }
    fn server_identity(&self) -> GatewayResult<ServerIdentity> {
        unimplemented!()
    }
    fn list_projects(&self) -> GatewayResult<Vec<crate::Project>> {
        Ok(self.projects.lock().unwrap().clone())
    }
    fn create_project(&self, title: &str, root: &str) -> GatewayResult<crate::Project> {
        self.creates
            .lock()
            .unwrap()
            .push((title.into(), root.into()));
        if self.refuse_create {
            return Err(GatewayError::Tool {
                code: "workspace_root_not_found".into(),
                message: format!("{root} does not exist"),
            });
        }
        let project = crate::Project {
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
    ) -> GatewayResult<crate::Project> {
        self.icons.lock().unwrap().push((id.into(), icon.cloned()));
        if self.refuse_icon {
            return Err(GatewayError::Protocol(
                "T3 did not keep the project icon".into(),
            ));
        }
        let mut projects = self.projects.lock().unwrap();
        let project = projects.iter_mut().find(|p| p.id == id).unwrap();
        project.project_icon = icon.cloned();
        Ok(project.clone())
    }
    fn list_threads(&self, _: &str) -> GatewayResult<Vec<ThreadSummary>> {
        unimplemented!()
    }
    fn read_thread(&self, _: &ThreadQuery) -> GatewayResult<ThreadPage> {
        unimplemented!()
    }
    fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        unimplemented!()
    }
    fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
        unimplemented!()
    }
    fn capabilities(&self) -> GatewayResult<Capabilities> {
        unimplemented!()
    }
    fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
    fn configure_model(&self, _: &str, _: &ModelSelection) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
}

#[test]
fn t3_creates_the_project_at_the_checkout_with_the_chosen_icon() {
    let vault = Vault::new();
    let folder = vault.folder("Dev/Areas/app");
    let fake = FakeT3::default();
    let mut draft = Draft::for_folder(&folder);
    draft.name = "App".into();
    draft.set_kind(Kind::Code);
    draft.source = "/home/god/src/app".into();
    draft.icon = Some(server());
    let created = create_in_t3(&fake, &draft, &vault.0, &folder).unwrap();
    assert!(!created.reused);
    assert_eq!(created.icon_error, None);
    assert_eq!(created.project.id, "new-1");
    assert_eq!(created.project.project_icon, Some(server()));
    assert_eq!(
        *fake.creates.lock().unwrap(),
        [("App".to_owned(), "/home/god/src/app".to_owned())]
    );
    assert_eq!(
        *fake.icons.lock().unwrap(),
        [("new-1".to_owned(), Some(server()))]
    );
}

#[test]
fn a_knowledge_folder_is_created_at_the_vault_folder_on_the_t3_host() {
    let vault = Vault::new();
    vault.folder("Dev/Areas/maestro");
    let folder = vault.folder("Music/Areas/Studio");
    let fake = FakeT3 {
        projects: Mutex::new(vec![t3("m", "/srv/vault/Dev/Areas/maestro")]),
        ..Default::default()
    };
    let mut draft = Draft::for_folder(&folder);
    draft.set_create_in_t3(true);
    let created = create_in_t3(&fake, &draft, &vault.0, &folder).unwrap();
    assert_eq!(
        *fake.creates.lock().unwrap(),
        [(
            "Studio".to_owned(),
            "/srv/vault/Music/Areas/Studio".to_owned()
        )]
    );
    assert!(fake.icons.lock().unwrap().is_empty(), "no icon chosen");
    assert_eq!(
        created.project.workspace_root,
        "/srv/vault/Music/Areas/Studio"
    );
}

#[test]
fn a_t3_project_already_at_that_root_is_used_not_duplicated() {
    let vault = Vault::new();
    let folder = vault.folder("HomeLab");
    let fake = FakeT3 {
        projects: Mutex::new(vec![t3("homelab", "/srv/vault/HomeLab/")]),
        ..Default::default()
    };
    let mut draft = Draft::for_folder(&folder);
    draft.set_create_in_t3(true);
    draft.icon = Some(server());
    let created = create_in_t3(&fake, &draft, &vault.0, &folder).unwrap();
    assert!(created.reused);
    assert_eq!(created.project.id, "homelab");
    assert!(fake.creates.lock().unwrap().is_empty());
    assert_eq!(
        *fake.icons.lock().unwrap(),
        [("homelab".to_owned(), Some(server()))]
    );
}

#[test]
fn a_t3_refusal_is_an_error_and_an_icon_refusal_a_warning() {
    let vault = Vault::new();
    let folder = vault.folder("Dev/Areas/app");
    let mut draft = Draft::for_folder(&folder);
    draft.set_kind(Kind::Code);
    draft.source = "/home/god/src/missing".into();
    draft.icon = Some(server());
    let refusing = FakeT3 {
        refuse_create: true,
        ..Default::default()
    };
    let error = create_in_t3(&refusing, &draft, &vault.0, &folder).unwrap_err();
    assert!(
        format!("{error:#}").contains("workspace_root_not_found"),
        "{error:#}"
    );
    assert!(refusing.icons.lock().unwrap().is_empty());

    let no_icon = FakeT3 {
        refuse_icon: true,
        ..Default::default()
    };
    let created = create_in_t3(&no_icon, &draft, &vault.0, &folder).unwrap();
    assert_eq!(created.project.id, "new-1");
    assert!(created
        .icon_error
        .is_some_and(|e| e.contains("did not keep")));
}

#[test]
fn the_connection_most_projects_use_is_picked() {
    let vault = Vault::new();
    let a = vault.folder("A");
    let b = vault.folder("B");
    let c = vault.folder("C");
    for (folder, env) in [(&a, "maestro"), (&b, "maestro"), (&c, "lan")] {
        std::fs::create_dir_all(folder.join("ops")).unwrap();
        std::fs::write(
            folder.join(MANIFEST),
            format!("name: x\nkind: knowledge\nt3:\n  environment: {env}\n"),
        )
        .unwrap();
    }
    let projects = crate::projects::discover(&vault.0).projects;
    assert_eq!(
        pick_environment(["lan", "maestro"], &projects).as_deref(),
        Some("maestro")
    );
    assert_eq!(
        pick_environment(["other"], &projects).as_deref(),
        Some("other")
    );
    assert_eq!(
        pick_environment(std::iter::empty::<&str>(), &projects),
        None
    );
}
