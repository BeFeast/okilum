//! Source-code files in the Reader (#998). tree-sitter highlights them in a
//! read-only editor, the engine Markdown code blocks already use; edit mode
//! opens the same file in a writable editor with the same language.
use super::*;
use gpui_component::input::{Editor, EditorState, WrappingIndent};
use std::time::Instant;

/// Above this a code file opens without highlighting, with a quiet note.
pub(crate) const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The tree-sitter grammar for a source file, from its name or extension.
/// `text` means a known code file without a compiled grammar. Content is
/// never sniffed: a wrong guess would colour prose as code.
pub(crate) fn language(rel: &str) -> Option<&'static str> {
    let path = Path::new(rel);
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "makefile" | "gnumakefile" => return Some("make"),
        "cmakelists.txt" => return Some("cmake"),
        "dockerfile" | "containerfile" | "justfile" => return Some("text"),
        _ => {}
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "py" | "pyw" | "pyi" => "python",
        "rs" => "rust",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "sh" | "bash" | "zsh" => "bash",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "scala" => "scala",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "proto" => "proto",
        "graphql" | "gql" => "graphql",
        "diff" | "patch" => "diff",
        "cmake" => "cmake",
        "mk" => "make",
        "ini" | "cfg" | "conf" | "env" | "fish" | "ps1" | "bat" | "dockerfile" => "text",
        _ => return None,
    })
}

pub(crate) fn eligible(rel: &str) -> bool {
    language(rel).is_some()
}

enum State {
    Loading,
    Content(Entity<EditorState>),
    Message(&'static str),
}

pub(crate) struct CodePreview {
    root: PathBuf,
    rel: String,
    state: State,
}

enum Loaded {
    Text(String),
    TooLarge,
    Unsupported,
}

impl CodePreview {
    pub(crate) fn new(
        root: PathBuf,
        rel: String,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let started = Instant::now();
        let task = cx.background_executor().spawn(async move {
            if std::fs::metadata(&path)?.len() > MAX_BYTES {
                return anyhow::Ok(Loaded::TooLarge);
            }
            Ok(match String::from_utf8(std::fs::read(&path)?) {
                Ok(text) => Loaded::Text(text),
                Err(_) => Loaded::Unsupported,
            })
        });
        let language = language(&rel).unwrap_or("text");
        // Each selection owns its own entity; a late read cannot reach a newer one.
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.state = match result {
                    Ok(Loaded::Text(text)) => {
                        let input = cx.new(|cx| editor(language, &text, true, window, cx));
                        timing(&input, started, cx);
                        State::Content(input)
                    }
                    Ok(Loaded::TooLarge) => State::Message("This file is too large to highlight."),
                    Ok(Loaded::Unsupported) => {
                        State::Message("This file can’t be previewed as text.")
                    }
                    Err(_) => State::Message("This file couldn’t be read."),
                };
                cx.notify();
            });
        })
        .detach();
        Self {
            root,
            rel,
            state: State::Loading,
        }
    }

    pub(crate) fn input(&self) -> Option<Entity<EditorState>> {
        match &self.state {
            State::Content(input) => Some(input.clone()),
            _ => None,
        }
    }
}

/// The code view and the code editor share one configuration; only `readonly`
/// differs. Line numbers on, folding off, wrap from the global preference.
pub(crate) fn editor(
    language: &'static str,
    text: &str,
    readonly: bool,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> EditorState {
    let mut input = EditorState::new(window, cx)
        .language(language)
        .line_number(true)
        .folding(false)
        .searchable(true)
        .replaceable(false)
        .soft_wrap(reader_ui_state::code_soft_wrap(cx))
        .wrapping_indent(WrappingIndent::None);
    input.set_value(text.to_owned(), window, cx);
    input.ensure_highlighter_factory(gpui_component::highlighter::input_highlighter_factory());
    input.prepare_highlighting(window, cx);
    input.set_readonly(readonly, cx);
    input
}

/// Code text size: the editor's 13 px scaled with the reading text size.
pub(crate) fn text_size(cx: &App) -> Pixels {
    px(13. * reader_ui_state::font_size(cx) / BODY_FONT_SIZE)
}

/// Opt-in measurement for #998: time from selection to highlighted text.
fn timing(input: &Entity<EditorState>, started: Instant, cx: &mut Context<CodePreview>) {
    if std::env::var("OKILUM_CODE_TIMING").as_deref() != Ok("1") {
        return;
    }
    eprintln!(
        "code_preview text_ready_ms={}",
        started.elapsed().as_millis()
    );
    let mut reported = false;
    cx.observe(input, move |_, input, cx| {
        if !reported && !input.read(cx).highlighting_pending() {
            reported = true;
            eprintln!(
                "code_preview highlighted_ms={}",
                started.elapsed().as_millis()
            );
        }
    })
    .detach();
}

impl Render for CodePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = v_flex()
            .id("reader-code-file")
            .debug_selector(|| "reader-code-file".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .px_6()
            .pb_6();
        match &self.state {
            State::Loading => view.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Opening file…"),
            ),
            State::Message(message) => {
                let root = self.root.clone();
                let rel = self.rel.clone();
                view.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(*message),
                        )
                        .child(
                            reader_icon_button(
                                "code-file-open",
                                IconName::ExternalLink,
                                "Open with default app",
                                cx,
                            )
                            .on_click(move |_, window, cx| {
                                reader_files::run(
                                    reader_files::FileAction::Open,
                                    &root,
                                    &rel,
                                    window,
                                    cx,
                                );
                            }),
                        ),
                )
            }
            State::Content(input) => view.child(
                Editor::new(input)
                    .appearance(false)
                    .font_family(crate::source_presentation::CODE_FONT)
                    .text_size(text_size(cx))
                    .size_full(),
            ),
        }
    }
}

impl Reader {
    /// ⌥Z / Alt+Z (#998): remembered for every code file and window.
    pub(crate) fn toggle_code_soft_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wrap = !reader_ui_state::code_soft_wrap(cx);
        reader_ui_state::set_code_soft_wrap(wrap, cx);
        let Some(preview) = &self.file_preview else {
            cx.notify();
            return;
        };
        let shown = preview.code.as_ref().and_then(|code| code.read(cx).input());
        let edited = eligible(&preview.rel)
            .then(|| self.source_input())
            .flatten();
        for input in shown.into_iter().chain(edited) {
            input.update(cx, |input, cx| input.set_soft_wrap(wrap, window, cx));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn languages_follow_names_and_extensions_only() {
        for (rel, expected) in [
            ("work/lan-forward.py", Some("python")),
            ("src/main.rs", Some("rust")),
            ("cfg/app.JSON", Some("json")),
            ("bin/run.sh", Some("bash")),
            ("ci.yml", Some("yaml")),
            ("Cargo.toml", Some("toml")),
            ("web/app.tsx", Some("tsx")),
            ("include/a.hpp", Some("cpp")),
            ("Makefile", Some("make")),
            ("build/CMakeLists.txt", Some("cmake")),
            ("Dockerfile", Some("text")),
            ("notes/Plan.md", None),
            ("data.csv", None),
            ("notes.txt", None),
            ("server.log", None),
            ("photo.png", None),
            ("README", None),
        ] {
            assert_eq!(language(rel), expected, "{rel}");
        }
    }

    #[test]
    fn every_grammar_name_is_compiled_in() {
        use gpui_component::highlighter::Language;
        for rel in [
            "a.py",
            "a.rs",
            "a.js",
            "a.ts",
            "a.tsx",
            "a.go",
            "a.sh",
            "a.json",
            "a.yaml",
            "a.toml",
            "a.sql",
            "a.c",
            "a.cpp",
            "a.cs",
            "a.swift",
            "a.kt",
            "a.java",
            "a.rb",
            "a.php",
            "a.lua",
            "a.html",
            "a.css",
            "a.scala",
            "a.zig",
            "a.ex",
            "a.proto",
            "a.graphql",
            "a.diff",
            "a.cmake",
            "a.mk",
        ] {
            let name = language(rel).unwrap();
            assert_ne!(Language::from_str(name), Language::Plain, "{rel} → {name}");
        }
    }
}
