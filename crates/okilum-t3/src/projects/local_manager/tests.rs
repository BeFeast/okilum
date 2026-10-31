use super::*;
use crate::{
    gateway::GatewayResult, Capabilities, GatewayError, GatewayStatus, ModelSelection,
    OutgoingMessage, SendReceipt, ServerIdentity, ThreadConfiguration, ThreadPage, ThreadQuery,
    ThreadSummary,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};

struct Fixture {
    root: PathBuf,
    area: PathBuf,
    checkout: PathBuf,
    manifest: PathBuf,
}
impl Fixture {
    fn new(toml: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("okilum-local-manager-{}", uuid::Uuid::new_v4()));
        let area = root.join("notes");
        let checkout = root.join("checkout");
        std::fs::create_dir_all(area.join("ops")).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        let manifest = area.join(if toml {
            "ops/project.toml"
        } else {
            "ops/project.yaml"
        });
        std::fs::write(&manifest, if toml {
            "# Keep heading\nname='Pulse'\nkind='code'\ncustom='Keep #42'\n[execution]\nsource_checkout='/remote/pulse'\ncommand='keep me' # Keep command\n[t3]\nschema_version=1\nenvironment='old-server'\nexecutor_project_id='old-project'\n[t3.manager]\nworkspace_mode='local'\n"
        } else {
            "# Keep heading\nname: Pulse\nkind: code\nexecution:\n  source_checkout: /remote/pulse\n  command: 'keep me' # Keep command\nt3:\n  schema_version: 1\n  environment: old-server\n  executor_project_id: old-project\n  manager: {project_id: null, thread_id: null, workspace_mode: local}\n# Keep footer\ncustom: 'Keep #42'\n"
        }).unwrap();
        Self {
            root,
            area,
            checkout,
            manifest,
        }
    }
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.manifest).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Fake {
    identity: Mutex<String>,
    projects: Mutex<Vec<crate::Project>>,
    calls: AtomicUsize,
    creates: AtomicUsize,
    lost_create: AtomicBool,
    wrong_root: Option<PathBuf>,
    edit_on_read: Mutex<Option<(PathBuf, String)>>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            identity: Mutex::new("device-A".into()),
            projects: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
            creates: AtomicUsize::new(0),
            lost_create: AtomicBool::new(false),
            wrong_root: None,
            edit_on_read: Mutex::new(None),
        }
    }
}
impl ManagerGateway for Fake {
    fn status(&self) -> GatewayStatus {
        GatewayStatus::Connected { expires_at: 0 }
    }
    fn server_identity(&self) -> GatewayResult<ServerIdentity> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ServerIdentity {
            environment_id: self.identity.lock().unwrap().clone(),
            label: "Local T3".into(),
        })
    }
    fn list_projects(&self) -> GatewayResult<Vec<crate::Project>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.projects.lock().unwrap().clone())
    }
    fn create_project(&self, title: &str, root: &str) -> GatewayResult<crate::Project> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        let p = crate::Project {
            id: "local-project".into(),
            title: title.into(),
            workspace_root: root.into(),
            ..Default::default()
        };
        self.projects.lock().unwrap().push(p.clone());
        if self.lost_create.swap(false, Ordering::SeqCst) {
            return Err(GatewayError::Unreachable("lost create response".into()));
        }
        Ok(p)
    }
    fn read_project(&self, id: &str) -> GatewayResult<crate::Project> {
        let mut p = self
            .list_projects()?
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        if let Some(root) = &self.wrong_root {
            p.workspace_root = root.to_string_lossy().into_owned();
        }
        if let Some((path, text)) = self.edit_on_read.lock().unwrap().take() {
            std::fs::write(path, text).unwrap();
        }
        Ok(p)
    }
    fn start_manager(&self, _: &str, _: &str, _: &str) -> GatewayResult<String> {
        panic!("setup cannot launch a thread")
    }
    fn list_threads(&self, _: &str) -> GatewayResult<Vec<ThreadSummary>> {
        panic!("setup cannot select a thread")
    }
    fn read_thread(&self, _: &ThreadQuery) -> GatewayResult<ThreadPage> {
        unimplemented!()
    }
    fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        panic!("setup cannot send")
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
fn local_setup_binds_manager_at_exact_checkout_and_preserves_other_manifest_bytes() {
    for toml in [false, true] {
        let f = Fixture::new(toml);
        let g = Fake::default();
        let env = environment("127.0.0.1:3778").unwrap();
        let session = Session::load(&f.area).unwrap();
        let stamp = std::fs::metadata(&f.manifest).unwrap().modified().unwrap();
        assert_eq!(session.suggested_workspace(), "/remote/pulse");
        assert_eq!(
            std::fs::metadata(&f.manifest).unwrap().modified().unwrap(),
            stamp
        );
        let project = session
            .bind(&g, &env, f.checkout.to_str().unwrap())
            .unwrap();
        assert_eq!(project.manager_project.as_deref(), Some("local-project"));
        assert_eq!(project.manager, Manager::NotStarted);
        assert_eq!(g.creates.load(Ordering::SeqCst), 1);
        let binding = project.native_binding().unwrap().unwrap();
        assert_eq!(binding.manager.project_id.as_deref(), Some("local-project"));
        assert!(binding.manager.thread_id.is_none());
        let text = std::fs::read_to_string(&f.manifest).unwrap();
        assert!(text.starts_with("# Keep heading\n"));
        assert!(text.contains(if toml {
            "custom='Keep #42'"
        } else {
            "# Keep footer\ncustom: 'Keep #42'"
        }));
        assert!(text.contains(if toml {
            "command='keep me' # Keep command"
        } else {
            "  command: 'keep me' # Keep command"
        }));
        let document = area_files::read(&f.area, "project").unwrap().unwrap();
        assert_eq!(
            document.value["execution"]["command"].as_str(),
            Some("keep me")
        );
        assert_eq!(
            document.value["t3"]["origin"].as_str(),
            Some(env.origin().as_str())
        );
        assert!(verify_launch(&f.area, &env, &g, "local-project")
            .unwrap()
            .is_some());
        *g.identity.lock().unwrap() = "device-B".into();
        assert!(verify_launch(&f.area, &env, &g, "local-project")
            .unwrap_err()
            .to_string()
            .contains("environment changed"));
        assert!(
            Session::load(&f.area).is_err(),
            "an established binding cannot be silently replaced"
        );
    }
}

#[test]
fn remote_origins_unavailable_local_folders_and_stale_forms_cannot_create_or_write() {
    for address in [
        "https://t3.oklabs.uk",
        "http://10.10.0.23:23110",
        "http://localhost.example:3778",
    ] {
        assert!(environment(address).is_err());
    }
    for address in ["localhost:3778", "http://[::1]:3778", "127.0.0.1:23110"] {
        assert!(environment(address).is_ok());
    }
    let f = Fixture::new(false);
    let g = Fake::default();
    let env = environment("localhost:3778").unwrap();
    let before = f.bytes();
    let s = Session::load(&f.area).unwrap();
    let missing = f.root.join("missing");
    for root in [
        "relative/folder",
        f.manifest.to_str().unwrap(),
        missing.to_str().unwrap(),
    ] {
        assert!(s.bind(&g, &env, root).is_err());
    }
    assert_eq!(g.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.bytes(), before);
    std::fs::write(
        &f.manifest,
        format!("{}\n# External edit\n", String::from_utf8(before).unwrap()),
    )
    .unwrap();
    let changed = f.bytes();
    assert!(s.bind(&g, &env, f.checkout.to_str().unwrap()).is_err());
    assert_eq!(g.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.bytes(), changed);
    let fresh = Session::load(&f.area).unwrap();
    std::fs::write(
        f.area.join("ops/thread-ids.md"),
        "| Role | Thread |\n|---|---|\n| Manager | `mcp:external` |\n",
    )
    .unwrap();
    assert!(fresh.bind(&g, &env, f.checkout.to_str().unwrap()).is_err());
    assert_eq!(g.creates.load(Ordering::SeqCst), 0);
}

#[test]
fn uncertain_project_create_reuses_the_exact_project_without_launching_or_creating_twice() {
    let f = Fixture::new(false);
    let g = Fake::default();
    g.lost_create.store(true, Ordering::SeqCst);
    let env = environment("localhost:3778").unwrap();
    let before = f.bytes();
    let s = Session::load(&f.area).unwrap();
    assert!(s.bind(&g, &env, f.checkout.to_str().unwrap()).is_err());
    assert_eq!(f.bytes(), before);
    assert_eq!(g.creates.load(Ordering::SeqCst), 1);
    let project = s.bind(&g, &env, f.checkout.to_str().unwrap()).unwrap();
    assert_eq!(project.manager_project.as_deref(), Some("local-project"));
    assert_eq!(g.creates.load(Ordering::SeqCst), 1);
    assert!(!f.area.join("ops/thread-ids.md").exists());
}

#[test]
fn ambiguous_workspaces_wrong_receipts_and_concurrent_edits_keep_the_manifest() {
    let f = Fixture::new(true);
    let env = environment("localhost:3778").unwrap();
    let before = f.bytes();
    let s = Session::load(&f.area).unwrap();
    let target = crate::Project {
        id: "first".into(),
        workspace_root: f.checkout.to_str().unwrap().into(),
        ..Default::default()
    };
    let g = Fake::default();
    *g.projects.lock().unwrap() = vec![
        target.clone(),
        crate::Project {
            id: "second".into(),
            ..target.clone()
        },
    ];
    assert!(s
        .bind(&g, &env, f.checkout.to_str().unwrap())
        .unwrap_err()
        .to_string()
        .contains("multiple projects"));
    assert_eq!(g.creates.load(Ordering::SeqCst), 0);
    assert_eq!(f.bytes(), before);
    let g = Fake {
        wrong_root: Some(f.area.clone()),
        ..Default::default()
    };
    g.projects.lock().unwrap().push(target.clone());
    assert!(s
        .bind(&g, &env, f.checkout.to_str().unwrap())
        .unwrap_err()
        .to_string()
        .contains("workspace changed"));
    assert_eq!(f.bytes(), before);
    let g = Fake::default();
    g.projects.lock().unwrap().push(target);
    let external = format!("{}\n# Concurrent\n", String::from_utf8(before).unwrap());
    *g.edit_on_read.lock().unwrap() = Some((f.manifest.clone(), external.clone()));
    assert!(s
        .bind(&g, &env, f.checkout.to_str().unwrap())
        .unwrap_err()
        .to_string()
        .contains("Project changed"));
    assert_eq!(std::fs::read_to_string(&f.manifest).unwrap(), external);
}

#[test]
fn launch_refuses_changed_checkout_and_legacy_bindings_are_not_reinterpreted() {
    let f = Fixture::new(false);
    let g = Fake::default();
    let env = environment("localhost:3778").unwrap();
    assert!(verify_launch(
        &f.area,
        &Environment::new("remote", "https://example.com").unwrap(),
        &g,
        "old-project"
    )
    .unwrap()
    .is_none());
    assert_eq!(g.calls.load(Ordering::SeqCst), 0);
    Session::load(&f.area)
        .unwrap()
        .bind(&g, &env, f.checkout.to_str().unwrap())
        .unwrap();
    let mut doc = area_files::read(&f.area, "project").unwrap().unwrap();
    // An external edit changes the checkout but leaves the pinned workspace alone.
    doc.value["execution"]["source_checkout"] = Value::from(f.area.to_str().unwrap());
    std::fs::write(&f.manifest, serde_yaml::to_string(&doc.value).unwrap()).unwrap();
    assert!(verify_launch(&f.area, &env, &g, "local-project")
        .unwrap_err()
        .to_string()
        .contains("checkout changed"));
}
