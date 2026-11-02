//! One-time channel discovery from signed publication identity, never from a URL name.
use crate::{descriptor, Release};
use anyhow::{ensure, Context, Result};
use std::{fs, io::Write, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Beta,
    Stable,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Beta => "Beta",
            Self::Stable => "Stable",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Beta => b"beta",
            Self::Stable => b"stable",
        }
    }
}

pub fn read(path: &Path) -> Result<Option<Channel>> {
    match fs::read_to_string(path) {
        Ok(value) => match value.as_str() {
            "beta" => Ok(Some(Channel::Beta)),
            "stable" => Ok(Some(Channel::Stable)),
            _ => {
                anyhow::bail!("Invalid saved AppImage update channel; select a channel in Settings")
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn prepared_preference(path: &Path, channel: Channel) -> Result<tempfile::NamedTempFile> {
    let parent = path.parent().context("Missing channel preference parent")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(channel.bytes())?;
    file.as_file().sync_all()?;
    Ok(file)
}

/// Only an explicit Settings action may replace an existing choice.
pub fn save(path: &Path, channel: Channel) -> Result<()> {
    prepared_preference(path, channel)?.persist(path)?;
    Ok(())
}

pub fn resolve(path: &Path, current: u64, source: &str, gpgv: &Path) -> Result<Channel> {
    resolve_with(path, current, source, |key, channel| {
        descriptor(key, channel, gpgv)
    })
}

fn resolve_with(
    path: &Path,
    current: u64,
    source: &str,
    mut fetch: impl FnMut(&str, &str) -> Result<Release>,
) -> Result<Channel> {
    if let Some(saved) = read(path)? {
        return Ok(saved);
    }
    let stable = fetch("okilum/linux/stable/latest.json", "stable")?;
    stable.validate("stable")?;
    let channel = if current <= stable.build {
        ensure!(
            current != stable.build || source == stable.source,
            "Installed source differs from the published Stable build"
        );
        Channel::Stable
    } else {
        // latest Beta may have advanced since this download. The immutable
        // archive proves this exact build/source was published, including betas
        // no longer at the moving feed head. Stable promotion reuses these bytes.
        let beta = fetch(
            &format!("okilum/linux/builds/{current}/release.json"),
            "beta",
        )?;
        beta.validate("beta")?;
        ensure!(
            beta.build == current && beta.source == source,
            "Installed AppImage has no matching published beta identity"
        );
        Channel::Beta
    };
    // An offline/failed discovery reaches no writes. Another process can save
    // an explicit choice while descriptors are in flight: never overwrite it.
    match prepared_preference(path, channel)?.persist_noclobber(path) {
        Ok(_) => Ok(channel),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            read(path)?.context("Channel preference disappeared during discovery")
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(channel: &str, build: u64) -> Release {
        let mut release = crate::tests::release();
        release.channel = channel.into();
        release.build = build;
        release.version = format!("0.1.{build}");
        release.key = format!("okilum/linux/builds/{build}/Okilum-x86_64.AppImage");
        release
    }

    #[test]
    fn no_preference_beta_only_build_keeps_beta_after_feed_advances() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/appimage-update-channel");
        let mut keys = Vec::new();
        let channel = resolve_with(&path, 7488, &"a".repeat(40), |key, channel| {
            keys.push(key.to_string());
            Ok(release(
                channel,
                if channel == "stable" { 7198 } else { 7488 },
            ))
        })
        .unwrap();
        assert_eq!(channel, Channel::Beta);
        assert_eq!(channel.label(), "Beta"); // Shared by About and Settings.
        assert_eq!(fs::read_to_string(&path).unwrap(), "beta");
        assert_eq!(
            keys,
            [
                "okilum/linux/stable/latest.json",
                "okilum/linux/builds/7488/release.json"
            ]
        );
    }

    #[test]
    fn no_preference_build_equal_to_stable_keeps_stable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appimage-update-channel");
        let channel = resolve_with(&path, 7198, &"a".repeat(40), |_, channel| {
            assert_eq!(channel, "stable");
            Ok(release(channel, 7198))
        })
        .unwrap();
        assert_eq!(channel, Channel::Stable);
        assert_eq!(channel.label(), "Stable");
        assert_eq!(fs::read_to_string(path).unwrap(), "stable");
    }

    #[test]
    fn older_install_uses_stable_without_requiring_beta_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appimage-update-channel");
        assert_eq!(
            resolve_with(&path, 7190, &"a".repeat(40), |_, channel| {
                assert_eq!(channel, "stable");
                Ok(release(channel, 7198))
            })
            .unwrap(),
            Channel::Stable
        );
    }

    #[test]
    fn conflicting_stable_identity_never_writes_preference() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appimage-update-channel");
        assert!(resolve_with(&path, 7198, &"b".repeat(40), |_, channel| {
            Ok(release(channel, 7198))
        })
        .is_err());
        assert!(!path.exists());
    }

    #[test]
    fn existing_preference_unchanged_without_network() {
        for channel in [Channel::Stable, Channel::Beta] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("appimage-update-channel");
            save(&path, channel).unwrap();
            assert_eq!(
                resolve_with(&path, 7488, &"a".repeat(40), |_, _| {
                    panic!("Saved choice must bypass discovery")
                })
                .unwrap(),
                channel
            );
            assert_eq!(fs::read(path).unwrap(), channel.bytes());
        }
    }

    #[test]
    fn offline_first_run_writes_nothing_and_can_retry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/appimage-update-channel");
        assert!(resolve_with(&path, 7488, &"a".repeat(40), |_, _| {
            anyhow::bail!("Offline")
        })
        .is_err());
        assert!(!path.parent().unwrap().exists());
        assert_eq!(read(&path).unwrap(), None);
        assert_eq!(
            resolve_with(&path, 7488, &"a".repeat(40), |_, channel| {
                Ok(release(
                    channel,
                    if channel == "stable" { 7198 } else { 7488 },
                ))
            })
            .unwrap(),
            Channel::Beta
        );
    }

    #[test]
    fn missing_beta_or_mismatched_identity_never_writes_preference() {
        for fault in ["offline", "source", "build", "channel", "fingerprint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("appimage-update-channel");
            assert!(
                resolve_with(&path, 7488, &"a".repeat(40), |_, channel| {
                    if channel == "stable" {
                        return Ok(release(channel, 7198));
                    }
                    let mut beta = release(channel, 7488);
                    match fault {
                        "offline" => anyhow::bail!("Offline"),
                        "source" => beta.source = "b".repeat(40),
                        "build" => beta = release(channel, 7494),
                        "channel" => beta.channel = "stable".into(),
                        "fingerprint" => beta.fingerprint = "untrusted".into(),
                        _ => unreachable!(),
                    }
                    Ok(beta)
                })
                .is_err(),
                "{fault}"
            );
            assert!(!path.exists(), "{fault}");
        }
    }

    #[test]
    fn explicit_choice_during_discovery_wins() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appimage-update-channel");
        assert_eq!(
            resolve_with(&path, 7488, &"a".repeat(40), |_, channel| {
                if channel == "beta" {
                    save(&path, Channel::Stable).unwrap();
                }
                Ok(release(
                    channel,
                    if channel == "stable" { 7198 } else { 7488 },
                ))
            })
            .unwrap(),
            Channel::Stable
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "stable");
    }

    #[test]
    fn invalid_existing_preference_is_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appimage-update-channel");
        fs::write(&path, "invalid").unwrap();
        assert!(resolve_with(&path, 7488, &"a".repeat(40), |_, _| {
            panic!("Invalid saved choice must remain visible")
        })
        .is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "invalid");
    }
}
