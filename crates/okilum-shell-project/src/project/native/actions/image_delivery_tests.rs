//! Exercise native input, binary transport and the immutable send journal together.
use super::*;
use ::core::prelude::v1::test;
use okilum_t3::{actions::Intent, chat_drafts::AttachmentState, stream::NativeToken};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
};

#[derive(Default)]
struct Received {
    uploads: Vec<Vec<u8>>,
    commands: Vec<Value>,
    provider_images: Vec<Vec<u8>>,
    fail_upload: bool,
}
struct Server {
    environment: Environment,
    client: Arc<NativeClient>,
    received: Arc<Mutex<Received>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
fn reply(stream: &mut TcpStream, status: &str, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}
impl Server {
    fn new(fail_upload: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let environment = Environment::new(
            "images",
            &format!("http://{}", listener.local_addr().unwrap()),
        )
        .unwrap();
        let token = NativeToken::from_secret(
            &environment,
            &serde_json::to_vec(&json!({
                "origin":environment.origin(), "access_token":"image-fixture",
                "expires_at":chrono::Utc::now().timestamp() as u64+3600,
                "scope":"orchestration:read orchestration:operate"
            }))
            .unwrap(),
        )
        .unwrap();
        let client = Arc::new(
            NativeClient::new(environment.clone(), token, Duration::from_secs(3)).unwrap(),
        );
        let received = Arc::new(Mutex::new(Received {
            fail_upload,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let state = received.clone();
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            while !stopping.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                // BSD/macOS inherits the listener's nonblocking mode.
                stream.set_nonblocking(false).unwrap();
                let state = state.clone();
                workers.push(thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut prefix = [0; 16];
                    let n = stream.peek(&mut prefix).unwrap();
                    if prefix[..n].starts_with(b"GET /ws?") {
                        let mut ws = tungstenite::accept(stream).unwrap();
                        let request: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                        let payload = &request["payload"];
                        let value = match request["tag"].as_str().unwrap() {
                            "server.getConfig" => json!({"environment":{"environmentId":"image-server","label":"Images"}}),
                            "attachments.createUploadUrl" => json!({"attachmentId":"image-asset","relativeUrl":"/upload","expiresAt":9999999999999_u64}),
                            "attachments.delete" => json!({}),
                            "assets.createUrl" => json!({"relativeUrl":"/asset","expiresAt":9999999999999_u64}),
                            "orchestration.dispatchCommand" => {
                                assert_eq!(payload["type"], "message.dispatch");
                                assert_eq!(payload["attachments"][0]["id"], "image-asset");
                                let mut state = state.lock().unwrap();
                                // The fixture provider resolves the dispatched server ID,
                                // never the clipboard, local path or expected test bytes.
                                let bytes = state.uploads.last().unwrap().clone();
                                if !state.commands.iter().any(|command| command["commandId"] == payload["commandId"]) {
                                    state.provider_images.push(bytes);
                                }
                                state.commands.push(payload.clone());
                                json!({"sequence":42})
                            }
                            other => panic!("unexpected image fixture RPC {other}"),
                        };
                        ws.send(tungstenite::Message::Text(json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":value}}).to_string().into())).unwrap();
                    } else {
                        let mut reader = BufReader::new(&mut stream);
                        let mut first = String::new();
                        reader.read_line(&mut first).unwrap();
                        let mut size = 0;
                        let mut headers = String::new();
                        loop {
                            let mut line = String::new();
                            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                            if line == "\r\n" { break; }
                            if let Some((key, value)) = line.split_once(':') {
                                if key.eq_ignore_ascii_case("content-length") { size = value.trim().parse().unwrap(); }
                            }
                            headers.push_str(&line);
                        }
                        assert!(size < 1024 * 1024);
                        let mut bytes = vec![0; size];
                        reader.read_exact(&mut bytes).unwrap();
                        drop(reader);
                        if first.starts_with("POST /api/auth/websocket-ticket ") {
                            assert!(headers.contains("Bearer image-fixture"));
                            reply(&mut stream, "200 OK", br#"{"ticket":"image-ticket"}"#);
                        } else if first.starts_with("POST /upload ") {
                            assert!(!headers.to_lowercase().contains("authorization:"));
                            let mut state = state.lock().unwrap();
                            state.uploads.push(bytes);
                            let fail = std::mem::take(&mut state.fail_upload);
                            reply(&mut stream, if fail { "503 Unavailable" } else { "204 No Content" }, b"");
                        } else if first.starts_with("GET /asset ") {
                            reply(&mut stream, "200 OK", state.lock().unwrap().uploads.last().unwrap());
                        } else { panic!("unexpected image fixture HTTP {first}"); }
                    }
                }));
            }
            crate::project::fixture::join_workers(workers);
        });
        Self {
            environment,
            client,
            received,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        crate::project::fixture::join_workers(self.worker.take());
    }
}

fn server_with_failed_worker() -> Server {
    let mut server = Server::new(false);
    server.stop.store(true, Ordering::Release);
    crate::project::fixture::join_workers(server.worker.take());
    server.worker = Some(thread::spawn(|| panic!("socket worker failure")));
    server
}

#[test]
fn server_cleanup_reports_worker_failure() {
    let server = server_with_failed_worker();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(server)))
        .expect_err("a fixture failure must fail an otherwise successful test");
    assert_eq!(
        failure.downcast_ref::<&str>(),
        Some(&"socket worker failure")
    );
}

#[test]
fn server_cleanup_preserves_failure_during_unwinding() {
    let server = server_with_failed_worker();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _server = server;
        panic!("original test failure");
    }))
    .expect_err("the original test failure must remain visible without aborting");
    assert_eq!(
        failure.downcast_ref::<&str>(),
        Some(&"original test failure")
    );
}

#[derive(Clone, Copy)]
enum Input {
    Chooser,
    Pixels,
    Tiff,
    CopiedFile,
    Drop,
}

async fn delivery(cx: &mut TestAppContext, input: Input, fail_upload: bool) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let server = Server::new(fail_upload);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("exact image.png");
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(8, 6)
        .write_to(
            &mut encoded,
            if matches!(input, Input::Tiff) {
                image::ImageFormat::Tiff
            } else {
                image::ImageFormat::Png
            },
        )
        .unwrap();
    let bytes = encoded.into_inner();
    let expected = if matches!(input, Input::Tiff) {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 6)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        png.into_inner()
    } else {
        bytes.clone()
    };
    std::fs::write(&path, &bytes).unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = crate::project::fixture::view(
                temp.path().to_owned(),
                Arc::new(crate::project::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            view.environment = Some(server.environment.clone());
            view.native.client = Some(server.client.clone());
            view.native.stream_ready = true;
            // The live stream's accepted snapshot is orthogonal to this input test.
            view.native.actions.update(cx, |actions, cx| {
                actions.set_state(
                    okilum_t3::actions::State {
                        ready: true,
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            view.compose.update(cx, |input, cx| {
                input.set_value("Keep exact text", window, cx)
            });
            view.draft_restore(window, cx);
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.condition(&view, |view, _| view.draft_ready()).await;
    view.update_in(visual, |view, window, cx| {
        view.active = true;
        view.draft_restore(window, cx);
        view.compose.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    });
    visual.run_until_parked();
    match input {
        Input::Chooser => {
            let button = visual.debug_bounds("project-attach").unwrap();
            visual.simulate_click(button.center(), Modifiers::default());
            visual.simulate_path_prompt_response(|options| {
                assert!(options.files && options.multiple && !options.directories);
                Some(vec![path.clone()])
            });
        }
        Input::Pixels | Input::CopiedFile => {
            visual.update(|_, cx| {
                cx.write_to_clipboard(match input {
                    Input::Pixels => gpui::ClipboardItem::new_image(&gpui::Image {
                        id: 2423,
                        format: gpui::ImageFormat::Png,
                        bytes: bytes.clone(),
                    }),
                    _ => gpui::ClipboardEntry::ExternalPaths(ExternalPaths(
                        vec![path.clone()].into_iter().collect(),
                    ))
                    .into(),
                })
            });
            visual.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            });
        }
        Input::Tiff => {
            // The macOS named-NSPasteboard test proves TIFF acquisition; inject
            // that offer at the production clipboard boundary for full delivery.
            struct Clipboard(Vec<u8>);
            impl crate::note_paste::NoteClipboard for Clipboard {
                fn read_offer(
                    &self,
                ) -> Result<
                    crate::note_paste::offer::PasteOffer,
                    crate::note_paste::offer::OfferError,
                > {
                    Ok(crate::note_paste::offer::PasteOffer::Image(
                        crate::note_paste::offer::PastedImage {
                            bytes: self.0.clone(),
                            kind: crate::note_paste::offer::ImageKind::Tiff,
                            source: None,
                        },
                    ))
                }
                fn has_content(&self) -> bool {
                    true
                }
                fn route(
                    &self,
                    _: &crate::note_paste::router::RouteContext,
                ) -> Result<crate::note_paste::router::Routed, crate::note_paste::offer::OfferError>
                {
                    unreachable!("chat must use its typed clipboard offer")
                }
                fn read_attachment(&self) -> Option<crate::note_paste::offer::ClipAttachment> {
                    None
                }
            }
            visual.update(|_, cx| {
                cx.set_global(crate::note_paste::ManagedNoteClipboard(Arc::new(
                    Clipboard(bytes.clone()),
                )))
            });
            visual.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            });
        }
        Input::Drop => {
            let position = visual
                .debug_bounds("project-composer-input")
                .unwrap()
                .center();
            visual.simulate_event(gpui::MouseMoveEvent {
                position,
                pressed_button: Some(MouseButton::Left),
                ..Default::default()
            });
            visual.run_until_parked();
            visual.simulate_event(gpui::FileDropEvent::Entered {
                position,
                paths: ExternalPaths(vec![path.clone()].into_iter().collect()),
            });
            visual.run_until_parked();
            visual.simulate_event(gpui::FileDropEvent::Submit { position });
        }
    }
    visual
        .condition(&view, |view, _| {
            view.draft_attachments().first().is_some_and(|row| {
                matches!(
                    row.state,
                    AttachmentState::Uploaded { .. } | AttachmentState::Error { .. }
                )
            })
        })
        .await;
    if !fail_upload {
        view.read_with(visual, |view, _| {
            assert!(
                matches!(
                    view.draft_attachments()[0].state,
                    AttachmentState::Uploaded { .. }
                ),
                "image must upload successfully: {:?}",
                view.draft_attachments()[0].state
            )
        });
    }
    if fail_upload {
        let id = view.read_with(visual, |view, cx| {
            assert!(matches!(
                view.draft_attachments()[0].state,
                AttachmentState::Error { .. }
            ));
            assert!(view.prepared_attachments().is_err());
            assert_eq!(view.compose.read(cx).value().as_str(), "Keep exact text");
            view.draft_attachments()[0].id.clone()
        });
        // Retry must send the captured image, even if the external file changed.
        std::fs::write(&path, b"replacement is not the captured image").unwrap();
        visual.run_until_parked();
        let selector: &'static str = Box::leak(format!("attachment-retry-{id}").into_boxed_str());
        let retry = visual.debug_bounds(selector).unwrap().center();
        visual.simulate_click(retry, Modifiers::default());
        visual
            .condition(&view, |view, _| {
                matches!(
                    view.draft_attachments()[0].state,
                    AttachmentState::Uploaded { .. }
                )
            })
            .await;
    }
    view.update_in(visual, |view, window, cx| {
        view.attachments.connection_changed();
        view.attachment_restore_connection(window, cx);
    });
    visual
        .condition(&view, |view, _| view.prepared_attachments().is_ok())
        .await;
    let (preview_id, reference) = view.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_str(), "Keep exact text");
        (
            view.draft_attachments()[0].id.clone(),
            view.prepared_attachments().unwrap()[0].clone(),
        )
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let preview_selector: &'static str =
        Box::leak(format!("attachment-preview-{preview_id}").into_boxed_str());
    assert!(
        visual.debug_bounds(preview_selector).is_some(),
        "the actual composer must show the uploaded image thumbnail"
    );
    let send = visual.debug_bounds("project-send").unwrap();
    visual.simulate_click(send.center(), Modifiers::default());
    visual
        .condition(&view, |view, cx| {
            !view.busy
                && view
                    .native
                    .actions
                    .read(cx)
                    .records
                    .first()
                    .is_some_and(|record| record.sequence == Some(42))
        })
        .await;
    view.update_in(visual, |view, window, cx| {
        assert!(view.draft_attachments().is_empty());
        // Reload durable commands and reconnect; neither action repeats uploads.
        view.attachments.connection_changed();
        view.attachment_restore_connection(window, cx);
        view.native_load_actions(window, cx);
    });
    visual.run_until_parked();
    let journal = view.read_with(visual, |view, _| view.native_journal().unwrap());
    let record = journal.records().unwrap().pop().unwrap();
    assert!(
        matches!(&record.command.intent, Intent::Message { attachments, .. } if attachments.as_slice() == std::slice::from_ref(&reference))
    );
    journal
        .send(&record, |_| {
            panic!("acknowledged send must never dispatch after reopen")
        })
        .unwrap();
    let received = server.received.lock().unwrap();
    assert_eq!(
        received.uploads,
        vec![expected.clone(); if fail_upload { 2 } else { 1 }]
    );
    assert_eq!(received.commands.len(), 1);
    assert_eq!(received.provider_images, vec![expected]);
}

#[gpui::test]
async fn chooser_upload_send_reaches_provider_with_exact_image(cx: &mut TestAppContext) {
    delivery(cx, Input::Chooser, false).await;
}
#[gpui::test]
async fn screenshot_paste_upload_send_reaches_provider_with_exact_image(cx: &mut TestAppContext) {
    delivery(cx, Input::Pixels, false).await;
}
#[gpui::test]
async fn issue2521_tiff_paste_upload_send_reaches_provider_with_lossless_png(
    cx: &mut TestAppContext,
) {
    delivery(cx, Input::Tiff, false).await;
}
#[gpui::test]
async fn copied_file_paste_upload_send_reaches_provider_with_exact_image(cx: &mut TestAppContext) {
    delivery(cx, Input::CopiedFile, false).await;
}
#[gpui::test]
async fn drop_upload_send_reaches_provider_with_exact_image(cx: &mut TestAppContext) {
    delivery(cx, Input::Drop, false).await;
}
#[gpui::test]
async fn failed_upload_retry_keeps_original_bytes_and_dispatches_once(cx: &mut TestAppContext) {
    delivery(cx, Input::Chooser, true).await;
}
