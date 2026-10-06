use tessera_inboxd::{
    auth::{Auth, Error, FLOW_SECONDS},
    store::Store,
};
use url::Url;
use webauthn_authenticator_rs::{softpasskey::SoftPasskey, WebauthnAuthenticator};
const ORIGIN: &str = "https://inbox-qa.example.test";
fn enroll(auth: &mut Auth, now: i64) -> (WebauthnAuthenticator<SoftPasskey>, String) {
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let token = auth.bootstrap(now).unwrap();
    let (flow, options) = auth.register_start(&token, now).unwrap();
    let response = key
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let session = auth.register_finish(&flow, &response, now).unwrap();
    (key, session)
}
fn login(
    auth: &mut Auth,
    key: &mut WebauthnAuthenticator<SoftPasskey>,
    now: i64,
) -> Result<String, Error> {
    let (flow, options) = auth.login_start(now)?;
    let response = key
        .do_authentication(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    auth.login_finish(&flow, &response, now)
}
#[test]
fn multiple_keys_revoke_sessions_and_last_key_protection() {
    let dir = tempfile::tempdir().unwrap();
    let mut auth = Auth::new(Store::open(&dir.path().join("db")).unwrap(), ORIGIN).unwrap();
    let (mut first, session) = enroll(&mut auth, 1000);
    let original = auth.passkeys(&session, 1000).unwrap()[0].id.clone();
    assert!(matches!(
        auth.revoke(&session, &original, 1000),
        Err(Error::LastKey)
    ));
    let (flow, options) = auth.add_start(&session, "Second phone", 1001).unwrap();
    let mut second = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = second
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let id = auth.add_finish(&session, &flow, &credential, 1002).unwrap();
    assert!(auth.add_finish(&session, &flow, &credential, 1002).is_err());
    assert_eq!(auth.passkeys(&session, 1002).unwrap().len(), 2);
    let second_session = login(&mut auth, &mut second, 1003).unwrap();
    let first_session = login(&mut auth, &mut first, 1004).unwrap();
    let (stale_flow, stale_options) = auth.login_start(1005).unwrap();
    let stale_credential = second
        .do_authentication(Url::parse(ORIGIN).unwrap(), stale_options)
        .unwrap();
    auth.revoke(&first_session, &id, 1006).unwrap();
    assert!(auth.authenticate(&second_session, 1006).is_err());
    assert!(auth
        .login_finish(&stale_flow, &stale_credential, 1006)
        .is_err());
    assert!(auth.authenticate(&first_session, 1006).is_ok());
    assert!(matches!(
        auth.add_start(&first_session, "Another", 1004 + FLOW_SECONDS + 1),
        Err(Error::RecentRequired)
    ));
    assert!(auth.bootstrap(1007).is_err());
    assert!(matches!(
        auth.revoke(&first_session, &original, 1007),
        Err(Error::LastKey)
    ));
}
#[test]
fn device_needs_owner_confirmation_one_use_expiry_and_no_session_from_link() {
    let dir = tempfile::tempdir().unwrap();
    let mut auth = Auth::new(Store::open(&dir.path().join("db")).unwrap(), ORIGIN).unwrap();
    let (_, session) = enroll(&mut auth, 1000);
    let (token, info) = auth.invite_start(&session, 1001).unwrap();
    assert!(auth.authenticate(&token, 1001).is_err());
    let (flow, options) = auth.device_start(&token, 1002).unwrap();
    let mut phone = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = phone
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let pending = auth
        .device_finish(&flow, &credential, "iPhone", 1003)
        .unwrap();
    assert_eq!(pending.state, "confirm");
    assert_eq!(auth.passkeys(&session, 1003).unwrap().len(), 1);
    assert!(auth.device_start(&token, 1003).is_err());
    assert!(auth
        .device_finish(&flow, &credential, "duplicate", 1003)
        .is_err());
    assert!(auth
        .approve_device(
            "wrong-session",
            &info.id,
            pending.code.as_deref().unwrap(),
            1003
        )
        .is_err());
    assert!(auth
        .approve_device(&session, &info.id, "wrong-code", 1003)
        .is_err());
    auth.approve_device(&session, &info.id, pending.code.as_deref().unwrap(), 1004)
        .unwrap();
    assert!(auth
        .approve_device(&session, &info.id, pending.code.as_deref().unwrap(), 1004)
        .is_err());
    assert_eq!(auth.device_status(&token, 1005).unwrap().state, "approved");
    assert!(login(&mut auth, &mut phone, 1006).is_ok());
    let (expiring, _) = auth.invite_start(&session, 1007).unwrap();
    assert!(auth.device_start(&expiring, 1007 + FLOW_SECONDS).is_err());
    let (cancelled, info) = auth.invite_start(&session, 1008).unwrap();
    auth.cancel_invite(&session, &info.id, 1009).unwrap();
    assert!(auth.device_start(&cancelled, 1010).is_err());
}
#[test]
fn migration_preserves_original_credential_and_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut auth = Auth::new(Store::open(&path).unwrap(), ORIGIN).unwrap();
    let (mut first, session) = enroll(&mut auth, 1000);
    let owner = auth.owner;
    assert!(auth.authenticate(&session, 1000).is_ok());
    drop(auth);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("UPDATE auth_owner SET passkey=(SELECT passkey FROM auth_passkeys LIMIT 1); DROP TABLE auth_passkeys; PRAGMA user_version=10;").unwrap();
    drop(db);
    let mut restored = Auth::new(Store::open(&path).unwrap(), ORIGIN).unwrap();
    assert_eq!(restored.owner, owner);
    let session = login(&mut restored, &mut first, 1001).unwrap();
    assert_eq!(restored.passkeys(&session, 1001).unwrap().len(), 1);
    assert!(restored.bootstrap(1001).is_err());
}

#[test]
fn registration_is_session_bound_and_revocation_cancels_invites() {
    let dir = tempfile::tempdir().unwrap();
    let mut auth = Auth::new(Store::open(&dir.path().join("db")).unwrap(), ORIGIN).unwrap();
    let (mut first, session) = enroll(&mut auth, 1000);
    let another_session = login(&mut auth, &mut first, 1001).unwrap();
    let (flow, options) = auth.add_start(&session, "Bound key", 1002).unwrap();
    let mut second = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = second
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    assert!(matches!(
        auth.add_finish(&another_session, &flow, &credential, 1003),
        Err(Error::Unauthorized)
    ));
    assert!(auth.add_finish(&session, &flow, &credential, 1003).is_err());
    let (flow, options) = auth.add_start(&session, "Second", 1004).unwrap();
    let credential = second
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let second_id = auth.add_finish(&session, &flow, &credential, 1005).unwrap();
    let (invite, _) = auth.invite_start(&session, 1006).unwrap();
    let (pending, options) = auth.device_start(&invite, 1007).unwrap();
    let mut third = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = third
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    auth.revoke(&session, &second_id, 1008).unwrap();
    assert!(auth
        .device_finish(&pending, &credential, "Third", 1009)
        .is_err());
    assert!(auth.device_status(&invite, 1009).is_err());
    assert!(matches!(
        auth.add_start(&session, "\n", 1010),
        Err(Error::InvalidName)
    ));
    assert!(auth.authenticate(&session, 1010).is_ok());
}

#[test]
fn competing_device_ceremonies_have_one_winner_and_restart_expires_invites() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut auth = Auth::new(Store::open(&path).unwrap(), ORIGIN).unwrap();
    let (mut original, session) = enroll(&mut auth, 1000);
    let (token, info) = auth.invite_start(&session, 1001).unwrap();
    let (flow_a, options_a) = auth.device_start(&token, 1002).unwrap();
    let (flow_b, options_b) = auth.device_start(&token, 1002).unwrap();
    let mut a = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let mut b = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let ca = a
        .do_registration(Url::parse(ORIGIN).unwrap(), options_a)
        .unwrap();
    let cb = b
        .do_registration(Url::parse(ORIGIN).unwrap(), options_b)
        .unwrap();
    let pending = auth.device_finish(&flow_a, &ca, "Winner", 1003).unwrap();
    assert!(auth.device_finish(&flow_b, &cb, "Loser", 1003).is_err());
    assert_eq!(
        auth.device_status(&token, 1004).unwrap().name.as_deref(),
        Some("Winner")
    );
    drop(auth);
    let mut restored = Auth::new(Store::open(&path).unwrap(), ORIGIN).unwrap();
    assert!(restored.device_status(&token, 1005).is_err());
    assert!(restored.authenticate(&session, 1005).is_err());
    let session = login(&mut restored, &mut original, 1006).unwrap();
    assert!(restored
        .approve_device(&session, &info.id, pending.code.as_deref().unwrap(), 1006)
        .is_err());
    assert_eq!(restored.passkeys(&session, 1006).unwrap().len(), 1);
}
