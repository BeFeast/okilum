// Kept independent of the parser while #1388 owns the shared log modules.
// The production module is wired into log/file and tail after that merge.
#[path = "../src/log/window.rs"]
mod window;

use std::cell::{Cell, RefCell};
use std::io;
use window::{read_tail, ReadAt, ReadBudget};

struct Guarded {
    len: u64,
    suffix: Vec<u8>,
    allowed: u64,
    reads: RefCell<Vec<(u64, usize)>>,
    max_read: usize,
    interrupt: Cell<bool>,
}

impl ReadAt for Guarded {
    fn read_at(&self, output: &mut [u8], offset: u64) -> io::Result<usize> {
        self.reads.borrow_mut().push((offset, output.len()));
        if offset < self.allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "full scan rejected",
            ));
        }
        if self.interrupt.replace(false) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = output
            .len()
            .min(self.max_read)
            .min(self.len.saturating_sub(offset) as usize);
        let base = self.len - self.suffix.len() as u64;
        for (i, byte) in output[..n].iter_mut().enumerate() {
            let at = offset + i as u64;
            *byte = if at < base {
                b'x'
            } else {
                self.suffix[(at - base) as usize]
            };
        }
        Ok(n)
    }
}

fn source(len: u64, suffix: &[u8], bytes: usize) -> Guarded {
    Guarded {
        len,
        suffix: suffix.to_vec(),
        allowed: len.saturating_sub(bytes as u64 + 1),
        reads: RefCell::new(Vec::new()),
        max_read: usize::MAX,
        interrupt: Cell::new(false),
    }
}

fn budget(bytes: usize, lines: usize, line_bytes: usize) -> ReadBudget {
    ReadBudget {
        bytes,
        lines,
        line_bytes,
    }
}

#[test]
fn a_five_gib_source_is_not_scanned_or_allocated() {
    let len = 5_u64 << 30;
    let s = source(len, b"discarded prefix\nreal penultimate\nreal last\n", 32);
    // Negative control: this source detects and rejects a full-file reader.
    assert_eq!(
        s.read_at(&mut [0; 8], 0).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    s.reads.borrow_mut().clear();
    let w = read_tail(&s, len, budget(32, 100, 100)).unwrap();
    assert_eq!(w.bytes, b"real penultimate\nreal last\n");
    assert_eq!(w.offset, len - w.bytes.len() as u64);
    assert_eq!(w.end, len);
    assert!(!w.shortened());
    assert!(w.bytes.capacity() <= 32);
    assert!(s
        .reads
        .borrow()
        .iter()
        .all(|&(at, n)| at >= len - 33 && n <= 32));
}

#[test]
fn an_invalid_budget_is_rejected_before_io_or_allocation() {
    let s = source(5_u64 << 30, b"last\n", 32);
    for limits in [
        budget(0, 1, 1),
        budget(window::WINDOW_BYTES + 1, 1, 1),
        budget(1, window::WINDOW_LINES + 1, 1),
        budget(1, 1, 0),
    ] {
        assert_eq!(
            read_tail(&s, s.len, limits).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert!(s.reads.borrow().is_empty());
}

#[test]
fn production_budget_reads_at_most_sixty_four_mib_and_one_boundary_byte() {
    let len = 5_u64 << 30;
    let s = source(len, b"\nreal final record\n", window::WINDOW_BYTES);
    let w = read_tail(&s, len, ReadBudget::default()).unwrap();
    assert_eq!(w.bytes, b"real final record\n");
    assert_eq!(w.offset, len - w.bytes.len() as u64);
    assert!(w.bytes.capacity() <= window::WINDOW_BYTES);
    let requested: usize = s.reads.borrow().iter().map(|&(_, n)| n).sum();
    assert_eq!(requested, window::WINDOW_BYTES + 1);
    assert!(s
        .reads
        .borrow()
        .iter()
        .all(|&(at, _)| at >= len - requested as u64));
}

#[test]
fn exact_newline_boundary_keeps_the_first_real_record() {
    let s = source(10, b"old\none\nx\n", 6);
    let w = read_tail(&s, 10, budget(6, 100, 100)).unwrap();
    assert_eq!(w.offset, 4);
    assert_eq!(w.bytes, b"one\nx\n");
}

#[test]
fn utf8_partial_head_and_crlf_are_not_invented_records() {
    let data = "старое\r\nновое\r\nlast\n".as_bytes();
    let s = source(data.len() as u64, data, 18);
    let w = read_tail(&s, s.len, budget(18, 100, 100)).unwrap();
    assert_eq!(w.bytes, "новое\r\nlast\n".as_bytes());
    assert!(std::str::from_utf8(&w.bytes).is_ok());
    assert_eq!(w.offset + w.bytes.len() as u64, s.len);
}

#[test]
fn millions_of_short_lines_cannot_grow_the_index_budget() {
    let s = source(1024, &b"x\n".repeat(512), 1024);
    let w = read_tail(&s, s.len, budget(1024, 3, 100)).unwrap();
    assert_eq!(w.bytes, b"x\nx\nx\n");
    assert_eq!(w.offset, 1018);
    // Incomplete last lines count against the same budget.
    let s = source(7, b"a\nb\nc\nd", 7);
    assert_eq!(read_tail(&s, 7, budget(7, 2, 100)).unwrap().bytes, b"c\nd");
}

#[test]
fn an_oversized_or_unterminated_line_never_expands_the_buffer() {
    let data = b"ok\n012345678901234567890123456789\nlast\n";
    let s = source(data.len() as u64, data, data.len());
    let w = read_tail(&s, s.len, budget(data.len(), 100, 8)).unwrap();
    assert!(w.oversized);
    assert_eq!(w.bytes, b"last\n");
    assert_eq!(w.offset, s.len - 5);
    let s = source(5_u64 << 30, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", 32);
    let w = read_tail(&s, s.len, budget(32, 100, 8)).unwrap();
    assert!(w.bytes.is_empty());
    assert!(w.oversized);
    assert_eq!(w.offset, s.len);
    assert_eq!(w.bytes.capacity(), 32);
}

#[test]
fn continuation_heads_are_removed_without_rejoining_unloaded_records() {
    let data = b"  missing head\n  continuation\nrecord one\n  child\nrecord two\n";
    let s = source(data.len() as u64, data, data.len());
    let mut w = read_tail(&s, s.len, budget(data.len(), 100, 100)).unwrap();
    w.align_record(|line| line.starts_with(b"record "));
    assert_eq!(w.bytes, b"record one\n  child\nrecord two\n");
    assert_eq!(w.offset, 30);
}

#[test]
fn short_reads_interruptions_append_and_truncate_keep_absolute_offsets() {
    let mut s = source(8, b"a\nb\nc\nd\n", 8);
    s.max_read = 2;
    s.interrupt.set(true);
    let w = read_tail(&s, 8, budget(8, 100, 100)).unwrap();
    assert_eq!(w.bytes, b"a\nb\nc\nd\n");
    // A captured end excludes a racing append.
    let w = read_tail(&s, 4, budget(8, 100, 100)).unwrap();
    assert_eq!(w.bytes, b"a\nb\n");
    assert_eq!(w.end, 4);
    // A shrink must return a short read, never zero-filled invented content.
    s.len = 4;
    s.suffix = b"a\nb\n".to_vec();
    let w = read_tail(&s, 8, budget(8, 100, 100)).unwrap();
    assert!(w.shortened());
    assert_eq!(w.bytes, b"a\nb\n");
    assert_eq!(w.end, 4);
    let w = read_tail(&s, 8, budget(2, 100, 100)).unwrap();
    assert!(w.bytes.is_empty());
    assert!(w.shortened());
}
