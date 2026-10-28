//! Compiled Linux Sprint identity. Runtime environment cannot enable it.
use std::path::{Path, PathBuf};

pub(crate) const fn enabled() -> bool {
    cfg!(all(target_os = "linux", feature = "linux-sprint"))
}

pub(crate) const fn app_id() -> &'static str {
    if enabled() {
        "okilum-sprint"
    } else {
        "okilum"
    }
}

pub(crate) const fn name() -> &'static str {
    if enabled() {
        "Okilum Sprint"
    } else {
        "Okilum"
    }
}

/// Used before the updater inspects APPIMAGE or opens any update engine.
pub(crate) const fn allows_updates() -> bool {
    !enabled()
}

fn settings(home: &Path) -> Vec<(&'static str, PathBuf)> {
    if !enabled() {
        return Vec::new();
    }
    let root = home.join(".local/state/okilum-sprint");
    vec![
        ("OKILUM_STATE_DIR", root.clone()),
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_CACHE_HOME", root.join("cache")),
        ("XDG_STATE_HOME", root.join("state")),
        ("XDG_DATA_HOME", root.join("data")),
        ("TMPDIR", root.join("tmp")),
    ]
}

/// Main calls this before creating threads or reading any application paths.
/// Even launching the extracted executable cannot inherit production XDG/state.
pub(crate) fn initialize() -> anyhow::Result<()> {
    if !enabled() {
        return Ok(());
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute() && home.parent().is_some())
        .ok_or_else(|| anyhow::anyhow!("Linux Sprint requires an absolute HOME"))?;
    for (key, value) in settings(&home) {
        if value
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            anyhow::bail!("Refusing symlinked Sprint directory: {}", value.display());
        }
        std::fs::create_dir_all(&value)?;
        std::env::set_var(key, value);
    }
    Ok(())
}

pub(crate) fn credential_key(key: &str) -> String {
    if enabled() {
        format!("okilum-sprint:{key}")
    } else {
        key.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn sprint_distribution_compiled_identity_and_process_paths_match() {
        let home = Path::new("/home/sprint-fixture");
        if cfg!(feature = "linux-sprint") {
            assert!(enabled());
            assert_eq!(app_id(), "okilum-sprint");
            assert_eq!(name(), "Okilum Sprint");
            assert!(!allows_updates());
            let actual = settings(home);
            assert_eq!(actual.len(), 6);
            assert_eq!(actual[0].1, home.join(".local/state/okilum-sprint"));
            for (_, path) in &actual {
                assert!(path.starts_with(home.join(".local/state/okilum-sprint")));
                assert_ne!(path, &home.join(".local/state/okilum"));
            }
            assert_eq!(credential_key("service"), "okilum-sprint:service");
        } else {
            assert!(!enabled());
            assert_eq!(app_id(), "okilum");
            assert_eq!(name(), "Okilum");
            assert!(allows_updates());
            #[cfg(target_os = "linux")]
            assert_eq!(
                crate::updater::available(),
                std::env::var_os("APPIMAGE").is_some()
            );
            assert!(
                settings(home).is_empty(),
                "ordinary XDG/state stays untouched"
            );
            assert_eq!(credential_key("service"), "service");
        }
    }

    #[test]
    fn sprint_distribution_startup_isolates_actual_state_config_and_index_paths() {
        const CHILD: &str = "OKILUM_SPRINT_PATH_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let before = std::env::var_os("OKILUM_STATE_DIR").unwrap();
            initialize().unwrap();
            let state = crate::reader_history::state_directory().unwrap();
            if enabled() {
                let home = PathBuf::from(std::env::var_os("HOME").unwrap());
                assert_eq!(state, home.join(".local/state/okilum-sprint"));
                assert_eq!(
                    crate::reader_open::cache_base().unwrap(),
                    state.join("cache")
                );
                assert_eq!(
                    crate::reader_layout::config_base().unwrap(),
                    state.join("config")
                );
                assert_eq!(std::env::temp_dir(), state.join("tmp"));
                assert_ne!(state, PathBuf::from(before));
                #[cfg(target_os = "linux")]
                {
                    let desktop = home.join(".local/share/applications/okilum.desktop");
                    std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
                    std::fs::write(&desktop, "working Okilum").unwrap();
                    crate::app_footprint::install_appimage_desktop();
                    assert_eq!(crate::app_footprint::uninstall_data(true), 2);
                    assert_eq!(std::fs::read_to_string(desktop).unwrap(), "working Okilum");
                }
            } else {
                assert_eq!(state, PathBuf::from(before));
                assert_eq!(
                    std::env::var_os("XDG_CONFIG_HOME").unwrap(),
                    "/unchanged/config"
                );
            }
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "linux_sprint::tests::sprint_distribution_startup_isolates_actual_state_config_and_index_paths", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("OKILUM_STATE_DIR", home.path().join("production"))
            .env("XDG_CONFIG_HOME", "/unchanged/config")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!home.path().join("production").exists());
    }
}
