//! Source operations are separate from validation and from read-only display.
//! A formatter may change whitespace only under the selected language's rules.
//! Syntax correction is a proposal; this module never applies or saves it.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub const MAX_BYTES: usize = crate::json_view::MAX_VIEW_BYTES;
pub const MAX_OUTPUT_BYTES: usize = crate::json_view::MAX_VIEW_BYTES;

#[derive(Clone, Default)]
pub struct Control(Arc<AtomicBool>);
impl Control {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub fn size_notice(action: &str, bytes: usize) -> String {
    format!(
        "{action} works with files up to 64 MiB; this file is {:.1} MiB ({bytes} bytes).",
        bytes as f64 / 1048576.
    )
}

fn budget_notice(action: &str, bytes: usize, error: crate::json_view::Error) -> String {
    match error {
        crate::json_view::Error::Limit {
            reason: "nesting depth",
            limit,
        } => format!(
            "{action} supports JSON nesting up to {limit} levels; this file is {bytes} bytes."
        ),
        _ => format!("{action} supports results up to 64 MiB; this file is {bytes} bytes."),
    }
}

/// No value tree: only bounded parser scratch plus at most 128 container levels.
/// None means cancellation, distinct from a syntax error or unavailable budget.
pub fn json_validation_controlled(
    source: &str,
    control: &Control,
) -> Option<Result<Result<(), Invalid>, String>> {
    if control.cancelled() {
        return None;
    }
    if source.len() > MAX_BYTES {
        return Some(Err(size_notice("Validate", source.len())));
    }
    match crate::json_view::formatting_bound(source, usize::MAX, &|| control.cancelled()) {
        Ok(None) => return None,
        Err(error) => return Some(Err(budget_notice("Validate", source.len(), error))),
        Ok(Some(_)) => {}
    }
    let result = json_validation(source);
    if result
        .as_ref()
        .is_err_and(|error| error.message.starts_with("recursion limit exceeded"))
    {
        return Some(Err(budget_notice(
            "Validate",
            source.len(),
            crate::json_view::Error::Limit {
                reason: "nesting depth",
                limit: crate::json_view::MAX_PRETTY_DEPTH,
            },
        )));
    }
    (!control.cancelled()).then_some(Ok(result))
}

/// Explicit dialects: sharing a highlighting grammar does not make JSONC JSON.
pub const LANGUAGES: &[&str] = &[
    "astro",
    "bash",
    "c",
    "cmake",
    "cpp",
    "csharp",
    "css",
    "diff",
    "ejs",
    "elixir",
    "erb",
    "go",
    "graphql",
    "html",
    "java",
    "javascript",
    "jsdoc",
    "json",
    "jsonc",
    "kotlin",
    "lua",
    "make",
    "markdown_inline",
    "markdown",
    "php",
    "proto",
    "python",
    "ruby",
    "rust",
    "scala",
    "sql",
    "svelte",
    "swift",
    "text",
    "toml",
    "tsx",
    "typescript",
    "xml",
    "yaml",
    "zig",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

pub fn json_validation(source: &str) -> Result<(), Invalid> {
    serde_json::from_str::<serde::de::IgnoredAny>(source.trim_start_matches('\u{feff}'))
        .map(|_| ())
        .map_err(|error| Invalid {
            line: error.line(),
            column: error.column(),
            message: error.to_string(),
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Formatted {
    Changed(String),
    Unchanged(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
    Mixed,
}

pub fn line_ending(source: &str) -> LineEnding {
    let crlf = source.contains("\r\n");
    if crlf
        && source
            .split_inclusive('\n')
            .any(|line| line.ends_with('\n') && !line.ends_with("\r\n"))
    {
        LineEnding::Mixed
    } else if crlf {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

/// Refusal is a successful no-change result, with a reason suitable for the UI.
/// No generic indentation/whitespace pass is used for unsupported languages.
pub fn format(language: &str, source: &str) -> Formatted {
    format_with_line_ending(language, source, line_ending(source))
}

/// A fenced body may have no newline of its own. Inherit the owning document's
/// newline convention without changing the body's final-newline choice.
pub fn format_with_line_ending(language: &str, source: &str, ending: LineEnding) -> Formatted {
    format_controlled(language, source, ending, &Control::default()).expect("uncancelled format")
}

pub fn format_controlled(
    language: &str,
    source: &str,
    ending: LineEnding,
    control: &Control,
) -> Option<Formatted> {
    if control.cancelled() {
        return None;
    }
    let unchanged = |text: String| Some(Formatted::Unchanged(text));
    if source.len() > MAX_BYTES {
        return unchanged(size_notice("Format", source.len()));
    }
    if language != "json" {
        return unchanged(format!("No safe built-in formatter for {language}; source unchanged. Its whitespace, comments and strings are not rewritten using another language's rules."));
    }
    if ending == LineEnding::Mixed || line_ending(source) == LineEnding::Mixed {
        return unchanged("Mixed line endings; source unchanged.".into());
    }
    let mut result = match crate::json_view::pretty_controlled(source, &|| control.cancelled()) {
        Ok(Some(result)) => result,
        Ok(None) => return None,
        Err(crate::json_view::Error::Invalid(error)) => return unchanged(format!(
            "JSON is invalid at {}:{}; this formatter cannot safely recover its structure. Source unchanged. Use Repair separately to review a syntax correction.", error.line, error.column)),
        Err(error) => return unchanged(budget_notice("Format", source.len(), error)),
    };
    if !source.ends_with('\n') {
        result.pop();
    }
    let final_len = result
        .len()
        .saturating_add(if ending == LineEnding::Crlf {
            result.bytes().filter(|b| *b == b'\n').count()
        } else {
            0
        })
        .saturating_add(if source.starts_with('\u{feff}') { 3 } else { 0 });
    if final_len > MAX_OUTPUT_BYTES {
        return unchanged(budget_notice(
            "Format",
            source.len(),
            crate::json_view::Error::Limit {
                reason: "formatted output bytes",
                limit: MAX_OUTPUT_BYTES,
            },
        ));
    }
    if control.cancelled() {
        return None;
    }
    if ending == LineEnding::Crlf {
        result = result.replace('\n', "\r\n");
    }
    if source.starts_with('\u{feff}') {
        result.insert(0, '\u{feff}');
    }
    if control.cancelled() {
        None
    } else if result == source {
        unchanged("Already formatted.".into())
    } else {
        Some(Formatted::Changed(result))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repair {
    pub source: String,
    pub proposed: String,
    pub explanation: String,
}

/// Conservative strict-JSON suggestions. Quoting bare identifier keys and
/// completing a balanced, otherwise complete container at EOF are explicit
/// repairs, never formatting. Ambiguous strings, mismatched delimiters and
/// unrecognized syntax are refused, rather than stripped or guessed.
pub fn repair(language: &str, source: &str) -> Result<Repair, String> {
    repair_controlled(language, source, &Control::default()).expect("uncancelled repair")
}

fn append(output: &mut String, text: &str, input_bytes: usize) -> Result<(), String> {
    let needed = output.len().saturating_add(text.len());
    if needed > MAX_OUTPUT_BYTES {
        return Err(budget_notice(
            "Repair",
            input_bytes,
            crate::json_view::Error::Limit {
                reason: "formatted output bytes",
                limit: MAX_OUTPUT_BYTES,
            },
        ));
    }
    if needed > output.capacity() {
        let capacity = needed
            .max(output.capacity().saturating_mul(2))
            .min(MAX_OUTPUT_BYTES);
        output.reserve_exact(capacity - output.len());
    }
    output.push_str(text);
    Ok(())
}

/// Linear construction: key quoting never shifts the remaining multi-MB source.
pub fn repair_controlled(
    language: &str,
    source: &str,
    control: &Control,
) -> Option<Result<Repair, String>> {
    if control.cancelled() {
        return None;
    }
    if source.len() > MAX_BYTES {
        return Some(Err(size_notice("Repair", source.len())));
    }
    if language != "json" {
        return Some(Err(format!("No safe automatic repair for {language}; source unchanged. No correction from another language is applied.")));
    }
    match json_validation_controlled(source, control)? {
        Err(reason) => return Some(Err(reason.replace("Validate", "Repair"))),
        Ok(Ok(())) => {
            return Some(Err(
                "JSON is already valid; no syntax repair is needed.".into()
            ))
        }
        Ok(Err(_)) => {}
    }
    let result = (|| -> Result<Repair, String> {
        let bytes = source.as_bytes();
        let mut at = if source.starts_with('\u{feff}') { 3 } else { 0 };
        let mut next_check = 0;
        let mut previous = 0;
        let mut stack = Vec::with_capacity(crate::json_view::MAX_PRETTY_DEPTH);
        let mut proposed = String::with_capacity(source.len());
        let mut copied = 0;
        let mut quoted_keys = 0;
        macro_rules! checkpoint {
            () => {
                if at >= next_check {
                    next_check = at + 16 * 1024;
                    if control.cancelled() {
                        return Err("Operation cancelled.".into());
                    }
                }
            };
        }
        while at < bytes.len() {
            checkpoint!();
            let ch = bytes[at];
            if ch.is_ascii_whitespace() {
                at += 1;
                continue;
            }
            match ch {
                b'"' => {
                    at += 1;
                    let mut closed = false;
                    while at < bytes.len() {
                        checkpoint!();
                        match bytes[at] {
                            b'\\' => at += 2,
                            b'"' => {
                                at += 1;
                                closed = true;
                                break;
                            }
                            _ => at += 1,
                        }
                    }
                    if !closed {
                        return Err(
                            "Unterminated or ambiguous JSON string; no safe repair proposed."
                                .into(),
                        );
                    }
                    previous = b'"';
                    continue;
                }
                b'{' | b'[' => {
                    if stack.len() >= crate::json_view::MAX_PRETTY_DEPTH {
                        return Err(budget_notice(
                            "Repair",
                            source.len(),
                            crate::json_view::Error::Limit {
                                reason: "nesting depth",
                                limit: crate::json_view::MAX_PRETTY_DEPTH,
                            },
                        ));
                    }
                    stack.push(if ch == b'{' { b'}' } else { b']' });
                }
                b'}' | b']' => {
                    if stack.pop() != Some(ch) {
                        return Err("Mismatched JSON delimiters; no safe repair proposed.".into());
                    }
                }
                b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$'
                    if stack.last() == Some(&b'}') && matches!(previous, b'{' | b',') =>
                {
                    let start = at;
                    at += 1;
                    while at < bytes.len()
                        && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'$'))
                    {
                        checkpoint!();
                        at += 1;
                    }
                    let mut next = at;
                    while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                        if next % (16 * 1024) == 0 && control.cancelled() {
                            return Err("Operation cancelled.".into());
                        }
                        next += 1;
                    }
                    if bytes.get(next) == Some(&b':') {
                        append(&mut proposed, &source[copied..start], source.len())?;
                        append(&mut proposed, "\"", source.len())?;
                        append(&mut proposed, &source[start..at], source.len())?;
                        append(&mut proposed, "\"", source.len())?;
                        copied = at;
                        quoted_keys += 1;
                    }
                    previous = b'"';
                    continue;
                }
                _ => {}
            }
            previous = ch;
            at += 1;
        }
        let completed = stack.len();
        let end = source.trim_end_matches([' ', '\t', '\r', '\n']).len();
        append(&mut proposed, &source[copied..end], source.len())?;
        for closer in stack.into_iter().rev() {
            append(
                &mut proposed,
                if closer == b'}' { "}" } else { "]" },
                source.len(),
            )?;
        }
        append(&mut proposed, &source[end..], source.len())?;
        if control.cancelled() {
            return Err("Operation cancelled.".into());
        }
        if proposed == source {
            return Err("No safe strict-JSON correction found; source unchanged. Missing values, comments and ambiguous tokens are not discarded or invented.".into());
        }
        match json_validation_controlled(&proposed, control) {
            None => return Err("Operation cancelled.".into()),
            Some(Err(reason)) => return Err(reason.replace("Validate", "Repair")),
            Some(Ok(Err(_))) => return Err("No safe strict-JSON correction found; source unchanged. Missing values, comments and ambiguous tokens are not discarded or invented.".into()),
            Some(Ok(Ok(()))) => {}
        }
        Ok(Repair { source: source.into(), proposed, explanation: format!(
            "Proposed syntax repair: quote {quoted_keys} bare JSON keys; insert {completed} closing delimiters at EOF. Review the original and proposed source before Apply. Values, comments and strings are not removed.") })
    })();
    (!control.cancelled()).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_bounds_expansion_and_inherits_single_line_fence_newlines() {
        let deep = format!("{}0{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(
            matches!(format("json", &deep), Formatted::Unchanged(reason) if reason.contains("128 levels"))
        );
        let wide = format!(
            "{}{}0{}",
            "[".repeat(96),
            "0,".repeat(400_000),
            "]".repeat(96)
        );
        assert!(
            matches!(format("json", &wide), Formatted::Unchanged(reason) if reason.contains("64 MiB"))
        );
        assert_eq!(
            format_with_line_ending("json", "{\"a\":1}", LineEnding::Crlf),
            Formatted::Changed("{\r\n  \"a\": 1\r\n}".into())
        );
        assert_eq!(line_ending("\n{}\r\n"), LineEnding::Mixed);
        assert_eq!(line_ending("{}\r\n[]\n"), LineEnding::Mixed);
        assert_eq!(line_ending("{}\r\n"), LineEnding::Crlf);
    }

    #[test]
    fn invalid_json_format_never_quotes_keys_or_completes_syntax() {
        for source in ["{ a: 1 }", "{\"a\":1", "{\"a\":}", "// comment\n{}", "[1,]"] {
            assert!(json_validation(source).is_err());
            assert!(matches!(format("json", source), Formatted::Unchanged(_)));
        }
        let repair = repair("json", "{ a: 1 }").unwrap();
        assert_eq!(repair.source, "{ a: 1 }");
        assert_eq!(repair.proposed, "{ \"a\": 1 }");
        assert!(json_validation(&repair.proposed).is_ok());
    }

    #[test]
    fn formatting_retains_tokens_duplicates_bom_newlines_and_values() {
        let source = "\u{feff}{\"z\":1.50,\"z\":-0,\"escaped\":\"\\u0041, { \\\"x\\\" }\",\"list\":[2e3]}\r\n";
        let Formatted::Changed(result) = format("json", source) else {
            panic!("valid JSON must format")
        };
        assert!(result.starts_with('\u{feff}') && result.ends_with("\r\n"));
        assert_eq!(result.matches("\"z\"").count(), 2);
        for token in ["1.50", "-0", "2e3", "\\u0041, { \\\"x\\\" }"] {
            assert!(result.contains(token));
        }
        assert!(!result.replace("\r\n", "").contains('\n'));
        assert_eq!(json_validation(&result), Ok(()));
        assert!(matches!(format("json", &result), Formatted::Unchanged(_)));
        let Formatted::Changed(result) = format("json", "{\"a\":1}") else {
            panic!()
        };
        assert!(!result.ends_with('\n'));
    }

    #[test]
    fn language_refusal_preserves_whitespace_sensitive_and_incomplete_input() {
        for language in LANGUAGES
            .iter()
            .copied()
            .filter(|language| *language != "json")
        {
            for source in [
                "if True:\n    print('a: {x}')\n",
                "\tkeep\t this\r\n",
                "{ a: 1",
                "# comment\n",
            ] {
                assert!(
                    matches!(format(language, source), Formatted::Unchanged(reason) if reason.contains(language))
                );
                assert!(repair(language, source).unwrap_err().contains(language));
            }
        }
        assert!(matches!(
            format("jsonc", "// comment\n{a:1}"),
            Formatted::Unchanged(_)
        ));
    }

    #[test]
    fn repair_completes_only_otherwise_complete_values_and_preserves_literals() {
        for (source, expected) in [
            ("{a: [1,2", "{\"a\": [1,2]}"),
            ("{\"a\": 1 \r\n", "{\"a\": 1} \r\n"),
            ("{a:\"key: {, \\\"\"}", "{\"a\":\"key: {, \\\"\"}"),
        ] {
            let suggestion = repair("json", source).unwrap();
            assert_eq!(suggestion.proposed, expected);
            assert!(json_validation(source).is_err());
            assert_eq!(json_validation(&suggestion.proposed), Ok(()));
        }
        for source in [
            "{a:",
            "{a: }",
            "{a:'value'}",
            "{a:\"unfinished",
            "{a:[1}",
            "// comment\n{a:1}",
            "{a: undefined}",
            "[a:1]",
        ] {
            assert!(repair("json", source).is_err(), "{source}");
        }
    }
}

#[cfg(test)]
mod large_tests;
