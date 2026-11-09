//! Source operations are separate from validation and from read-only display.
//! A formatter may change whitespace only under the selected language's rules.
//! Syntax correction is a proposal; this module never applies or saves it.

pub const MAX_BYTES: usize = 256 * 1024;

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

/// Refusal is a successful no-change result, with a reason suitable for the UI.
/// No generic indentation/whitespace pass is used for unsupported languages.
pub fn format(language: &str, source: &str) -> Formatted {
    if source.len() > MAX_BYTES {
        return Formatted::Unchanged(
            "Formatting exceeds the 256 KiB safety limit; source unchanged.".into(),
        );
    }
    if language != "json" {
        return Formatted::Unchanged(format!(
            "No safe built-in formatter for {language}; source unchanged. Its whitespace, comments and strings are not rewritten using another language's rules."
        ));
    }
    if let Err(error) = json_validation(source) {
        return Formatted::Unchanged(format!(
            "JSON is invalid at {}:{}; this formatter cannot safely recover its structure. Source unchanged. Use Repair separately to review a syntax correction.",
            error.line, error.column
        ));
    }
    let crlf = source.contains("\r\n");
    if crlf && source.replace("\r\n", "").contains('\n') {
        return Formatted::Unchanged("Mixed line endings; source unchanged.".into());
    }
    let Ok(mut result) = crate::json_view::pretty(source) else {
        return Formatted::Unchanged("JSON formatting was refused; source unchanged.".into());
    };
    if !source.ends_with('\n') {
        result.pop();
    }
    if crlf {
        result = result.replace('\n', "\r\n");
    }
    if source.starts_with('\u{feff}') {
        result.insert(0, '\u{feff}');
    }
    if result == source {
        Formatted::Unchanged("Already formatted.".into())
    } else {
        Formatted::Changed(result)
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
    if source.len() > MAX_BYTES {
        return Err("Repair exceeds the 256 KiB safety limit; source unchanged.".into());
    }
    if language != "json" {
        return Err(format!("No safe automatic repair for {language}; source unchanged. No correction from another language is applied."));
    }
    if json_validation(source).is_ok() {
        return Err("JSON is already valid; no syntax repair is needed.".into());
    }
    let bytes = source.as_bytes();
    let mut at = if source.starts_with('\u{feff}') { 3 } else { 0 };
    let mut previous = 0;
    let mut stack = Vec::new();
    let mut insertions: Vec<(usize, &'static str)> = Vec::new();
    while at < bytes.len() {
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
                        "Unterminated or ambiguous JSON string; no safe repair proposed.".into(),
                    );
                }
                previous = b'"';
                continue;
            }
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
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
                    at += 1;
                }
                let mut next = at;
                while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    insertions.push((start, "\""));
                    insertions.push((at, "\""));
                }
                previous = b'"';
                continue;
            }
            _ => {}
        }
        previous = ch;
        at += 1;
    }
    let mut proposed = source.to_owned();
    for (at, text) in insertions.iter().rev() {
        proposed.insert_str(*at, text);
    }
    let completed = stack.len();
    // Keep authored trailing whitespace in place and show the exact insertion.
    let end = proposed.trim_end_matches([' ', '\t', '\r', '\n']).len();
    let closers: String = stack.into_iter().rev().map(char::from).collect();
    proposed.insert_str(end, &closers);
    if proposed == source || json_validation(&proposed).is_err() {
        return Err("No safe strict-JSON correction found; source unchanged. Missing values, comments and ambiguous tokens are not discarded or invented.".into());
    }
    Ok(Repair {
        source: source.into(),
        proposed,
        explanation: format!(
            "Proposed syntax repair: quote {} bare JSON keys; insert {completed} closing delimiters at EOF. Review the original and proposed source before Apply. Values, comments and strings are not removed.",
            insertions.len() / 2
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
