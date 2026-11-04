//! No peer, daemon, installed service or Internet connection participates.
#![cfg(unix)]
use anyhow::Result;
use okilum_sync_controller::direct::Controller;

#[test]
fn issue2589_first_computer_add_is_retryable_and_preserves_pending_request() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = Controller::open(root.path().join("source"), vec![])?;
    let begin = || {
        source.begin_pairing_at(
            "First Mac",
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        )
    };
    let first = begin()?.offer_url.unwrap();
    assert_eq!(begin()?.offer_url.as_deref(), Some(first.as_str()));
    let target = Controller::open(root.path().join("target"), vec![])?;
    target.redeem(&first, "Second computer")?;
    let pending = source.refresh_pairing()?.pending.unwrap();
    assert_eq!(begin()?.offer_url.as_deref(), Some(first.as_str()));
    assert_eq!(
        source.snapshot()?.pending.unwrap().request_id,
        pending.request_id
    );
    source.approve(pending.request_id)?;
    assert_ne!(begin()?.offer_url.as_deref(), Some(first.as_str()));
    source.cancel()?;
    assert!(
        begin()?.offer_url.is_some(),
        "cancelled invitations are replaced"
    );
    Ok(())
}

#[test]
fn issue2589_listener_failure_retains_os_cause_and_disabled_access_stays_revoked() -> Result<()> {
    let root = tempfile::tempdir()?;
    let access = okilum_sync::NetworkAccess::new(true);
    let _scope = access.enter();
    let controller = Controller::open(root.path().join("source"), vec![])?;
    let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
    let error = controller
        .begin_pairing_at("Mac", occupied.local_addr()?, "127.0.0.1".parse()?)
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot start LAN pairing listener"));
    assert!(error.downcast_ref::<std::io::Error>().is_some());
    assert!(
        controller.begin_pairing("Mac")?.offer_url.is_some(),
        "fresh enabled lease permits real-interface pairing"
    );
    access.revoke();
    assert_eq!(
        controller.begin_pairing("Mac").unwrap_err().to_string(),
        "Sync is disabled"
    );
    Ok(())
}
