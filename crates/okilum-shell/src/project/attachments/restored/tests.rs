use super::*;
use ::core::prelude::v1::test;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Host(Entity<ProjectView>);
impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.0.clone())
            .children(Root::render_dialog_layer(window, cx))
    }
}

fn image_bytes() -> Vec<u8> {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        12,
        8,
        image::Rgba([20, 180, 60, 255]),
    ));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    encoded.into_inner()
}

fn row() -> Attachment {
    Attachment {
        id: "restored-image".into(),
        kind: DraftKind::Image,
        name: "Screenshot.png".into(),
        mime_type: "image/png".into(),
        size_bytes: image_bytes().len() as u64,
        source: None,
        state: AttachmentState::Uploaded {
            attachment_id: "confirmed-asset".into(),
            environment_id: "server-environment".into(),
        },
    }
}

fn client() -> Arc<NativeClient> {
    let environment = Environment::new("fixture", "http://127.0.0.1:1").unwrap();
    let token = okilum_t3::stream::NativeToken::from_secret(&environment, &serde_json::to_vec(&serde_json::json!({"origin":environment.origin(),"access_token":"fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
    Arc::new(NativeClient::new(environment, token, Duration::from_millis(10)).unwrap())
}

#[gpui::test]
fn restored_upload_previews_without_a_local_source_and_keeps_its_exact_reference(
    cx: &mut TestAppContext,
) {
    // The durable draft writer completes on its real OS thread.
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = crate::project::fixture::view(
                temp.path().to_owned(),
                Arc::new(crate::project::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            let client = client();
            view.native.set_detail_test_client(client.clone());
            view.attachments.identity = Some((
                Arc::downgrade(&client),
                ServerIdentity {
                    environment_id: "server-environment".into(),
                    label: "Fixture".into(),
                },
            ));
            let reads = reads.clone();
            view.attachments.restored.fetch = Some(Arc::new(move |asset| {
                assert_eq!(asset.id, "confirmed-asset");
                assert_eq!(asset.metadata.name, "Screenshot.png");
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(image_bytes())
            }));
            view.compose.update(cx, |input, cx| {
                input.set_value("Retain text and image", window, cx)
            });
            view.draft_set_attachments(vec![row()], window, cx);
            view.draft_restore(window, cx);
            view
        });
        let host = cx.new(|_| Host(view));
        Root::new(host, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, cx| {
        root.view()
            .clone()
            .downcast::<Host>()
            .unwrap()
            .read(cx)
            .0
            .clone()
    });
    view.update_in(visual, |view, window, cx| {
        view.active = true;
        view.draft_restore(window, cx);
        view.restore_attachment_previews(window, cx);
    });
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "positive control: the confirmed asset was downloaded"
    );
    view.read_with(visual, |view, cx| {
        assert!(view.attachments.sources.is_empty());
        assert!(view.attachment_preview(&row()).is_some());
        assert_eq!(view.draft_attachments(), &[row()]);
        assert_eq!(
            view.prepared_attachments().unwrap()[0].id,
            "confirmed-asset"
        );
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Retain text and image"
        );
    });
    let preview = visual
        .debug_bounds("attachment-preview-restored-image")
        .expect("restored uploaded image has a real composer thumbnail");
    visual.simulate_click(preview.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("attachment-expanded-image").is_some(),
        "the restored reference opens the real image viewer"
    );
    view.update_in(visual, |view, window, cx| {
        view.restore_attachment_previews(window, cx)
    });
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "repaint/reopen does not download again"
    );
    view.update_in(visual, |view, _, cx| {
        let mut changed = row();
        changed.name = "replacement.png".into();
        assert!(
            view.attachment_preview(&changed).is_none(),
            "the same local ID with another server reference cannot reuse old pixels"
        );
        view.attachments.identity.as_mut().unwrap().1.environment_id = "another-environment".into();
        assert!(
            view.attachment_preview(&row()).is_none(),
            "a new environment cannot display a prior environment's cached preview"
        );
        view.native.set_detail_test_client(client());
        assert!(
            view.attachment_preview(&row()).is_none(),
            "a replacement connection must confirm attachment access again"
        );
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Retain text and image"
        );
    });
}

#[gpui::test]
fn preview_failure_retries_only_the_download_and_needs_reselect_stays_unsendable(
    cx: &mut TestAppContext,
) {
    // The durable draft writer completes on its real OS thread.
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = crate::project::fixture::view(
                temp.path().to_owned(),
                Arc::new(crate::project::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            let client = client();
            view.native.set_detail_test_client(client.clone());
            view.attachments.identity = Some((
                Arc::downgrade(&client),
                ServerIdentity {
                    environment_id: "server-environment".into(),
                    label: "Fixture".into(),
                },
            ));
            let reads = reads.clone();
            view.attachments.restored.fetch = Some(Arc::new(move |_| {
                if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("offline");
                }
                Ok(image_bytes())
            }));
            view.compose.update(cx, |input, cx| {
                input.set_value("Never discard this draft", window, cx)
            });
            view.draft_set_attachments(vec![row()], window, cx);
            view.draft_restore(window, cx);
            view
        });
        let host = cx.new(|_| Host(view));
        Root::new(host, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, cx| {
        root.view()
            .clone()
            .downcast::<Host>()
            .unwrap()
            .read(cx)
            .0
            .clone()
    });
    view.update_in(visual, |view, window, cx| {
        view.active = true;
        view.draft_restore(window, cx);
        view.restore_attachment_previews(window, cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.prepared_attachments().unwrap()[0].id,
            "confirmed-asset",
            "a preview failure does not lose the completed upload"
        )
    });
    view.update_in(visual, |view, window, cx| {
        view.restore_attachment_previews(window, cx)
    });
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "no automatic download retry"
    );
    let retry = visual
        .debug_bounds("attachment-preview-retry-restored-image")
        .unwrap();
    visual.simulate_click(retry.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert!(visual
        .debug_bounds("attachment-preview-restored-image")
        .is_some());
    view.update_in(visual, |view, window, cx| {
        let mut unavailable = row();
        unavailable.state = AttachmentState::Pending;
        view.draft_set_attachments(vec![unavailable], window, cx);
        view.attachment_restore_connection(window, cx);
        assert_eq!(
            view.draft_attachments()[0].state,
            AttachmentState::NeedsReselect
        );
        assert!(view.prepared_attachments().is_err());
        assert!(
            view.attachment_preview(&view.draft_attachments()[0])
                .is_none(),
            "no stale preview for a changed draft row"
        );
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Never discard this draft"
        );
    });
    visual.run_until_parked();
    assert!(visual
        .debug_bounds("attachment-retry-restored-image")
        .is_some());
    assert!(visual
        .debug_bounds("attachment-status-restored-image")
        .is_some());
    let remove = visual
        .debug_bounds("attachment-remove-restored-image")
        .unwrap();
    visual.simulate_click(remove.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert!(view.draft_attachments().is_empty());
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Never discard this draft"
        );
    });
}
