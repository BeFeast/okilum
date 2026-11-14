//! Untitled new files kept as drafts (#1528), like Notepad++ and Zed.
//!
//! «New untitled file» needs no name and no folder. Its text is a real file
//! in Okilum's own state directory (`<state>/untitled/<id>/Untitled.md`),
//! opened in single-file mode, so the editor's autosave and recovery drafts
//! protect it exactly like any other file: a crash or kill loses nothing, and
//! quitting never asks «Save changes?». The draft reappears in Recent with an
//! «Unsaved» badge until Save puts it in a chosen folder. Save proposes the
//! folder of the file the draft was created from.
use super::*;
use serde::{Deserialize, Serialize};

/// Below the session directory; never inside a vault or an index.
const DIRECTORY: &str = "untitled";
/// The one document of a draft folder.
pub(super) const FILE: &str = "Untitled.md";
const META: &str = "draft.json";
/// Written when Save put the text elsewhere; the folder is removed later,
/// once no window can still be showing it.
const SAVED: &str = "saved";
/// How long a saved draft folder stays before it is removed.
const SAVED_LIFETIME: u64 = 600;
/// Recent shows at most this many drafts.
const LISTED: usize = 40;
/// An empty draft nobody typed into is removed after a day.
const EMPTY_DRAFT_LIFETIME: u64 = 86_400;
/// Bytes read for a draft's label.
const LABEL_BYTES: u64 = 4096;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Meta {
    /// The folder of the file the draft was created from: where Save starts.
    #[serde(default)]
    origin: Option<PathBuf>,
    #[serde(default)]
    created: u64,
}

/// One unsaved draft, as Recent lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Listed {
    pub file: PathBuf,
    pub label: String,
    pub modified: u64,
}

/// The person's home folder.
pub(super) fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = dirs::home_dir();
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.filter(|home| home.is_absolute())
}

fn drafts_directory(session: &Path) -> PathBuf {
    session.join(DIRECTORY)
}

fn seconds(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Create an empty draft and return its file. The folder and its metadata are
/// synced before the path is handed out, so a crash right after leaves a
/// draft that Recent can list, never a dangling reference.
pub(super) fn create(session: &Path, origin: Option<&Path>) -> anyhow::Result<PathBuf> {
    let parent = drafts_directory(session);
    std::fs::create_dir_all(&parent)?;
    collect_empty(&parent, reader_sidebar::now());
    let directory = parent.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir(&directory)?;
    let meta = Meta {
        origin: origin
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf),
        created: reader_sidebar::now(),
    };
    let write = |name: &str, bytes: &[u8]| -> anyhow::Result<()> {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    };
    write(META, &serde_json::to_vec(&meta)?)?;
    write(FILE, b"")?;
    #[cfg(unix)]
    {
        std::fs::File::open(&directory)?.sync_all()?;
        std::fs::File::open(&parent)?.sync_all()?;
    }
    Ok(directory.join(FILE))
}

/// Remove drafts that stayed empty for a day (a New nobody typed into) and
/// saved drafts whose window has long moved on to the saved file.
fn collect_empty(parent: &Path, now: u64) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten().take(256) {
        let directory = entry.path();
        if let Ok(saved) = std::fs::symlink_metadata(directory.join(SAVED)) {
            let at = saved.modified().map(seconds).unwrap_or(now);
            if now.saturating_sub(at) > SAVED_LIFETIME {
                let _ = std::fs::remove_dir_all(&directory);
            }
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(directory.join(FILE)) else {
            continue;
        };
        let modified = metadata.modified().map(seconds).unwrap_or(now);
        if metadata.is_file()
            && metadata.len() == 0
            && now.saturating_sub(modified) > EMPTY_DRAFT_LIFETIME
        {
            let _ = std::fs::remove_dir_all(&directory);
        }
    }
}

/// The draft folder `root` is, when it is one: a direct child of this
/// session's drafts directory. Both sides are compared canonically.
pub(super) fn draft_folder(session: &Path, root: &Path) -> Option<PathBuf> {
    let parent = drafts_directory(session).canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    (root.parent() == Some(parent.as_path())).then_some(root)
}

/// Where Save starts: the folder the draft came from, while it exists.
pub(super) fn origin(folder: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(folder.join(META)).ok()?;
    let meta: Meta = serde_json::from_slice(&bytes).ok()?;
    meta.origin.filter(|origin| origin.is_dir())
}

/// The label of a draft: its first non-empty line without heading marks.
pub(super) fn label(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(|line| line.trim_start_matches('#').trim())
        .find(|line| !line.is_empty())?;
    let mut label: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        label.push('…');
    }
    Some(label)
}

/// A file name for Save: the label without characters no system accepts.
pub(super) fn suggested_name(text: &str) -> String {
    let stem: String = label(text)
        .unwrap_or_default()
        .trim_end_matches('…')
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim().trim_matches('.').trim();
    format!("{}.md", if stem.is_empty() { "Untitled" } else { stem })
}

/// Unsaved drafts with text, newest first, bounded.
pub(super) fn list(session: &Path) -> Vec<Listed> {
    let Ok(entries) = std::fs::read_dir(drafts_directory(session)) else {
        return Vec::new();
    };
    let mut listed: Vec<Listed> = entries
        .flatten()
        .take(256)
        .filter_map(|entry| {
            if entry.path().join(SAVED).exists() {
                return None;
            }
            let file = entry.path().join(FILE);
            let metadata = std::fs::symlink_metadata(&file).ok()?;
            if !metadata.is_file() || metadata.len() == 0 {
                return None;
            }
            let mut head = Vec::new();
            use std::io::Read as _;
            std::fs::File::open(&file)
                .ok()?
                .take(LABEL_BYTES)
                .read_to_end(&mut head)
                .ok()?;
            Some(Listed {
                label: label(&String::from_utf8_lossy(&head)).unwrap_or_else(|| "Untitled".into()),
                modified: metadata.modified().map(seconds).unwrap_or_default(),
                file,
            })
        })
        .collect();
    listed.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.file.cmp(&b.file)));
    listed.truncate(LISTED);
    listed
}

/// Mark a draft saved: Recent stops listing it and a later New removes its
/// folder. Removing it at once could race the window still showing it.
/// Only a draft folder of this session is touched.
pub(super) fn mark_saved(session: &Path, folder: &Path) -> anyhow::Result<()> {
    let folder = draft_folder(session, folder)
        .ok_or_else(|| anyhow::anyhow!("Not an untitled draft of this session"))?;
    std::fs::write(folder.join(SAVED), b"")?;
    Ok(())
}

impl Reader {
    /// The draft folder of the document on screen, if it is an untitled draft.
    pub(super) fn current_draft(&self) -> Option<PathBuf> {
        if !self.single_file || self.current_rel != FILE {
            return None;
        }
        draft_folder(self.session_directory.as_deref()?, &self.vault_root)
    }

    /// The folder a new draft remembers for Save: the open file's folder.
    fn draft_origin(&self) -> Option<PathBuf> {
        if let Some(folder) = self.current_draft() {
            return origin(&folder);
        }
        if self.vault_root.as_os_str().is_empty() {
            return None;
        }
        let document = self.vault_root.join(self.selected_file());
        let folder = if self.selected_file().is_empty() {
            self.vault_root.clone()
        } else {
            document.parent()?.to_path_buf()
        };
        folder.is_dir().then_some(folder)
    }

    /// «New untitled file»: a draft with no name, opened for typing. In a
    /// single-file window it replaces the file on screen (no tabs: Recent
    /// brings the file back); a vault window keeps its vault and the draft
    /// gets its own window.
    pub(super) fn new_untitled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session_directory.clone() else {
            reader_toast::error(
                "Draft storage is unavailable, so no untitled file was created.",
                window,
                cx,
            );
            return;
        };
        let origin = self.draft_origin();
        let in_place = self.single_file;
        let created = cx
            .background_executor()
            .spawn(async move { create(&session, origin.as_deref()).map(|file| (file, session)) });
        cx.spawn_in(window, async move |this, cx| {
            let created = created.await;
            let _ = this.update_in_own_window(cx, |this, window, cx| match created {
                Ok((file, session)) => {
                    if in_place {
                        this.close_recent_gallery(window, cx);
                        this.start_loading(
                            Opts {
                                open_path: Some(file),
                                single_file: true,
                                created_note: true,
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                    } else if let Err(error) = reader_open::open_window(
                        Opts {
                            open_path: Some(file),
                            single_file: true,
                            created_note: true,
                            session_directory: Some(session),
                            ..Default::default()
                        },
                        cx,
                    ) {
                        reader_open::show_error(error, cx);
                    }
                }
                Err(error) => {
                    diagnostic_error!("Could not create an untitled draft: {error:#}");
                    reader_toast::error("The untitled file couldn’t be created.", window, cx);
                }
            });
        })
        .detach();
    }

    /// Open a file Recent lists outside this window's vault: a draft or a file
    /// opened on its own. A single-file window switches to it; a vault window
    /// opens it in its own window (or focuses the window that has it).
    pub(super) fn open_recent_file(
        &mut self,
        file: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_file && self.vault_root.join(&self.current_rel) == file {
            return;
        }
        if self.single_file {
            self.start_loading(
                Opts {
                    open_path: Some(file),
                    single_file: true,
                    ..Default::default()
                },
                window,
                cx,
            );
        } else if let Err(error) = reader_open::open_window(
            Opts {
                open_path: Some(file),
                single_file: true,
                session_directory: self.session_directory.clone(),
                ..Default::default()
            },
            cx,
        ) {
            reader_open::show_error(error, cx);
        }
    }

    /// Save on an untitled draft: choose its name and folder (proposed: the
    /// folder it was created from), write it there without overwriting
    /// anything, show the saved file in this window and drop the draft.
    pub(super) fn save_draft_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(folder), Some(session)) = (self.current_draft(), self.session_directory.clone())
        else {
            return;
        };
        let draft_file = folder.join(FILE);
        let contents = self
            .source_text(cx)
            .or_else(|| std::fs::read_to_string(&draft_file).ok())
            .unwrap_or_default();
        let directory = origin(&folder)
            .or_else(|| {
                home().map(|home| {
                    let documents = home.join("Documents");
                    if documents.is_dir() {
                        documents
                    } else {
                        home
                    }
                })
            })
            .unwrap_or_else(std::env::temp_dir);
        let name = suggested_name(&contents);
        let root = self.vault_root.clone();
        let picker = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut path))) = picker.await else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("md");
            }
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                // The picker can outlive the draft on screen.
                if this.vault_root != root || this.current_draft().is_none() {
                    return;
                }
                // Text typed while the picker was open is saved too.
                let contents = this.source_text(cx).unwrap_or(contents);
                if let Err(error) = reader_deleted_note::save_new_copy(&path, &contents) {
                    diagnostic_error!("Could not save the untitled draft: {error:#}");
                    reader_toast::error(
                        "The file couldn’t be saved there. Choose a new filename; existing files are never overwritten.",
                        window,
                        cx,
                    );
                    return;
                }
                // Its text is saved: the draft's editor has nothing left to protect.
                this.source_saved_elsewhere(cx);
                this.remember_source_mode();
                this.editing = None;
                this.start_loading(
                    Opts {
                        open_path: Some(path.clone()),
                        single_file: true,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                let label = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                reader_toast::transient(format!("Saved as «{label}»"), window, cx);
                let directory = this.session_directory.clone();
                cx.background_executor()
                    .spawn(async move {
                        if let Err(error) = mark_saved(&session, &folder) {
                            diagnostic_error!("Could not retire the saved draft: {error:#}");
                        }
                        if let Some(directory) = directory {
                            let _ = reader_history::ReadingHistory::forget_root(&directory, &folder);
                        }
                    })
                    .detach();
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{size, TestAppContext, VisualTestContext};

    #[test]
    fn draft_is_durable_listed_and_keeps_its_origin() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let origin_folder = temp.path().join("docs");
        std::fs::create_dir_all(&origin_folder).unwrap();
        let file = create(&session, Some(&origin_folder)).unwrap();
        assert_eq!(file.file_name().unwrap(), FILE);
        assert!(file.is_file());
        let folder = file.parent().unwrap();
        assert_eq!(
            origin(folder).unwrap().canonicalize().unwrap(),
            origin_folder.canonicalize().unwrap()
        );
        assert!(draft_folder(&session, folder).is_some());
        // An empty draft is not listed: nothing to protect yet.
        assert!(list(&session).is_empty());
        std::fs::write(&file, "# Shopping list\n\nmilk\n").unwrap();
        let listed = list(&session);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "Shopping list");
        assert_eq!(listed[0].file, file);
        mark_saved(&session, folder).unwrap();
        assert!(list(&session).is_empty(), "a saved draft leaves Recent");
        let parent = drafts_directory(&session);
        collect_empty(&parent, reader_sidebar::now());
        assert!(folder.exists(), "the window may still show it for a while");
        collect_empty(&parent, reader_sidebar::now() + SAVED_LIFETIME + 60);
        assert!(!folder.exists(), "a saved draft folder is removed later");
    }

    #[test]
    fn only_this_sessions_drafts_count_or_can_be_removed() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let other = temp.path().join("notes");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(FILE), "keep").unwrap();
        create(&session, None).unwrap();
        assert!(draft_folder(&session, &other).is_none());
        assert!(draft_folder(&session, &session.join(DIRECTORY)).is_none());
        assert!(mark_saved(&session, &other).is_err());
        assert!(!other.join(SAVED).exists());
        assert!(
            other.join(FILE).is_file(),
            "a non-draft folder is never removed"
        );
    }

    #[test]
    fn empty_drafts_expire_but_typed_ones_stay() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let empty = create(&session, None).unwrap();
        let typed = create(&session, None).unwrap();
        std::fs::write(&typed, "words").unwrap();
        let parent = drafts_directory(&session);
        collect_empty(&parent, reader_sidebar::now());
        assert!(empty.exists(), "a fresh empty draft may still be on screen");
        collect_empty(&parent, reader_sidebar::now() + EMPTY_DRAFT_LIFETIME + 60);
        assert!(!empty.exists(), "an empty draft is collected after a day");
        assert!(typed.exists(), "a draft with text is never collected");
    }

    /// Real time for the watcher and writers, test time for the Reader's
    /// timers, until `done` holds or ~15 s are up.
    fn settle(
        visual: &mut VisualTestContext,
        reader: &Entity<Reader>,
        what: &str,
        done: impl Fn(&Reader, &App) -> bool,
    ) {
        for _ in 0..150 {
            std::thread::sleep(Duration::from_millis(100));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
            if reader.read_with(visual, |reader, cx| done(reader, cx)) {
                return;
            }
        }
        let state = reader.read_with(visual, |r, _| {
            format!(
                "root={:?} rel={:?} single={} editing={} loading={:?} notice={:?}",
                r.vault_root,
                r.current_rel,
                r.single_file,
                r.editing.is_some(),
                r.loading.as_ref().map(|l| l.active),
                r.link_notice
            )
        });
        panic!("not reached after 15 s: {what}; {state}");
    }

    fn open_single<'a>(
        cx: &'a mut TestAppContext,
        file: &Path,
        session: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(file.to_path_buf()),
                        session_directory: Some(session.to_path_buf()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.simulate_resize(size(px(1400.), px(900.)));
        visual.run_until_parked();
        (entity.unwrap(), visual)
    }

    /// #1528 end to end: New untitled file needs no name; its text reaches
    /// Okilum's own storage while typing, so a kill loses nothing; after a
    /// restart Recent lists it with an «Unsaved» badge; Save proposes the
    /// folder it came from and the saved file replaces the draft.
    #[gpui::test]
    fn untitled_draft_survives_a_kill_and_saves_beside_its_source(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let docs = docs.canonicalize().unwrap();
        std::fs::write(docs.join("Start.md"), "# Start\n").unwrap();
        let session = temp.path().join("state");

        let (reader, visual) = open_single(cx, &docs.join("Start.md"), &session);
        reader.update_in(visual, |r, window, cx| {
            assert!(r.single_file, "positive control: a file opened on its own");
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.current_draft().is_none());
            r.new_untitled(window, cx);
        });
        settle(visual, &reader, "the draft is open for typing", |r, _| {
            r.current_draft().is_some() && r.editing.is_some()
        });
        let draft = reader.read_with(visual, |r, _| r.vault_root.join(FILE));
        assert!(draft.starts_with(session.canonicalize().unwrap()));
        assert_eq!(std::fs::read_to_string(&draft).unwrap(), "");
        visual.simulate_input("Shopping list\nmilk");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            // Positive control: the keystrokes reached the draft's editor.
            assert_eq!(r.source_text(cx).as_deref(), Some("Shopping list\nmilk"));
        });
        // Nothing explicit: autosave alone must put the text on disk.
        for _ in 0..150 {
            if std::fs::read_to_string(&draft).unwrap_or_default() == "Shopping list\nmilk" {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        }
        assert_eq!(
            std::fs::read_to_string(&draft).unwrap(),
            "Shopping list\nmilk",
            "the text is durable before any close: a kill now loses nothing"
        );
        // Kill: the window goes away without Save or any question.
        reader.update_in(visual, |_, window, _| window.remove_window());
        visual.run_until_parked();

        // Restart on the original file.
        let (reader, visual) = open_single(cx, &docs.join("Start.md"), &session);
        reader.update_in(visual, |r, window, cx| r.toggle_recent_gallery(window, cx));
        let draft_path = draft.to_string_lossy().into_owned();
        settle(visual, &reader, "Recent lists the draft", |r, _| {
            r.recent_gallery_entries()
                .iter()
                .any(|entry| entry.path == draft_path && entry.unsaved)
        });
        reader.read_with(visual, |r, _| {
            let entries = r.recent_gallery_entries();
            assert_eq!(entries[0].path, draft_path, "unsaved drafts lead");
            assert_eq!(entries[0].label, "Shopping list");
            let start = docs.join("Start.md").to_string_lossy().into_owned();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.path == start && !entry.unsaved),
                "the file on screen is listed too: {entries:?}"
            );
        });
        visual.update(|window, _| window.refresh());
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-gallery-overlay").is_some());
        assert!(
            visual.debug_bounds("recent-gallery-unsaved-0").is_some(),
            "the draft's card carries the Unsaved badge"
        );
        // The gallery opened on the file on screen; the draft card arrived
        // from the background read before it, and selection stayed put.
        reader.read_with(visual, |r, cx| {
            let start = docs.join("Start.md").to_string_lossy().into_owned();
            assert_eq!(r.recent_switcher.gallery_selection(cx), Some(start));
        });
        visual.simulate_keystrokes("left");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.recent_switcher.gallery_selection(cx).as_deref(),
                Some(draft_path.as_str())
            );
        });
        visual.simulate_keystrokes("enter");
        settle(visual, &reader, "the draft is back on screen", |r, _| {
            r.current_draft().is_some() && r.note_source.contains("milk")
        });

        // Save: the dialog starts in the folder the draft came from.
        reader.update_in(visual, |r, window, cx| {
            r.focus_handle.focus(window, cx);
        });
        visual.simulate_keystrokes("secondary-s");
        visual.run_until_parked();
        assert!(visual.did_prompt_for_new_path(), "Save asks where");
        let proposed = Rc::new(std::cell::RefCell::new(None));
        let saved = docs.join("Shopping list.md");
        visual.simulate_new_path_selection({
            let (proposed, saved) = (proposed.clone(), saved.clone());
            move |directory| {
                *proposed.borrow_mut() = Some(directory.to_path_buf());
                Some(saved)
            }
        });
        settle(
            visual,
            &reader,
            "the saved file replaces the draft",
            |r, _| r.current_rel == "Shopping list.md" && r.current_draft().is_none(),
        );
        assert_eq!(proposed.borrow().as_deref(), Some(docs.as_path()));
        assert_eq!(
            std::fs::read_to_string(&saved).unwrap(),
            "Shopping list\nmilk"
        );
        settle(visual, &reader, "the saved draft leaves Recent", |_, _| {
            list(&session).is_empty()
        });
        reader.read_with(visual, |r, _| assert!(r.single_file));
    }

    /// In a single-file window New note proposes the open file's folder, not
    /// the tree selection (#1528); explicit destinations and New Folder keep
    /// the tree's.
    #[gpui::test]
    fn single_file_new_note_goes_beside_the_open_file(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("folder");
        std::fs::create_dir_all(root.join("Child")).unwrap();
        std::fs::write(root.join("Start.md"), "# Start\n").unwrap();
        std::fs::write(root.join("Child/Next.md"), "# Next\n").unwrap();
        let (reader, visual) = open_single(cx, &root.join("Start.md"), &temp.path().join("state"));
        reader.update_in(visual, |r, window, cx| {
            assert!(r.single_file);
            r.load_quick_folder("Child".into(), cx);
            r.open_note("Child/Next.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_eq!(r.selected_file(), "Child/Next.md");
            // A real, unrelated selection is the positive control: without
            // the fix New note proposes the root instead of Child.
            r.tree.cursor = Some("Start.md".into());
            assert_eq!(r.tree.selected_creation_folder().as_deref(), Some(""));
            r.new_note(None, window, cx);
            assert_eq!(r.creation.as_ref().unwrap().folder, "Child");
            let input = r.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Sibling", window, cx));
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(root.join("Child/Sibling.md").is_file());
        assert!(!root.join("Sibling.md").exists());
        reader.update_in(visual, |r, window, cx| {
            assert!(r.single_file, "creating a file never enrolls a vault");
            r.new_note(Some(""), window, cx);
            assert_eq!(r.creation.as_ref().unwrap().folder, "");
            r.cancel_creation(window, cx);
            r.tree.cursor = Some("Start.md".into());
            r.new_folder(None, window, cx);
            assert_eq!(r.creation.as_ref().unwrap().folder, "");
            r.cancel_creation(window, cx);
        });
    }

    #[test]
    fn names_and_labels_come_from_the_first_line() {
        assert_eq!(label("\n\n## Plan: Q4 \nrest").as_deref(), Some("Plan: Q4"));
        assert_eq!(label("   \n"), None);
        assert_eq!(suggested_name("## Plan: Q4/5?\n"), "Plan- Q4-5-.md");
        assert_eq!(suggested_name(""), "Untitled.md");
        assert_eq!(suggested_name("..."), "Untitled.md");
        let long = "x".repeat(100);
        assert_eq!(suggested_name(&long), format!("{}.md", "x".repeat(60)));
    }
}
