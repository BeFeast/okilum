//! Source-bound conflict comparison and explicit worker actions. No daemon starts.
use okilum_sync_controller::conflicts::{self, ApplyOutcome, Choice, Preview};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub const ACTIONS: [(Choice, &str); 3] = [
    (Choice::KeepThis, "Keep this"),
    (Choice::KeepThat, "Keep that"),
    (Choice::KeepBoth, "Keep both"),
];

pub struct Pane {
    pub byte_encoding: &'static str,
    pub path: PathBuf,
    /// Filename evidence for the copy, not a claim about the local device.
    pub provenance: String,
    pub lines: Vec<Line>,
}
#[derive(Clone)]
pub struct Line {
    pub text: String,
    pub changed: bool,
}
pub struct Comparison {
    pub this: Pane,
    pub that: Pane,
}

/// The host renders two panes and ACTIONS. Only Ready enables buttons.
pub enum State {
    Ready,
    Applying,
    Resolved,
    NeedsPreview,
    NeedsAttention,
}
pub struct ConflictPanel {
    preview: Arc<Preview>,
    pub comparison: Comparison,
    pub state: State,
    pub message: String,
    pub recovery: Option<conflicts::Applied>,
    pending: Option<uuid::Uuid>,
}

pub struct ApplyJob {
    id: uuid::Uuid,
    preview: Arc<Preview>,
    choice: Choice,
    recovery_directory: PathBuf,
}
pub struct Completion {
    id: uuid::Uuid,
    result: anyhow::Result<ApplyOutcome>,
}
impl ApplyJob {
    /// Run off the UI thread; return Completion to the originating panel only.
    pub fn run(self) -> Completion {
        Completion {
            id: self.id,
            result: conflicts::apply(&self.preview, self.choice, &self.recovery_directory),
        }
    }
}
impl ConflictPanel {
    /// Invoke from the explicit inspect action's worker, never during rendering.
    pub fn open(root: &Path, copy: &Path) -> anyhow::Result<Self> {
        let preview = Arc::new(conflicts::preview(root, copy)?);
        let comparison = comparison(&preview);
        Ok(Self {
            preview,
            comparison,
            state: State::Ready,
            message: "Choose which version to keep. Both originals remain available for recovery."
                .into(),
            recovery: None,
            pending: None,
        })
    }

    /// Dirty/draft ownership is checked by the Reader at the moment of the click.
    /// Refusal does not consume the preview; no filesystem work has started.
    pub fn begin_apply(
        &mut self,
        choice: Choice,
        recovery_directory: &Path,
        has_unsaved_draft: bool,
    ) -> Option<ApplyJob> {
        if !matches!(self.state, State::Ready) {
            return None;
        }
        if has_unsaved_draft {
            self.message = "Save or close the unsaved draft before resolving this conflict.".into();
            return None;
        }
        let id = uuid::Uuid::new_v4();
        self.pending = Some(id);
        self.state = State::Applying;
        self.message = "Preserving both originals and applying your choice…".into();
        Some(ApplyJob {
            id,
            preview: self.preview.clone(),
            choice,
            recovery_directory: recovery_directory.to_owned(),
        })
    }

    /// Inspect durable exact preimages in the controller regression tests.
    #[cfg(test)]
    pub fn previous_versions(&self) -> anyhow::Result<Vec<conflicts::RetainedVersion>> {
        match &self.recovery {
            Some(receipt) => Ok(conflicts::retained_versions(&receipt.recovery_record)?.into()),
            None => Ok(Vec::new()),
        }
    }

    /// Returns false for a completion from another/reopened panel. Failures never
    /// report resolution and never authorize a retry against fresh unseen bytes.
    pub fn finish(&mut self, completion: Completion) -> bool {
        if self.pending != Some(completion.id) {
            return false;
        }
        self.pending = None;
        match completion.result {
            Ok(ApplyOutcome::Applied(receipt)) => {
                self.state = State::Resolved;
                self.message =
                    "Conflict resolved. Both originals were retained for recovery.".into();
                self.recovery = Some(receipt);
            }
            Ok(ApplyOutcome::NeedsAttention { recovery, reason }) => {
                self.state = State::NeedsAttention;
                self.message = format!("Resolution needs attention. Inspect the note and retained originals before trying again. {reason}");
                self.recovery = Some(recovery);
            }
            Err(error) => {
                self.state = State::NeedsPreview;
                self.message = format!(
                    "Resolution refused. Open a fresh preview before trying again. {error}"
                );
            }
        }
        true
    }
}
fn display(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        // Byte-exact hex fallback cannot silently replace invalid UTF-8 with �.
        Err(_) => bytes.iter().map(|b| format!("{b:02x} ")).collect(),
    }
}
fn comparison(preview: &Preview) -> Comparison {
    let this_text = display(preview.this().bytes());
    let that_text = display(preview.that().bytes());
    let old: Vec<_> = this_text.split_inclusive('\n').collect();
    let new: Vec<_> = that_text.split_inclusive('\n').collect();
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &old,
        &new,
        Some(std::time::Instant::now() + std::time::Duration::from_millis(100)),
    );
    let mut this = Vec::new();
    let mut that = Vec::new();
    for op in ops {
        for change in op.iter_changes(&old, &new) {
            let line = || Line {
                text: change.value().to_owned(),
                changed: change.tag() != similar::ChangeTag::Equal,
            };
            match change.tag() {
                similar::ChangeTag::Equal => {
                    this.push(line());
                    that.push(line());
                }
                similar::ChangeTag::Delete => this.push(line()),
                similar::ChangeTag::Insert => that.push(line()),
            }
        }
    }
    let name = preview.that().path().file_name().unwrap().to_string_lossy();
    let suffix = name
        .rsplit_once(".sync-conflict-")
        .map(|(_, s)| s)
        .unwrap_or("");
    // Syncthing suffix carries a device prefix and timestamp, no timezone and
    // no trustworthy local-device identity. Do not invent either.
    let modified = |version: &conflicts::Version| {
        chrono::DateTime::from_timestamp(version.modified_seconds(), 0)
            .map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "Unknown modification time".into())
    };
    let provenance = if name.contains(".okilum-conflict-") {
        let suffix = name.rsplit_once(".okilum-conflict-").unwrap().1;
        format!("Unsaved editor version · created {suffix} (local time on the creating computer). Device unknown; suffix is random. Modified {}", modified(preview.that()))
    } else {
        format!("Syncthing copy · {suffix} (creating device prefix and local time; timezone/device name unverified). Modified {}", modified(preview.that()))
    };
    Comparison {
        this: Pane {
            byte_encoding: if std::str::from_utf8(preview.this().bytes()).is_ok() {
                "UTF-8 source"
            } else {
                "Exact bytes (hexadecimal)"
            },
            path: preview.this().path().to_owned(),
            provenance: format!(
                "Current file · modified {}. Origin device unknown.",
                modified(preview.this())
            ),
            lines: this,
        },
        that: Pane {
            byte_encoding: if std::str::from_utf8(preview.that().bytes()).is_ok() {
                "UTF-8 source"
            } else {
                "Exact bytes (hexadecimal)"
            },
            path: preview.that().path().to_owned(),
            provenance,
            lines: that,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const COPY: &str = "note.sync-conflict-20261007-123456-ABCDEFG.md";
    #[test]
    fn panel_composes_diff_explicit_choice_worker_and_success_without_losing_bom_or_crlf() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("note.md"), "\u{feff}same\r\nthis\r\n").unwrap();
        std::fs::write(vault.path().join(COPY), "\u{feff}same\r\nthat\r\n").unwrap();
        let mut panel = ConflictPanel::open(vault.path(), Path::new(COPY)).unwrap();
        assert_eq!(panel.comparison.this.lines[0].text, "\u{feff}same\r\n");
        assert!(!panel.comparison.this.lines[0].changed);
        assert!(panel.comparison.this.lines[1].changed);
        assert!(panel.comparison.that.lines[1].changed);
        assert!(panel
            .begin_apply(Choice::KeepThat, state.path(), true)
            .is_none());
        let job = panel
            .begin_apply(ACTIONS[1].0, state.path(), false)
            .unwrap();
        assert!(panel
            .begin_apply(Choice::KeepThis, state.path(), false)
            .is_none());
        assert!(panel.finish(job.run()));
        assert!(matches!(panel.state, State::Resolved));
        assert_eq!(
            std::fs::read(vault.path().join("note.md")).unwrap(),
            "\u{feff}same\r\nthat\r\n".as_bytes()
        );
        assert!(panel.recovery.is_some());
        let versions = panel.previous_versions().unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].bytes, "\u{feff}same\r\nthis\r\n".as_bytes());
        assert_eq!(versions[1].bytes, "\u{feff}same\r\nthat\r\n".as_bytes());
    }
    #[test]
    fn stale_inputs_require_new_preview_and_foreign_completion_cannot_resolve_panel() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("note.md"), b"this").unwrap();
        std::fs::write(vault.path().join(COPY), b"that").unwrap();
        let mut old = ConflictPanel::open(vault.path(), Path::new(COPY)).unwrap();
        let job = old
            .begin_apply(Choice::KeepThat, state.path(), false)
            .unwrap();
        let mut reopened = ConflictPanel::open(vault.path(), Path::new(COPY)).unwrap();
        std::fs::write(vault.path().join(COPY), b"changed").unwrap();
        let completion = job.run();
        assert!(!reopened.finish(Completion {
            id: completion.id,
            result: Ok(ApplyOutcome::NeedsAttention {
                recovery: conflicts::Applied {
                    canonical: "ignored".into(),
                    kept_both: None,
                    recovery_record: "ignored".into(),
                    originals: ["ignored".into(), "ignored".into()]
                },
                reason: "foreign".into()
            })
        }));
        assert!(matches!(reopened.state, State::Ready));
        assert!(old.finish(completion));
        assert!(matches!(old.state, State::NeedsPreview));
        assert!(old
            .begin_apply(Choice::KeepThis, state.path(), false)
            .is_none());
        assert_eq!(
            std::fs::read(vault.path().join("note.md")).unwrap(),
            b"this"
        );
        assert_eq!(std::fs::read(vault.path().join(COPY)).unwrap(), b"changed");
    }
}

mod view;
pub(crate) use view::{open, InventoryView};

pub(crate) fn append_history(
    state: &Path,
    root: &Path,
    listing: &mut okilum_core::source_history::Listing,
) -> anyhow::Result<()> {
    let history = conflicts::history(&state.join("sync-conflicts"), root)?;
    listing.warnings.extend(history.warnings);
    for entry in history.entries {
        for (index, version) in entry.versions.into_iter().enumerate() {
            let (text, raw) = match String::from_utf8(version.bytes) {
                Ok(text) => (text, None),
                Err(error) => (
                    String::from_utf8_lossy(error.as_bytes()).into_owned(),
                    Some(error.into_bytes()),
                ),
            };
            listing.versions.push(okilum_core::source_history::Version {
                note: root.join(&entry.canonical),
                text,
                raw,
                created: entry.created,
                label: format!(
                    "Sync conflict · {}{}",
                    if index == 0 {
                        "this version"
                    } else {
                        "other version"
                    },
                    if entry.pending {
                        " · interrupted resolution"
                    } else {
                        ""
                    }
                ),
                protected: true,
                link_move: false,
            });
        }
    }
    Ok(())
}

impl crate::Reader {
    pub(crate) fn render_sync_conflict_banner(
        &self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui::prelude::*;
        use gpui_component::{button::Button, h_flex, Sizable};
        let (root, rel, copies, complete) = self.sync_note_conflicts.as_ref()?;
        if *root != self.vault_root || *rel != self.current_rel || (copies.is_empty() && *complete)
        {
            return None;
        }
        let label = if copies.is_empty() {
            "Conflict check unavailable. Review Sync conflicts.".into()
        } else {
            format!("This note has {} conflicting version(s).", copies.len())
        };
        let root = root.clone();
        let state = self.session_directory.clone()?;
        let owner = cx.entity().downgrade();
        let preferred = copies.first().cloned();
        Some(
            h_flex()
                .id("sync-conflict-banner")
                .debug_selector(|| "sync-conflict-banner".into())
                .px_4()
                .py_1()
                .gap_2()
                .text_sm()
                .child(label)
                .child(
                    Button::new("sync-conflict-review")
                        .debug_selector(|| "sync-conflict-review".into())
                        .small()
                        .label("Review versions")
                        .on_click(move |_, window, cx| {
                            open(
                                root.clone(),
                                state.clone(),
                                Some(owner.clone()),
                                preferred.clone(),
                                window,
                                cx,
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// Startup runs under reader_instance's primary-process lock, once per process.
pub(crate) fn recover_before_open(opts: &crate::Opts) -> Vec<String> {
    let directory = opts
        .session_directory
        .clone()
        .map(Ok)
        .unwrap_or_else(crate::reader_history::state_directory);
    match directory
        .and_then(|directory| conflicts::recover_startup(&directory.join("sync-conflicts")))
    {
        Ok(messages) => messages,
        Err(error) => vec![format!(
            "Sync conflict recovery needs attention: {error}. Use Recover notes."
        )],
    }
}

pub(crate) struct StartupNotice(pub Vec<String>);
impl gpui::Global for StartupNotice {}
pub(crate) fn show_startup_notice(window: &mut gpui::Window, cx: &mut gpui::App) {
    if cx.try_global::<StartupNotice>().is_none() {
        return;
    }
    let messages = std::mem::take(&mut cx.global_mut::<StartupNotice>().0);
    if !messages.is_empty() {
        crate::reader_toast::error(messages.join("\n"), window, cx);
    }
}
