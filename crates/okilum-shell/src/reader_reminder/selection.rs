//! Retained selection authority for the existing reminder writer and menu.
use super::*;
use gpui_component::input::projection::SourceStamp;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, PartialEq)]
pub(super) struct Owner {
    pub(super) root: PathBuf,
    state: Option<PathBuf>,
    index_directory: Option<PathBuf>,
    document: String,
    navigation: u64,
    preparation: u64,
    content: Entity<TextViewState>,
    destination: String,
}

impl Owner {
    pub(super) fn capture(reader: &Reader) -> Self {
        Self {
            root: reader.vault_root.clone(),
            state: reader.session_directory.clone(),
            index_directory: reader.index_dir.clone(),
            document: reader.current_rel.clone(),
            navigation: reader.navigation.generation,
            preparation: reader.navigation.preparation_generation,
            content: reader.content.clone(),
            destination: reader.reminder_prefs.note.clone(),
        }
    }

    pub(super) fn current(&self, reader: &Reader) -> bool {
        !reader.single_file
            && reader.file_preview.is_none()
            && !self.document.is_empty()
            && self.root == reader.vault_root
            && self.state == reader.session_directory
            && self.index_directory == reader.index_dir
            && self.document == reader.current_rel
            && self.navigation == reader.navigation.generation
            && self.preparation == reader.navigation.preparation_generation
            && self.content == reader.content
            && self.destination == reader.reminder_prefs.note
    }
}

#[derive(Clone, PartialEq)]
enum Evidence {
    Read,
    Editor {
        input: Entity<EditorState>,
        stamp: SourceStamp,
        range: std::ops::Range<usize>,
    },
    /// Compatibility for callers already making an explicit current action.
    Direct,
}

#[derive(Clone, PartialEq)]
pub(super) struct Request {
    owner: Owner,
    selection: String,
    source: okilum_core::source_projection::revision::Text,
    date: time::Date,
    evidence: Evidence,
}

impl Request {
    pub(super) fn read(reader: &Reader, cx: &App) -> Option<Self> {
        let (selection, date) = reader.reminder_for_selection(cx)?;
        Some(Self::direct(reader, selection, date, None).with_evidence(Evidence::Read))
    }

    pub(super) fn editor(
        reader: &Reader,
        input: &Entity<EditorState>,
        state: &EditorState,
    ) -> Option<Self> {
        let selection = state.selected_text().to_string();
        let date = reminder_dates::parse(&selection, today())?;
        let source = state.source_snapshot();
        Some(Self {
            owner: Owner::capture(reader),
            selection,
            date,
            source: okilum_core::source_projection::revision::Text::from_identified_rope(
                source.text.rope().clone(),
                source.text.id(),
            ),
            evidence: Evidence::Editor {
                input: input.clone(),
                stamp: source.stamp,
                range: state.selected_range(),
            },
        })
    }

    pub(super) fn direct(
        reader: &Reader,
        selection: String,
        date: time::Date,
        source: Option<String>,
    ) -> Self {
        Self {
            owner: Owner::capture(reader),
            selection,
            source: source
                .unwrap_or_else(|| {
                    reader
                        .note_canonical_source
                        .as_deref()
                        .unwrap_or(&reader.note_source)
                        .to_owned()
                })
                .into(),
            date,
            evidence: Evidence::Direct,
        }
    }

    fn with_evidence(mut self, evidence: Evidence) -> Self {
        self.evidence = evidence;
        self
    }

    pub(super) fn date(&self) -> time::Date {
        self.date
    }

    fn current(&self, reader: &Reader, cx: &App) -> bool {
        self.owner.current(reader)
            && match &self.evidence {
                Evidence::Read => {
                    reader.editing.is_none()
                        // Root clears TextView selection on the menu item's
                        // left press. The immutable menu packet owns that
                        // selection; source/view identity remains authoritative.
                        && reader
                            .note_canonical_source
                            .as_deref()
                            .unwrap_or(&reader.note_source)
                            == &*self.source
                }
                Evidence::Editor {
                    input,
                    stamp,
                    range,
                } => {
                    reader
                        .editing
                        .as_ref()
                        .is_some_and(|editing| editing.input() == input)
                        && input.read(cx).source_stamp() == *stamp
                        && input.read(cx).selected_range() == *range
                }
                Evidence::Direct => true,
            }
    }

    pub(super) fn apply(&self, reader: &mut Reader, window: &mut Window, cx: &mut Context<Reader>) {
        let current = self.current(reader, cx);
        #[cfg(test)]
        {
            if cx.try_global::<test_hooks::Harness>().is_none() {
                cx.set_global(test_hooks::Harness::default());
            }
            cx.global_mut::<test_hooks::Harness>()
                .attempts
                .push(current);
        }
        if !current {
            reader_toast::error(
                "Reminder selection changed. Reopen its context menu.",
                window,
                cx,
            );
            return;
        }
        let Some(state) = self.owner.state.as_ref() else {
            reader_toast::error("Reminders need Okilum's app storage.", window, cx);
            return;
        };
        if self.owner.document == self.owner.destination {
            reader_toast::error(
                "Choose another reminders note in Settings to keep this source note unchanged.",
                window,
                cx,
            );
            return;
        }
        let found = reminder_context::locate(&self.source, &self.selection);
        let sentence = found.sentence.unwrap_or_else(|| {
            if reader.current_title.is_empty() {
                reader_move::display_name(&self.owner.document)
            } else {
                reader.current_title.clone()
            }
        });
        let line = match reminder_task::format(
            &sentence,
            &self.owner.document,
            found.heading.as_deref(),
            self.date,
        ) {
            Ok(line) => line,
            Err(error) => {
                reader_toast::error(error, window, cx);
                return;
            }
        };
        let drafts = state.join("editor-drafts");
        let request = self.clone();
        let observed = self.clone();
        let valid = Arc::new(AtomicBool::new(true));
        let revoke = valid.clone();
        let subscription = cx.observe_self(move |reader, cx| {
            if !observed.current(reader, cx) {
                revoke.store(false, Ordering::Release);
            }
        });
        let input_subscription = match &self.evidence {
            Evidence::Editor { input, .. } => {
                let expected = self.clone();
                let input_flag = valid.clone();
                Some(cx.observe(input, move |reader, _, cx| {
                    if !expected.current(reader, cx) {
                        input_flag.store(false, Ordering::Release);
                    }
                }))
            }
            _ => None,
        };
        let release_flag = valid.clone();
        let release = cx.on_release(move |_, _| release_flag.store(false, Ordering::Release));
        #[cfg(test)]
        let hooks = test_hooks::take(cx, &self.owner.root);
        cx.spawn_in(window, async move |this, cx| {
            let owner = request.owner.clone();
            let job_drafts = drafts.clone();
            let allowed = valid.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    test_hooks::wait(&hooks, true).await;
                    anyhow::ensure!(
                        allowed.load(Ordering::Acquire),
                        "Reminder view changed before save; nothing was written"
                    );
                    // Atomic replacement can succeed even when the existing
                    // note is read-only. Respect that explicit user choice;
                    // the existing writer still validates paths and revisions.
                    match std::fs::symlink_metadata(owner.root.join(&owner.destination)) {
                        Ok(metadata) => anyhow::ensure!(
                            !metadata.permissions().readonly(),
                            "The reminders note is read-only; nothing was written"
                        ),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                    let result = write::add(&owner.root, &job_drafts, &owner.destination, &line);
                    #[cfg(test)]
                    test_hooks::wait(&hooks, false).await;
                    result
                })
                .await;
            drop(subscription);
            drop(input_subscription);
            drop(release);
            let _ = this.update_in_own_window(cx, |reader, window, cx| {
                let current = valid.load(Ordering::Acquire) && request.current(reader, cx);
                #[cfg(test)]
                test_hooks::complete(cx, &request.owner.root, result.is_ok(), current);
                if !current {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(receipt) => {
                        reader.refresh_reminder_source(receipt.path(), cx);
                        reader.announce_reminder(
                            receipt,
                            request.owner,
                            drafts,
                            request.date,
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        reader_toast::error(format!("Reminder not added: {error:#}"), window, cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Reader {
    pub(super) fn refresh_reminder_source(&mut self, path: &str, cx: &mut Context<Self>) {
        self.queue_vault_mutation(
            okilum_core::Changes {
                changed: [path.to_owned()].into_iter().collect(),
                ..Default::default()
            },
            cx,
        );
    }
}

#[cfg(test)]
pub(super) mod test_hooks {
    use super::*;
    #[derive(Default)]
    pub(crate) struct Hooks {
        pub before: Option<async_channel::Receiver<()>>,
        pub after: Option<async_channel::Receiver<()>>,
        pub entered: Option<async_channel::Sender<()>>,
    }
    #[derive(Default)]
    pub(crate) struct Harness {
        pub queued: std::collections::HashMap<PathBuf, Hooks>,
        pub completed: std::collections::HashMap<PathBuf, Vec<(bool, bool)>>,
        pub undone: Vec<bool>,
        pub attempts: Vec<bool>,
    }
    impl Global for Harness {}
    pub(crate) fn take(cx: &mut App, root: &Path) -> Hooks {
        if cx.try_global::<Harness>().is_none() {
            cx.set_global(Harness::default());
        }
        cx.global_mut::<Harness>()
            .queued
            .remove(root)
            .unwrap_or_default()
    }
    pub(crate) async fn wait(hooks: &Hooks, before: bool) {
        if let Some(hold) = if before { &hooks.before } else { &hooks.after } {
            if let Some(entered) = &hooks.entered {
                let _ = entered.try_send(());
            }
            let _ = hold.recv().await;
        }
    }
    pub(crate) fn complete(cx: &mut App, root: &Path, ok: bool, current: bool) {
        cx.global_mut::<Harness>()
            .completed
            .entry(root.to_owned())
            .or_default()
            .push((ok, current));
    }
}
