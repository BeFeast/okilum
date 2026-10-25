use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use anyhow::{Context, Result};
use okilum_t3::{manifest::AreaBinding, Environment};

#[cfg(test)]
pub(super) fn area_for(vault: &Path, note: &Path) -> Result<PathBuf> {
    let mut directory = note.parent().unwrap_or(vault);
    while directory.starts_with(vault) {
        match std::fs::metadata(directory.join("ops/project.yaml")) {
            Ok(metadata) if metadata.is_file() => return Ok(directory.to_owned()),
            Ok(_) => anyhow::bail!("The area's project manifest is not a file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot read this area's project manifest"),
        }
        if directory == vault {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent;
    }
    Ok(vault.to_owned())
}

pub(super) fn load(state: &Path, binding: &AreaBinding) -> Result<Option<Environment>> {
    let source = match std::fs::read(state.join("t3-connections.json")) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Cannot read saved T3 connections"),
    };
    let saved: BTreeMap<String, String> =
        serde_json::from_slice(&source).context("Saved T3 connections are unreadable")?;
    saved
        .get(&binding.environment)
        .map(|origin| Environment::new(&binding.environment, origin))
        .transpose()
}

pub(super) fn save(state: &Path, env: &Environment) -> Result<()> {
    std::fs::create_dir_all(state)?;
    let path = state.join("t3-connections.json");
    let mut saved: BTreeMap<String, String> = match std::fs::read(&path) {
        Ok(source) => {
            serde_json::from_slice(&source).context("Saved T3 connections are unreadable")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error).context("Cannot read saved T3 connections"),
    };
    saved.insert(env.alias.clone(), env.origin());
    save_document(state, "t3-connections.json", &serde_json::to_vec(&saved)?)
}

fn save_document(state: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(state)?;
    let path = state.join(name);
    let temporary = state.join(format!(".t3-origin-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        #[cfg(unix)]
        std::fs::File::open(state)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

/// Manifest origin, then its named connection registry, then last successful origin.
pub(super) fn prefill_origin(
    area: &Path,
    alias: &str,
    state: Option<&Path>,
) -> Result<Option<String>> {
    match project_origin(area, alias, state)? {
        Some(origin) => Ok(Some(origin)),
        None => last_origin(state),
    }
}

pub(super) fn project_origin(
    area: &Path,
    alias: &str,
    state: Option<&Path>,
) -> Result<Option<String>> {
    let manifest: serde_yaml::Value = match std::fs::read_to_string(area.join("ops/project.yaml")) {
        Ok(source) => serde_yaml::from_str(&source)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_yaml::Value::Null,
        Err(error) => return Err(error.into()),
    };
    if let Some(origin) = manifest["t3"]["origin"].as_str() {
        return Ok(Some(Environment::from_input(alias, origin)?.origin()));
    }
    let Some(state) = state else {
        return Ok(None);
    };
    let saved = match std::fs::read(state.join("t3-connections.json")) {
        Ok(bytes) => serde_json::from_slice::<BTreeMap<String, String>>(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(e.into()),
    };
    if let Some(origin) = saved.get(alias) {
        return Ok(Some(Environment::from_input(alias, origin)?.origin()));
    }
    Ok(None)
}

pub(super) fn last_origin(state: Option<&Path>) -> Result<Option<String>> {
    let Some(state) = state else {
        return Ok(None);
    };
    let alias = "app";
    match std::fs::read(state.join("t3-last-origin.json")) {
        Ok(bytes) => {
            let origin: String = serde_json::from_slice(&bytes)?;
            Ok(Some(Environment::from_input(alias, &origin)?.origin()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn save_last_origin(state: &Path, env: &Environment) -> Result<()> {
    save_document(
        state,
        "t3-last-origin.json",
        &serde_json::to_vec(&env.origin())?,
    )
}

#[cfg(test)]
mod origin_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn shared_settings_do_not_create_a_project_override() {
        let base = tempfile::tempdir().unwrap();
        let area = base.path().join("area");
        let state = base.path().join("state");
        std::fs::create_dir_all(area.join("ops")).unwrap();
        std::fs::write(area.join("ops/project.yaml"), "t3: {environment: maestro}").unwrap();
        let app = Environment::from_input("app", "t3.oklabs.uk").unwrap();
        save(&state, &app).unwrap();
        save_last_origin(&state, &app).unwrap();
        assert_eq!(
            project_origin(&area, "maestro", Some(&state)).unwrap(),
            None
        );
        assert_eq!(
            prefill_origin(&area, "maestro", Some(&state)).unwrap(),
            Some(app.origin())
        );
        let other = Environment::from_input("maestro", "10.10.0.23:23110").unwrap();
        save(&state, &other).unwrap();
        assert_eq!(
            project_origin(&area, "maestro", Some(&state)).unwrap(),
            Some(other.origin())
        );
        assert_eq!(
            last_origin(Some(&state)).unwrap(),
            Some(app.origin()),
            "project override must not replace the app default"
        );
    }

    #[test]
    fn prefill_prefers_manifest_then_registry_then_last_success() {
        let base = std::env::temp_dir().join(format!("okilum-origins-{}", uuid::Uuid::new_v4()));
        let area = base.join("area");
        let state = base.join("state");
        std::fs::create_dir_all(area.join("ops")).unwrap();
        let manifest = area.join("ops/project.yaml");
        std::fs::write(&manifest, "t3: {environment: maestro}").unwrap();
        assert_eq!(
            prefill_origin(&area, "maestro", Some(&state)).unwrap(),
            None
        );
        save_last_origin(
            &state,
            &Environment::new("other", "https://last.example").unwrap(),
        )
        .unwrap();
        assert_eq!(
            prefill_origin(&area, "maestro", Some(&state))
                .unwrap()
                .as_deref(),
            Some("https://last.example")
        );
        save(
            &state,
            &Environment::new("maestro", "http://10.10.0.23:23110").unwrap(),
        )
        .unwrap();
        assert_eq!(
            prefill_origin(&area, "maestro", Some(&state))
                .unwrap()
                .as_deref(),
            Some("http://10.10.0.23:23110")
        );
        std::fs::write(
            &manifest,
            "t3: {environment: maestro, origin: 't3.oklabs.uk/path/'}",
        )
        .unwrap();
        assert_eq!(
            prefill_origin(&area, "maestro", Some(&state))
                .unwrap()
                .as_deref(),
            Some("https://t3.oklabs.uk")
        );
        assert_eq!(
            prefill_origin(&area, "maestro", None).unwrap().as_deref(),
            Some("https://t3.oklabs.uk")
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
