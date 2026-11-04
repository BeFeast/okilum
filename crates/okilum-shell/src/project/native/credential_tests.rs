//! Synthetic OS store: hosted GPUI tests never use a developer's login Keychain.
use super::*;
use ::core::prelude::v1::test;
use std::io::{BufRead, BufReader, Read, Write};

#[gpui::test]
async fn pairing_persists_before_connecting_and_restores_without_other_sessions(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    cx.update(reader_features::enable_test_modules);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let environment = Environment::new(
        "fixture",
        &format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    struct Server(Option<std::thread::JoinHandle<()>>);
    impl Drop for Server {
        fn drop(&mut self) {
            super::super::fixture::join_workers(self.0.take());
        }
    }
    let server = Server(Some(std::thread::spawn(move || {
        for _ in 0..2 {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "pairing request never arrived"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            // BSD inherits O_NONBLOCK from the listener.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert_eq!(first, "POST /oauth/token HTTP/1.1\r\n");
            let mut length = None;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            let mut body = vec![0; length.unwrap()];
            reader.read_exact(&mut body).unwrap();
            let form: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(&body).collect();
            assert_eq!(form["subject_token"], "synthetic-one-time-pairing");
            assert_eq!(form["client_os"], std::env::consts::OS);
            drop(reader);
            let reply = serde_json::json!({"access_token":"synthetic-native-token", "token_type":"Bearer", "expires_in":3600, "scope":"orchestration:read orchestration:operate"}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    })));
    let files = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(super::super::credentials::TestStore::default()));
    let mcp_key = environment.credential_url();
    let native_key = NativeToken::credential_key(&environment);
    let pairing_key = NativeToken::pairing_key(&environment);
    let mcp_bytes = StoredToken {
        access_token: "synthetic-mcp-token".into(),
        expires_at: u64::MAX,
        scope: "mcp".into(),
        client_id: "fixture".into(),
    }
    .to_secret();
    {
        let mut store = store.lock().unwrap();
        store
            .entries
            .insert(mcp_key.clone(), ("Okilum".into(), mcp_bytes.clone()));
        store.entries.insert(
            pairing_key.clone(),
            ("fixture".into(), b"synthetic-one-time-pairing".to_vec()),
        );
        store.refuse_write = true;
    }
    let (view, visual) = cx.add_window_view(|window, cx| {
        let mut view = super::super::fixture::view(
            files.path().to_owned(),
            Arc::new(super::super::fixture::Fake::default()),
            window,
            cx,
        );
        view.active = false; // Pair/restore only; no live subscription in this fixture.
        view.threads.enabled = true;
        view.environment = Some(environment.clone());
        view.credentials.test = Some(store.clone());
        view.native_restore(false, window, cx);
        view
    });
    visual.condition(&view, |view, _| !view.native.busy).await;
    view.read_with(visual, |view, _| {
        assert!(!view.native.active());
        assert!(
            view.native.open && view.connection_details,
            "a browser-only Mac must see the native pairing prompt"
        );
    });
    view.update_in(visual, |view, window, cx| {
        view.pair_native_credential("synthetic-one-time-pairing".into(), window, cx)
    });
    visual.condition(&view, |view, _| !view.native.busy).await;
    view.read_with(visual, |view, _| {
        assert!(
            !view.native.active(),
            "failed persistence must not connect a session-only token"
        );
        assert!(view
            .native
            .setup_error
            .as_deref()
            .unwrap()
            .contains("could not be saved"));
    });
    {
        let mut store = store.lock().unwrap();
        assert!(!store.entries.contains_key(&native_key));
        assert!(store.entries.contains_key(&pairing_key));
        store.refuse_write = false;
    }
    view.update_in(visual, |view, window, cx| {
        view.pair_native_credential("synthetic-one-time-pairing".into(), window, cx)
    });
    visual.condition(&view, |view, _| !view.native.busy).await;
    view.read_with(visual, |view, _| assert!(view.native.active()));
    {
        let store = store.lock().unwrap();
        assert_eq!(store.entries[&mcp_key].1, mcp_bytes);
        assert!(!store.entries.contains_key(&pairing_key));
        let saved = &store.entries[&native_key].1;
        assert!(!String::from_utf8_lossy(saved).contains("synthetic-one-time-pairing"));
        NativeToken::from_secret(&environment, saved).unwrap();
    }
    view.update_in(visual, |view, window, cx| {
        view.native.clear();
        view.native_restore(false, window, cx);
    });
    visual.condition(&view, |view, _| !view.native.busy).await;
    view.read_with(visual, |view, _| assert!(view.native.active()));
    drop(server);
}
