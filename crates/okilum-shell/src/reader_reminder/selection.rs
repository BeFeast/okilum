//! Retained selection authority for the existing reminder writer and menu.
use super::*;
use gpui_component::input::projection::SourceStamp;
use std::sync::atomic::{AtomicBool, Ordering};

/// Resolve only a date span containing the actual UTF-8 hit, never a nearby date.
/// Bound the work to this line and at most three words (the date grammar).
pub(super) fn date_at(
    text: &str,
    offset: usize,
    now: time::Date,
) -> Option<(std::ops::Range<usize>, time::Date)> {
    if !text.is_char_boundary(offset) || offset >= text.len() {
        return None;
    }
    let mut start = offset.saturating_sub(MAX_DATE_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = text.len().min(offset.saturating_add(MAX_DATE_BYTES));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    start = text[start..offset]
        .rfind('\n')
        .map_or(start, |at| start + at + 1);
    end = text[offset..end].find('\n').map_or(end, |at| offset + at);
    let mut words = Vec::new();
    let mut clipped = Vec::new();
    let mut at = start;
    for word in text[start..end].split_inclusive(char::is_whitespace) {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
        // The bounded window must not turn part of a longer token into a date.
        let continues_left = at == start
            && start > 0
            && text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace());
        let continues_right = at + word.len() == end
            && end < text.len()
            && text[end..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace());
        if !trimmed.is_empty() {
            let left = at + word.find(trimmed)?;
            // Ignore distant words without copying the document or scanning candidates there.
            if left <= offset.saturating_add(MAX_DATE_BYTES)
                && left + trimmed.len() + MAX_DATE_BYTES >= offset
            {
                let range = left..left + trimmed.len();
                if continues_left || continues_right {
                    clipped.push(range.clone());
                }
                words.push(range);
            }
        }
        at += word.len();
    }
    let hit = words.iter().position(|word| word.contains(&offset))?;
    let mut found = None;
    for first in hit.saturating_sub(2)..=hit {
        for last in hit..words.len().min(first + 3) {
            let range = words[first].start..words[last].end;
            if range.len() > MAX_DATE_BYTES
                || clipped
                    .iter()
                    .any(|part| range.start < part.end && part.start < range.end)
            {
                continue;
            }
            // A numeric year belongs to the date, even when it is invalid.
            // Do not silently reinterpret "1 November 20277" as next November.
            if words
                .get(last + 1)
                .is_some_and(|next| text[next.clone()].chars().all(|c| c.is_ascii_digit()))
            {
                continue;
            }
            if let Some(date) = reminder_dates::parse(&text[range.clone()], now) {
                if found
                    .as_ref()
                    .is_some_and(|(_, old_date)| *old_date != date)
                {
                    // A hit shared by different dates has no unambiguous intent.
                    return None;
                }
                if found
                    .as_ref()
                    .is_none_or(|(old, _): &(std::ops::Range<usize>, time::Date)| {
                        range.len() > old.len()
                    })
                {
                    found = Some((range, date));
                }
            }
        }
    }
    found
}

/// The caret's date in the editor source, read from a bounded window of the
/// Rope around the caret: the document is never flattened for this.
pub(super) fn date_at_caret(
    text: &gpui_component::input::projection::SourceText,
    caret: usize,
    now: time::Date,
) -> Option<(String, time::Date)> {
    if !text.is_char_boundary(caret) {
        return None;
    }
    // Wider than `date_at`'s own bound, so its token-clipping checks see the
    // characters just outside it.
    let reach = 2 * MAX_DATE_BYTES;
    let mut start = caret.saturating_sub(reach);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = text.len().min(caret.saturating_add(reach));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let window = text.rope().slice(start..end).to_string();
    let (range, date) = date_at(&window, caret - start, now)?;
    Some((window[range].to_owned(), date))
}

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
            document: reader.pane.current_rel.clone(),
            navigation: reader.pane.navigation.generation,
            preparation: reader.pane.navigation.preparation_generation,
            content: reader.pane.content.clone(),
            destination: reader.reminder_prefs.note.clone(),
        }
    }

    pub(super) fn current(&self, reader: &Reader) -> bool {
        !reader.single_file
            && reader.pane.file_preview.is_none()
            && !self.document.is_empty()
            && self.root == reader.vault_root
            && self.state == reader.session_directory
            && self.index_directory == reader.index_dir
            && self.document == reader.pane.current_rel
            && self.navigation == reader.pane.navigation.generation
            && self.preparation == reader.pane.navigation.preparation_generation
            && self.content == reader.pane.content
            && self.destination == reader.reminder_prefs.note
    }
}

#[derive(Clone, PartialEq)]
enum Evidence {
    Read,
    /// A right press on displayed text without a selection: the TextView's
    /// displayed document must still be the one the press hit.
    ReadPress {
        displayed: SharedString,
    },
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

    /// A date under a right press in Read mode, when nothing is selected.
    pub(super) fn read_at(reader: &Reader, position: Point<Pixels>, cx: &App) -> Option<Self> {
        let content = reader.pane.content.read(cx);
        if !content.selected_text().is_empty() {
            return Self::read(reader, cx);
        }
        if reader.single_file
            || reader.pane.file_preview.is_some()
            || reader.pane.current_rel.is_empty()
            || reader.pointer_on_link()
            || reader.pane.editing.is_some()
        {
            return None;
        }
        let (text, offset) = reader.pressed_text(position)?;
        let (range, date) = date_at(&text, offset, today())?;
        let displayed = content.document_source()?;
        Some(
            Self::direct(reader, text[range].to_owned(), date, None)
                .with_evidence(Evidence::ReadPress { displayed }),
        )
    }

    pub(super) fn editor(
        reader: &Reader,
        input: &Entity<EditorState>,
        state: &EditorState,
    ) -> Option<Self> {
        let source = state.source_snapshot();
        let (selection, date) = if state.selected_range().is_empty() {
            date_at_caret(&source.text, state.cursor(), today())?
        } else {
            let selection = state.selected_text().to_string();
            let date = reminder_dates::parse(&selection, today())?;
            (selection, date)
        };
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
                        .pane
                        .note_canonical_source
                        .as_deref()
                        .unwrap_or(&reader.pane.note_source)
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
                Evidence::ReadPress { displayed } => {
                    reader.pane.editing.is_none()
                        && reader.pane.content.read(cx).document_source().as_ref()
                            == Some(displayed)
                        && reader
                            .pane
                            .note_canonical_source
                            .as_deref()
                            .unwrap_or(&reader.pane.note_source)
                            == &*self.source
                }
                Evidence::Read => {
                    reader.pane.editing.is_none()
                        // Root clears TextView selection on the menu item's
                        // left press. The immutable menu packet owns that
                        // selection; source/view identity remains authoritative.
                        && reader
                            .pane.note_canonical_source
                            .as_deref()
                            .unwrap_or(&reader.pane.note_source)
                            == &*self.source
                }
                Evidence::Editor {
                    input,
                    stamp,
                    range,
                } => {
                    reader
                        .pane
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
            if reader.pane.current_title.is_empty() {
                reader_move::display_name(&self.owner.document)
            } else {
                reader.pane.current_title.clone()
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
