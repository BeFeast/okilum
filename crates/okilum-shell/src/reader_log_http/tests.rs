use super::*;
use ::core::prelude::v1::test;
use okilum_core::log::http::{Error, Status};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

fn source() -> Source {
    Source {
        name: "Application".into(),
        url: "https://example.invalid/app.log".into(),
    }
}
struct Fake {
    replies: VecDeque<Result<(Vec<u8>, Status), Error>>,
    calls: Arc<AtomicUsize>,
    dropped: Option<mpsc::Sender<()>>,
}
impl worker::Input for Fake {
    fn poll(&mut self) -> Result<(Vec<u8>, Status), Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.replies
            .pop_front()
            .unwrap_or(Ok((Vec::new(), Status::Connected)))
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(done) = self.dropped.take() {
            let _ = done.send(());
        }
    }
}
fn factory(
    replies: Vec<Result<(Vec<u8>, Status), Error>>,
    calls: Arc<AtomicUsize>,
    dropped: Option<mpsc::Sender<()>>,
) -> worker::Factory {
    Arc::new(move |_| {
        Ok(Box::new(Fake {
            replies: replies.clone().into(),
            calls: calls.clone(),
            dropped: dropped.clone(),
        }))
    })
}
#[test]
fn configuration_is_vault_scoped_atomic_and_never_saves_url_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let vault = dir.path().join("a");
    let other = dir.path().join("b");
    assert!(model::load(&state, &vault).unwrap().is_empty());
    model::save(&state, &vault, &source()).unwrap();
    assert_eq!(model::load(&state, &vault).unwrap(), vec![source()]);
    assert!(model::load(&state, &other).unwrap().is_empty());
    for url in [
        "http://host/log",
        "https://u:p@host/log",
        "https://host/log?token=secret",
        "https://host/log#fragment",
    ] {
        assert!(model::save(
            &state,
            &vault,
            &Source {
                url: url.into(),
                ..source()
            }
        )
        .is_err());
        assert_eq!(model::load(&state, &vault).unwrap(), vec![source()]);
    }
    let mut renamed = source();
    renamed.name = "Renamed".into();
    assert_eq!(
        model::save(&state, &vault, &renamed).unwrap(),
        vec![renamed]
    );
}
#[test]
fn worker_assembles_chunks_reconnects_and_reports_continuity_loss_without_duplicates() {
    let calls = Arc::new(AtomicUsize::new(0));
    let replies = vec![
        Ok((b"first\r\n\xe9".to_vec(), Status::Connected)),
        Ok((b"\x9b\xaa\n".to_vec(), Status::Connected)),
        Ok((
            Vec::new(),
            Status::Disconnected {
                retry_after: Duration::from_millis(5),
            },
        )),
        Err(Error::SourceChanged),
    ];
    let (_worker, events) =
        worker::Worker::start(source(), factory(replies, calls.clone(), None)).unwrap();
    let one = events.recv_blocking().unwrap();
    assert_eq!(one.data.unwrap().bytes(), b"first\r\n");
    let two = events.recv_blocking().unwrap();
    assert_eq!(two.data.unwrap().bytes(), "first\r\n雪\n".as_bytes());
    let reconnect = events.recv_blocking().unwrap();
    assert!(reconnect.active && reconnect.message.contains("Disconnected"));
    assert!(reconnect.data.is_none());
    let stopped = events.recv_blocking().unwrap();
    assert!(!stopped.active && stopped.message.contains("changed or shrank"));
    assert!(stopped.data.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(events.recv_blocking().is_err());
}
#[test]
fn stop_releases_the_owned_input_without_another_poll() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (done, closed) = mpsc::channel();
    let (worker, events) = worker::Worker::start(
        source(),
        factory(
            vec![Ok((b"row\n".to_vec(), Status::Connected))],
            calls.clone(),
            Some(done),
        ),
    )
    .unwrap();
    assert_eq!(
        events.recv_blocking().unwrap().data.unwrap().index().len(),
        1
    );
    drop(worker);
    drop(events);
    closed.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "positive first poll and no poll after Stop"
    );
}
#[gpui::test]
fn mounted_start_stream_rows_stop_and_stale_reply_keep_the_visible_session(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let (dropped, released) = mpsc::channel();
    let mock = factory(
        vec![Ok((
            b"level=info msg=hello\n".to_vec(),
            Status::Disconnected {
                retry_after: Duration::from_secs(3600),
            },
        ))],
        calls.clone(),
        Some(dropped),
    );
    // A real OS worker cannot wake GPUI's deterministic test scheduler. Queue
    // the first receipt before Start registers a local waiter, then hold the
    // next poll in the worker's interruptible reconnect wait until real Stop.
    // Pure worker tests above cover connected/chunked/reconnected publication.
    let (worker, events) = worker::Worker::start(
        Source {
            name: "Test".into(),
            url: "https://example.invalid/log".into(),
        },
        mock.clone(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while events.is_empty() {
        assert!(
            Instant::now() < deadline,
            "real worker must queue its first frame"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        events.len(),
        1,
        "positive control: a real frame is held for Start"
    );
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut panel = Panel::new(vault, dir.path().join("state"), window, cx);
            panel.factory = mock;
            panel.ready_worker = Some((worker, events));
            panel
                .name
                .update(cx, |i, cx| i.set_value("Test", window, cx));
            panel.url.update(cx, |i, cx| {
                i.set_value("https://example.invalid/log", window, cx)
            });
            panel
        });
        panel = Some(view.clone());
        Root::new(view, window, cx)
    });
    let panel = panel.unwrap();
    visual.simulate_resize(size(px(1200.), px(800.)));
    visual.run_until_parked();
    let start = visual
        .debug_bounds("https-log-start")
        .expect("Start control mounted");
    visual.simulate_click(start.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        visual.run_until_parked();
        if panel.read_with(visual, |p, _| p.data.is_some()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker must publish the positive-control line"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let generation = panel.read_with(visual, |p, _| p.generation);
    panel.read_with(visual, |p, cx| {
        let view = p.view.as_ref().expect("ordinary LogView mounted").read(cx);
        assert_eq!(
            view.data().unwrap().raw(0).unwrap(),
            b"level=info msg=hello"
        );
    });
    assert!(calls.load(Ordering::SeqCst) >= 1);
    panel.update(visual, |p, cx| {
        let data = p.data.as_ref().unwrap();
        let mut assembler = okilum_core::log::stream::Assembler::new(
            data.path().to_path_buf(),
            data.scope().into(),
        )
        .unwrap();
        let next = assembler
            .append(b"level=info msg=hello\nlevel=warn msg=next\n")
            .unwrap()
            .unwrap();
        let view = p.view.as_ref().unwrap().clone();
        view.update(cx, |view, _| {
            view.selected = Some(0);
            view.marks.insert((0, 0));
        });
        p.receive(
            generation,
            worker::Frame {
                data: Some(next),
                message: "Connected".into(),
                active: true,
            },
            cx,
        );
        {
            let view = view.read(cx);
            assert_eq!(view.selected, Some(0));
            assert!(view.marks.contains(&(0, 0)));
            let job = view
                .export_job(reader_log::ExportScope::Shown)
                .expect("stream export snapshot");
            let mut out = Vec::new();
            assert_eq!(
                job.write(&mut out, &okilum_core::log::export::ExportFormat::Raw)
                    .unwrap(),
                2
            );
            assert_eq!(out, b"level=info msg=hello\nlevel=warn msg=next\n");
        }
    });
    let stop = visual
        .debug_bounds("https-log-stop")
        .expect("Stop control mounted");
    visual.simulate_click(stop.center(), Modifiers::default());
    visual.run_until_parked();
    released
        .recv_timeout(Duration::from_secs(2))
        .expect("Stop releases the real input without waiting for reconnect");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no poll after Stop");
    panel.update(visual, |p, cx| {
        p.receive(
            generation,
            worker::Frame {
                data: None,
                message: "stale connection".into(),
                active: true,
            },
            cx,
        );
        assert!(!p.active && p.message.starts_with("Stopped"));
        assert_eq!(
            p.data.as_ref().unwrap().bytes(),
            b"level=info msg=hello\nlevel=warn msg=next\n"
        );
    });
}
