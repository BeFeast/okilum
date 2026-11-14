//! Contents while a data file is edited (#2814 item 12). Rows come from the
//! last text that parsed. A JSON file uses the strict verdict of the edit
//! buffer (#2811); YAML, TOML and XML build their tree from the buffer on a
//! worker once typing pauses. Open rows are kept by key path. While the text
//! does not parse, the rows stay, the heading says «Last valid text», and a
//! click lands where the node went through the edits made since.
use super::*;
use crate::reader_editor::FileChrome;
use gpui_component::input::projection::{SourceMutation, SourceStamp};
use std::time::Instant;

/// The edit buffer's tree is built once typing pauses this long.
const EDIT_DEBOUNCE: Duration = Duration::from_millis(120);
/// A diff for a jump through edits never takes longer than this; on the
/// deadline it is coarser, never wrong.
const MAP_DEADLINE: Duration = Duration::from_millis(250);

pub(super) struct EditSource {
    pub(super) input: Entity<EditorState>,
    language: &'static str,
    /// The newest document built from the buffer, with the text revision it
    /// came from: «where you are» samples only while both are current.
    built: Option<(Arc<Document>, SourceStamp)>,
    /// The JSON verdict the rows were built for, by pointer.
    built_from: Option<usize>,
    /// The current text does not parse: its error line (1-based) and message.
    pub(super) stale: Option<(usize, String)>,
    /// Contents jumps so far: a repeated jump to the same caret is a move.
    jumps: u64,
    task: Option<Task<()>>,
    jump: Option<Task<()>>,
    /// Bound to the buffer's JSON verdict rather than to its own parse.
    verdict: bool,
    _observer: Subscription,
}

impl View {
    /// The edit language Contents reads, or `None` for the private outline
    /// (Rust, Python, `.jsonc`) and for notes.
    pub(super) fn edit_contents_language(reader: &Reader) -> Option<&'static str> {
        reader.file_preview.as_ref()?;
        let rel = reader.selected_file();
        let ext = Path::new(rel).extension()?.to_str()?.to_ascii_lowercase();
        if ext == "json" {
            return Some("json");
        }
        reader_code_file::language(rel).filter(|l| matches!(*l, "yaml" | "toml" | "xml"))
    }

    pub(super) fn bind_edit(
        &mut self,
        input: Entity<EditorState>,
        chrome: Option<Entity<FileChrome>>,
        language: &'static str,
        file: String,
        cx: &mut Context<Self>,
    ) {
        if self.snapshot.active
            && self.snapshot.file == file
            && self
                .snapshot
                .edit
                .as_ref()
                .is_some_and(|e| e.input == input && e.verdict == chrome.is_some())
        {
            return;
        }
        // Leave the viewer and the private outline; keep this file's rows
        // and open state until the buffer's tree lands.
        self.input_observers.clear();
        self.input = None;
        self.task = None;
        self.language = None;
        self.code = None;
        self.code_observer = None;
        self.snapshot.active = true;
        self.snapshot.seen = None;
        self.snapshot.follow_caret = false;
        self.snapshot.reveal = None;
        let observer = match &chrome {
            Some(chrome) => cx.observe(chrome, |this, chrome, cx| this.edit_verdict(&chrome, cx)),
            // Every text revision, Reload's `set_value` included (it emits
            // no Change).
            None => cx.subscribe(&input, |this, _, _: &SourceMutation, cx| {
                this.schedule_edit_parse(true, cx);
            }),
        };
        self.snapshot.edit = Some(EditSource {
            input,
            language,
            built: None,
            built_from: None,
            stale: None,
            jumps: 0,
            task: None,
            jump: None,
            verdict: chrome.is_some(),
            _observer: observer,
        });
        if self.snapshot.file != file || self.snapshot.doc.is_none() {
            self.apply_source(file, ContentsSource::Loading, cx);
        }
        match chrome {
            Some(chrome) => self.edit_verdict(&chrome, cx),
            None => self.schedule_edit_parse(false, cx),
        }
    }

    /// JSON: the edit buffer's strict verdict decides; the tree is built
    /// from the last valid revision only when that revision changes.
    fn edit_verdict(&mut self, chrome: &Entity<FileChrome>, cx: &mut Context<Self>) {
        let (last, current) = {
            let chrome = chrome.read(cx);
            (chrome.last_valid.clone(), chrome.current.clone())
        };
        let Some(edit) = self.snapshot.edit.as_mut() else {
            return;
        };
        let invalid = current
            .as_ref()
            .filter(|snapshot| matches!(snapshot.verdict, Ok(Err(_))))
            .map(|snapshot| {
                snapshot
                    .diagnostic
                    .as_ref()
                    .map_or((1, String::new()), |d| (d.line, d.message.clone()))
            });
        if edit.stale != invalid {
            edit.stale = invalid.clone();
            cx.notify();
        }
        let Some(last) = last else {
            if invalid.is_some() && self.snapshot.doc.is_none() {
                let file = self.snapshot.file.clone();
                self.apply_source(file, ContentsSource::Invalid(invalid_reason("json")), cx);
            }
            return;
        };
        let id = Arc::as_ptr(&last) as usize;
        if edit.built_from == Some(id) {
            return;
        }
        edit.built_from = Some(id);
        let stamp = last.source.stamp;
        edit.task = Some(cx.spawn(async move |this, cx| {
            let doc = cx
                .background_executor()
                .spawn(async move {
                    Arc::new(Document::for_file(
                        last.source.text.rope().to_string(),
                        "json",
                        true,
                    ))
                })
                .await;
            let _ = this.update(cx, |this, cx| this.edit_document(doc, stamp, cx));
        }));
    }

    /// YAML, TOML, XML (and JSON without a buffer verdict): build the tree
    /// from the buffer once typing pauses. A newer keystroke drops the
    /// pending build.
    fn schedule_edit_parse(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let Some(edit) = self.snapshot.edit.as_mut() else {
            return;
        };
        let input = edit.input.clone();
        let language = edit.language;
        edit.task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(EDIT_DEBOUNCE).await;
            }
            let Ok(snapshot) = this.update(cx, |_, cx| input.read(cx).source_snapshot()) else {
                return;
            };
            let stamp = snapshot.stamp;
            let doc = cx
                .background_executor()
                .spawn(async move {
                    Arc::new(Document::for_file(
                        snapshot.text.rope().to_string(),
                        language,
                        language == "json",
                    ))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(edit) = this.snapshot.edit.as_mut() else {
                    retire(doc, cx);
                    return;
                };
                // A newer revision has its own build scheduled.
                if edit.input.read(cx).source_stamp() != stamp {
                    retire(doc, cx);
                    return;
                }
                // Only strict verdicts (JSON, YAML) make text stale; TOML and
                // XML keep the rows their tolerant parse gives.
                if let Some(error) = &doc.invalid {
                    let stale = Some((error.line, error.message.clone()));
                    retire(doc, cx);
                    if edit.stale != stale {
                        edit.stale = stale;
                        cx.notify();
                    }
                    if this.snapshot.doc.is_none() {
                        let file = this.snapshot.file.clone();
                        this.apply_source(
                            file,
                            ContentsSource::Invalid(invalid_reason(language)),
                            cx,
                        );
                    }
                    return;
                }
                if edit.stale.take().is_some() {
                    cx.notify();
                }
                this.edit_document(doc, stamp, cx);
            });
        }));
    }

    fn edit_document(&mut self, doc: Arc<Document>, stamp: SourceStamp, cx: &mut Context<Self>) {
        let Some(edit) = self.snapshot.edit.as_mut() else {
            return;
        };
        retire(edit.built.replace((doc.clone(), stamp)), cx);
        let file = self.snapshot.file.clone();
        let source = if doc.semantic {
            ContentsSource::Ready(doc)
        } else {
            ContentsSource::Absent
        };
        self.apply_source(file, source, cx);
    }

    /// The editor's caret and top line against the shown rows, or `None`
    /// while the rows are not of the current text (typing, a pending build,
    /// text that does not parse): the mark then stays where it is.
    pub(super) fn edit_whereabouts(&self, doc: &Arc<Document>, cx: &App) -> Option<Whereabouts> {
        let edit = self.snapshot.edit.as_ref()?;
        let (built, stamp) = edit.built.as_ref()?;
        let input = edit.input.read(cx);
        if !Arc::ptr_eq(built, doc) || input.source_stamp() != *stamp {
            return None;
        }
        use gpui_component::RopeExt as _;
        let node = |index: Option<usize>| index.filter(|&i| i > 0 && i < doc.structure.len());
        let line = input.top_visible_line()?;
        let mut top = input.text().line_start_offset(line);
        top += doc.text[top.min(doc.text.len())..]
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count();
        let caret = input.cursor();
        Some(Whereabouts {
            view: reader_code_file::View::Raw,
            page: 0,
            caret: Some((0, caret)),
            jumps: edit.jumps,
            caret_node: node(doc.row_at(&(caret..caret))),
            top: line,
            top_node: node(doc.row_at(&(top..top))),
            settled: input.scroll_settled(),
        })
    }

    /// A click on a row while editing: the caret goes to the node's start.
    /// Rows of an older text find it through the edits made since.
    pub(super) fn jump_edit(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rows) = self.snapshot.doc.clone() else {
            return;
        };
        let Some(edit) = self.snapshot.edit.as_mut() else {
            return;
        };
        let Some(start) = rows.structure.get(index).map(|row| row.range.start) else {
            return;
        };
        edit.jumps += 1;
        let input = edit.input.clone();
        let current = edit.built.as_ref().is_some_and(|(built, stamp)| {
            Arc::ptr_eq(built, &rows) && *stamp == input.read(cx).source_stamp()
        });
        if current {
            edit.jump = None;
            place_caret(&input, start, window, cx);
            return;
        }
        let snapshot = input.read(cx).source_snapshot();
        edit.jump = Some(cx.spawn_in(window, async move |this, cx| {
            let stamp = snapshot.stamp;
            let at = cx
                .background_executor()
                .spawn(
                    async move { map_offset(&rows.text, &snapshot.text.rope().to_string(), start) },
                )
                .await;
            let _ = this.update_in(cx, |_, window, cx| {
                // A keystroke during the diff moved the text again: the
                // offset is of the revision it was computed for.
                if input.read(cx).source_stamp() == stamp {
                    place_caret(&input, at, window, cx);
                }
            });
        }));
    }
}

fn place_caret(input: &Entity<EditorState>, at: usize, window: &mut Window, cx: &mut App) {
    input.update(cx, |input, cx| {
        use gpui_component::input::RopeExt as _;
        let at = at.min(input.text().len());
        let position = input.text().offset_to_position(at);
        input.unfold_at(position, cx);
        input.set_selected_range(at..at, cx);
        input.scroll_line_to_top(position.line as usize, cx);
        input.focus_handle(cx).focus(window, cx);
    });
}

fn invalid_reason(language: &str) -> &'static str {
    match language {
        "yaml" => "No outline: the file is not valid YAML.",
        _ => "No outline: the file is not valid JSON.",
    }
}

/// A byte offset in `old`, carried into `new` through a line diff: on an
/// unchanged line it keeps its column; inside a changed region it lands at
/// the start of what replaced it.
pub(super) fn map_offset(old: &str, new: &str, offset: usize) -> usize {
    if old == new {
        return offset.min(new.len());
    }
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &a,
        &b,
        Some(Instant::now() + MAP_DEADLINE),
    );
    let (mut old_at, mut new_at) = (0, 0);
    for op in ops {
        let old_len: usize = a[op.old_range()].iter().map(|l| l.len()).sum();
        let new_len: usize = b[op.new_range()].iter().map(|l| l.len()).sum();
        if offset < old_at + old_len {
            let mut at = match op.tag() {
                similar::DiffTag::Equal => new_at + (offset - old_at),
                _ => new_at,
            };
            while !new.is_char_boundary(at) {
                at -= 1;
            }
            return at;
        }
        old_at += old_len;
        new_at += new_len;
    }
    new.len()
}
