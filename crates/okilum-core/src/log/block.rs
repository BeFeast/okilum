//! Multi-line entries (#1161): crash blocks and continuation lines.
//!
//! An entry is a run of lines. Its first line, the head, is a record, the
//! start of a recognised block, or a line that is neither. Then:
//!
//! - A block (a Go panic, a sanitizer report, a C++ stack trace, ...) takes
//!   every following line up to the next record, blank lines included: its
//!   lines need not be indented (`goroutine 1 [running]:`, `READ of size 8`).
//!   A block is its own entry, not part of the record before it, because the
//!   record a crash interrupted is rarely the record that caused it; its level
//!   is `Error`.
//! - Any other entry takes the indented lines that follow it (`\tat ...`,
//!   `    File "x.py", line 3`) and Java's `Caused by:`.
//!
//! The entry's byte range covers all its lines, so a raw copy returns them
//! exactly. Fields come from the head alone. A `Plain` file has no records to
//! anchor anything to and keeps one entry per line.

use super::Level;

/// Lines one entry takes at most. A longer run starts a new entry, so a
/// runaway tail cannot become one row the size of the file.
pub(super) const MAX_LINES: u32 = 10_000;

/// The level of a block entry.
pub(super) const LEVEL: Level = Level::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    GoPanic,
    RustPanic,
    PythonTraceback,
    JavaException,
    JavaScriptError,
    SanitizerReport,
    StackTrace,
}

impl BlockKind {
    pub fn label(self) -> &'static str {
        match self {
            BlockKind::GoPanic => "Go panic",
            BlockKind::RustPanic => "Rust panic",
            BlockKind::PythonTraceback => "Python traceback",
            BlockKind::JavaException => "Java exception",
            BlockKind::JavaScriptError => "JavaScript error",
            BlockKind::SanitizerReport => "Sanitizer report",
            BlockKind::StackTrace => "Stack trace",
        }
    }
}

/// The first line of an entry, without its line ending.
pub fn head(raw: &[u8]) -> &[u8] {
    let line = memchr::memchr(b'\n', raw).map_or(raw, |end| &raw[..end]);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The lines after the head, exactly as written, if the entry has any.
pub fn continuation(raw: &[u8]) -> Option<&[u8]> {
    memchr::memchr(b'\n', raw).map(|end| &raw[end + 1..])
}

/// Lines in an entry, blank ones inside it included.
pub fn line_count(raw: &[u8]) -> usize {
    memchr::memchr_iter(b'\n', raw).count() + 1
}

/// The block an entry starts, judged from its first two lines: a sanitizer
/// report opens with a rule of `=` that means nothing on its own.
pub fn block_kind(raw: &[u8]) -> Option<BlockKind> {
    let first = head(raw);
    if is_rule(first) {
        let second = continuation(raw).map(head)?;
        return is_sanitizer(second).then_some(BlockKind::SanitizerReport);
    }
    starts_block(first)
}

/// A line that a preceding entry takes as its own.
pub(super) fn is_continuation(line: &[u8]) -> bool {
    matches!(line.first(), Some(b' ' | b'\t')) || line.starts_with(b"Caused by: ")
}

fn is_rule(line: &[u8]) -> bool {
    line.len() >= 20 && line.iter().all(|&c| c == b'=')
}

/// `==90112==ERROR: AddressSanitizer: ...` and the other sanitizers' headers.
fn is_sanitizer(line: &[u8]) -> bool {
    let after_pid = line
        .strip_prefix(b"==")
        .and_then(|rest| {
            let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            rest[digits..].strip_prefix(b"==").filter(|_| digits > 0)
        })
        .unwrap_or(line);
    (after_pid.starts_with(b"ERROR: ") || after_pid.starts_with(b"WARNING: "))
        && contains(after_pid, b"Sanitizer")
}

fn starts_block(line: &[u8]) -> Option<BlockKind> {
    const STACK_TRACES: [&[u8]; 6] = [
        // backward-cpp
        b"Stack trace (most recent call last)",
        // glog CHECK failures and its failure signal handler
        b"*** Check failure stack trace: ***",
        b"*** Aborted at ",
        // glibc's fatal-error dump, backtrace_symbols() helpers
        b"======= Backtrace: =========",
        b"Obtained ",
        // libstdc++ on an uncaught exception
        b"terminate called after throwing an instance of",
    ];
    if line.starts_with(b"panic: ")
        || line.starts_with(b"fatal error: ")
        || is_goroutine_header(line)
    {
        return Some(BlockKind::GoPanic);
    }
    if line.starts_with(b"thread '") && contains(line, b"' panicked at") {
        return Some(BlockKind::RustPanic);
    }
    if line.starts_with(b"Traceback (most recent call last):") {
        return Some(BlockKind::PythonTraceback);
    }
    if line.starts_with(b"Exception in thread \"") || is_java_throwable(line) {
        return Some(BlockKind::JavaException);
    }
    if is_sanitizer(line)
        || line.starts_with(b"WARNING: ThreadSanitizer:")
        || contains(line, b": runtime error: ")
    {
        return Some(BlockKind::SanitizerReport);
    }
    if STACK_TRACES.iter().any(|start| line.starts_with(start))
        && (!line.starts_with(b"Obtained ") || line.ends_with(b"stack frames."))
    {
        return Some(BlockKind::StackTrace);
    }
    if is_js_error(line) {
        return Some(BlockKind::JavaScriptError);
    }
    None
}

/// `goroutine 1187 [running]:`, as a dump without a panic line starts.
fn is_goroutine_header(line: &[u8]) -> bool {
    line.strip_prefix(b"goroutine ").is_some_and(|rest| {
        let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        digits > 0 && rest[digits..].starts_with(b" [") && line.ends_with(b"]:")
    })
}

/// `java.lang.IllegalStateException: boom`: a qualified class name ending in
/// `Exception`, `Error` or `Throwable`, alone or before `: `.
fn is_java_throwable(line: &[u8]) -> bool {
    let name_end = line.iter().position(|&c| c == b':').unwrap_or(line.len());
    let name = &line[..name_end];
    let rest = &line[name_end..];
    (rest.is_empty() || rest.starts_with(b": "))
        && name.first().is_some_and(u8::is_ascii_lowercase)
        && name.contains(&b'.')
        && name
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'$'))
        && [&b"Exception"[..], b"Error", b"Throwable"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

/// `TypeError: Cannot read properties of undefined`, `Error: boom`, and the
/// same after Node's `Uncaught `.
fn is_js_error(line: &[u8]) -> bool {
    let line = line.strip_prefix(b"Uncaught ").unwrap_or(line);
    let Some(colon) = memchr::memchr(b':', line) else {
        return false;
    };
    let name = &line[..colon];
    line[colon..].starts_with(b": ")
        && name.ends_with(b"Error")
        && name.first().is_some_and(u8::is_ascii_uppercase)
        && name.iter().all(u8::is_ascii_alphanumeric)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(haystack, needle).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_and_continuations() {
        assert_eq!(head(b"a\r\n\tb\n c"), b"a");
        assert_eq!(continuation(b"a\r\n\tb\n c"), Some(&b"\tb\n c"[..]));
        assert_eq!(continuation(b"a"), None);
        assert_eq!(line_count(b"a\n\nb"), 3);
        assert!(is_continuation(b"\tat com.example.Main.run(Main.java:3)"));
        assert!(is_continuation(b"    File \"x.py\", line 3, in <module>"));
        assert!(is_continuation(b"Caused by: java.io.IOException: closed"));
        assert!(!is_continuation(b"{\"msg\":\"a\""));
        assert!(!is_continuation(b"goroutine 1 [running]:"));
    }

    #[test]
    fn recognised_block_starts() {
        let cases: &[(&str, BlockKind)] = &[
            (
                "panic: runtime error: invalid memory address or nil pointer dereference",
                BlockKind::GoPanic,
            ),
            ("fatal error: concurrent map writes", BlockKind::GoPanic),
            ("goroutine 1187 [running]:", BlockKind::GoPanic),
            (
                "thread 'main' panicked at src/main.rs:2:5:",
                BlockKind::RustPanic,
            ),
            (
                "Traceback (most recent call last):",
                BlockKind::PythonTraceback,
            ),
            (
                "Exception in thread \"main\" java.lang.NullPointerException",
                BlockKind::JavaException,
            ),
            (
                "java.lang.IllegalStateException: pool closed",
                BlockKind::JavaException,
            ),
            (
                "TypeError: Cannot read properties of undefined (reading 'id')",
                BlockKind::JavaScriptError,
            ),
            ("Uncaught Error: boom", BlockKind::JavaScriptError),
            (
                "==90112==ERROR: AddressSanitizer: heap-use-after-free on address 0x60b0000003a8",
                BlockKind::SanitizerReport,
            ),
            (
                "WARNING: ThreadSanitizer: data race (pid=7)",
                BlockKind::SanitizerReport,
            ),
            (
                "src/x.cpp:12:5: runtime error: signed integer overflow",
                BlockKind::SanitizerReport,
            ),
            (
                "Stack trace (most recent call last) in thread 4414:",
                BlockKind::StackTrace,
            ),
            ("*** Check failure stack trace: ***", BlockKind::StackTrace),
            ("Obtained 12 stack frames.", BlockKind::StackTrace),
            (
                "terminate called after throwing an instance of 'std::bad_alloc'",
                BlockKind::StackTrace,
            ),
        ];
        for (line, kind) in cases {
            assert_eq!(block_kind(line.as_bytes()), Some(*kind), "{line}");
        }
    }

    #[test]
    fn ordinary_lines_start_no_block() {
        for line in [
            "Error while loading config, retrying",
            "panicked at\tmain.rs",
            "error: build failed",
            "Errors: 0",
            "com.example.Service started",
            "Obtained a lease",
            "goroutine leak suspected",
            "=====================",
            "WARNING: disk almost full",
            "",
        ] {
            assert_eq!(block_kind(line.as_bytes()), None, "{line}");
        }
    }

    #[test]
    fn a_rule_opens_a_sanitizer_report_only_before_its_header() {
        let rule = "=".repeat(65);
        assert_eq!(
            block_kind(
                format!("{rule}\n==7==ERROR: LeakSanitizer: detected memory leaks").as_bytes()
            ),
            Some(BlockKind::SanitizerReport)
        );
        assert_eq!(
            block_kind(format!("{rule}\nRelease notes").as_bytes()),
            None
        );
        assert_eq!(block_kind(rule.as_bytes()), None);
    }
}
