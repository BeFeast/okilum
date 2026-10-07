#![cfg(target_os = "linux")]
use anyhow::{Context, Result};
use tessera_sync_controller::pairing::{Service, Session};
#[test]
#[ignore = "requires independent CT141 Inbox sandbox with its explicit test CA"]
fn real_https_pairing_retry_and_private_restart() -> Result<()> {
    let origin = std::env::var("TESSERA_SYNC_TEST_ORIGIN").context("set CT141 sandbox origin")?;
    let certificate = std::fs::read(
        std::env::var("TESSERA_SYNC_TEST_CA").context("set sandbox root certificate")?,
    )?;
    let service = Service::new(&origin, Some(&certificate))?;
    let id = ["AAAAAAA"; 8].join("-");
    let session = Session::new(id, "Controller protocol fixture".into())?;
    // Wrong trust fails before reaching the service; the explicit sandbox root
    // below is the positive control for the same HTTPS endpoint.
    assert!(Service::new(&origin, None)?.start(&session).is_err());
    let first = service.start(&session)?;
    let restored: Session = serde_json::from_slice(&serde_json::to_vec(&session)?)?;
    let retry = service.start(&restored)?;
    assert_eq!(first.request.code, retry.request.code);
    assert_eq!(first.request.expires, retry.request.expires);
    assert_eq!(first.request.id, session.id());
    assert!(
        service.exchange(&restored)?.is_none(),
        "no grant before browser approval"
    );
    assert!(
        service.status(&restored).is_err(),
        "unapproved credential has no authority"
    );
    Ok(())
}
