//! «Clear recent notes»: empties this vault's Recent list, with no question
//! asked. A toast offers Undo, which puts back exactly what was taken away.
use super::*;
use gpui_component::notification::Notification;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// How long the Undo toast stays.
const UNDO_LIFETIME: Duration = Duration::from_secs(8);
/// The palette keeps this many recent paths (`Palette::remember`).
const SWITCHER_KEPT: usize = 100;

/// What a clear took from the vault, kept for Undo.
struct Cleared {
    root: PathBuf,
    recent: Vec<(String, u64)>,
    archive: Vec<(String, u64)>,
    switcher: Vec<String>,
}

/// The palette's «Clear recent notes» command is offered for these typed
/// words, from «clear» on: a shorter query is looking for a note.
pub(super) fn command_matches(query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.starts_with("clear")
        && ["clear recent notes", "clear recents"]
            .iter()
            .any(|command| command.starts_with(&query))
}

impl Reader {
    /// Something that «Clear recent notes» would remove.
    pub(super) fn has_recents(&self) -> bool {
        !self.sidebar.recent.is_empty()
            || !self.quick_open.recent.is_empty()
            || self
                .sidebar
                .archive_shortcuts
                .as_ref()
                .is_some_and(|archive| !archive.recent.is_empty())
    }

    /// Empties the Recent lists of the open vault and saves that with the
    /// vault's sidebar state, like the recents themselves.
    pub(super) fn clear_recents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_recents() {
            return;
        }
        let cleared = Cleared {
            root: self.vault_root.clone(),
            recent: std::mem::take(&mut self.sidebar.recent),
            archive: self
                .sidebar
                .archive_shortcuts
                .as_mut()
                .map(|archive| std::mem::take(&mut archive.recent))
                .unwrap_or_default(),
            switcher: std::mem::take(&mut self.quick_open.recent),
        };
        self.save_sidebar(cx);
        cx.notify();
        self.show_recents_cleared(cleared, window, cx);
    }

    fn show_recents_cleared(
        &mut self,
        cleared: Cleared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let held = Rc::new(RefCell::new(Some(cleared)));
        let shown: Rc<Cell<Option<reader_toast::ToastKey>>> = Rc::default();
        let reader = cx.weak_entity();
        let toast = shown.clone();
        let notification =
            Notification::new()
                .message("Recent notes cleared")
                .action(move |_, _, _| {
                    let held = held.clone();
                    let toast = toast.clone();
                    let reader = reader.clone();
                    Button::new("undo-clear-recents")
                        .small()
                        .label("Undo")
                        .debug_selector(|| "undo-clear-recents".into())
                        .on_click(move |_, window, cx| {
                            // One Undo per clear, however often the button is hit.
                            if let Some(cleared) = held.borrow_mut().take() {
                                let _ =
                                    reader.update(cx, |this, cx| this.restore_recents(cleared, cx));
                            }
                            if let Some(key) = toast.get() {
                                reader_toast::remove(key, window, cx);
                            }
                        })
                });
        shown.set(Some(reader_toast::push(
            notification,
            Some(UNDO_LIFETIME),
            window,
            cx,
        )));
    }

    /// Puts back what a clear took. A note opened since stays on top; with
    /// nothing opened in between, the list is exactly what it was.
    fn restore_recents(&mut self, cleared: Cleared, cx: &mut Context<Self>) {
        // The toast belongs to the vault it cleared.
        if cleared.root != self.vault_root {
            return;
        }
        let mut recent = std::mem::take(&mut self.sidebar.recent);
        for (path, at) in cleared.recent {
            if !recent.iter().any(|(listed, _)| *listed == path) {
                recent.push((path, at));
            }
        }
        recent.truncate(reader_sidebar::RECENT_KEPT);
        self.sidebar.recent = recent;
        if let Some(archive) = self
            .sidebar
            .archive_shortcuts
            .as_mut()
            .filter(|archive| archive.recent.is_empty())
        {
            archive.recent = cleared.archive;
        }
        let newer = std::mem::take(&mut self.quick_open.recent);
        let mut switcher = cleared.switcher;
        switcher.retain(|path| !newer.contains(path));
        switcher.extend(newer);
        let excess = switcher.len().saturating_sub(SWITCHER_KEPT);
        switcher.drain(..excess);
        self.quick_open.recent = switcher;
        self.save_sidebar(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::command_matches;
    use ::core::prelude::v1::test;

    #[test]
    fn the_command_answers_to_its_name_not_to_a_prefix_of_a_note() {
        for query in [
            "clear",
            "Clear rec",
            "clear recent notes",
            " clear recents ",
        ] {
            assert!(command_matches(query), "{query}");
        }
        for query in [
            "",
            "c",
            "cl",
            "recent",
            "clearance",
            "clear notes",
            "clear x",
        ] {
            assert!(!command_matches(query), "{query}");
        }
    }
}
