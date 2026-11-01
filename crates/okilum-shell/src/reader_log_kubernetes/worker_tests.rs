use super::*;
use ::core::prelude::v1::test;
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

struct Fake {
    events: VecDeque<Input>,
    calls: Arc<AtomicUsize>,
    cancel_after_read: Option<Arc<AtomicBool>>,
}
impl Source for Fake {
    fn next(&mut self) -> io::Result<Option<Input>> {
        if let Some(cancel) = &self.cancel_after_read {
            cancel.store(true, Ordering::Release);
        }
        Ok(self.events.pop_front())
    }
    fn cancel(&mut self) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
fn execute(events: Vec<Input>) -> Update {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = Fake {
        events: events.into(),
        calls: calls.clone(),
        cancel_after_read: None,
    };
    let mut updates = Vec::new();
    run(
        source,
        Assembler::new("pod.log".into(), "fake".into()).unwrap(),
        &AtomicBool::new(false),
        |update, _| {
            updates.push(update);
            true
        },
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    updates.pop().unwrap()
}

#[test]
fn fragmented_stdout_remains_exact_and_stderr_is_not_a_record() {
    let update = execute(vec![
        Input::Data(b"one\r".to_vec()),
        Input::Diagnostic(b"warning: fixture".to_vec()),
        Input::Data(b"\n\xe9".to_vec()),
        Input::Data(b"\x9b\xaa\npartial".to_vec()),
        Input::Exit(true, "success".into()),
    ]);
    assert_eq!(update.status, Status::Complete);
    assert_eq!(update.snapshot.unwrap().bytes(), "one\r\n雪\n".as_bytes());
    assert_eq!(update.diagnostic, "warning: fixture");
    assert_eq!(update.pending, 7);
}

#[test]
fn failed_exit_and_history_limit_retain_previous_rows() {
    let failed = execute(vec![
        Input::Data(b"kept\n".to_vec()),
        Input::Exit(false, "exit status: 1".into()),
    ]);
    assert!(matches!(failed.status, Status::Failed(_)));
    assert_eq!(failed.snapshot.unwrap().bytes(), b"kept\n");
    let limit = execute(vec![
        Input::Data(b"kept\n".to_vec()),
        Input::Data(vec![b'x'; 64 * 1024 + 1]),
    ]);
    assert!(matches!(limit.status, Status::Failed(ref error) if error.contains("64 KiB")));
    assert_eq!(limit.snapshot.unwrap().bytes(), b"kept\n");
}

#[test]
fn stop_during_receive_cancels_exact_source_before_another_read() {
    let calls = Arc::new(AtomicUsize::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let source = Fake {
        events: vec![
            Input::Data(b"before stop\n".to_vec()),
            Input::Data(b"must not read\n".to_vec()),
        ]
        .into(),
        calls: calls.clone(),
        cancel_after_read: Some(cancel.clone()),
    };
    let mut last = None;
    run(
        source,
        Assembler::new("pod.log".into(), "cancel".into()).unwrap(),
        &cancel,
        |update, _| {
            last = Some(update);
            true
        },
    );
    let last = last.unwrap();
    assert_eq!(last.status, Status::Stopped);
    assert_eq!(last.snapshot.unwrap().bytes(), b"before stop\n");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}
