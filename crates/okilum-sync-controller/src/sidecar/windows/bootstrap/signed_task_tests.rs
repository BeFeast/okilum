//! Production Connect/Stop acceptance, driven by the signed hosted fixture.
//! No alternative launch, peer policy or account state path is available here.
use super::*;
use crate::{direct::Controller, managed_launch::Generation};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    identity: DaemonIdentity,
    generation: Generation,
    certificate: String,
    key: String,
    pairing_seed: String,
    peers: serde_json::Value,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn observed(identity: DaemonIdentity, controller: &Controller) -> Result<Evidence> {
    let home = identity
        .config_file
        .parent()
        .context("fixture home missing")?;
    let direct = active_state_directory()?.with_file_name("SyncDirect");
    let journal: serde_json::Value =
        serde_json::from_slice(&private::read(&direct.join("direct.json"))?)?;
    ensure!(journal["seed"].is_array(), "pairing identity missing");
    Ok(Evidence {
        generation: crate::managed_launch::live_generation(home)?,
        certificate: hash(&private::public_file(&home.join("cert.pem"))?),
        key: hash(&private::public_file(&home.join("key.pem"))?),
        pairing_seed: hash(&serde_json::to_vec(&journal["seed"])?),
        peers: serde_json::to_value(controller.snapshot()?.peers)?,
        identity,
    })
}
fn same_identity(before: &Evidence, after: &Evidence) {
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.certificate, after.certificate);
    assert_eq!(before.key, after.key);
    assert_eq!(before.pairing_seed, after.pairing_seed);
    assert_eq!(before.peers, after.peers);
    assert_eq!(after.peers.as_array().unwrap().len(), 1);
}

#[test]
#[ignore = "requires the disposable hosted account and signed installed fixture"]
fn windows_signed_task_survives_package_move_and_owns_stop() -> Result<()> {
    ensure!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_OS").as_deref() == Ok("Windows"),
        "native task acceptance requires the disposable hosted Windows account"
    );
    let access = okilum_sync::NetworkAccess::new(true);
    let _scope = access.enter();
    let state = active_state_directory()?;
    ensure!(
        PathBuf::from(
            std::env::var_os("OKILUM_NATIVE_TASK_FIXTURE_STATE")
                .context("fixture state missing")?
        ) == state,
        "fixture state must be the actual native account state"
    );
    let evidence = PathBuf::from(
        std::env::var_os("OKILUM_NATIVE_TASK_FIXTURE_EVIDENCE")
            .context("fixture evidence missing")?,
    );
    let phase = std::env::var("OKILUM_NATIVE_TASK_FIXTURE_PHASE")?;
    if phase == "cleanup" {
        return disable_current_app();
    }
    let direct = state.with_file_name("SyncDirect");
    if phase == "initial" {
        ensure!(
            !state.exists() && !direct.exists(),
            "fixture refuses existing account state"
        );
        assert!(connect_current_app(false).is_err());
        assert!(!state.exists(), "refused consent created native state");
        let identity = connect_current_app(true)?;
        let controller = Controller::open(direct, vec![])?;
        let other_root = tempfile::tempdir()?;
        let other = Controller::open(other_root.path().join("other"), vec![])?;
        let invitation = controller
            .begin_pairing_at(
                "Native installed fixture",
                "127.0.0.1:0".parse()?,
                "127.0.0.1".parse()?,
            )?
            .offer_url
            .context("fixture invitation missing")?;
        other.redeem(&invitation, "Isolated second computer")?;
        let pending = controller
            .snapshot()?
            .pending
            .context("fixture request missing")?;
        controller.approve(pending.request_id)?;
        assert_eq!(other.refresh_pairing()?.peers.len(), 1);
        let saved = observed(identity, &controller)?;
        assert_eq!(saved.peers.as_array().unwrap().len(), 1);
        std::fs::write(evidence, serde_json::to_vec(&saved)?)?;
        other.shutdown();
        controller.shutdown();
        // Leave the scheduled helper alive across package removal, like an update.
        return Ok(());
    }
    ensure!(phase == "resumed", "unknown native fixture phase");
    let before: Evidence = serde_json::from_slice(&std::fs::read(evidence)?)?;
    let identity = connect_current_app(true)?;
    let controller = Controller::open(direct, vec![])?;
    let resumed = observed(identity, &controller)?;
    same_identity(&before, &resumed);
    assert_eq!(
        before.generation, resumed.generation,
        "package update restarted the retained native task"
    );
    disable_current_app()?;
    assert!(
        crate::managed_launch::live_generation(before.identity.config_file.parent().unwrap())
            .is_err()
    );
    let identity = connect_current_app(true)?;
    let restarted = observed(identity, &controller)?;
    same_identity(&before, &restarted);
    assert_ne!(before.generation.id, restarted.generation.id);
    disable_current_app()?;
    assert!(
        crate::managed_launch::live_generation(before.identity.config_file.parent().unwrap())
            .is_err()
    );
    controller.shutdown();
    Ok(())
}
