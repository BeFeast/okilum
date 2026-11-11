//! Display-only JSON pretty-printing for the Reader code view (#1131).
//!
//! The original tokens are re-indented, never re-serialised: key order,
//! number spelling, string escapes and duplicate keys stay exactly as written.
//! The file on disk is never touched.

/// Why a file cannot be pretty-printed, at a 1-based line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub line: usize,
    pub column: usize,
}

/// Display refusal is distinct from a JSON syntax error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(Invalid),
    Limit { reason: &'static str, limit: usize },
}

pub const MAX_VIEW_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PRETTY_DEPTH: usize = 128;
pub const MAX_PRETTY_BYTES: usize = 1024 * 1024;

/// Conservative bound on indentation expansion, checked before parsing or
/// allocating output. Brackets inside strings (including escaped quotes) do
/// not count as nesting. The input bytes already bound verbatim token copies;
/// each structural token adds at most one newline and its indentation.
pub fn within_pretty_budget(text: &str) -> bool {
    budget(text, MAX_PRETTY_BYTES).is_ok()
}

fn budget(text: &str, max_bytes: usize) -> Result<(), Error> {
    formatting_bound(text, max_bytes, &|| false).map(|_| ())
}

/// Shared preallocation/depth bound. Cancellation never returns partial output.
pub fn formatting_bound(
    text: &str,
    max_bytes: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<usize>, Error> {
    let mut bound = text.len().saturating_add(1);
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (at, byte) in text.bytes().enumerate() {
        if at % (16 * 1024) == 0 && cancelled() {
            return Ok(None);
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_PRETTY_DEPTH {
                    return Err(Error::Limit {
                        reason: "nesting depth",
                        limit: MAX_PRETTY_DEPTH,
                    });
                }
                bound = bound.saturating_add(1 + depth * 2);
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                bound = bound.saturating_add(1 + depth * 2);
            }
            b',' => bound = bound.saturating_add(1 + depth * 2),
            b':' => bound = bound.saturating_add(1),
            _ => {}
        }
        if bound > max_bytes {
            return Err(Error::Limit {
                reason: "formatted output bytes",
                limit: max_bytes,
            });
        }
    }
    if bound > max_bytes {
        Err(Error::Limit {
            reason: "formatted output bytes",
            limit: max_bytes,
        })
    } else {
        Ok(Some(bound))
    }
}

/// Two-space indent, one member per line, `"key": value`. Empty objects and
/// arrays stay `{}` / `[]`. A leading byte order mark is not shown.
pub fn pretty(text: &str) -> Result<String, Error> {
    pretty_controlled(text, &|| false).map(|text| text.expect("uncancelled formatting"))
}

/// Worker formatter: cancellation retires the complete result, never a prefix.
pub fn pretty_controlled(
    text: &str,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<String>, Error> {
    let Some(bound) = formatting_bound(text, MAX_VIEW_BYTES, cancelled)? else {
        return Ok(None);
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(text) {
        if error.to_string().starts_with("recursion limit exceeded") {
            return Err(Error::Limit {
                reason: "nesting depth",
                limit: MAX_PRETTY_DEPTH,
            });
        }
        return Err(Error::Invalid(Invalid {
            line: error.line(),
            column: error.column(),
        }));
    }
    if cancelled() {
        return Ok(None);
    }
    let mut out = String::with_capacity(bound);
    let mut depth = 0usize;
    let mut visited = 0usize;
    let mut next_check = 0usize;
    let stopped = std::cell::Cell::new(false);
    let mut chars = text
        .chars()
        .take_while(|c| {
            visited += c.len_utf8();
            if visited < next_check {
                return true;
            }
            next_check = visited + 16 * 1024;
            let keep = !cancelled();
            if !keep {
                stopped.set(true);
            }
            keep
        })
        .peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                // Copy the string verbatim, escapes included.
                while let Some(c) = chars.next() {
                    out.push(c);
                    match c {
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                out.push(escaped);
                            }
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '{' | '[' => {
                let close = if c == '{' { '}' } else { ']' };
                skip_whitespace(&mut chars);
                if chars.peek() == Some(&close) {
                    chars.next();
                    out.push(c);
                    out.push(close);
                } else {
                    depth += 1;
                    out.push(c);
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out.push('\n');
    Ok((!stopped.get() && !cancelled()).then_some(out))
}

fn skip_whitespace<I: Iterator<Item = char>>(chars: &mut std::iter::Peekable<I>) {
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
}

fn newline(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation_budget_refuses_deep_and_wide_expansion_before_allocation() {
        let deep = format!("{}0{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(!within_pretty_budget(&deep));
        assert!(pretty(&deep).is_err());
        let wide = format!(
            "{}{}0{}",
            "[".repeat(96),
            "0,".repeat(80_000),
            "]".repeat(96)
        );
        assert!(wide.len() < 256 * 1024);
        assert!(!within_pretty_budget(&wide));
        assert!(pretty(&wide).is_ok());
        // Actual nested formatting and literal brackets remain supported.
        assert!(pretty(&format!("{}0{}", "[".repeat(32), "]".repeat(32))).is_ok());
        assert!(pretty(&format!("\"{}\"", "[".repeat(100_000))).is_ok());
    }

    #[test]
    fn large_view_2787_formats_the_whole_file_with_stable_verdict() {
        let raw = format!(
            "{{\"openapi\":\"3.0.4\",\"description\":\"{}\",\"tail\":42}}",
            "x".repeat(3_553_578)
        );
        assert!(
            !within_pretty_budget(&raw),
            "editing keeps its smaller bound"
        );
        for _ in 0..20 {
            let shown = pretty(&raw).unwrap();
            assert!(shown.contains("\n  \"tail\": 42\n"));
            assert!(crate::code_operations::json_validation(&shown).is_ok());
        }
        let huge = format!("\"{}\"", "x".repeat(MAX_VIEW_BYTES));
        assert_eq!(
            pretty(&huge),
            Err(Error::Limit {
                reason: "formatted output bytes",
                limit: MAX_VIEW_BYTES
            })
        );
        assert!(matches!(pretty("{broken}"), Err(Error::Invalid(_))));
    }

    #[test]
    fn one_line_json_becomes_indented_with_order_and_spelling_kept() {
        let raw = r#"{"z":1,"a":{"list":[1.50,2e3,-0],"empty":{},"none":[]},"s":"a, b: {\"c\"} \\ é é","t":true,"n":null}"#;
        let shown = pretty(raw).unwrap();
        assert_eq!(
            shown,
            r#"{
  "z": 1,
  "a": {
    "list": [
      1.50,
      2e3,
      -0
    ],
    "empty": {},
    "none": []
  },
  "s": "a, b: {\"c\"} \\ é é",
  "t": true,
  "n": null
}
"#
        );
        // Re-reading the shown text gives the same value as the file.
        let a: serde_json::Value = serde_json::from_str(raw).unwrap();
        let b: serde_json::Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn already_pretty_and_bom_files_settle_to_the_same_text() {
        let once = pretty("\u{feff}[ {\"k\" : [ ] } ,\r\n 2 ]").unwrap();
        assert_eq!(once, "[\n  {\n    \"k\": []\n  },\n  2\n]\n");
        assert_eq!(pretty(&once).unwrap(), once);
    }

    #[test]
    fn invalid_json_reports_where_and_shows_nothing_pretty() {
        assert_eq!(
            pretty("{\n  \"a\": 1,\n  \"b\": \n}"),
            Err(Error::Invalid(Invalid { line: 4, column: 1 }))
        );
        // Comments are not JSON: JSONC files stay raw.
        assert!(pretty("// note\n{}").is_err());
        assert!(pretty("").is_err());
        // Positive control: the smallest valid documents pass.
        assert_eq!(pretty("0").unwrap(), "0\n");
        assert_eq!(pretty(" {} ").unwrap(), "{}\n");
    }
}
