//! Conservative display-only language inference, bounded and cached by CodeBlock.
use gpui::SharedString;
use gpui_base::text::{CodeBlock, CodeBlockLanguageFn};
use std::sync::{Arc, OnceLock};

pub(super) fn resolver() -> Arc<CodeBlockLanguageFn> {
    static RESOLVER: OnceLock<Arc<CodeBlockLanguageFn>> = OnceLock::new();
    RESOLVER.get_or_init(|| Arc::new(language)).clone()
}

fn language(block: &CodeBlock) -> Option<SharedString> {
    if let Some(explicit) = block.lang() {
        return Some(normalize(&explicit).into());
    }
    if !block.is_fenced() {
        return None;
    }
    detect(&block.code()).map(Into::into)
}

fn normalize(lang: &str) -> String {
    let lower = lang.to_ascii_lowercase();
    match lower.as_str() {
        "sh" | "shell" => "bash",
        "yml" => "yaml",
        "js" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "rs" => "rust",
        _ => &lower,
    }
    .to_owned()
}

// Scores rank explicit signatures, not statistical probabilities. A winner
// needs >=95 and must be unique. Uncertain/large blocks stay plain.
const MIN_CONFIDENCE: u8 = 95;
fn detect(code: &str) -> Option<&'static str> {
    if code.len() > 8192 || code.lines().count() > 128 {
        return None;
    }
    let code = code.trim();
    if let Some(first) = code.lines().next()?.strip_prefix("#!") {
        let words: Vec<_> = first.split_whitespace().collect();
        let program = words.first()?.rsplit('/').next()?;
        let program = if program == "env" {
            // Accept the ordinary portable form, not arbitrary env options.
            if words.len() != 2 {
                return None;
            }
            words[1]
        } else {
            program
        };
        return match program {
            "bash" | "sh" => Some("bash"),
            "python" | "python3" => Some("python"),
            "node" => Some("javascript"),
            _ => None,
        };
    }
    let lines: Vec<_> = code
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut candidates = Vec::new();
    if (code.starts_with('{') || code.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(code).is_ok_and(|v| {
            v.as_object().is_some_and(|v| !v.is_empty())
                || v.as_array().is_some_and(|v| !v.is_empty())
        })
    {
        candidates.push(("json", 100));
    }
    // YAML is a superset of many innocent snippets. Require a mapping with
    // multiple keys and typed/nested structure, and exclude JSON/TOML shapes.
    if !code.starts_with(['{', '[']) && lines.iter().filter(|l| l.contains(": ")).count() >= 2 {
        if let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str(code) {
            if map.len() >= 2
                && map.values().any(|v| {
                    matches!(
                        v,
                        serde_yaml::Value::Mapping(_)
                            | serde_yaml::Value::Sequence(_)
                            | serde_yaml::Value::Bool(_)
                            | serde_yaml::Value::Number(_)
                    )
                })
            {
                candidates.push(("yaml", 95));
            }
        }
    }
    let assignments: Vec<_> = lines.iter().filter_map(|l| l.split_once(" = ")).collect();
    if lines
        .first()
        .is_some_and(|l| l.starts_with('[') && l.ends_with(']') && !l.contains(','))
        && assignments.len() >= 2
        && assignments.iter().all(|(k, _)| {
            !k.is_empty()
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
        })
        && assignments
            .iter()
            .any(|(_, v)| v.starts_with('"') && v.ends_with('"'))
        && assignments
            .iter()
            .any(|(_, v)| matches!(*v, "true" | "false") || v.parse::<i64>().is_ok())
        && lines.len() == assignments.len() + 1
    {
        candidates.push(("toml", 95));
    }
    if lines.iter().any(|l| {
        (l.starts_with("fn ") || l.starts_with("pub fn ")) && l.contains('(') && l.ends_with('{')
    }) && (code.contains("println!(") || code.contains("use std::") || code.contains("let mut "))
    {
        candidates.push(("rust", 98));
    }
    if lines
        .iter()
        .any(|l| l.starts_with("def ") && l.contains('(') && l.ends_with(':'))
        && lines
            .iter()
            .any(|l| l.starts_with("import ") || l.starts_with("from "))
        && code.lines().any(|l| l.starts_with("    "))
    {
        candidates.push(("python", 95));
    }
    if code.contains("console.log(")
        && lines
            .iter()
            .any(|l| l.starts_with("function ") || (l.starts_with("const ") && l.contains("=>")))
    {
        candidates.push(("javascript", 95));
    }
    // A single command could be documentation. Require two recognizable prompts.
    if lines.len() >= 2
        && lines.iter().all(|l| {
            l.strip_prefix("$ ").is_some_and(|cmd| {
                matches!(
                    cmd.split_whitespace().next(),
                    Some("git" | "cd" | "ls" | "echo" | "cargo" | "npm" | "curl" | "sudo")
                )
            })
        })
    {
        candidates.push(("bash", 95));
    }
    let mut winners = candidates
        .into_iter()
        .filter(|(_, score)| *score >= MIN_CONFIDENCE);
    let winner = winners.next()?;
    winners.next().is_none().then_some(winner.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_aliases_and_unknown_languages() {
        for (input, expected) in [
            ("sh", "bash"),
            ("YML", "yaml"),
            ("js", "javascript"),
            ("rs", "rust"),
            ("custom", "custom"),
        ] {
            assert_eq!(normalize(input), expected);
        }
    }
    #[test]
    fn strong_signatures_and_ambiguity() {
        for (code, expected) in [
            ("#!/usr/bin/env bash\necho hello", Some("bash")),
            ("#!/usr/bin/python3\nprint(1)", Some("python")),
            ("{\"a\": 1}", Some("json")),
            ("[1, 2, 3]", Some("json")),
            ("enabled: true\nname: demo", Some("yaml")),
            ("[package]\nname = \"demo\"\nversion = 1", Some("toml")),
            ("fn main() {\n println!(\"hi\");\n}", Some("rust")),
            (
                "import os\ndef run():\n    return os.getcwd()",
                Some("python"),
            ),
            ("const show = () => console.log(1);", Some("javascript")),
            ("$ cd example\n$ git status", Some("bash")),
            ("hello world", None),
            ("x = 42", None),
            ("{}", None),
            ("[]", None),
            ("Title: Example\nAuthor: Somebody", None),
            ("echo hello", None),
            ("[section]\nname = demo", None),
            ("$ git status", None),
            ("#!/usr/bin/env unknown\nx", None),
            (
                "fn main() {\nprintln!(\"hi\");\n}\nfunction show() { console.log(1); }",
                None,
            ),
        ] {
            assert_eq!(detect(code), expected, "{code:?}");
        }
        assert_eq!(
            detect(&format!("{{\"long\":\"{}\"}}", "a".repeat(9000))),
            None
        );
    }
}

#[cfg(test)]
mod rendered_tests {
    use super::*;
    use crate::*;
    use ::core::prelude::v1::test;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    struct Fixture {
        text: Entity<TextViewState>,
        hidden: bool,
        resolver: Arc<CodeBlockLanguageFn>,
        highlighted: Arc<Mutex<Vec<Option<SharedString>>>>,
    }
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let highlighted = self.highlighted.clone();
            div()
                .w(px(600.))
                .child(div().h(px(if self.hidden { 2000. } else { 0. })))
                .child(
                    gpui_base::text::TextView::new(&self.text)
                        .code_block_language(self.resolver.clone())
                        .code_block_highlighter(move |block| {
                            highlighted.lock().unwrap().push(block.lang());
                            vec![]
                        })
                        .code_block_actions(reader_code::actions),
                )
        }
    }

    #[gpui::test]
    fn inference_is_visible_cached_shared_with_highlighter_and_replaced(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut fixture = None;
        let mut text = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| TextViewState::markdown("```\n{\"value\":42}\n```", cx));
            text = Some(state.clone());
            let counter = calls.clone();
            let view = cx.new(|_| Fixture {
                text: state,
                hidden: true,
                highlighted: seen.clone(),
                resolver: Arc::new(move |block| {
                    counter.fetch_add(1, Ordering::Relaxed);
                    language(block)
                }),
            });
            fixture = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "offscreen blocks must not infer"
        );
        let fixture = fixture.unwrap();
        fixture.update(visual, |v, cx| {
            v.hidden = false;
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "positive control: visible block inferred"
        );
        assert!(visual.debug_bounds("reader-code-language-0").is_some());
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|v| v.as_deref() == Some("json")),
            "highlighter receives inferred language"
        );
        fixture.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let text = text.unwrap();
        text.update(visual, |v, cx| v.set_text("```sh\n{\"value\":42}\n```", cx));
        visual.run_until_parked();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            seen.lock().unwrap().last().unwrap().as_deref(),
            Some("bash")
        );
        text.update(visual, |v, cx| v.set_text("    {\"value\":42}", cx));
        visual.run_until_parked();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        assert!(visual.debug_bounds("reader-code-language-0").is_none());
        assert_eq!(seen.lock().unwrap().last().unwrap(), &None);
        text.update(visual, |v, cx| v.set_text("```\nordinary words\n```", cx));
        visual.run_until_parked();
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        assert!(visual.debug_bounds("reader-code-language-0").is_none());
        assert_eq!(seen.lock().unwrap().last().unwrap(), &None);
    }
}
