//! One-use fresh preparation intent. Creation is explicit, never on Settings open.
//! An interrupted attempt remains recovery pending; it cannot regenerate identity.
use crate::sidecar::{authority::Envelope, store::UnixStore, Binding};
use anyhow::{ensure, Context, Result};
use std::io::Write;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path},
    time::Instant,
};
use uuid::Uuid;

fn directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn directory_at(path: &Path) -> Result<fs::File> {
    use rustix::fs::{openat, Mode, CWD};
    let mut directory = fs::File::from(openat(CWD, "/", directory_flags(), Mode::empty())?);
    for component in path.components() {
        if let Component::Normal(name) = component {
            directory = fs::File::from(openat(&directory, name, directory_flags(), Mode::empty())?);
        }
    }
    Ok(directory)
}

/// Call only after explicit opt-in and trusted package verification. The callback
/// prepares offline payload/configuration, never registers or starts a daemon.
/// The account root MUST be new: an existing root without a journal is uncertainty,
/// not permission to generate another identity. No automatic resume is offered.
pub fn fresh(
    state: &Path,
    deadline: Instant,
    prepare: impl FnOnce(Uuid, Uuid) -> Result<Binding>,
) -> Result<Binding> {
    ensure!(Instant::now() < deadline, "preparation deadline elapsed");
    ensure!(
        state.is_absolute()
            && state
                .components()
                .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
        "plain absolute state root required"
    );
    let parent = state.parent().context("missing state parent")?;
    use rustix::fs::{mkdirat, openat, Mode, OFlags};
    let parent_fd = directory_at(parent)?;
    let name = state.file_name().context("missing state name")?;
    mkdirat(&parent_fd, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)
        .context("recovery pending: state already exists or cannot be created")?;
    // All creation and flushes stay bound to the opened directory chain.
    // A crash retains this root, even before the intent is complete.
    let root_fd = fs::File::from(openat(&parent_fd, name, directory_flags(), Mode::empty())?);
    let created = root_fd.metadata()?;
    let installation = Uuid::new_v4();
    let instance = Uuid::new_v4();
    let mut intent = fs::File::from(openat(
        &root_fd,
        "preparation.json",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    intent.write_all(&serde_json::to_vec(&serde_json::json!({
        "schema": 1, "installation": installation, "instance": instance
    }))?)?;
    intent.sync_all()?;
    root_fd.sync_all()?;
    parent_fd.sync_all()?;
    ensure!(Instant::now() < deadline, "preparation deadline elapsed");
    let binding = prepare(installation, instance)?;
    ensure!(Instant::now() < deadline, "preparation deadline elapsed");
    let m = directory_at(state)?.metadata()?;
    ensure!(
        m.dev() == created.dev() && m.ino() == created.ino(),
        "recovery pending: preparation root replaced"
    );
    ensure!(
        m.is_dir()
            && !m.file_type().is_symlink()
            && m.uid() == rustix::process::geteuid().as_raw()
            && m.mode() & 0o7077 == 0,
        "private owned state root required"
    );
    ensure!(
        binding.installation == installation
            && binding.instance == instance
            && Path::new(&binding.state_directory) == state
            && binding.owner == rustix::process::geteuid().as_raw().to_string()
            && !binding.device_identity.is_empty(),
        "prepared binding changed"
    );
    let store = UnixStore::open_existing(state)?;
    let mut tx = store.begin(deadline)?;
    ensure!(
        tx.current()?.is_none(),
        "recovery pending: preparation already has authority"
    );
    tx.commit(Envelope::first_with_launch(binding.clone()))?;
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        os::unix::fs::{symlink, DirBuilderExt},
        time::Duration,
    };
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }
    fn binding(state: &Path, installation: Uuid, instance: Uuid) -> Binding {
        Binding {
            installation,
            instance,
            owner: rustix::process::geteuid().as_raw().to_string(),
            supervisor: "/fixture/helper".into(),
            state_directory: state.to_str().unwrap().into(),
            device_identity: "fixture".into(),
        }
    }
    #[test]
    fn explicit_fresh_attempt_persists_intent_before_payload_and_never_repeats() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().canonicalize()?.join("new-state");
        let calls = Cell::new(0);
        let saved = fresh(&state, soon(), |installation, instance| {
            calls.set(calls.get() + 1);
            assert!(state.join("preparation.json").is_file());
            assert!(!state.join("sidecar.json").exists());
            Ok(binding(&state, installation, instance))
        })?;
        assert!(fresh(&state, soon(), |_, _| {
            calls.set(calls.get() + 1);
            unreachable!()
        })
        .is_err());
        assert_eq!(calls.get(), 1);
        let store = UnixStore::open_existing(&state)?;
        let tx = store.begin(soon())?;
        assert_eq!(tx.current()?.unwrap().binding(), &saved);
        assert_eq!(
            tx.current()?
                .unwrap()
                .launch_registration()
                .unwrap()
                .phase(),
            crate::sidecar::authority::launch::Phase::Ready
        );
        Ok(())
    }
    #[test]
    fn failed_or_missing_preparation_cannot_regenerate_identity() -> Result<()> {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        for partial in [false, true] {
            let state = root.join(if partial { "partial" } else { "existing" });
            if partial {
                assert!(fresh(&state, soon(), |_, _| anyhow::bail!(
                    "injected offline failure"
                ))
                .is_err());
            } else {
                fs::create_dir(&state)?;
            }
            assert!(fresh(&state, soon(), |_, _| panic!("must not regenerate")).is_err());
            assert!(!state.join("sidecar.json").exists());
        }
        Ok(())
    }
    #[test]
    fn changed_binding_or_replaced_root_never_publishes_launch_authority() -> Result<()> {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        for replace in [false, true] {
            let state = root.join(if replace { "replaced" } else { "wrong-binding" });
            assert!(fresh(&state, soon(), |installation, instance| {
                let mut prepared = binding(&state, installation, instance);
                if replace {
                    fs::rename(&state, root.join("retained-original"))?;
                    fs::DirBuilder::new().mode(0o700).create(&state)?;
                } else {
                    prepared.instance = Uuid::new_v4();
                }
                Ok(prepared)
            })
            .is_err());
            assert!(!state.join("sidecar.json").exists());
        }
        Ok(())
    }
    #[test]
    fn symlinked_ancestor_cannot_redirect_fresh_intent() -> Result<()> {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        let real = root.join("real");
        fs::create_dir(&real)?;
        symlink(&real, root.join("alias"))?;
        assert!(fresh(&root.join("alias/state"), soon(), |_, _| panic!(
            "must not prepare through link"
        ))
        .is_err());
        assert!(!real.join("state").exists());
        // Positive control: the real directory remains usable.
        let state = real.join("state");
        fresh(&state, soon(), |a, b| Ok(binding(&state, a, b)))?;
        Ok(())
    }
}
