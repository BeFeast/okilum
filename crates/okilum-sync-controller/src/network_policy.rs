//! Computer-wide managed transport policy, independent of any vault or index.
//! Existing Syncthing is read-only here: no preparation receipt, no write authority.
use crate::{
    daemon::{self, DaemonIdentity},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::SocketAddr, path::Path};

pub(crate) const RELAY_LISTENER: &str = "dynamic+https://relays.syncthing.net/endpoint";
const JOURNAL: &str = "network-policy-v1.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Policy {
    PublicDiscoveryAndRelays,
    LocalNetworkAndVpn,
}
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub global_discovery: bool,
    pub relays: bool,
    pub nat: bool,
    pub stun: bool,
}
impl State {
    pub fn local_only(&self) -> bool {
        !self.global_discovery && !self.relays && !self.nat && !self.stun
    }
    pub fn description(&self) -> String {
        let state = |on| if on { "on" } else { "off" };
        format!(
            "Public discovery: {} · Relays: {} · Router connections: {} · Address lookup: {}",
            state(self.global_discovery),
            state(self.relays),
            state(self.nat),
            state(self.stun)
        )
    }
}
fn state(config: &Value) -> Result<State> {
    let options = &config["options"];
    let flag = |key: &str| {
        options[key]
            .as_bool()
            .context("network policy option unavailable")
    };
    Ok(State {
        global_discovery: flag("globalAnnounceEnabled")?,
        relays: flag("relaysEnabled")?,
        nat: flag("natEnabled")?,
        stun: flag("natEnabled")?
            && options["stunKeepaliveStartS"]
                .as_u64()
                .context("network policy STUN unavailable")?
                > 0
            && options["stunKeepaliveMinS"]
                .as_u64()
                .context("network policy STUN unavailable")?
                > 0,
    })
}
/// A read cannot grant authority to modify an external runtime.
pub fn read(identity: &DaemonIdentity) -> Result<State> {
    state(&identity.connect()?.config()?)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    identity: DaemonIdentity,
    policy: Policy,
    applied: bool,
}
fn changes(config: &Value, listen: SocketAddr, policy: Policy) -> Result<Value> {
    state(config)?;
    let tcp = format!("tcp://{listen}");
    let quic = format!("quic://{listen}");
    let addresses = config["options"]["listenAddresses"]
        .as_array()
        .context("network policy listen addresses unavailable")?;
    ensure!(
        !addresses.is_empty()
            && addresses.iter().all(|a| a
                .as_str()
                .is_some_and(|s| s == tcp || s == quic || s == RELAY_LISTENER)),
        "network policy custom listener requires review"
    );
    let mut local: Vec<Value> = addresses
        .iter()
        .filter(|a| a.as_str() != Some(RELAY_LISTENER))
        .cloned()
        .collect();
    ensure!(
        !local.is_empty(),
        "network policy local listener unavailable"
    );
    let public = policy == Policy::PublicDiscoveryAndRelays;
    if public {
        local.push(json!(RELAY_LISTENER));
    } else {
        // Explicit relay addresses bypass dynamic discovery. Never silently rewrite
        // a device or claim local-only mode with an unsupported route still active.
        for device in config["devices"]
            .as_array()
            .context("network policy devices unavailable")?
        {
            ensure!(
                device["addresses"]
                    .as_array()
                    .context("network policy device addresses unavailable")?
                    .iter()
                    .all(|a| a.as_str().is_some_and(|s| s == "dynamic"
                        || s.strip_prefix("tcp://")
                            .or_else(|| s.strip_prefix("quic://"))
                            .is_some_and(|s| s.parse::<SocketAddr>().is_ok()))),
                "network policy custom peer route requires review"
            );
        }
    }
    Ok(json!({
        "globalAnnounceEnabled": public,
        "localAnnounceEnabled": true,
        "relaysEnabled": public,
        "natEnabled": public,
        "stunKeepaliveStartS": if public { 180 } else { 0 },
        "stunKeepaliveMinS": if public { 20 } else { 0 },
        "listenAddresses": local,
    }))
}
/// `None` migrates pre-2047 managed installs once, then only reads. An explicit
/// choice replaces the desired policy. The durable intent precedes PATCH, so a
/// crash retries the chosen policy rather than re-enabling public transport.
/// The selected identity must match the pinned private preparation receipt.
pub fn reconcile(
    prepared: &Path,
    selected: &DaemonIdentity,
    choice: Option<Policy>,
) -> Result<State> {
    ensure!(
        daemon::managed_transport_identity(prepared)? == *selected,
        "network policy selected managed identity changed"
    );
    let _lock = private::lock(prepared)?;
    let api = selected.connect()?;
    let file = prepared.join(JOURNAL);
    let saved: Option<Journal> = if file.try_exists()? {
        Some(serde_json::from_slice(&private::read(&file)?)?)
    } else {
        None
    };
    if let Some(saved) = &saved {
        ensure!(
            saved.identity == *selected,
            "network policy journal identity changed"
        );
        if choice.is_none() && saved.applied {
            return state(&api.config()?);
        }
    }
    let policy = choice
        .or(saved.as_ref().map(|s| s.policy))
        .unwrap_or(Policy::PublicDiscoveryAndRelays);
    let _admission = crate::managed_launch::runtime_configuration(selected)?;
    let config = api.config()?;
    let changes = changes(&config, daemon::managed_listen_address(prepared)?, policy)?;
    let mut intent = Journal {
        identity: selected.clone(),
        policy,
        applied: false,
    };
    private::write(&file, &serde_json::to_vec(&intent)?)?;
    if changes
        .as_object()
        .unwrap()
        .iter()
        .any(|(k, v)| config["options"][k] != *v)
    {
        api.patch_options(&changes)?;
    }
    let actual = api.config()?;
    for (key, expected) in changes.as_object().unwrap() {
        ensure!(
            actual["options"][key] == *expected,
            "network policy read-back mismatch: {key}"
        );
    }
    intent.applied = true;
    private::write(&file, &serde_json::to_vec(&intent)?)?;
    state(&actual)
}
