//! Opt-in, thread-local measurements for one background Reader preparation.
//! No source, path, target or rendered text is retained. Timings are inclusive.
use serde::Serialize;
use std::{cell::RefCell, collections::BTreeMap, time::Instant};

#[derive(Default, Serialize)]
pub struct Report {
    pub phases: BTreeMap<&'static str, Measurement>,
    pub counts: BTreeMap<&'static str, u64>,
}
#[derive(Default, Serialize)]
pub struct Measurement {
    pub calls: u64,
    pub duration_ms: f64,
}
thread_local! {
    static REPORT: RefCell<Option<Report>> = const { RefCell::new(None) };
}

/// Restores an outer recorder even when preparation unwinds.
struct Restore(Option<Report>);
impl Drop for Restore {
    fn drop(&mut self) {
        REPORT.with(|r| *r.borrow_mut() = self.0.take());
    }
}
pub fn capture<T>(f: impl FnOnce() -> T) -> (T, Report) {
    let _restore = Restore(REPORT.with(|r| r.replace(Some(Report::default()))));
    let value = f();
    let report = REPORT.with(|r| r.borrow_mut().take().unwrap_or_default());
    (value, report)
}
pub struct Phase {
    name: &'static str,
    started: Instant,
}
pub fn phase(name: &'static str) -> Option<Phase> {
    REPORT.with(|r| r.borrow().is_some()).then(|| Phase {
        name,
        started: Instant::now(),
    })
}
impl Drop for Phase {
    fn drop(&mut self) {
        REPORT.with(|r| {
            if let Some(report) = r.borrow_mut().as_mut() {
                let m = report.phases.entry(self.name).or_default();
                m.calls += 1;
                m.duration_ms += self.started.elapsed().as_secs_f64() * 1000.;
            }
        });
    }
}
pub(crate) fn count(name: &'static str, value: usize) {
    REPORT.with(|r| {
        if let Some(report) = r.borrow_mut().as_mut() {
            *report.counts.entry(name).or_default() += value as u64;
        }
    });
}
/// Count explicit Rust filesystem API calls, not kernel syscalls or cache misses.
pub(crate) fn fs<T>(f: impl FnOnce() -> T) -> T {
    let _phase = phase("filesystem");
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_only_the_scoped_job_and_restores_nested_capture() {
        count("links", 99);
        let ((), outer) = capture(|| {
            let _phase = phase("parse");
            count("links", 2);
            let (value, inner) = capture(|| {
                count("links", 3);
                fs(|| 7)
            });
            assert_eq!(value, 7);
            assert_eq!(inner.counts["links"], 3);
            assert_eq!(inner.phases["filesystem"].calls, 1);
            count("links", 1);
        });
        assert_eq!(outer.counts["links"], 3);
        assert_eq!(outer.phases["parse"].calls, 1);
        assert!(!outer.phases.contains_key("filesystem"));
        assert!(phase("inactive").is_none());
    }
    #[test]
    fn reader_report_preserves_output_and_observes_real_work() {
        let dir = tempfile::tempdir().unwrap();
        let source =
            "# שלום\n\n[[Target]] [missing](missing.md)\n\n![[Target]]\n\n![image](missing.png)\n";
        std::fs::write(dir.path().join("note.md"), source).unwrap();
        std::fs::write(dir.path().join("Target.md"), "# Target\n\nBody.\n").unwrap();
        let vault = crate::Vault::scan(dir.path()).unwrap();
        let expected = crate::render::reader_document(&vault, "note.md").unwrap();
        let (actual, report) = capture(|| crate::render::reader_document(&vault, "note.md"));
        let actual = actual.unwrap();
        assert_eq!(actual.rendered, expected.rendered);
        assert_eq!(actual.canonical_source, expected.canonical_source);
        assert_eq!(actual.links, expected.links);
        for name in [
            "read_source",
            "parse",
            "links",
            "embeds",
            "filesystem",
            "wiki_index_lookup",
            "markdown_index_lookup",
        ] {
            assert!(report.phases[name].calls > 0, "{name}");
        }
        assert!(report.counts["parsed_links"] > 0);
        assert!(report.counts["embeds"] > 0);
        assert!(!serde_json::to_string(&report).unwrap().contains("Target"));
    }
    #[test]
    fn restores_after_panic() {
        let _ = std::panic::catch_unwind(|| capture(|| panic!("probe")));
        assert!(phase("inactive").is_none());
    }
}
