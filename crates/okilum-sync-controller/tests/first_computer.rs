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

#[test]
fn issue2589_headless_inviter_sees_when_the_joiner_received_approval() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = Controller::open(root.path().join("source"), vec![])?;
    let url = source
        .begin_pairing_at(
            "Linux",
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        )?
        .offer_url
        .unwrap();
    assert!(!source.approval_delivered()?, "nothing approved yet");
    let target = Controller::open(root.path().join("target"), vec![])?;
    let pending = target.redeem(&url, "Mac")?;
    assert!(pending.pending.is_some(), "the joiner waits for approval");
    let request = source.refresh_pairing()?.pending.unwrap();
    assert_eq!(source.computer_name()?, "Linux");
    assert_eq!(target.computer_name()?, "Mac");
    source.approve(request.request_id)?;
    assert!(
        !source.approval_delivered()?,
        "approve alone does not reach the joiner"
    );
    let joined = target.refresh_pairing()?;
    assert!(joined.pending.is_none() && joined.peers.len() == 1);
    // The listener records delivery right after its write returns.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !source.approval_delivered()? && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        source.approval_delivered()?,
        "the approval reply was written"
    );
    assert_eq!(source.snapshot()?.peers.len(), 1);
    Ok(())
}
