//! Production opt-in composition. No caller-supplied executable, HOME or daemon path.
//! Only Ready contains an authenticated live REST identity; approval is not readiness.
use super::{
    bundle::{self, BundleGuard, NativeSignature},
    connect::{complete_explicit_connect, verify_rest, ConnectRegistration},
    inventory,
    payload::Payload,
    preparation,
    recovery::ManagedStatus,
    registration::NativeRegistrationGuard,
    Status,
};
use crate::{
    daemon::{DaemonIdentity, CLIENT_VERSION},
    private,
    sidecar::{
        store::{Selection, UnixStore},
        supervisor::ipc::code_requirement::CodeRequirement,
        Binding,
    },
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    net::TcpListener,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    process::Command,
    time::Instant,
};

pub use super::connect::{ConnectBudget, ConnectOutcome};

pub struct SigningPolicy {
    pub app: CodeRequirement,
    pub helper: CodeRequirement,
    pub runtime: CodeRequirement,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    identity: DaemonIdentity,
}

/// Explicit Add computer / Connect action only; never call while opening Settings.
/// Existing state is retained and verified, never repaired or regenerated here.
/// An error/approval response must never trigger a fallback daemon spawn.
pub fn connect_current_app(
    confirmed_connect: bool,
    policy: SigningPolicy,
    budget: ConnectBudget,
) -> Result<ConnectOutcome> {
    ensure!(confirmed_connect, "explicit Connect consent required");
    ensure!(
        okilum_sync::NetworkAccess::current().allowed(),
        "Sync is disabled"
    );
    // Preparation only; registration and the helper's start have their own bound.
    let deadline = Instant::now()
        .checked_add(budget.preparation())
        .context("deadline overflow")?;
    let own = std::env::current_exe()?.canonicalize()?;
    let executable = own
        .file_name()
        .and_then(|s| s.to_str())
        .context("app executable unavailable")?;
    let app = own
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("app bundle unavailable")?;
    ensure!(
        app.extension().is_some_and(|s| s == "app")
            && own == app.join("Contents/MacOS").join(executable),
        "Connect requires its signed installed app"
    );
    let state = super::account_state_directory()?;
    // A failed first preparation can leave the state folder without a generated
    // configuration. Keep it aside for diagnosis and prepare from scratch (#2589).
    if fs::symlink_metadata(&state).is_ok()
        && fs::symlink_metadata(state.join("config/config.xml")).is_err()
    {
        let aside = state.with_file_name(format!(
            "Sync.incomplete-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
        ));
        fs::rename(&state, &aside)?;
    }
    let fresh = match fs::symlink_metadata(&state) {
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
    };
    let (binding, identity) = if fresh {
        inventory::require_fresh(deadline)?;
        // Independent package verification precedes identity generation: no fake Binding.
        let (source, digest, provenance) = bundle::trusted_preparation_runtime(
            app,
            executable,
            &policy.app,
            &policy.helper,
            &policy.runtime,
        )?;
        ensure!(Instant::now() < deadline, "preparation deadline elapsed");
        let parent = state.parent().context("state parent missing")?;
        private::directory(parent)?;
        let mut identity = None;
        let binding = preparation::fresh(&state, deadline, |installation, instance| {
            let prepared = prepare_payload(&state, &source, &digest, &provenance, deadline)?;
            let binding = Binding {
                installation,
                instance,
                owner: rustix::process::geteuid().as_raw().to_string(),
                supervisor: app
                    .join(bundle::HELPER)
                    .to_str()
                    .context("helper path must be UTF-8")?
                    .into(),
                state_directory: state.to_str().context("state path must be UTF-8")?.into(),
                device_identity: prepared.device_id.clone(),
            };
            let guard = BundleGuard::new(
                app,
                executable,
                binding.clone(),
                policy.app.clone(),
                policy.helper.clone(),
                NativeSignature,
            )?;
            let (_, bound_digest, bound_provenance) =
                guard.bundled_runtime(&binding, &policy.runtime)?;
            ensure!(
                bound_digest == digest && bound_provenance == provenance,
                "signed bundle changed during preparation"
            );
            // Repeat complete absence evidence before publishing launch authority.
            inventory::require_fresh(deadline)?;
            identity = Some(prepared);
            Ok(binding)
        })?;
        (binding, identity.context("prepared identity unavailable")?)
    } else {
        // No inference from a missing endpoint, config, or interrupted intent.
        let store = UnixStore::open_existing(&state)?;
        let binding = {
            let tx = store.begin(deadline)?;
            tx.current()?
                .context("recovery pending: incomplete preparation")?
                .binding()
                .clone()
        };
        let saved: Receipt =
            serde_json::from_slice(&private::read(&state.join("prepared-identity.json"))?)?;
        ensure!(
            saved.identity.device_id == binding.device_identity,
            "saved identity binding changed"
        );
        (binding, saved.identity)
    };
    let store = UnixStore::open_existing(&state)?;
    let selection = store
        .read_selection(deadline)?
        .context("recovery pending: missing runtime selection")?;
    let bundle = BundleGuard::new(
        app,
        executable,
        binding.clone(),
        policy.app.clone(),
        policy.helper.clone(),
        NativeSignature,
    )?;
    let (_, digest, _) = bundle.bundled_runtime(&binding, &policy.runtime)?;
    ensure!(
        digest == selection.digest(),
        "selected runtime differs from signed bundle; explicit update required"
    );
    let payload = Payload::new(binding.clone(), selection, identity.clone());
    payload.verify(&binding)?;
    let ready_to_dispatch = {
        let tx = store.begin(deadline)?;
        tx.current()?
            .context("recovery pending: missing journal")?
            .launch_registration()
            .is_some_and(|r| r.phase() == crate::sidecar::authority::launch::Phase::Ready)
    };
    if ready_to_dispatch {
        inventory::require_fresh(deadline)?;
    }
    ensure!(
        Instant::now() < deadline,
        "Connect deadline elapsed before registration"
    );
    ensure!(
        okilum_sync::NetworkAccess::current().allowed(),
        "Sync is disabled"
    );
    // The registration guard's per-call budget is the startup bound, not what
    // preparation left over: a nearly spent remainder could consume the one
    // dispatch before the native register call (#2589).
    let mut registration = NativeRegistrationGuard::open(
        app,
        executable,
        binding,
        payload,
        policy.app,
        policy.helper,
        budget.startup(),
    )?;
    complete_explicit_connect(&mut registration, identity, budget, verify_rest)
}

impl ConnectRegistration for NativeRegistrationGuard {
    fn register_prepared(&mut self) -> Result<ManagedStatus> {
        NativeRegistrationGuard::register_prepared(self)
    }
    fn native_status(&mut self) -> Result<Status> {
        super::native::registration_status()
    }
    fn managed_status(&mut self, deadline: Instant) -> ManagedStatus {
        NativeRegistrationGuard::status_until(self, deadline)
    }
}

fn prepare_payload(
    state: &Path,
    source: &Path,
    digest: &str,
    provenance: &[u8],
    deadline: Instant,
) -> Result<DaemonIdentity> {
    for directory in [
        state.join("runtime"),
        state.join("runtime").join(CLIENT_VERSION),
        state.join("config"),
        state.join("data"),
    ] {
        private::directory(&directory)?;
    }
    let runtime = state.join("runtime").join(CLIENT_VERSION).join("syncthing");
    let mut input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&runtime)?;
    ensure!(
        input.metadata()?.is_file() && input.metadata()?.len() <= 128 * 1024 * 1024,
        "invalid signed runtime size"
    );
    let copied = std::io::copy(
        &mut std::io::Read::by_ref(&mut input).take(128 * 1024 * 1024 + 1),
        &mut target,
    )?;
    ensure!(
        copied <= 128 * 1024 * 1024 && Instant::now() < deadline,
        "runtime copy exceeded preparation bounds"
    );
    target.sync_all()?;
    drop(target);
    fs::File::open(runtime.parent().unwrap())?.sync_all()?;
    let selection = Selection::new(CLIENT_VERSION, digest, &runtime)?;
    selection.verify_file(state)?;
    private::write(&runtime.with_file_name("provenance.json"), provenance)?;
    let config = state.join("config");
    let command = |args: &[&str]| {
        let mut cmd = Command::new(&runtime);
        cmd.args(args);
        cmd
    };
    let version = inventory::output(&mut command(&["--version"]), deadline, 16384)?;
    ensure!(
        std::str::from_utf8(&version)?.split_whitespace().nth(1) == Some(CLIENT_VERSION),
        "unsupported signed runtime version"
    );
    selection.verify_file(state)?;
    inventory::output(
        command(&["generate", "--home"]).arg(&config),
        deadline,
        65536,
    )?;
    for name in ["config.xml", "cert.pem", "key.pem"] {
        let path = config.join(name);
        let m = fs::symlink_metadata(&path)?;
        ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "generated identity file replaced"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let config_file = config.join("config.xml");
    let mut xml = xmltree::Element::parse(private::read(&config_file)?.as_slice())?;
    // These are newly generated defaults only. No existing config is rewritten.
    xml.children
        .retain(|n| !matches!(n,xmltree::XMLNode::Element(e) if e.name=="folder"));
    let rest = TcpListener::bind("127.0.0.1:0")?;
    let rest_address = rest.local_addr()?;
    let listener = TcpListener::bind("0.0.0.0:0")?;
    let listen_address = listener.local_addr()?;
    let gui = xml.get_mut_child("gui").context("generated GUI missing")?;
    gui.attributes.insert("tls".into(), "false".into());
    set(gui, "address", &rest_address.to_string());
    let options = xml
        .get_mut_child("options")
        .context("generated options missing")?;
    for (name, value) in [
        ("globalAnnounceEnabled", "false"),
        ("localAnnounceEnabled", "true"),
        ("natEnabled", "false"),
        ("relaysEnabled", "false"),
        ("startBrowser", "false"),
        ("autoUpgradeIntervalH", "0"),
        ("urAccepted", "-1"),
        ("crashReportingEnabled", "false"),
    ] {
        set(options, name, value);
    }
    options
        .children
        .retain(|n| !matches!(n,xmltree::XMLNode::Element(e) if e.name=="listenAddress"));
    set(options, "listenAddress", &format!("tcp://{listen_address}"));
    let mut bytes = Vec::new();
    xml.write(&mut bytes)?;
    private::write(&config_file, &bytes)?;
    selection.verify_file(state)?;
    let id = inventory::output(
        command(&["device-id", "--home"]).arg(&config),
        deadline,
        16384,
    )?;
    let device_id = std::str::from_utf8(&id)?.trim().to_owned();
    ensure!(
        device_id.len() == 63
            && device_id
                .bytes()
                .all(|c| c.is_ascii_uppercase() || (b'2'..=b'7').contains(&c) || c == b'-'),
        "invalid generated device identity"
    );
    let identity = DaemonIdentity {
        config_file,
        device_id,
        certificate_hash: format!(
            "{:x}",
            Sha256::digest(private::public_file(&config.join("cert.pem"))?)
        ),
    };
    identity.verify_local()?;
    private::write(
        &state.join("prepared-identity.json"),
        &serde_json::to_vec(&Receipt {
            identity: identity.clone(),
        })?,
    )?;
    UnixStore::open_existing(state)?.publish_selection(deadline, &selection)?;
    // Releasing ephemeral reservations has a normal bind race: startup/readback must fail,
    // never select another endpoint or kill an occupant to compensate.
    drop((rest, listener));
    Ok(identity)
}
fn set(parent: &mut xmltree::Element, name: &str, value: &str) {
    if parent.get_child(name).is_none() {
        parent
            .children
            .push(xmltree::XMLNode::Element(xmltree::Element::new(name)));
    }
    parent.get_mut_child(name).unwrap().children = vec![xmltree::XMLNode::Text(value.into())];
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
