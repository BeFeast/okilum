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

/// Two-space indent, one member per line, `"key": value`. Empty objects and
/// arrays stay `{}` / `[]`. A leading byte order mark is not shown.
pub fn pretty(text: &str) -> Result<String, Invalid> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(text) {
        return Err(Invalid {
            line: error.line(),
            column: error.column(),
        });
    }
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut depth = 0usize;
    let mut chars = text.chars().peekable();
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
    Ok(out)
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
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
            Err(Invalid { line: 4, column: 1 })
        );
        // Comments are not JSON: JSONC files stay raw.
        assert!(pretty("// note\n{}").is_err());
        assert!(pretty("").is_err());
        // Positive control: the smallest valid documents pass.
        assert_eq!(pretty("0").unwrap(), "0\n");
        assert_eq!(pretty(" {} ").unwrap(), "{}\n");
    }
}
