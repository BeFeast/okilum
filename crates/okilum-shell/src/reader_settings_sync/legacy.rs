//! Read-only compatibility boundary for the pre-direct Linux desktop setup.
use std::{io::Read, path::Path};

pub(super) const MESSAGE: &str = "An older Sync connection is saved on this computer. Your files and connection settings have been kept. This version cannot finish or manage that connection. Existing syncing can continue while Syncthing is running. If notes are not arriving, check this connection in Syncthing on both computers before setting up a new connection.";

#[derive(Debug)]
pub(super) struct PreviousConnection;
impl std::fmt::Display for PreviousConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(MESSAGE)
    }
}
impl std::error::Error for PreviousConnection {}

// Match the canonical old record without exposing any enrollment actions.
// A lone `retired` flag in a damaged record cannot authorize a new daemon.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedSetup {
    #[serde(rename = "setup")]
    _setup: okilum_sync_controller::desktop::Setup,
    #[serde(rename = "generation")]
    _generation: uuid::Uuid,
    retired: bool,
}

pub(super) fn check(direct_state: &Path) -> anyhow::Result<()> {
    let Some(parent) = direct_state.parent() else {
        anyhow::bail!("Sync state parent unavailable");
    };
    let setup = parent.join("setup.json");
    match std::fs::symlink_metadata(&setup) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            // Only a completed removal retires the old setup. Missing, corrupt
            // or unreadable records remain visible, without exposing credentials.
            let retired = (|| -> anyhow::Result<bool> {
                let mut bytes = Vec::new();
                std::fs::File::open(&setup)?
                    .take(65537)
                    .read_to_end(&mut bytes)?;
                anyhow::ensure!(bytes.len() <= 65536, "oversized setup");
                let record: SavedSetup = serde_json::from_slice(&bytes)?;
                Ok(record.retired)
            })();
            if matches!(retired, Ok(true)) {
                return Ok(());
            }
        }
        _ => {}
    }
    Err(PreviousConnection.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader_settings_sync::{perform_with_locations, user_error, Epoch, Operation};
    use okilum_sync_controller::{
        daemon::DaemonIdentity,
        desktop::{Desktop, Setup},
        pairing::{Session, State},
        runtime::Selection,
    };
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
    };

    #[test]
    fn issue2049_enrolled_install_is_explained_before_any_direct_operation() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("sync");
        fs::DirBuilder::new().mode(0o700).create(&state)?;
        let generation = uuid::Uuid::new_v4();
        let journal = state.join(generation.to_string()).join("enrollment");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&journal)?;
        let config = root.path().join("config.xml");
        let note = root.path().join("note.md");
        fs::write(&config, b"existing Syncthing configuration")?;
        fs::write(&note, b"keep these notes")?;
        let setup = Setup {
            origin: "https://not-contacted.invalid".into(),
            name: "Old computer".into(),
            destination: root.path().to_owned(),
            selection: Selection::Reuse(DaemonIdentity {
                config_file: config.clone(),
                device_id: ["AAAAAAA"; 8].join("-"),
                certificate_hash: "fixture".into(),
            }),
        };
        let runtime = state.join(generation.to_string()).join("runtime");
        fs::DirBuilder::new().mode(0o700).create(&runtime)?;
        let Selection::Reuse(identity) = &setup.selection else {
            unreachable!()
        };
        let runtime_file = runtime.join("runtime.json");
        fs::write(
            &runtime_file,
            serde_json::to_vec(&okilum_sync_controller::runtime::Snapshot {
                selection: setup.selection.clone(),
                desired_enabled: true,
                removed: false,
                identity: Some(identity.clone()),
            })?,
        )?;
        fs::write(
            state.join("setup.json"),
            serde_json::to_vec(&serde_json::json!({
                "setup": setup, "generation": generation, "retired": false
            }))?,
        )?;
        let session = Session::new(["AAAAAAA"; 8].join("-"), "Old computer".into())?;
        fs::write(
            journal.join("pairing.json"),
            serde_json::to_vec(&serde_json::json!({
                "origin": "https://not-contacted.invalid", "session": session,
                "intent": "Pair", "exchange_started": true, "vault": null,
                "descriptor": null, "removal": null,
                "registration": {"id": uuid::Uuid::new_v4(), "vault": uuid::Uuid::new_v4(),
                    "device_id": (["AAAAAAA"; 8].join("-")), "name": "Old computer",
                    "state": "hub_ready", "last_error": null}
            }))?,
        )?;
        for path in [
            state.join("setup.json"),
            journal.join("pairing.json"),
            runtime_file.clone(),
        ] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        // Positive control: this is a readable production enrollment, not an
        // arbitrary marker that could hide an unfired compatibility test.
        let desktop = Desktop {
            state: state.clone(),
            home: root.path().to_owned(),
            state_home: root.path().to_owned(),
            config_home: root.path().to_owned(),
        };
        assert_eq!(
            desktop.read()?.pairing.unwrap().registration.unwrap().state,
            State::HubReady
        );
        assert!(desktop.read()?.runtime.unwrap().desired_enabled);
        let files = [
            state.join("setup.json"),
            journal.join("pairing.json"),
            runtime_file,
            config.clone(),
            note,
        ];
        let before = files
            .iter()
            .map(fs::read)
            .collect::<std::io::Result<Vec<_>>>()?;
        let direct = state.join("direct");
        for operation in [
            Operation::Read,
            Operation::EnterPairing,
            Operation::Begin("New computer".into()),
            Operation::SelectConnection(config.clone()),
            Operation::Share(root.path().to_owned()),
            Operation::Cancel,
            Operation::Catalog(uuid::Uuid::new_v4()),
        ] {
            let error = perform_with_locations(None, operation, &Epoch::default().ticket(), || {
                Ok((direct.clone(), vec![config.clone()]))
            })
            .err()
            .expect("legacy setup must stop direct bootstrap");
            assert_eq!(user_error(&error), MESSAGE);
            assert!(
                !direct.exists(),
                "no direct controller or second daemon was prepared"
            );
        }
        assert_eq!(
            before,
            files
                .iter()
                .map(fs::read)
                .collect::<std::io::Result<Vec<_>>>()?
        );
        assert_eq!(
            desktop.read()?.pairing.unwrap().registration.unwrap().state,
            State::HubReady
        );
        Ok(())
    }

    #[test]
    fn issue2049_absent_and_retired_setups_allow_direct_but_uncertain_ones_do_not(
    ) -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let direct = root.path().join("direct");
        check(&direct)?;
        assert_eq!(fs::read_dir(root.path())?.count(), 0, "absence stays inert");
        let setup = root.path().join("setup.json");
        let retired = serde_json::json!({
            "setup": Setup {
                origin: "https://not-contacted.invalid".into(),
                name: "Old computer".into(), destination: root.path().to_owned(),
                selection: Selection::Reuse(DaemonIdentity {
                    config_file: root.path().join("config.xml"),
                    device_id: ["AAAAAAA"; 8].join("-"), certificate_hash: "fixture".into(),
                }),
            },
            "generation": uuid::Uuid::new_v4(), "retired": true
        });
        fs::write(&setup, serde_json::to_vec(&retired)?)?;
        check(&direct)?;
        for bytes in [
            br#"{"retired":false}"#.as_slice(),
            br#"{"retired":true}"#,
            b"{broken",
            b"{}",
        ] {
            fs::write(&setup, bytes)?;
            assert_eq!(user_error(&check(&direct).unwrap_err()), MESSAGE);
            assert_eq!(fs::read(&setup)?, bytes);
        }
        fs::remove_file(&setup)?;
        std::os::unix::fs::symlink(root.path().join("missing"), &setup)?;
        assert_eq!(user_error(&check(&direct).unwrap_err()), MESSAGE);
        assert!(!direct.exists());
        Ok(())
    }
}
