use super::*;
use ::core::prelude::v1::test;
use std::sync::Mutex;

struct Fake {
    bytes: Arc<Mutex<Vec<u8>>>,
    reads: Arc<Mutex<Vec<u64>>>,
    cancel_on_read: Option<Arc<AtomicBool>>,
}
impl Transport for Fake {
    fn open(&mut self, _: &Target) -> io::Result<()> {
        Ok(())
    }
    fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
        self.reads.lock().unwrap().push(offset);
        if let Some(cancel) = &self.cancel_on_read {
            cancel.store(true, Ordering::Release);
        }
        let bytes = self.bytes.lock().unwrap();
        let start = (offset as usize).min(bytes.len());
        Ok(bytes[start..bytes.len().min(start + limit)].to_vec())
    }
    fn disconnect(&mut self) {}
}
fn fixture(bytes: &[u8]) -> Fake {
    Fake {
        bytes: Arc::new(Mutex::new(bytes.to_vec())),
        reads: Arc::new(Mutex::new(Vec::new())),
        cancel_on_read: None,
    }
}
#[test]
fn snapshot_reads_exact_lines_and_reports_unfinished_tail() {
    let fake = fixture("one\r\n雪\npartial".as_bytes());
    let mut frames = Vec::new();
    run(
        Target::parse("sftp://fixture/var/app.log").unwrap(),
        fake,
        false,
        "snapshot".into(),
        Arc::new(AtomicBool::new(false)),
        |frame| {
            frames.push(frame);
            true
        },
    );
    let last = frames.last().unwrap();
    assert!(!last.active && !last.error);
    assert_eq!(
        last.data.as_ref().unwrap().bytes(),
        "one\r\n雪\n".as_bytes()
    );
    assert!(last.message.contains("7 trailing bytes"));
}
#[test]
fn rewrite_stops_follow_without_replacing_loaded_rows() {
    let fake = fixture(b"old\n");
    let bytes = fake.bytes.clone();
    let mut last = None;
    run(
        Target::parse("sftp://fixture/app.log").unwrap(),
        fake,
        true,
        "rewrite".into(),
        Arc::new(AtomicBool::new(false)),
        |frame| {
            if frame.active && frame.data.is_some() {
                *bytes.lock().unwrap() = b"new\n".to_vec();
            }
            last = Some(frame);
            true
        },
    );
    let last = last.unwrap();
    assert!(!last.active && last.error);
    assert!(last.message.contains("continuity lost"));
    assert_eq!(last.data.unwrap().bytes(), b"old\n");
}
#[test]
fn cancellation_between_prefix_reads_does_not_start_another_operation() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut fake = fixture(b"first\n");
    fake.cancel_on_read = Some(cancel.clone());
    let reads = fake.reads.clone();
    let mut last = None;
    run(
        Target::parse("sftp://fixture/app.log").unwrap(),
        fake,
        true,
        "cancel".into(),
        cancel,
        |frame| {
            last = Some(frame);
            true
        },
    );
    assert_eq!(*reads.lock().unwrap(), vec![0]);
    let last = last.unwrap();
    assert!(!last.active && !last.error);
    assert_eq!(last.message, "Stopped");
}
