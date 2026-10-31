//! «Open block in external editor» (#1993). Okilum does not grow a language
//! server for code in notes (#1992); serious editing of a fenced block goes
//! to a real editor instead. The block's body is written to a temporary file
//! outside the vault, named with the block language's extension from the
//! shared alias table, and opened in the editor «Open in» would offer first
//! (Zed before the others), else `$VISUAL`/`$EDITOR` when it is a windowed
//! editor, else the system's default.
//!
//! Every save there comes back into this block only, as one exact source
//! edit through the note editor: the usual autosave writes it safely and one
//! Undo takes it back. When the block changed in Okilum meanwhile, or is gone,
//! or the same code now appears in two blocks, nothing is guessed: the edited
//! code is not applied and a toast says so and offers it for copying. A save
//! that would end the fence early is refused the same way.
//!
//! External editing ends when the note editor closes (another note, Reader
//! mode, a closed window) or on «Stop external editing»; its temporary file
//! is removed then, and a later save there changes nothing.
use super::code_comforts::{self, Block};
use super::lists;
use super::*;
use gpui_component::notification::Notification;
use std::ffi::OsString;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

gpui::actions!(okilum, [OpenBlockExternally, StopBlockExternally]);

/// How often an open block's file is checked for a save.
const POLL: Duration = Duration::from_millis(400);
/// A bigger file is not a code block's edit; it is not read.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Which editor, and how it is started.

/// A started editor: the program, its arguments (never a shell) and its name
/// for menus and toasts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launch {
    pub label: String,
    program: OsString,
    args: Vec<OsString>,
}

/// Editors that need a terminal: Okilum has none to give them, so `$EDITOR`
/// naming one is skipped rather than started invisibly.
const TERMINAL_EDITORS: &[&str] = &[
    "vi", "vim", "nvim", "view", "nano", "pico", "emacs", "ed", "ex", "micro", "hx", "helix",
    "kak", "joe", "ne", "mg", "jed", "mcedit", "less", "more",
];

/// `$VISUAL` / `$EDITOR` as a program and its arguments, split on spaces
/// (no shell), when it names a windowed editor.
fn env_editor(value: &OsString) -> Option<(OsString, Vec<OsString>)> {
    let value = value.to_str()?;
    let mut words = value.split_whitespace();
    let program = words.next()?;
    let name = Path::new(program)
        .file_stem()?
        .to_str()?
        .to_ascii_lowercase();
    if TERMINAL_EDITORS.contains(&name.as_str()) {
        return None;
    }
    Some((program.into(), words.map(OsString::from).collect()))
}

/// The editor for `path`: a detected one (its label, program and arguments
/// with the file), else `$VISUAL`, else `$EDITOR`, else the system's default.
fn plan(
    detected: Option<(&str, (PathBuf, Vec<OsString>))>,
    env: [Option<OsString>; 2],
    os: Os,
    path: &Path,
) -> Launch {
    if let Some((label, (program, args))) = detected {
        return Launch {
            label: label.into(),
            program: program.into(),
            args,
        };
    }
    if let Some((program, mut args)) = env.iter().flatten().find_map(env_editor) {
        args.push(path.into());
        let label = Path::new(&program)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "external editor".into());
        return Launch {
            label,
            program,
            args,
        };
    }
    let (label, program, mut args): (&str, &str, Vec<OsString>) = match os {
        Os::Mac => ("TextEdit", "/usr/bin/open", vec!["-t".into()]),
        Os::Windows => ("Notepad", "notepad.exe", Vec::new()),
        Os::Linux => ("default editor", "xdg-open", Vec::new()),
    };
    args.push(path.into());
    Launch {
        label: label.into(),
        program: program.into(),
        args,
    }
}

fn current_plan(path: &Path) -> Launch {
    plan(
        crate::open_in::preferred().map(|editor| (editor.label, editor.command(path))),
        [std::env::var_os("VISUAL"), std::env::var_os("EDITOR")],
        Os::CURRENT,
        path,
    )
}

/// The name the context menu and the toasts use for the editor.
pub(super) fn editor_label() -> String {
    current_plan(Path::new("block.txt")).label
}

fn spawn(launch: &Launch) -> anyhow::Result<()> {
    let mut child = std::process::Command::new(&launch.program)
        .args(&launch.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Some launchers stay as long as the editor; reap off the executor.
    std::thread::Builder::new()
        .name("external-block-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The block and its file: pure text rules.

/// The opening fence's character and length, for a body that starts after it.
fn opening_fence(text: &str, body: &Range<usize>) -> Option<(char, usize)> {
    let start = lists::line_start(text, body.start.checked_sub(1)?);
    let line = &text[start..lists::line_end(text, start)];
    code_comforts::fence(line).map(|(ch, run, _)| (ch, run))
}

/// The note's newline: the first one decides, as the editor does.
fn newline(text: &str) -> &'static str {
    match text.find('\n') {
        Some(at) if at > 0 && text.as_bytes()[at - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

/// The file an editor gets: the body and a final newline, as editors expect.
fn export(body: &str, newline: &str) -> String {
    format!("{body}{newline}")
}

/// The body a saved file stands for: without a byte-order mark and the final
/// newline, in the note's line endings.
fn import(file: &str, newline: &str) -> String {
    let file = file.strip_prefix('\u{feff}').unwrap_or(file);
    let file = file
        .strip_suffix("\r\n")
        .or_else(|| file.strip_suffix('\n'))
        .unwrap_or(file);
    let lf = file.replace("\r\n", "\n");
    if newline == "\r\n" {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

/// Whether one of `body`'s lines would close a fence of `ch` × `len`.
fn closes_fence(body: &str, (ch, len): (char, usize)) -> bool {
    body.split('\n').any(|line| {
        code_comforts::fence(line)
            .is_some_and(|(c, run, info)| c == ch && run >= len && info.is_empty())
    })
}

/// The body of the block this edit belongs to, in the note's current text:
/// where it was, unchanged; or, when text before it moved it, the one block
/// whose body is still exactly the same. `None` when it changed, is gone or
/// is no longer one block.
fn locate(text: &str, range: &Range<usize>, body: &str) -> Option<Range<usize>> {
    let is_block = |range: &Range<usize>| {
        text.get(range.clone()) == Some(body)
            && code_comforts::block_at(text, range.start).is_some_and(|block| block.body == *range)
    };
    if is_block(range) {
        return Some(range.clone());
    }
    if body.is_empty() {
        return None;
    }
    let mut found = text
        .match_indices(body)
        .map(|(at, _)| at..at + body.len())
        .filter(is_block);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// What a save in the editor does to the note.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    /// The file says what the block already says.
    Unchanged,
    /// Replace the block's body at `range` with `body`.
    Apply { range: Range<usize>, body: String },
    /// The block changed in Okilum, is gone or is ambiguous.
    Conflict,
    /// A line of the edited code would end the block.
    ClosesFence,
}

fn decide(
    text: &str,
    range: &Range<usize>,
    body: &str,
    fence: (char, usize),
    file: &str,
) -> Outcome {
    let new = import(file, newline(text));
    if new == body {
        return Outcome::Unchanged;
    }
    let Some(range) = locate(text, range, body) else {
        return Outcome::Conflict;
    };
    if closes_fence(&new, fence) {
        return Outcome::ClosesFence;
    }
    Outcome::Apply { range, body: new }
}

/// A file name an editor shows in its tab: the note's name, safe on every
/// file system.
fn file_stem(note: &str) -> String {
    let stem = Path::new(note)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(60)
        .collect();
    let clean = clean.trim_matches(['.', ' ']).to_owned();
    if clean.is_empty() {
        "code block".into()
    } else {
        clean
    }
}

// ---------------------------------------------------------------------------
// Open blocks of one note editor.

/// One block open in an external editor.
pub(super) struct Session {
    id: u64,
    /// Removed with everything in it when the session ends.
    _directory: tempfile::TempDir,
    file: PathBuf,
    label: String,
    /// The block's body in the note at the last sync, and its text then.
    range: Range<usize>,
    body: String,
    fence: (char, usize),
    /// The file's bytes as last written or handled: a poll that finds them
    /// again has nothing to do.
    seen: Vec<u8>,
}

impl Session {
    fn start(text: &str, block: &Block, note: &str) -> anyhow::Result<Self> {
        let fence = opening_fence(text, &block.body)
            .ok_or_else(|| anyhow::anyhow!("the code block has no opening fence"))?;
        let body = text[block.body.clone()].to_owned();
        let directory = tempfile::Builder::new().prefix("okilum-block-").tempdir()?;
        let extension = okilum_core::code_language::file_extension(block.language.as_deref());
        let file = directory
            .path()
            .join(format!("{}.{extension}", file_stem(note)));
        let contents = export(&body, newline(text));
        std::fs::write(&file, &contents)?;
        Ok(Self {
            id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            _directory: directory,
            file,
            label: String::new(),
            range: block.body.clone(),
            body,
            fence,
            seen: contents.into_bytes(),
        })
    }

    /// Whether this session edits the block whose body is `body` in `text`:
    /// the block it last synced, still unchanged, or one at its old place.
    fn edits(&self, text: &str, body: &Range<usize>) -> bool {
        locate(text, &self.range, &self.body).as_ref() == Some(body)
            || (self.range.start <= body.end && body.start <= self.range.end)
    }
}

/// The blocks a note editor has open externally.
#[derive(Default)]
pub(super) struct Sessions(Vec<Session>);

impl Sessions {
    fn get(&mut self, id: u64) -> Option<&mut Session> {
        self.0.iter_mut().find(|session| session.id == id)
    }

    /// Whether the block whose body is `body` is open externally.
    fn holds(&self, text: &str, body: &Range<usize>) -> bool {
        self.0.iter().any(|session| session.edits(text, body))
    }
}

/// Facts for the editor's context menu: the caret is in a code block's body,
/// and whether that block is open externally.
pub(super) fn menu_fact(text: &str, cursor: usize, sessions: Option<&Sessions>) -> Option<bool> {
    let block = code_comforts::block_at(text, cursor)?;
    Some(sessions.is_some_and(|sessions| sessions.holds(text, &block.body)))
}

/// The context menu's snapshot observes the input; a session that starts or
/// ends without a text edit tells it so.
fn refresh_menu_facts(input: &Entity<EditorState>, cx: &mut App) {
    input.update(cx, |_, cx| cx.notify());
}

impl Reader {
    /// See [`menu_fact`]; for the editor's context menu snapshot.
    pub(crate) fn external_block_fact(&self, text: &str, cursor: usize) -> Option<bool> {
        menu_fact(text, cursor, self.editing.as_ref().map(|e| &e.external))
    }

    /// The note editor's input, its text and the block around the caret, when
    /// the block may be opened externally.
    fn external_block(&self, cx: &App) -> Option<(Entity<EditorState>, String, Block)> {
        let editing = self.editing.as_ref().filter(|editing| editing.markdown)?;
        let input = editing.input.clone();
        let (text, cursor) = {
            let state = input.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let block = code_comforts::block_at(&text, cursor)?;
        Some((input, text, block))
    }

    /// «Open block in external editor» (the context menu and the command).
    pub(super) fn open_block_externally(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_external_block(
            |path| {
                let launch = current_plan(path);
                spawn(&launch)
                    .map(|()| launch.label.clone())
                    .map_err(|error| anyhow::anyhow!("Cannot open in {}: {error:#}", launch.label))
            },
            window,
            cx,
        );
    }

    /// Start editing the caret's block with `launch`, which opens the file
    /// and names the editor. Opening a block that is already open starts it
    /// afresh from the note's current text.
    fn start_external_block(
        &mut self,
        launch: impl FnOnce(&Path) -> anyhow::Result<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let Some((input, text, block)) = self.external_block(cx) else {
            reader_toast::transient(
                "Place the caret inside a code block to open it in an external editor.",
                window,
                cx,
            );
            return None;
        };
        if !input.read(cx).is_editable() {
            reader_toast::error("This note can’t be changed right now.", window, cx);
            return None;
        }
        let mut session = match Session::start(&text, &block, &self.current_rel) {
            Ok(session) => session,
            Err(error) => {
                reader_toast::error(
                    format!("The code block couldn’t be prepared for editing: {error:#}"),
                    window,
                    cx,
                );
                return None;
            }
        };
        let label = match launch(&session.file) {
            Ok(label) => label,
            Err(error) => {
                reader_toast::error(format!("{error:#}"), window, cx);
                return None;
            }
        };
        session.label = label.clone();
        let id = session.id;
        let editing = self.editing.as_mut()?;
        // One session per block: the old file goes with the old session.
        editing
            .external
            .0
            .retain(|other| !other.edits(&text, &block.body));
        editing.external.0.push(session);
        refresh_menu_facts(&editing.input, cx);
        self.watch_external_block(id, cx);
        let reader = cx.weak_entity();
        reader_toast::push(
            Notification::new()
                .message(format!(
                    "Editing the code block in {label}. Saving there updates this note."
                ))
                .action(move |_, _, cx| {
                    let reader = reader.clone();
                    reader_icon_button(
                        "external-block-stop",
                        IconName::Close,
                        "Stop external editing",
                        cx,
                    )
                    .debug_selector(|| "external-block-stop".into())
                    .on_click(cx.listener(move |notice, _, window, cx| {
                        let _ =
                            reader.update(cx, |reader, cx| reader.stop_external_session(id, cx));
                        notice.dismiss(window, cx);
                    }))
                }),
            Some(Duration::from_secs(6)),
            window,
            cx,
        );
        cx.notify();
        Some(id)
    }

    /// «Stop external editing» for the caret's block.
    pub(super) fn stop_block_externally(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let (text, cursor) = {
            let state = editing.input.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let Some(block) = code_comforts::block_at(&text, cursor) else {
            return;
        };
        let before = editing.external.0.len();
        editing
            .external
            .0
            .retain(|session| !session.edits(&text, &block.body));
        if editing.external.0.len() != before {
            refresh_menu_facts(&editing.input, cx);
            reader_toast::transient("Stopped editing the code block externally.", window, cx);
        }
        cx.notify();
    }

    fn stop_external_session(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(editing) = self.editing.as_mut() {
            editing.external.0.retain(|session| session.id != id);
            refresh_menu_facts(&editing.input, cx);
        }
        cx.notify();
    }

    /// Check the block's file for a save until the session ends.
    fn watch_external_block(&mut self, id: u64, cx: &mut Context<Self>) {
        let handle = self.reader_window;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            let alive = handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.poll_external_block(id, window, cx))
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
    }

    /// One check of a session's file; `false` once the session is over.
    fn poll_external_block(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = self.editing.as_mut().and_then(|e| e.external.get(id)) else {
            return false;
        };
        // Missing for a moment while an editor replaces the file: wait.
        let Ok(meta) = std::fs::metadata(&session.file) else {
            return true;
        };
        if meta.len() > MAX_FILE_BYTES {
            return true;
        }
        let Ok(bytes) = std::fs::read(&session.file) else {
            return true;
        };
        if bytes == session.seen {
            return true;
        }
        session.seen = bytes.clone();
        let label = session.label.clone();
        match String::from_utf8(bytes) {
            Ok(file) => self.external_block_saved(id, &file, window, cx),
            Err(_) => reader_toast::error(
                format!("The file saved in {label} isn’t UTF-8 text, so the note was not changed."),
                window,
                cx,
            ),
        }
        true
    }

    /// A save in the external editor: write it back into its block only.
    fn external_block_saved(
        &mut self,
        id: u64,
        file: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let input = editing.input.clone();
        let Some(session) = editing.external.get(id) else {
            return;
        };
        let label = session.label.clone();
        let (text, selection) = {
            let state = input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        match decide(&text, &session.range, &session.body, session.fence, file) {
            Outcome::Unchanged => {}
            Outcome::ClosesFence => reader_toast::error(
                format!(
                    "A line of the code saved in {label} would end the code block, so the note was not changed."
                ),
                window,
                cx,
            ),
            Outcome::Conflict => {
                let edited = import(file, newline(&text));
                reader_toast::push(
                    Notification::warning(format!(
                        "This code block changed in Okilum while it was open in {label}, so the edited code was not applied."
                    ))
                    .action(move |_, _, cx| {
                        let edited = edited.clone();
                        reader_icon_button(
                            "external-block-copy",
                            IconName::Copy,
                            "Copy edited code",
                            cx,
                        )
                        .debug_selector(|| "external-block-copy".into())
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(edited.clone()))
                        })
                    }),
                    None,
                    window,
                    cx,
                );
            }
            Outcome::Apply { range, body } => {
                let edits = vec![(range.clone(), body.clone())];
                let selection = lists::map_offset(&edits, selection.start, false)
                    ..lists::map_offset(&edits, selection.end, true);
                let applied = input.update(cx, |state, cx| {
                    state.replace_source_edits(&edits, selection, window, cx)
                });
                if !applied {
                    reader_toast::error(
                        "This note can’t be changed right now, so the edited code was not applied.",
                        window,
                        cx,
                    );
                    return;
                }
                let previous = std::mem::replace(&mut session.body, body);
                session.range = range.start..range.start + session.body.len();
                let applied_range = session.range.clone();
                let stamp = input.read(cx).source_stamp();
                let reader = cx.weak_entity();
                reader_toast::push(
                    Notification::new()
                        .message(format!("Code block updated from {label}"))
                        .action(move |_, _, cx| {
                            let reader = reader.clone();
                            let previous = previous.clone();
                            let range = applied_range.clone();
                            Button::new("external-block-undo")
                                .small()
                                .label("Undo")
                                .debug_selector(|| "external-block-undo".into())
                                .on_click(cx.listener(move |notice, _, window, cx| {
                                    let _ = reader.update(cx, |reader, cx| {
                                        reader.undo_external_write(
                                            id,
                                            stamp,
                                            range.clone(),
                                            &previous,
                                            window,
                                            cx,
                                        )
                                    });
                                    notice.dismiss(window, cx);
                                }))
                        }),
                    Some(Duration::from_secs(8)),
                    window,
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// The toast's Undo: put the block's previous code back, while nothing
    /// else changed the note since the write-back. The editor's own Undo
    /// takes the write-back back at any time, as one step.
    fn undo_external_write(
        &mut self,
        id: u64,
        stamp: gpui_component::input::projection::SourceStamp,
        range: Range<usize>,
        previous: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let input = editing.input.clone();
        if input.read(cx).source_stamp() != stamp {
            reader_toast::transient(
                "The note changed since; use Undo in the editor instead.",
                window,
                cx,
            );
            return;
        }
        let end = range.start + previous.len();
        let applied = input.update(cx, |state, cx| {
            state.replace_source_edits(
                &[(range.clone(), previous.to_owned())],
                end..end,
                window,
                cx,
            )
        });
        if applied {
            if let Some(session) = editing.external.get(id) {
                session.range = range.start..end;
                session.body = previous.to_owned();
            }
        }
        cx.notify();
    }

    /// The external-editing actions on the note editor's column.
    pub(super) fn external_block_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .on_action(cx.listener(|this, _: &OpenBlockExternally, window, cx| {
                this.open_block_externally(window, cx)
            }))
            .on_action(cx.listener(|this, _: &StopBlockExternally, window, cx| {
                this.stop_block_externally(window, cx)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    const RS: (char, usize) = ('`', 3);

    #[test]
    fn the_file_round_trips_the_body_in_the_notes_line_endings() {
        assert_eq!(export("fn a() {}", "\n"), "fn a() {}\n");
        assert_eq!(import("fn a() {}\n", "\n"), "fn a() {}");
        assert_eq!(import("fn a() {}", "\n"), "fn a() {}", "no final newline");
        assert_eq!(import("a\n\n", "\n"), "a\n", "an added blank line stays");
        assert_eq!(import("\u{feff}a\r\nb\r\n", "\n"), "a\nb");
        assert_eq!(import("a\nb\n", "\r\n"), "a\r\nb");
        let body = "  a\r\n\tb";
        assert_eq!(import(&export(body, "\r\n"), "\r\n"), body);
    }

    #[test]
    fn a_save_applies_to_its_block_only() {
        let text = "# T\n\n```rust\nfn a() {}\n```\n\n```rust\nfn b() {}\n```\n";
        let range = text.find("fn a").unwrap()..text.find("fn a").unwrap() + 9;
        assert_eq!(
            decide(text, &range, "fn a() {}", RS, "fn a() {\n    1\n}\n"),
            Outcome::Apply {
                range: range.clone(),
                body: "fn a() {\n    1\n}".into()
            }
        );
        assert_eq!(
            decide(text, &range, "fn a() {}", RS, "fn a() {}\n"),
            Outcome::Unchanged,
            "a save without a change"
        );
        // Text above moved the block: the same unique body is still found.
        let moved = format!("Intro.\n{text}");
        let Outcome::Apply { range: found, .. } =
            decide(&moved, &range, "fn a() {}", RS, "fn a() { 2 }\n")
        else {
            panic!("moved block");
        };
        assert_eq!(&moved[found], "fn a() {}");
    }

    #[test]
    fn a_changed_missing_or_ambiguous_block_is_a_conflict() {
        let text = "```py\nx = 1\n```\n";
        let range = 6..11;
        assert_eq!(
            decide(text, &range, "x = 1", ('`', 3), "x = 2\n"),
            Outcome::Apply {
                range: range.clone(),
                body: "x = 2".into()
            },
            "positive control"
        );
        let changed = "```py\nx = 10\n```\n";
        assert_eq!(
            decide(changed, &range, "x = 1", RS, "x = 2\n"),
            Outcome::Conflict
        );
        assert_eq!(
            decide("x = 1\n", &range, "x = 1", RS, "x = 2\n"),
            Outcome::Conflict
        );
        let twice = "Intro\n```py\nx = 1\n```\n```py\nx = 1\n```\n";
        assert_eq!(
            decide(twice, &range, "x = 1", RS, "x = 2\n"),
            Outcome::Conflict,
            "two equal blocks: no guess"
        );
        // The same text outside a block is not the block.
        let prose = "Intro x = 1\n```py\nx = 1\n```\n";
        assert!(matches!(
            decide(prose, &range, "x = 1", RS, "x = 2\n"),
            Outcome::Apply { .. }
        ));
    }

    #[test]
    fn code_that_would_end_the_fence_is_refused() {
        let text = "```md\nx\n```\n";
        let range = 6..7;
        assert_eq!(
            decide(text, &range, "x", RS, "x\n```\nafter\n"),
            Outcome::ClosesFence
        );
        assert!(
            matches!(
                decide(text, &range, "x", RS, "x\n``\n```js\n"),
                Outcome::Apply { .. }
            ),
            "shorter runs and opening fences are code"
        );
        assert!(matches!(
            decide("~~~~\nx\n~~~~\n", &(5..6), "x", ('~', 4), "```\n~~~\n"),
            Outcome::Apply { .. }
        ));
    }

    #[test]
    fn the_file_name_is_safe_and_names_the_note() {
        assert_eq!(file_stem("Projects/Plan: v2.md"), "Plan- v2");
        assert_eq!(file_stem("Заметка.md"), "Заметка");
        assert_eq!(file_stem("..md"), "code block");
        assert_eq!(file_stem(""), "code block");
    }

    #[test]
    fn the_editor_is_a_detected_one_then_a_windowed_env_editor_then_the_system() {
        let path = Path::new("/tmp/x/Note.rs");
        let zed = plan(
            Some(("Zed", ("/bin/zed".into(), vec![path.into()]))),
            [Some("gedit".into()), None],
            Os::Linux,
            path,
        );
        assert_eq!(zed.label, "Zed");
        assert_eq!(zed.program, OsString::from("/bin/zed"));
        assert_eq!(zed.args, [OsString::from("/tmp/x/Note.rs")]);
        let visual = plan(
            None,
            [Some("/usr/bin/gedit -s".into()), Some("vim".into())],
            Os::Linux,
            path,
        );
        assert_eq!(visual.label, "gedit");
        assert_eq!(visual.program, OsString::from("/usr/bin/gedit"));
        assert_eq!(visual.args, ["-s", "/tmp/x/Note.rs"].map(OsString::from));
        // A terminal editor needs a terminal Okilum doesn't have.
        let system = plan(
            None,
            [Some("nvim".into()), Some("vi".into())],
            Os::Linux,
            path,
        );
        assert_eq!(system.program, OsString::from("xdg-open"));
        let mac = plan(None, [None, None], Os::Mac, path);
        assert_eq!((mac.label.as_str(), mac.args.len()), ("TextEdit", 2));
        assert_eq!(mac.args[0], OsString::from("-t"));
        assert_eq!(plan(None, [None, None], Os::Windows, path).label, "Notepad");
    }

    // -----------------------------------------------------------------------
    // In a note editor.

    struct Note {
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        root: PathBuf,
        _directory: tempfile::TempDir,
    }

    fn open<'a>(cx: &'a mut TestAppContext, text: &str) -> (Note, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), text).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        input.update_in(visual, |input, window, cx| {
            input.focus_handle(cx).focus(window, cx)
        });
        (
            Note {
                reader,
                input,
                root,
                _directory: directory,
            },
            visual,
        )
    }

    fn caret(note: &Note, visual: &mut VisualTestContext, offset: usize) {
        note.input
            .update(visual, |i, cx| i.set_selected_range(offset..offset, cx));
        visual.run_until_parked();
    }

    fn value(note: &Note, visual: &mut VisualTestContext) -> String {
        note.input.read_with(visual, |i, _| i.value().to_string())
    }

    /// Open the caret's block with a stand-in editor; the file it got.
    fn start(note: &Note, visual: &mut VisualTestContext) -> (u64, PathBuf) {
        let opened = Rc::new(std::cell::RefCell::new(None));
        let record = opened.clone();
        let id = note
            .reader
            .update_in(visual, |r, window, cx| {
                r.start_external_block(
                    move |path| {
                        *record.borrow_mut() = Some(path.to_path_buf());
                        Ok("Test Editor".into())
                    },
                    window,
                    cx,
                )
            })
            .expect("the caret is in a code block");
        visual.run_until_parked();
        let path = opened.borrow().clone().expect("the editor got a file");
        (id, path)
    }

    /// The editor saves: the poll that follows picks it up.
    fn save(visual: &mut VisualTestContext, path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        poll(visual);
    }

    fn poll(visual: &mut VisualTestContext) {
        visual.executor().advance_clock(POLL * 2);
        visual.run_until_parked();
    }

    fn shown_fact(note: &Note, visual: &mut VisualTestContext) -> Option<bool> {
        note.reader.read_with(visual, |r, _| {
            r.editing.as_ref().unwrap().menu_facts().code_block()
        })
    }

    /// A toast's control once the toast has slid into place.
    fn toast_control(visual: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
        poll(visual);
        let bounds = visual.debug_bounds(selector).expect(selector);
        let viewport = visual.update(|window, _| window.viewport_size());
        assert!(
            bounds.bottom() <= viewport.height,
            "{selector} is on screen"
        );
        bounds
    }

    fn sessions(note: &Note, visual: &mut VisualTestContext) -> usize {
        note.reader.read_with(visual, |r, _| {
            r.editing.as_ref().map_or(0, |e| e.external.0.len())
        })
    }

    const NOTE: &str = "# Plan\n\n```rs\nfn main() {}\n```\n\nAfter.\n";

    /// #1993: a save in the editor comes back into the block only, as one
    /// Undo step, and the note's usual autosave writes it to disk.
    #[gpui::test]
    fn a_save_in_the_editor_comes_back_into_the_block(cx: &mut TestAppContext) {
        let (note, visual) = open(cx, NOTE);
        caret(&note, visual, NOTE.find("main").unwrap());
        let (_, path) = start(&note, visual);
        assert_eq!(path.extension().unwrap(), "rs", "the alias map's extension");
        assert!(!path.starts_with(&note.root), "outside the vault");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}\n");
        // Positive control: nothing changes until the editor saves.
        poll(visual);
        assert_eq!(value(&note, visual), NOTE);

        save(visual, &path, "fn main() {\n    println!(\"hi\");\n}\n");
        let edited = "# Plan\n\n```rs\nfn main() {\n    println!(\"hi\");\n}\n```\n\nAfter.\n";
        assert_eq!(value(&note, visual), edited);
        assert!(
            visual.debug_bounds("external-block-undo").is_some(),
            "the toast offers Undo"
        );
        // The usual autosave, not a separate writer.
        visual.executor().advance_clock(Duration::from_secs(2));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(note.root.join("note.md")).unwrap(),
            edited
        );

        // A second save keeps following the same block.
        save(visual, &path, "fn main() {}\n");
        assert_eq!(value(&note, visual), NOTE);
        save(visual, &path, "fn main() { run() }\n");
        let again = NOTE.replace("fn main() {}", "fn main() { run() }");
        assert_eq!(value(&note, visual), again);
        // The editor's Undo takes the write-back back in one step.
        visual.dispatch_action(gpui_component::input::Undo);
        visual.run_until_parked();
        assert_eq!(value(&note, visual), NOTE);
    }

    /// The toast's Undo restores the block's previous code.
    #[gpui::test]
    fn the_toast_undo_restores_the_previous_code(cx: &mut TestAppContext) {
        let (note, visual) = open(cx, NOTE);
        caret(&note, visual, NOTE.find("main").unwrap());
        let (_, path) = start(&note, visual);
        save(visual, &path, "fn main() { 1 }\n");
        assert_eq!(value(&note, visual), NOTE.replace("{}", "{ 1 }"));
        let undo = toast_control(visual, "external-block-undo");
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(value(&note, visual), NOTE);
        // The block is still followed after the toast's Undo.
        save(visual, &path, "fn main() { 2 }\n");
        assert_eq!(value(&note, visual), NOTE.replace("{}", "{ 2 }"));
    }

    /// When the block changed in Okilum meanwhile, the save is not applied,
    /// the Okilum edit stays, and the edited code can be copied.
    #[gpui::test]
    fn a_block_changed_in_okilum_meanwhile_is_a_conflict(cx: &mut TestAppContext) {
        let (note, visual) = open(cx, NOTE);
        caret(&note, visual, NOTE.find("main").unwrap());
        let (_, path) = start(&note, visual);
        let at = NOTE.find("{}").unwrap() + 1;
        note.input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(at..at), " local ", window, cx)
        });
        visual.run_until_parked();
        let local = NOTE.replace("{}", "{ local }");
        assert_eq!(
            value(&note, visual),
            local,
            "positive control: the Okilum edit"
        );
        save(visual, &path, "fn main() { external }\n");
        assert_eq!(value(&note, visual), local, "the save is not applied");
        assert!(visual.debug_bounds("external-block-undo").is_none());
        // The conflict is said, with the edited code to copy.
        let copy = toast_control(visual, "external-block-copy");
        visual.simulate_click(copy.center(), Modifiers::default());
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("fn main() { external }")
            )
        });
        assert_eq!(sessions(&note, visual), 1);
    }

    /// Cancelling: an editor closed without saving changes nothing; Stop and
    /// leaving the note editor end the session, remove its file, and a later
    /// save there changes nothing.
    #[gpui::test]
    fn cancelling_leaves_the_note_alone(cx: &mut TestAppContext) {
        let (note, visual) = open(cx, NOTE);
        caret(&note, visual, NOTE.find("main").unwrap());
        let (_, path) = start(&note, visual);
        // Closed without saving, or saved unchanged: nothing happens.
        poll(visual);
        save(visual, &path, "fn main() {}\n");
        assert_eq!(value(&note, visual), NOTE);
        assert!(visual.debug_bounds("external-block-undo").is_none());

        note.reader
            .update_in(visual, |r, window, cx| r.stop_block_externally(window, cx));
        visual.run_until_parked();
        assert_eq!(sessions(&note, visual), 0);
        assert!(!path.exists(), "the temporary file is removed");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save(visual, &path, "fn main() { late }\n");
        assert_eq!(
            value(&note, visual),
            NOTE,
            "a save after Stop changes nothing"
        );

        // Leaving the editor ends the session the same way.
        let (_, path) = start(&note, visual);
        assert_eq!(sessions(&note, visual), 1);
        note.reader
            .update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        assert!(note.reader.read_with(visual, |r, _| r.editing.is_none()));
        assert!(!path.exists());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save(visual, &path, "fn main() { late }\n");
        assert_eq!(
            std::fs::read_to_string(note.root.join("note.md")).unwrap(),
            NOTE
        );
    }

    /// Outside a code block the command does nothing but say so; a block is
    /// opened only once at a time.
    #[gpui::test]
    fn only_a_code_block_opens_and_reopening_restarts_it(cx: &mut TestAppContext) {
        let (note, visual) = open(cx, NOTE);
        caret(&note, visual, NOTE.find("After").unwrap());
        let none = note.reader.update_in(visual, |r, window, cx| {
            r.start_external_block(|_| panic!("no editor outside a block"), window, cx)
        });
        assert_eq!(none, None);
        assert_eq!(shown_fact(&note, visual), None, "no menu entry in prose");
        caret(&note, visual, NOTE.find("main").unwrap());
        assert_eq!(shown_fact(&note, visual), Some(false), "«Open block in …»");
        let (first, old) = start(&note, visual);
        assert_eq!(
            shown_fact(&note, visual),
            Some(true),
            "and «Stop external editing»"
        );
        let (second, new) = start(&note, visual);
        assert_ne!(first, second);
        assert_eq!(sessions(&note, visual), 1);
        assert!(!old.exists() && new.exists());
        // Text typed above moves the block; it is still the open one.
        note.input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "Intro\n", window, cx)
        });
        caret(&note, visual, NOTE.find("main").unwrap() + "Intro\n".len());
        assert_eq!(shown_fact(&note, visual), Some(true));
        note.reader
            .update_in(visual, |r, window, cx| r.stop_block_externally(window, cx));
        visual.run_until_parked();
        assert_eq!(shown_fact(&note, visual), Some(false));
    }

    #[test]
    fn the_menu_fact_needs_a_code_block() {
        let text = "a\n```\nb\n```\n";
        assert_eq!(menu_fact(text, 0, None), None);
        assert_eq!(menu_fact(text, 6, None), Some(false));
    }
}
