use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
async fn selected_native_manager_restores_stored_token(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let environment = Environment::new("fixture", "https://pairing.example.test").unwrap();
    let token = okilum_t3::StoredToken {
        access_token: "synthetic-mcp-token".into(),
        expires_at: u64::MAX,
        scope: "mcp".into(),
        client_id: "fixture".into(),
    };
    let store = Arc::new(std::sync::Mutex::new(TestStore::default()));
    let (view, visual) = cx.add_window_view(|window, cx| {
        let mut view = ProjectView::empty(None, window, cx);
        view.credentials.test = Some(store.clone());
        view.threads.enabled = true;
        view
    });
    let write = view.read_with(visual, |view, cx| {
        view.credentials.write(
            &environment.credential_url(),
            "Okilum",
            &token.to_secret(),
            cx,
        )
    });
    write.await.unwrap();
    let read = view.read_with(visual, |view, cx| {
        view.credentials.gateway(
            environment.clone(),
            None,
            okilum_t3::NetworkAccess::default(),
            cx,
        )
    });
    let gateway = read.await.unwrap();
    assert!(matches!(gateway.status(), GatewayStatus::Connected { .. }));
    assert_eq!(
        store.lock().unwrap().reads,
        [
            environment.credential_url(),
            okilum_t3::stream::NativeToken::credential_key(&environment),
        ]
    );
}

#[gpui::test]
async fn selected_native_manager_restores_native_token_on_connection(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    cx.update(reader_features::enable_test_modules);
    let files = tempfile::tempdir().unwrap();
    let environment = Environment::new("fixture", "https://pairing.example.test").unwrap();
    let key = okilum_t3::stream::NativeToken::credential_key(&environment);
    let secret = serde_json::to_vec(&serde_json::json!({
        "origin": environment.origin(), "access_token": "synthetic-native-token",
        "expires_at": u64::MAX, "scope": "orchestration:read"
    }))
    .unwrap();
    let store = Arc::new(std::sync::Mutex::new(TestStore::default()));
    store
        .lock()
        .unwrap()
        .entries
        .insert(key.clone(), ("fixture".into(), secret));
    let (view, visual) = cx.add_window_view(|window, cx| {
        let mut view = fixture::view(
            files.path().to_owned(),
            Arc::new(fixture::Fake::default()),
            window,
            cx,
        );
        view.active = false;
        view.threads.enabled = true;
        view.credentials.test = Some(store.clone());
        view.finish_connection(
            Ok((environment, Arc::new(fixture::Fake::default()))),
            window,
            cx,
        );
        view
    });
    visual
        .condition(&view, |view, _| !view.native_pairing_busy())
        .await;
    view.read_with(visual, |view, _| assert!(view.native.active()));
    assert_eq!(store.lock().unwrap().reads, [key]);
}
