//! Best-effort diagnostic reports outside canonical notes; never a loading gate.
use super::*;
use anyhow::Result;
use gpui_component::WindowExt;

const LOG_FILE: &str = "reader-diagnostic.log";
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Timings use one process clock. UI callbacks only enqueue; disk I/O is owned
/// by the diagnostic thread, including startup before a Reader exists.
#[derive(Clone)]
pub(crate) struct Trace {
    inner: Arc<TraceInner>,
    root: Option<PathBuf>,
}

struct TraceInner {
    start: std::time::Instant,
    launch: String,
    send: std::sync::mpsc::Sender<(Option<PathBuf>, serde_json::Value)>,
    seen: std::sync::Mutex<std::collections::HashSet<(Option<PathBuf>, &'static str)>>,
}

pub(crate) struct LaunchTrace(pub Trace);
impl Global for LaunchTrace {}

pub(crate) struct Phase {
    trace: Trace,
    name: &'static str,
    start: std::time::Instant,
}

impl Trace {
    #[cfg(test)]
    pub(crate) fn new(state: Option<PathBuf>, root: Option<PathBuf>) -> Self {
        Self::new_at(state, root, std::time::Instant::now())
    }

    pub(crate) fn new_at(
        state: Option<PathBuf>,
        root: Option<PathBuf>,
        start: std::time::Instant,
    ) -> Self {
        let (send, receive) = std::sync::mpsc::channel::<(Option<PathBuf>, serde_json::Value)>();
        std::thread::spawn(move || {
            while let Ok((root, report)) = receive.recv() {
                append_report(root.as_deref(), state.as_deref(), &report);
            }
        });
        let trace = Self {
            inner: Arc::new(TraceInner {
                start,
                launch: uuid::Uuid::new_v4().to_string(),
                send,
                seen: Default::default(),
            }),
            root,
        };
        trace.event(
            "process_start",
            serde_json::json!({ "version": env!("TESSERA_RELEASE_VERSION"), "build": env!("TESSERA_BUILD_VERSION"), "os": std::env::consts::OS }),
        );
        trace
    }

    pub(crate) fn for_root(&self, root: PathBuf) -> Self {
        Self {
            inner: self.inner.clone(),
            root: Some(root),
        }
    }

    pub(crate) fn phase(&self, name: &'static str) -> Phase {
        Phase {
            trace: self.clone(),
            name,
            start: std::time::Instant::now(),
        }
    }

    pub(crate) fn event(&self, phase: &'static str, details: serde_json::Value) {
        let _ = self.inner.send.send((
            self.root.clone(),
            serde_json::json!({
                "time": timestamp(), "launch": self.inner.launch, "phase": phase,
                "elapsed_ms": self.inner.start.elapsed().as_secs_f64() * 1000.,
                "vault": self.root.as_deref().map(tessera_core::vault::display_path), "details": details,
            }),
        ));
    }

    pub(crate) fn once(&self, phase: &'static str, details: serde_json::Value) {
        if self
            .inner
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((self.root.clone(), phase))
        {
            self.event(phase, details);
        }
    }
}

pub(crate) fn trace(cx: &App) -> Option<Trace> {
    cx.try_global::<LaunchTrace>().map(|trace| trace.0.clone())
}

pub(crate) fn phase(cx: &App, name: &'static str) -> Option<Phase> {
    trace(cx).map(|trace| trace.phase(name))
}

impl Drop for Phase {
    fn drop(&mut self) {
        self.trace.event(
            self.name,
            serde_json::json!({
                "duration_ms": self.start.elapsed().as_secs_f64() * 1000.,
            }),
        );
    }
}

fn log_path(state: Option<&Path>) -> Result<PathBuf> {
    Ok(state
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(reader_history::state_directory)?
        .join(LOG_FILE))
}

fn record(root: &Path, state: Option<&Path>, report: serde_json::Value) {
    append_report(Some(root), state, &report);
}

fn append_report(root: Option<&Path>, state: Option<&Path>, report: &serde_json::Value) {
    let result = (|| -> Result<()> {
        use std::io::Write;
        static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = log_path(state)?;
        let directory = path.parent().unwrap();
        if let Some(root) = root {
            reader_loading::validate_external_cache(directory, root)?;
        }
        std::fs::create_dir_all(directory)?;
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES) {
            // Keep the previous bounded log as well, without discarding timings
            // when a later warning/failure is reported.
            let previous = path.with_extension("log.previous");
            if previous.exists() {
                std::fs::remove_file(&previous)?;
            }
            std::fs::rename(&path, previous)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        serde_json::to_writer(&mut file, report)?;
        file.write_all(b"\n")?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Cannot save Reader diagnostic: {error:#}");
    }
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
fn record_scan(vault: &Vault, state: Option<&Path>) {
    record_scan_with_warnings(vault, &[], state);
}

pub(super) fn record_scan_with_warnings(
    vault: &Vault,
    warnings: &[tessera_core::vault::UnreadableEntry],
    state: Option<&Path>,
) {
    if vault.unreadable.is_empty() && warnings.is_empty() {
        return;
    }
    for item in vault.unreadable.iter().chain(warnings) {
        eprintln!(
            "Skipping {} during {}: {}",
            item.path.display(),
            item.operation,
            item.error
        );
    }
    record(
        &vault.root,
        state,
        serde_json::json!({
            "time": timestamp(), "vault": vault.root, "unreadable": vault.unreadable, "preparation_warnings": warnings,
        }),
    );
}

#[cfg(any(unix, windows))]
pub(super) fn record_move_preview(
    root: &Path,
    state: Option<&Path>,
    timings: &tessera_core::link_rewrite::PreviewTimings,
) {
    record(
        root,
        state,
        serde_json::json!({
            "time": timestamp(), "vault": root, "phase": "MOVE_PREVIEW", "details": timings,
        }),
    );
}

pub(super) fn record_failure(root: &Path, state: Option<&Path>, error: &anyhow::Error) {
    eprintln!(
        "Reader preparation failed for {}: {error:#}",
        root.display()
    );
    record(
        root,
        state,
        serde_json::json!({
            "time": timestamp(), "vault": root, "error": format!("{error:#}"),
        }),
    );
}

#[derive(Default)]
struct DiagnosticDisclosure {
    expanded: bool,
    copied: bool,
}

impl Reader {
    pub(super) fn show_unreadable_items(&self, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<_> = self
            .vault
            .unreadable
            .iter()
            .chain(self.loading.iter().flat_map(|load| &load.warnings))
            .collect();
        let rows: Vec<_> = items
            .iter()
            .map(|item| {
                let (title, summary) = if item.operation.contains("search") {
                    (
                        "Search".to_string(),
                        match item.operation {
                            "persist search cache" => "Search data could not be saved.",
                            "prepare search (unavailable)" => "Content search is unavailable.",
                            _ => "Saved search data could not be reused.",
                        },
                    )
                } else if item.operation.starts_with("watch vault") {
                    (
                        "Vault updates".to_string(),
                        "Automatic refresh is unavailable. Retry to check for changes.",
                    )
                } else {
                    let name = item
                        .path
                        .file_name()
                        .unwrap_or(item.path.as_os_str())
                        .to_string_lossy();
                    let title = if item
                        .path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                    {
                        item.path
                            .file_stem()
                            .unwrap_or(item.path.as_os_str())
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        name.into_owned()
                    };
                    (
                        title,
                        match item.operation {
                            "decode note" => "This note’s text encoding could not be read.",
                            "read note" => "This note could not be read.",
                            "list siblings" => "This folder could not be listed.",
                            _ => "This item could not be read.",
                        },
                    )
                };
                let location = item
                    .path
                    .strip_prefix(&self.vault_root)
                    .ok()
                    .and_then(Path::parent)
                    .map(|parent| {
                        parent
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(" › ")
                    })
                    .filter(|location| !location.is_empty());
                (title, summary, location)
            })
            .collect();
        // Keep complete paths and original errors in the optional report and
        // clipboard, where they are needed to diagnose duplicate file names.
        let reports: Vec<_> = items
            .iter()
            .map(|item| {
                format!(
                    "{}\n{}: {}",
                    tessera_core::vault::display_path(&item.path),
                    item.operation,
                    tessera_core::vault::display_error(&item.error),
                )
            })
            .collect();
        let mut details = format!(
            "Vault: {}\n\n{}",
            tessera_core::vault::display_path(&self.vault_root),
            reports.join("\n\n")
        );
        if let Ok(path) = log_path(self.session_directory.as_deref()) {
            details.push_str(&format!(
                "\n\nDiagnostic log: {}",
                tessera_core::vault::display_path(&path)
            ));
        }
        let disclosure = cx.new(|_| DiagnosticDisclosure::default());
        let reader = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let expanded = disclosure.read(cx).expanded;
            let copied = disclosure.read(cx).copied;
            let toggle = disclosure.clone();
            let copy_state = disclosure.clone();
            let copy_details = details.clone();
            let reader = reader.clone();
            let muted = cx.theme().muted_foreground;
            dialog
                .title("Items needing attention")
                .width(px(560.).min((window.viewport_size().width - px(80.)).max(px(280.))))
                .max_h((window.viewport_size().height - px(80.)).max(px(240.)))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("You can keep reading your other notes."),
                        )
                        .child(
                            div()
                                .id("unreadable-items")
                                .max_h(px(280.))
                                .overflow_y_scroll()
                                .child(v_flex().gap_3().children(rows.iter().map(
                                    |(title, summary, location)| {
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child(title.clone()),
                                            )
                                            .when_some(location.as_ref(), |view, location| {
                                                view.child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(muted)
                                                        .child(location.clone()),
                                                )
                                            })
                                            .child(
                                                div().text_sm().text_color(muted).child(*summary),
                                            )
                                    },
                                ))),
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_1()
                                .child(
                                    Button::new("unreadable-details")
                                        .ghost()
                                        .small()
                                        .icon(if expanded {
                                            IconName::ChevronUp
                                        } else {
                                            IconName::Info
                                        })
                                        .tooltip(if expanded {
                                            "Hide technical details"
                                        } else {
                                            "Show technical details"
                                        })
                                        .on_click(move |_, window, cx| {
                                            toggle.update(cx, |state, _| {
                                                state.expanded = !state.expanded
                                            });
                                            window.refresh();
                                        }),
                                )
                                .child(
                                    Button::new("copy-unreadable-paths")
                                        .ghost()
                                        .small()
                                        .icon(if copied {
                                            IconName::Check
                                        } else {
                                            IconName::Copy
                                        })
                                        .tooltip(if copied { "Copied" } else { "Copy details" })
                                        .on_click(move |_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy_details.clone(),
                                            ));
                                            copy_state.update(cx, |state, _| state.copied = true);
                                            window.refresh();
                                        }),
                                )
                                .child(
                                    Button::new("retry-unreadable-paths")
                                        .ghost()
                                        .small()
                                        .icon(IconName::RotateCw)
                                        .tooltip("Retry")
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            if let Some(reader) = reader.upgrade() {
                                                reader.update(cx, |this, cx| {
                                                    this.refresh_inventory(
                                                        tessera_core::Changes::default(),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }
                                        }),
                                ),
                        )
                        .when(expanded, |view| {
                            view.child(
                                div()
                                    .id("unreadable-technical-details")
                                    .max_h(px(180.))
                                    .overflow_y_scroll()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(details.clone()),
                            )
                        }),
                )
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn first_paint_markers_are_scoped_to_each_resolved_vault() {
        let base =
            std::env::temp_dir().join(format!("tessera-root-trace-{}", uuid::Uuid::new_v4()));
        let first = base.join("first");
        let second = base.join("second");
        let state = base.join("state");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let trace = Trace::new(Some(state.clone()), None);
        trace
            .for_root(first.clone())
            .once("inventory_first_paint", serde_json::json!({"control": 1}));
        trace
            .for_root(first.clone())
            .once("inventory_first_paint", serde_json::json!({"control": 2}));
        trace
            .for_root(second.clone())
            .once("inventory_first_paint", serde_json::json!({"control": 3}));
        let mut paints = Vec::new();
        for _ in 0..100 {
            let text = std::fs::read_to_string(state.join(LOG_FILE)).unwrap_or_default();
            paints = text
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|event| event["phase"] == "inventory_first_paint")
                .collect();
            if paints.len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            paints.len(),
            2,
            "both root-specific markers actually reached the log"
        );
        assert_eq!(
            paints[0]["vault"],
            tessera_core::vault::display_path(&first)
        );
        assert_eq!(
            paints[1]["vault"],
            tessera_core::vault::display_path(&second)
        );
        assert_eq!(paints[1]["details"]["control"], 3);
        drop(trace);
        std::fs::remove_dir_all(base).unwrap();
    }

    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn asynchronous_phases_and_later_failures_share_a_bounded_log() {
        let base = std::env::temp_dir().join(format!("tessera-phase-log-{}", uuid::Uuid::new_v4()));
        let root = base.join("notes");
        let state = base.join("state");
        std::fs::create_dir_all(&root).unwrap();
        let trace = Trace::new(Some(state.clone()), Some(root.clone()));
        {
            let _phase = trace.phase("TEST_PHASE");
        }
        trace.once("PAINT", serde_json::json!({ "notes": 5000 }));
        trace.once("PAINT", serde_json::json!({ "notes": 1 }));
        record_failure(
            &root,
            Some(&state),
            &anyhow::anyhow!("failure positive control"),
        );
        let path = state.join(LOG_FILE);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let events = loop {
            let events = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect::<Vec<_>>();
            if events.len() == 4 {
                break events;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "diagnostic thread did not record phases"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(events.iter().filter(|e| e["phase"] == "PAINT").count(), 1);
        let phase = events.iter().find(|e| e["phase"] == "TEST_PHASE").unwrap();
        assert!(phase["details"]["duration_ms"].as_f64().unwrap() >= 0.);
        assert!(phase["elapsed_ms"].as_f64().unwrap() >= 0.);
        assert!(events
            .iter()
            .any(|e| e["error"] == "failure positive control"));
        std::fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        record_failure(&root, Some(&state), &anyhow::anyhow!("rotated"));
        assert!(path.with_extension("log.previous").exists());
        assert!(std::fs::metadata(&path).unwrap().len() < MAX_LOG_BYTES);
        assert!(!root.join(LOG_FILE).exists());
        drop(trace);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn scan_report_contains_the_failed_path_and_never_writes_inside_notes() {
        let base =
            std::env::temp_dir().join(format!("tessera-diagnostic-{}", uuid::Uuid::new_v4()));
        let root = base.join("notes");
        let state = base.join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.md"), "canonical body").unwrap();
        let mut vault = Vault::from_note_paths(["note.md".into()]);
        vault.root = root.clone();
        vault.unreadable.push(tessera_core::vault::UnreadableEntry {
            path: root.join("locked.md"),
            operation: "read note",
            error: "Access is denied".into(),
        });
        record_scan(&vault, Some(&state));
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join(LOG_FILE)).unwrap()).unwrap();
        assert_eq!(
            report["unreadable"][0]["path"],
            root.join("locked.md").to_str().unwrap()
        );
        assert_eq!(report["unreadable"][0]["error"], "Access is denied");
        record_scan(&vault, Some(&root));
        assert!(!root.join(LOG_FILE).exists());
        assert_eq!(
            std::fs::read(root.join("note.md")).unwrap(),
            b"canonical body"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
