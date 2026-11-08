//! Isolated offline composition fixtures, not native daemon/service acceptance.
use super::*;
use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

fn fixture(root: &Path) -> Result<(PathBuf, String)> {
    let runtime = root.join("fixture-runtime");
    let id = ["AAAAAAA"; 8].join("-");
    let source = format!(
        r##"#!/bin/sh
case "$1" in
--version) echo 'syncthing v2.1.6 fixture' ;;
generate)
    test "$2" = --home || exit 2
    test ! -e "$3/fixture-fired" || exit 3
    /bin/echo fired > "$3/fixture-fired"
    /bin/cat > "$3/config.xml" <<'XML'
<configuration><folder id="default" path="/must-not-sync"/><gui tls="false"><address>127.0.0.1:8384</address><apikey>isolated-fixture-key</apikey></gui><options><listenAddress>default</listenAddress></options></configuration>
XML
    /bin/echo fixture-certificate > "$3/cert.pem"
    /bin/echo fixture-private-key > "$3/key.pem"
    ;;
device-id) echo '{id}' ;;
*) exit 2 ;;
esac
"##
    );
    fs::write(&runtime, source)?;
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o500))?;
    let digest = format!("{:x}", Sha256::digest(fs::read(&runtime)?));
    Ok((runtime, digest))
}
fn state(root: &Path, name: &str) -> Result<PathBuf> {
    let state = root.join(name);
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    Ok(state)
}
#[test]
fn offline_preparation_stages_exact_bytes_and_creates_private_lan_config() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root = root.path().canonicalize()?;
    let (source, digest) = fixture(&root)?;
    let state = state(&root, "state")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let identity = prepare_payload(&state, &source, &digest, b"fixture provenance", deadline)?;
    assert!(
        state.join("config/fixture-fired").is_file(),
        "generator positive control"
    );
    let selection = UnixStore::open_existing(&state)?
        .read_selection(deadline)?
        .unwrap();
    assert_eq!(fs::read(selection.verify_file(&state)?)?, fs::read(source)?);
    identity.verify_local()?;
    let xml = xmltree::Element::parse(private::read(&identity.config_file)?.as_slice())?;
    assert!(xml.get_child("folder").is_none());
    let options = xml.get_child("options").unwrap();
    assert_eq!(
        options
            .get_child("globalAnnounceEnabled")
            .unwrap()
            .get_text()
            .as_deref(),
        Some("false")
    );
    assert_eq!(
        options
            .get_child("localAnnounceEnabled")
            .unwrap()
            .get_text()
            .as_deref(),
        Some("true")
    );
    assert!(
        prepare_payload(
            &state,
            &root.join("fixture-runtime"),
            &digest,
            b"fixture provenance",
            deadline
        )
        .is_err(),
        "never replace generated identity"
    );
    Ok(())
}
#[test]
fn changed_runtime_refuses_before_offline_generation_or_selection() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root = root.path().canonicalize()?;
    let (source, _) = fixture(&root)?;
    let state = state(&root, "refused")?;
    assert!(prepare_payload(
        &state,
        &source,
        &"0".repeat(64),
        b"fixture",
        Instant::now() + Duration::from_secs(3)
    )
    .is_err());
    assert!(!state.join("config/fixture-fired").exists());
    assert!(UnixStore::open_existing(&state)?
        .read_selection(Instant::now() + Duration::from_secs(1))?
        .is_none());
    Ok(())
}
#[test]
fn no_opt_in_cannot_reach_inventory_state_or_bundle() {
    let requirement =
        CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture").unwrap();
    let policy = SigningPolicy {
        app: requirement.clone(),
        helper: requirement.clone(),
        runtime: requirement,
    };
    let error = connect_current_app(false, policy, ConnectBudget::CONNECT).unwrap_err();
    assert!(error.to_string().contains("explicit Connect consent"));
}

#[test]
fn native_launch_preparation_preserves_existing_identity_and_never_repairs_a_lost_registry(
) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let (source, digest) = fixture(&root)?;
    let state = state(&root, "existing")?;
    let identity = prepare_payload(
        &state,
        &source,
        &digest,
        b"fixture provenance",
        Instant::now() + Duration::from_secs(5),
    )?;
    let vault = root.join("existing-vault");
    fs::create_dir(&vault)?;
    fs::write(vault.join("kept.bin"), [0, 255, 42])?;
    let config = state.join("config/config.xml");
    let mut xml = xmltree::Element::parse(private::read(&config)?.as_slice())?;
    let mut folder = xmltree::Element::new("folder");
    folder
        .attributes
        .insert("id".into(), "existing-folder-id".into());
    folder
        .attributes
        .insert("path".into(), vault.to_str().unwrap().into());
    xml.children.push(xmltree::XMLNode::Element(folder));
    let mut bytes = vec![];
    xml.write(&mut bytes)?;
    private::write(&config, &bytes)?;
    let certificate = fs::read(state.join("config/cert.pem"))?;
    let secret = fs::read(state.join("config/key.pem"))?;
    let instance = crate::managed_launch::ManagedInstance {
        home: state.join("config"),
        executable: UnixStore::open_existing(&state)?
            .read_selection(Instant::now() + Duration::from_secs(5))?
            .unwrap()
            .verify_file(&state)?,
    };
    pin_launch_reference(&state, &instance)?;
    let pinned = managed_launch_reference(&state)?.context("launch reference was not persisted")?;
    pin_launch_reference(&state, &instance)?;
    assert_eq!(managed_launch_reference(&state)?, Some(pinned));
    assert_eq!(fs::read(&config)?, bytes);
    assert_eq!(fs::read(state.join("config/cert.pem"))?, certificate);
    assert_eq!(fs::read(state.join("config/key.pem"))?, secret);
    assert_eq!(fs::read(vault.join("kept.bin"))?, [0, 255, 42]);
    identity.verify_local()?;
    fs::rename(
        state.join("managed-launch"),
        state.join("retained-registry"),
    )?;
    assert!(managed_launch_reference(&state).is_err());
    assert!(pin_launch_reference(&state, &instance).is_err());
    assert!(
        !state.join("managed-launch").exists(),
        "lost authority must never be regenerated"
    );
    assert_eq!(fs::read(&config)?, bytes);
    Ok(())
}
