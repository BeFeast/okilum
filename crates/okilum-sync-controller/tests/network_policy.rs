//! Real pinned runtimes in an isolated network namespace, never the public internet.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "support/direct_daemon.rs"]
mod direct_daemon;
use anyhow::{Context, Result};
use direct_daemon::Peer;
use okilum_sync_controller::{
    daemon,
    network_policy::{self, Policy},
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

fn offline_binary() -> Result<PathBuf> {
    anyhow::ensure!(
        std::env::var_os("OKILUM_SYNC_OFFLINE_NAMESPACE").is_some(),
        "run scripts/ci/sync-network-policy.sh inside its network namespace"
    );
    // Positive isolation check, independent of a caller's environment claim.
    let interfaces = fs::read_to_string("/proc/net/dev")?;
    anyhow::ensure!(
        interfaces
            .lines()
            .skip(2)
            .all(|line| line.split(':').next().unwrap().trim() == "lo"),
        "network fixture must have only loopback"
    );
    let binary = PathBuf::from(
        std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT").context("pinned runtime missing")?,
    )
    .canonicalize()?;
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&binary)?)),
        "d72663e4f9efe0b8de5e1dacb13c1c8decc82e76623cf2fa47fd306f3e1d3a3e"
    );
    Ok(binary)
}
fn options(public: bool, listen: std::net::SocketAddr) -> Value {
    let mut addresses = vec![format!("tcp://{listen}")];
    if public {
        addresses.push("dynamic+https://relays.syncthing.net/endpoint".into());
    }
    json!({"globalAnnounceEnabled":public,"localAnnounceEnabled":true,
        "relaysEnabled":public,"natEnabled":public,"stunKeepaliveStartS":if public {180} else {0},
        "stunKeepaliveMinS":if public {20} else {0},
        "listenAddresses":addresses})
}
fn assert_options(peer: &Peer, public: bool) -> Result<()> {
    let actual = peer.api.config()?;
    for (key, value) in options(public, peer.listen).as_object().unwrap() {
        assert_eq!(actual["options"][key], *value, "read-back {key}");
    }
    Ok(())
}
fn unrelated(config: &Value) -> Value {
    let mut config = config.clone();
    for key in options(true, "0.0.0.0:22000".parse().unwrap())
        .as_object()
        .unwrap()
        .keys()
    {
        config["options"].as_object_mut().unwrap().remove(key);
    }
    config
}

#[test]
#[ignore = "requires pinned runtime and offline namespace; scripts/ci/sync-network-policy.sh"]
fn managed_network_default_and_switch_round_trip_read_back() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (mut peer, prepared) = Peer::managed_network(root.path(), "managed", offline_binary()?)?;
    assert_options(&peer, true)?;
    let identity = daemon::managed_identity(&prepared)?;
    let original = peer.api.config()?;
    let public = network_policy::reconcile(&prepared, &identity, None)?;
    assert!(public.global_discovery && public.relays && public.nat && public.stun);
    let local = network_policy::reconcile(&prepared, &identity, Some(Policy::LocalNetworkAndVpn))?;
    assert!(local.local_only());
    assert_options(&peer, false)?;
    assert_eq!(unrelated(&peer.api.config()?), unrelated(&original));
    // Restart proves the switch was saved by Syncthing and its local journal.
    peer.stop()?;
    peer.start()?;
    assert!(network_policy::reconcile(&prepared, &identity, None)?.local_only());
    assert_options(&peer, false)?;
    network_policy::reconcile(&prepared, &identity, Some(Policy::PublicDiscoveryAndRelays))?;
    assert_options(&peer, true)?;
    assert_eq!(unrelated(&peer.api.config()?), unrelated(&original));
    Ok(())
}
#[test]
#[ignore = "requires pinned runtime and offline namespace; scripts/ci/sync-network-policy.sh"]
fn managed_network_legacy_migration_happens_once_outside_index() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (peer, prepared) = Peer::managed_lan(root.path(), "legacy-7504-7510", offline_binary()?)?;
    assert_eq!(peer.api.config()?["options"]["relaysEnabled"], false);
    let identity = daemon::managed_identity(&prepared)?;
    let journal = prepared.join("network-policy-v1.json");
    assert!(!journal.exists());
    let original = peer.api.config()?;
    network_policy::reconcile(&prepared, &identity, None)?;
    assert_options(&peer, true)?;
    let receipt = fs::read(&journal)?;
    assert_eq!(unrelated(&peer.api.config()?), unrelated(&original));
    assert!(!prepared.join(".okilum-index").exists());
    // A new local choice is durable; the next launch cannot migrate it back.
    network_policy::reconcile(&prepared, &identity, Some(Policy::LocalNetworkAndVpn))?;
    let local_receipt = fs::read(&journal)?;
    assert_ne!(receipt, local_receipt);
    network_policy::reconcile(&prepared, &identity, None)?;
    assert_eq!(fs::read(&journal)?, local_receipt);
    assert_options(&peer, false)?;
    // Applied migration is not an every-refresh rewrite of runtime options.
    peer.api
        .patch_options(&json!({"localAnnounceEnabled":false}))?;
    network_policy::reconcile(&prepared, &identity, None)?;
    assert_eq!(peer.api.config()?["options"]["localAnnounceEnabled"], false);
    Ok(())
}
#[test]
#[ignore = "requires pinned runtime and offline namespace; scripts/ci/sync-network-policy.sh"]
fn managed_network_pending_choice_recovers_without_public_reset() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (peer, prepared) = Peer::managed_network(root.path(), "interrupted", offline_binary()?)?;
    let identity = daemon::managed_identity(&prepared)?;
    network_policy::reconcile(&prepared, &identity, None)?;
    let file = prepared.join("network-policy-v1.json");
    let mut intent: Value = serde_json::from_slice(&fs::read(&file)?)?;
    intent["policy"] = json!("LocalNetworkAndVpn");
    intent["applied"] = json!(false);
    fs::write(&file, serde_json::to_vec(&intent)?)?;
    assert!(network_policy::reconcile(&prepared, &identity, None)?.local_only());
    assert_options(&peer, false)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(file)?)?["applied"],
        true
    );
    Ok(())
}
#[test]
#[ignore = "requires pinned runtime and offline namespace; scripts/ci/sync-network-policy.sh"]
fn managed_network_external_options_unchanged_negative_control() -> Result<()> {
    let root = tempfile::tempdir()?;
    let binary = offline_binary()?;
    let (managed, prepared) = Peer::managed_network(root.path(), "owned", binary.clone())?;
    let external = Peer::new(root.path(), "external", binary)?;
    let identity = daemon::discover(&[external.home.join("config.xml")])
        .candidates
        .remove(0)
        .identity;
    let before = external.api.config()?;
    let current = network_policy::read(&identity)?;
    assert_eq!(
        current.global_discovery,
        before["options"]["globalAnnounceEnabled"]
    );
    assert_eq!(current.relays, before["options"]["relaysEnabled"]);
    // Passing a managed receipt for the wrong selected runtime grants no authority.
    assert!(network_policy::reconcile(
        &prepared,
        &identity,
        Some(Policy::PublicDiscoveryAndRelays)
    )
    .is_err());
    assert_eq!(external.api.config()?, before);
    assert!(network_policy::reconcile(
        &external.home,
        &identity,
        Some(Policy::PublicDiscoveryAndRelays)
    )
    .is_err());
    assert!(!external.home.join("network-policy-v1.json").exists());
    assert_eq!(external.api.config()?, before);
    assert!(!prepared.join("network-policy-v1.json").exists());
    // Positive control: the same operation does change the authenticated owned runtime.
    let owned = daemon::managed_identity(&prepared)?;
    network_policy::reconcile(&prepared, &owned, Some(Policy::LocalNetworkAndVpn))?;
    assert_options(&managed, false)?;
    assert_eq!(external.api.config()?, before);
    Ok(())
}
#[test]
#[ignore = "requires pinned runtime and offline namespace; scripts/ci/sync-network-policy.sh"]
fn managed_network_custom_relay_route_refuses_false_local_claim() -> Result<()> {
    let root = tempfile::tempdir()?;
    let binary = offline_binary()?;
    let (peer, prepared) = Peer::managed_network(root.path(), "custom", binary.clone())?;
    let other = Peer::new(root.path(), "other", binary)?;
    peer.api.add_device(
        &json!({"deviceID":other.id,"addresses":["relay://127.0.0.1:1"],"paused":true}),
    )?;
    let identity = daemon::managed_identity(&prepared)?;
    let before = peer.api.config()?;
    assert!(
        network_policy::reconcile(&prepared, &identity, Some(Policy::LocalNetworkAndVpn)).is_err()
    );
    assert_eq!(peer.api.config()?, before);
    assert!(!prepared.join("network-policy-v1.json").exists());
    // #2407 explicit TCP hints + dynamic remain valid in local mode.
    peer.api.set_device_addresses(
        &other.id,
        &[format!("tcp://{}", other.listen), "dynamic".into()],
    )?;
    network_policy::reconcile(&prepared, &identity, Some(Policy::LocalNetworkAndVpn))?;
    assert_options(&peer, false)?;
    assert_eq!(
        peer.api.device(&other.id)?["addresses"],
        json!([format!("tcp://{}", other.listen), "dynamic"])
    );
    Ok(())
}
