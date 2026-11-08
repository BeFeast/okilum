use super::*;
use crate::sidecar::{authority::Envelope, supervisor::ipc::Scope, Binding};
use uuid::Uuid;

#[test]
fn windows_native_readiness_does_not_lock_startup_reads_and_rechecks_cancelled_intent() -> Result<()>
{
    let temporary = tempfile::tempdir()?;
    let state = temporary.path().join("state");
    let state_text = state.to_str().context("fixture state is not Unicode")?;
    drop(super::super::private::PrivateDirectory::prepare(
        state_text,
    )?);
    let store = WindowsStore::open_existing(state_text)?;
    let binding = Binding {
        installation: Uuid::new_v4(),
        instance: Uuid::new_v4(),
        owner: super::super::security::current_sid()?,
        state_directory: state_text.into(),
        supervisor: temporary.path().join("helper.exe").to_str().unwrap().into(),
        device_identity: "readiness-fixture".into(),
    };
    store
        .begin(Instant::now() + Duration::from_secs(2))?
        .commit(Envelope::first(binding.clone()))?;

    // A real private reader pins the same directory while a helper starts.
    // Prove that the kernel excludes the unchanged store transaction here.
    let mut reader = Some(super::super::private::PrivateDirectory::open_existing(
        state_text,
    )?);
    assert!(store
        .begin(Instant::now() + Duration::from_millis(10))
        .is_err());
    let mut probes = 0;
    wait_for_native_ready(
        &store,
        &binding,
        Instant::now() + Duration::from_secs(1),
        || {
            probes += 1;
            if probes < 3 {
                return Ok(false);
            }
            drop(reader.take());
            Ok(true)
        },
    )?;
    assert_eq!(probes, 3, "the readiness observation must actually run");

    // Authentication alone cannot override an intervening Disable. Change the
    // actual journal during the observation, before the final intent check.
    let refused = wait_for_native_ready(
        &store,
        &binding,
        Instant::now() + Duration::from_secs(1),
        || {
            let mut transaction = store.begin(Instant::now() + Duration::from_secs(1))?;
            let current = transaction.current()?.context("fixture journal missing")?;
            let (disabled, _) = current.disable(Scope {
                installation: binding.installation,
                instance: binding.instance,
                generation: Uuid::new_v4(),
            })?;
            transaction.commit(disabled)?;
            Ok(true)
        },
    )
    .expect_err("cancelled intent must refuse readiness");
    assert!(format!("{refused:#}").contains("cancelled or changed"));
    Ok(())
}
