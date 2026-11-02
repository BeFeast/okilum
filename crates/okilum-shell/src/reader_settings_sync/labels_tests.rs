use super::*;
use anyhow::{Context as _, Result};
use okilum_sync_controller::daemon::{DaemonIdentity, Preparation};
use std::process::{Child, Command, Stdio};

struct Managed {
    _root: tempfile::TempDir,
    state: PathBuf,
    vault: PathBuf,
    identity: DaemonIdentity,
    child: Child,
}
impl Managed {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let parent = root.path().join("sync");
        std::fs::create_dir(&parent)?;
        let managed = parent.join("managed");
        std::fs::create_dir(&managed)?;
        let prepared = managed.join("prepared");
        let binary = PathBuf::from(
            std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT")
                .context("set checksum-pinned Syncthing 2.1.6 binary")?,
        )
        .canonicalize()?;
        let rest = std::net::TcpListener::bind("127.0.0.1:0")?;
        let listen = std::net::TcpListener::bind("127.0.0.1:0")?;
        let (instance, identity) = okilum_sync_controller::daemon::prepare(
            &prepared,
            &Preparation {
                executable: binary.clone(),
                rest_address: rest.local_addr()?,
                listen_address: listen.local_addr()?,
            },
        )?;
        drop(rest);
        drop(listen);
        let child = Command::new(binary)
            .env_clear()
            .env("HOME", &instance.home)
            .env("GOMAXPROCS", "2")
            .env("STMONITORED", "1")
            .args([
                "serve",
                "--no-browser",
                "--no-restart",
                "--no-upgrade",
                "--home",
            ])
            .arg(&instance.home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let vault = root.path().join("Notes");
        std::fs::create_dir(&vault)?;
        let fixture = Self {
            state: parent.join("direct"),
            vault,
            identity,
            child,
            _root: root,
        };
        let start = std::time::Instant::now();
        while fixture.identity.connect().is_err() {
            anyhow::ensure!(
                start.elapsed() < Duration::from_secs(20),
                "fixture REST never became ready"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(fixture.identity.connect()?.version()?["version"], "v2.1.6");
        Ok(fixture)
    }
    fn perform(&self, controller: Option<Controller>, operation: Operation) -> Result<Output> {
        perform_with_locations(controller, operation, &Epoch::default().ticket(), || {
            Ok((self.state.clone(), vec![self.identity.config_file.clone()]))
        })
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6 in OKILUM_SYNC_DIRECT_CLIENT"]
fn sync_labels_share_and_attach_publish_current_count_without_refresh() -> Result<()> {
    let source = Managed::new()?;
    let target = Managed::new()?;
    let initial = source.perform(None, Operation::Read)?;
    assert_eq!(
        initial.runtimes[0].folders, 0,
        "empty inventory positive control"
    );
    assert!(initial.runtimes[0].managed);
    let shared = source.perform(
        Some(initial.controller),
        Operation::Share(source.vault.clone()),
    )?;
    assert_eq!(
        shared.runtimes[0].folders, 1,
        "Share result itself must contain fresh count"
    );
    let url = shared
        .controller
        .begin_pairing_at("Source", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
        .offer_url
        .unwrap();
    let empty_target = target.perform(None, Operation::Read)?;
    assert_eq!(empty_target.runtimes[0].folders, 0);
    empty_target.controller.redeem(&url, "Target")?;
    let pending = shared.controller.snapshot()?.pending.unwrap();
    shared.controller.approve(pending.request_id)?;
    let paired = empty_target.controller.refresh()?;
    let peer = paired.peers[0].id;
    let catalog = empty_target.controller.catalog(peer)?;
    let attached = target.perform(
        Some(empty_target.controller),
        Operation::Attach(peer, catalog[0].id.clone(), target.vault.clone()),
    )?;
    assert_eq!(
        attached.runtimes[0].folders, 1,
        "Attach result itself must contain fresh count"
    );
    assert_eq!(attached.snapshot.attached[0].label, catalog[0].label);
    assert_eq!(
        super::super::super::vault_name::read_sync_names(target._root.path()),
        vec![(target.vault.clone(), catalog[0].label.clone())]
    );
    let external = linux_runtime::inspect(
        &target._root.path().join("external/direct"),
        std::slice::from_ref(&target.identity.config_file),
    )?;
    assert!(
        !external.choices[0].managed,
        "external inventory has no owned preparation receipt"
    );
    shared.controller.shutdown();
    attached.controller.shutdown();
    Ok(())
}

#[gpui::test]
fn sync_labels_managed_external_and_paused_wording(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture(window, cx));
        Root::new(view, window, cx)
    });
    let root = tempfile::tempdir().unwrap();
    let daemon = DaemonIdentity {
        config_file: root.path().join("config.xml"),
        device_id: "fixture".into(),
        certificate_hash: "fixture".into(),
    };
    let mut vault = ExistingVault {
        daemon: daemon.clone(),
        version: "v2.1.6".into(),
        folder_id: "notes".into(),
        path: root.path().into(),
        label: "Notes".into(),
        paused: false,
        folder_type: "sendreceive".into(),
        peers: vec![],
        root_identity: None,
    };
    visual.update(|window, cx| {
        let view = cx.new(|cx| fixture(window, cx));
        view.update(cx, |view, _| {
            view.runtimes = vec![linux_runtime::Choice {
                identity: daemon,
                folders: 1,
                managed: true,
                selected: true,
            }];
            assert_eq!(view.vault_connection_label(&vault), "Synced by Okilum");
            view.runtimes[0].managed = false;
            assert_eq!(
                view.vault_connection_label(&vault),
                "Uses your existing Syncthing"
            );
            vault.paused = true;
            assert_eq!(
                view.vault_connection_label(&vault),
                "Uses your existing Syncthing · Paused"
            );
            view.runtimes[0].managed = true;
            assert_eq!(
                view.vault_connection_label(&vault),
                "Synced by Okilum · Paused"
            );
        });
    });
}

#[gpui::test]
fn sync_labels_catalog_fallback_keeps_explicit_name_and_reset(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    let vault = root.path().join("local-replica");
    std::fs::create_dir(&vault).unwrap();
    cx.update(|cx| {
        crate::vault_name::remember_sync(vec![(vault.clone(), "Catalog notes".into())], cx);
        assert_eq!(crate::vault_name::shown(&vault, cx), "Catalog notes");
        assert!(!vault.join(".okilum").exists());
        crate::vault_name::set(&vault, "My notes", cx);
        crate::vault_name::remember_sync(vec![(vault.clone(), "Catalog notes".into())], cx);
        assert_eq!(crate::vault_name::shown(&vault, cx), "My notes");
        crate::vault_name::set(&vault, "", cx);
        assert_eq!(crate::vault_name::shown(&vault, cx), "local-replica");
    });
}
