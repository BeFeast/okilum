//! Explicit offline preparation with durable identities and resumable phases.
//! No task registration, daemon serve, pairing or canonical-vault write occurs here.
use super::{bootstrap::Receipt, payload, signature::AuthenticodePolicy};
use crate::sidecar::supervisor::ipc::windows_discovery::ImagePolicy;
use crate::sidecar::{
    store::{Selection, WindowsStore},
    Binding,
};
use crate::{
    daemon::{DaemonIdentity, CLIENT_VERSION},
    managed_launch::{ManagedInstance, Reference},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use xmltree::{Element, XMLNode};

pub(super) struct Sources {
    pub app: PathBuf,
    pub helper: PathBuf,
    pub runtime: PathBuf,
    pub resources: PathBuf,
    pub policy: AuthenticodePolicy,
}
impl Sources {
    pub fn installed() -> Result<Self> {
        let app = std::env::current_exe()?.canonicalize()?;
        ensure!(
            app.file_name().is_some_and(|name| name
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("okilum.exe"))),
            "Connect requires the installed Okilum app"
        );
        let directory = app.parent().context("installed app directory missing")?;
        let helper = directory.join("okilum-sync-supervisor.exe");
        let runtime = directory.join("syncthing.exe");
        ensure!(helper.try_exists()? && runtime.try_exists()?, "Sync requires the installed signed build; the portable app has no background Sync helper");
        let sources = Self {
            app: app.clone(),
            helper,
            runtime,
            resources: directory.join("Syncthing"),
            policy: AuthenticodePolicy::from_build()?,
        };
        sources.verify()?;
        Ok(sources)
    }
    fn verify(&self) -> Result<()> {
        self.policy.verify_image(&self.app)?;
        self.policy.verify_image(&self.helper)?;
        payload::verify(&self.runtime, &self.policy)?;
        Ok(())
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    Allocated,
    Staged,
    Generating,
    Generated,
    Configured,
    Ready,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    installation: Uuid,
    instance: Uuid,
    owner: String,
    helper_hash: String,
    runtime_hash: String,
    phase: Phase,
    configured: Option<Vec<u8>>,
    identity: Option<DaemonIdentity>,
    launch: Option<Reference>,
}
pub(super) struct Prepared {
    pub binding: Binding,
    pub identity: DaemonIdentity,
    pub helper_hash: String,
    pub runtime_hash: String,
}
fn text(path: &Path) -> Result<String> {
    let raw = path.to_str().context("Sync path is not Unicode")?;
    let plain = raw.strip_prefix(r"\\?\").unwrap_or(raw);
    super::path(plain)?;
    Ok(plain.into())
}
fn hash(path: &Path) -> Result<String> {
    crate::managed_launch::digest(path)
}
fn save(state: &Path, record: &Record) -> Result<()> {
    private::write(
        &state.join("preparation.json"),
        &serde_json::to_vec(record)?,
    )
}
fn stage(source: &Path, destination: &Path, expected: &str) -> Result<()> {
    if destination.try_exists()? {
        ensure!(
            hash(destination)? == expected,
            "retained Sync executable changed; recovery required"
        );
        return Ok(());
    }
    let source_directory = okilum_core::windows_files::Directory::open(
        source.parent().context("source parent missing")?,
    )?;
    let mut source =
        source_directory.open_file(source.file_name().context("source name missing")?)?;
    ensure!(
        source.metadata()?.len() <= 100_000_000,
        "Sync executable exceeds preparation bound"
    );
    let mut bytes = vec![];
    (&mut source).take(100_000_001).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 100_000_000 && format!("{:x}", Sha256::digest(&bytes)) == expected,
        "Sync executable changed during preparation"
    );
    let temporary = destination.with_file_name(format!(".stage-{}", Uuid::new_v4()));
    let mut writer = private::open_rw(&temporary)?;
    writer.write_all(&bytes)?;
    writer.publish_noreplace(destination)?;
    let mut permissions = fs::metadata(destination)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(destination, permissions)?;
    ensure!(
        hash(destination)? == expected,
        "staged Sync executable differs"
    );
    Ok(())
}
fn command_output(
    runtime: &Path,
    state: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>> {
    ensure!(
        Instant::now() < deadline,
        "Sync preparation deadline elapsed"
    );
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let home = state.join("generated-home");
    private::directory(&home)?;
    let mut command = Command::new(runtime);
    command
        .args(args)
        .arg("--home")
        .arg(state.join("config"))
        .env_clear()
        .env("USERPROFILE", &home)
        .env("HOME", &home)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = Child(command.spawn()?);
    loop {
        if let Some(status) = child.0.try_wait()? {
            ensure!(status.success(), "offline Sync preparation command failed");
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "offline Sync preparation command exceeded deadline"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut bytes = vec![];
    child
        .0
        .stdout
        .take()
        .context("Sync preparation output missing")?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "Sync preparation output exceeded bound"
    );
    Ok(bytes)
}
fn set(parent: &mut Element, name: &str, value: &str) {
    parent
        .children
        .retain(|node| !matches!(node, XMLNode::Element(element) if element.name == name));
    let mut element = Element::new(name);
    element.children.push(XMLNode::Text(value.into()));
    parent.children.push(XMLNode::Element(element));
}
fn configured(state: &Path) -> Result<Vec<u8>> {
    let mut xml =
        Element::parse(private::public_file(&state.join("config").join("config.xml"))?.as_slice())?;
    // This phase accepts only its own freshly generated identity, before any
    // task exists. Existing paired configuration never enters this function.
    xml.children
        .retain(|node| !matches!(node, XMLNode::Element(element) if element.name == "folder"));
    let rest = TcpListener::bind("127.0.0.1:0")?;
    let listen = TcpListener::bind("0.0.0.0:0")?;
    let gui = xml
        .get_mut_child("gui")
        .context("generated Sync GUI configuration missing")?;
    gui.attributes.insert("tls".into(), "false".into());
    set(gui, "address", &rest.local_addr()?.to_string());
    let options = xml
        .get_mut_child("options")
        .context("generated Sync options missing")?;
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
    set(
        options,
        "listenAddress",
        &format!("tcp://{}", listen.local_addr()?),
    );
    let mut bytes = vec![];
    xml.write(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn prepare(state: &Path, sources: &Sources, deadline: Instant) -> Result<Prepared> {
    sources.verify()?;
    let state = PathBuf::from(text(state)?);
    fs::create_dir_all(state.parent().context("Sync state parent missing")?)?;
    private::directory(&state)?;
    let lock = private::managed_lock(&state.join("prepare.lock"), true)?;
    lock.try_lock()
        .context("another Sync preparation is in progress")?;
    let file = state.join("preparation.json");
    let mut record: Record = if file.try_exists()? {
        serde_json::from_slice(&private::read(&file)?)?
    } else {
        ensure!(fs::read_dir(&state)?.all(|entry| entry.is_ok_and(|entry| entry.file_name() == "prepare.lock")),
            "existing Sync state has no preparation receipt; preserve it and review the existing connection");
        let record = Record {
            schema: 1,
            installation: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            owner: super::security::current_sid()?,
            helper_hash: hash(&sources.helper)?,
            runtime_hash: payload::verify(&sources.runtime, &sources.policy)?,
            phase: Phase::Allocated,
            configured: None,
            identity: None,
            launch: None,
        };
        save(&state, &record)?;
        record
    };
    ensure!(
        record.schema == 1
            && !record.installation.is_nil()
            && !record.instance.is_nil()
            && record.owner == super::security::current_sid()?,
        "Sync preparation ownership changed"
    );
    for digest in [&record.helper_hash, &record.runtime_hash] {
        ensure!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid prepared executable digest"
        );
    }
    let runtime_parent = state.join("runtime");
    private::directory(&runtime_parent)?;
    let runtime_parent =
        runtime_parent.join(format!("{CLIENT_VERSION}-{}", &record.runtime_hash[..16]));
    private::directory(&runtime_parent)?;
    let runtime = runtime_parent.join("syncthing.exe");
    let helper_parent = state.join("helper");
    private::directory(&helper_parent)?;
    let helper_parent = helper_parent.join(&record.helper_hash);
    private::directory(&helper_parent)?;
    let helper = helper_parent.join("okilum-sync-supervisor.exe");
    if record.phase == Phase::Allocated {
        stage(&sources.runtime, &runtime, &record.runtime_hash)?;
        stage(&sources.helper, &helper, &record.helper_hash)?;
        let resource_source = okilum_core::windows_files::Directory::open(&sources.resources)?;
        for name in [
            "LICENSE.txt",
            "AUTHORS.txt",
            "README.txt",
            "release-key.asc",
            "sha256sum-v2.1.6.txt.asc",
            "provenance.json",
        ] {
            let input = resource_source.open_file(std::ffi::OsStr::new(name))?;
            ensure!(
                input.metadata()?.len() <= 1024 * 1024,
                "runtime provenance/notice exceeds bound"
            );
            let mut bytes = vec![];
            input.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 1024 * 1024,
                "runtime provenance/notice exceeds bound"
            );
            let destination = runtime_parent.join(name);
            if destination.try_exists()? {
                ensure!(
                    private::read(&destination)? == bytes,
                    "retained runtime provenance changed"
                );
            } else {
                private::write(&destination, &bytes)?;
            }
        }
        drop(resource_source);
        record.phase = Phase::Staged;
        save(&state, &record)?;
    }
    ensure!(
        hash(&helper)? == record.helper_hash
            && payload::verify(&runtime, &sources.policy)? == record.runtime_hash,
        "retained Sync package changed"
    );
    sources.policy.verify_image(&helper)?;
    for name in ["config", "data"] {
        private::directory(&state.join(name))?;
    }
    if record.phase == Phase::Staged {
        ensure!(
            fs::read_dir(state.join("config"))?.next().is_none(),
            "fresh preparation found an existing identity"
        );
        record.phase = Phase::Generating;
        save(&state, &record)?;
        command_output(&runtime, &state, &["generate"], deadline, 65536)?;
    }
    if record.phase == Phase::Generating {
        for name in ["config.xml", "cert.pem", "key.pem"] {
            private::public_file(&state.join("config").join(name)).with_context(|| {
                "interrupted identity generation requires recovery; no identity was regenerated"
            })?;
        }
        record.phase = Phase::Generated;
        save(&state, &record)?;
    }
    if record.phase == Phase::Generated {
        let xml = configured(&state)?;
        record.configured = Some(xml);
        record.phase = Phase::Configured;
        save(&state, &record)?;
    }
    if record.phase == Phase::Configured {
        let config = state.join("config").join("config.xml");
        private::write_generated(
            &config,
            record
                .configured
                .as_deref()
                .context("prepared configuration missing")?,
        )?;
        let id = command_output(&runtime, &state, &["device-id"], deadline, 16384)?;
        let device_id = std::str::from_utf8(&id)?.trim().to_owned();
        ensure!(
            device_id.len() == 63
                && device_id.bytes().all(|byte| byte.is_ascii_uppercase()
                    || (b'2'..=b'7').contains(&byte)
                    || byte == b'-'),
            "invalid generated device identity"
        );
        let identity = DaemonIdentity {
            config_file: config.canonicalize()?,
            device_id,
            certificate_hash: format!(
                "{:x}",
                Sha256::digest(private::public_file(
                    &state.join("config").join("cert.pem")
                )?)
            ),
        };
        identity.verify_local()?;
        let selection = Selection::new(
            &format!("{CLIENT_VERSION}-{}", &record.runtime_hash[..16]),
            &record.runtime_hash,
            &runtime,
        )?;
        WindowsStore::open_existing(&text(&state)?)?.publish_selection(deadline, &selection)?;
        let launch = crate::managed_launch::initialize(&ManagedInstance {
            home: state.join("config").canonicalize()?,
            executable: runtime.canonicalize()?,
        })?;
        record.identity = Some(identity);
        record.launch = Some(launch);
        record.configured = None;
        record.phase = Phase::Ready;
        save(&state, &record)?;
    }
    ensure!(
        record.phase == Phase::Ready && Instant::now() < deadline,
        "Sync preparation is not ready"
    );
    let identity = record.identity.context("prepared identity missing")?;
    let launch = record.launch.context("prepared launch reference missing")?;
    identity.verify_local()?;
    launch.validate(&ManagedInstance {
        home: state.join("config").canonicalize()?,
        executable: runtime.canonicalize()?,
    })?;
    private::write(
        &state.join("prepared-identity.json"),
        &serde_json::to_vec(&Receipt {
            identity: identity.clone(),
            launch,
        })?,
    )?;
    Ok(Prepared {
        binding: Binding {
            installation: record.installation,
            instance: record.instance,
            owner: record.owner,
            supervisor: text(&helper)?,
            state_directory: text(&state)?,
            device_identity: identity.device_id.clone(),
        },
        identity,
        helper_hash: record.helper_hash,
        runtime_hash: record.runtime_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires pinned signed runtime and isolated LOCALAPPDATA from the native runner"]
    fn windows_offline_preparation_preserves_identity_and_refuses_uncertain_regeneration(
    ) -> Result<()> {
        let selected = PathBuf::from(
            std::env::var_os("OKILUM_WINDOWS_FIRST_SYNC_FIXTURE")
                .context("isolated fixture marker missing")?,
        );
        ensure!(
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from) == Some(selected),
            "account state is not isolated"
        );
        let runtime = PathBuf::from(
            std::env::var_os("OKILUM_SYNC_WINDOWS_CLIENT").context("signed fixture missing")?,
        );
        ensure!(
            format!("{:x}", Sha256::digest(fs::read(&runtime)?)) == payload::UPSTREAM_HASH,
            "fixture runtime pin changed"
        );
        let sources = Sources {
            app: runtime.clone(),
            helper: runtime.clone(),
            runtime: runtime.clone(),
            resources: PathBuf::from(
                std::env::var_os("OKILUM_SYNC_WINDOWS_RESOURCES")
                    .context("fixture notices missing")?,
            ),
            policy: super::super::signature::fixture_policy(&runtime)?,
        };
        let state = super::super::bootstrap::active_state_directory()?;
        let first = prepare(&state, &sources, Instant::now() + Duration::from_secs(60))?;
        let config = state.join("config");
        let original =
            ["config.xml", "cert.pem", "key.pem"].map(|name| fs::read(config.join(name)).unwrap());
        let envelope = fs::read(state.join("prepared-identity.json"))?;
        let second = prepare(&state, &sources, Instant::now() + Duration::from_secs(30))?;
        assert_eq!(first.binding, second.binding);
        assert_eq!(first.identity, second.identity);
        assert_eq!(envelope, fs::read(state.join("prepared-identity.json"))?);
        for (index, name) in ["config.xml", "cert.pem", "key.pem"].iter().enumerate() {
            assert_eq!(original[index], fs::read(config.join(name))?);
        }
        // Model a crash while the first generate was in flight: uncertainty
        // must not trigger a second invocation and mint another certificate.
        let mut interrupted: Record =
            serde_json::from_slice(&private::read(&state.join("preparation.json"))?)?;
        interrupted.phase = Phase::Generating;
        interrupted.identity = None;
        interrupted.launch = None;
        interrupted.configured = None;
        save(&state, &interrupted)?;
        fs::remove_file(config.join("key.pem"))?;
        let error = prepare(&state, &sources, Instant::now() + Duration::from_secs(30))
            .err()
            .context("uncertain generation unexpectedly succeeded")?;
        assert!(format!("{error:#}").contains("no identity was regenerated"));
        assert!(!config.join("key.pem").exists());
        assert_eq!(fs::read(config.join("cert.pem"))?, original[1]);
        assert_eq!(fs::read(config.join("config.xml"))?, original[0]);
        Ok(())
    }
}
