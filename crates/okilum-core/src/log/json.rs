//! A borrowing JSON-object scanner. It reports every leaf with its key path
//! and the exact source bytes of its value, so nothing is renumbered,
//! reordered or deduplicated on the way to the screen.

use std::borrow::Cow;

use super::record::{Escape, RawValue, ValueKind};

/// Nested objects deeper than this are reported whole, as raw JSON text.
const MAX_FLATTEN_DEPTH: usize = 16;

/// Scans one line that must be exactly one JSON object (surrounding
/// whitespace allowed). `visit` receives leaves in source order; an empty
/// object value is a leaf. Returns `Err` for anything that is not an object.
pub(crate) fn scan<'a>(
    line: &'a [u8],
    visit: &mut dyn FnMut(&[Cow<'a, str>], RawValue<'a>),
) -> Result<(), ()> {
    let mut cursor = Cursor { b: line, i: 0 };
    cursor.ws();
    if cursor.peek() != Some(b'{') {
        return Err(());
    }
    let mut path = Vec::new();
    cursor.members(&mut path, visit, 0)?;
    cursor.ws();
    if cursor.i != line.len() {
        return Err(());
    }
    Ok(())
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), ()> {
        if self.peek() == Some(byte) {
            self.i += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    /// At an opening quote; returns the content between the quotes.
    fn string(&mut self) -> Result<(&'a [u8], bool), ()> {
        self.expect(b'"')?;
        let start = self.i;
        let mut escaped = false;
        loop {
            let at = memchr::memchr2(b'"', b'\\', &self.b[self.i..]).ok_or(())?;
            self.i += at;
            if self.b[self.i] == b'"' {
                let content = &self.b[start..self.i];
                self.i += 1;
                return Ok((content, escaped));
            }
            escaped = true;
            self.i += 2;
            if self.i > self.b.len() {
                return Err(());
            }
        }
    }

    fn literal(&mut self, word: &[u8]) -> Result<&'a [u8], ()> {
        if self.b[self.i..].starts_with(word) {
            let raw = &self.b[self.i..self.i + word.len()];
            self.i += word.len();
            Ok(raw)
        } else {
            Err(())
        }
    }

    fn number(&mut self) -> Result<&'a [u8], ()> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let digits = self.i;
        while matches!(
            self.peek(),
            Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
        ) {
            self.i += 1;
        }
        let raw = &self.b[start..self.i];
        if self.i == digits || !self.b[digits].is_ascii_digit() {
            return Err(());
        }
        Ok(raw)
    }

    /// Skips an array or object without flattening it, string-aware.
    fn container(&mut self) -> Result<&'a [u8], ()> {
        let start = self.i;
        let mut depth = 0usize;
        loop {
            match self.peek().ok_or(())? {
                b'"' => {
                    self.string()?;
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        self.i += 1;
                        return Ok(&self.b[start..self.i]);
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<RawValue<'a>, ()> {
        let (kind, raw, escape) = match self.peek().ok_or(())? {
            b'"' => {
                let (raw, escaped) = self.string()?;
                let escape = if escaped { Escape::Json } else { Escape::None };
                (ValueKind::String, raw, escape)
            }
            b'{' => (ValueKind::Object, self.container()?, Escape::None),
            b'[' => (ValueKind::Array, self.container()?, Escape::None),
            b't' => (ValueKind::Bool, self.literal(b"true")?, Escape::None),
            b'f' => (ValueKind::Bool, self.literal(b"false")?, Escape::None),
            b'n' => (ValueKind::Null, self.literal(b"null")?, Escape::None),
            b'-' | b'0'..=b'9' => (ValueKind::Number, self.number()?, Escape::None),
            _ => return Err(()),
        };
        Ok(RawValue { kind, raw, escape })
    }

    /// At `{`. Returns whether the object had any member.
    fn members(
        &mut self,
        path: &mut Vec<Cow<'a, str>>,
        visit: &mut dyn FnMut(&[Cow<'a, str>], RawValue<'a>),
        depth: usize,
    ) -> Result<bool, ()> {
        self.expect(b'{')?;
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(false);
        }
        loop {
            self.ws();
            let (raw, escaped) = self.string()?;
            let key = if escaped {
                Cow::Owned(unescape(raw))
            } else {
                String::from_utf8_lossy(raw)
            };
            self.ws();
            self.expect(b':')?;
            self.ws();
            path.push(key);
            if self.peek() == Some(b'{') && depth + 1 < MAX_FLATTEN_DEPTH {
                let start = self.i;
                if !self.members(path, visit, depth + 1)? {
                    let raw = &self.b[start..self.i];
                    visit(
                        path,
                        RawValue {
                            kind: ValueKind::Object,
                            raw,
                            escape: Escape::None,
                        },
                    );
                }
            } else {
                let value = self.value()?;
                visit(path, value);
            }
            path.pop();
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(true);
                }
                _ => return Err(()),
            }
        }
    }
}

/// Decodes JSON string escapes. Invalid escapes and lone surrogates become
/// U+FFFD rather than failing the record: the raw line stays one click away.
pub(crate) fn unescape(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    let push = |out: &mut Vec<u8>, c: char| {
        let mut buf = [0; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    };
    while i < raw.len() {
        let Some(at) = memchr::memchr(b'\\', &raw[i..]) else {
            out.extend_from_slice(&raw[i..]);
            break;
        };
        out.extend_from_slice(&raw[i..i + at]);
        i += at + 1;
        let Some(&code) = raw.get(i) else {
            push(&mut out, char::REPLACEMENT_CHARACTER);
            break;
        };
        i += 1;
        match code {
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            b'/' => out.push(b'/'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'u' => {
                let unit = |at: usize| {
                    raw.get(at..at + 4)
                        .and_then(|hex| std::str::from_utf8(hex).ok())
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                };
                let Some(high) = unit(i) else {
                    push(&mut out, char::REPLACEMENT_CHARACTER);
                    continue;
                };
                i += 4;
                let c = if (0xD800..0xDC00).contains(&high) && raw.get(i..i + 2) == Some(b"\\u") {
                    match unit(i + 2).filter(|low| (0xDC00..0xE000).contains(low)) {
                        Some(low) => {
                            i += 6;
                            char::from_u32(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00))
                        }
                        None => None,
                    }
                } else {
                    char::from_u32(high)
                };
                push(&mut out, c.unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            _ => push(&mut out, char::REPLACEMENT_CHARACTER),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(line: &str) -> Option<Vec<(String, ValueKind, String)>> {
        let mut out = Vec::new();
        scan(line.as_bytes(), &mut |path, value| {
            out.push((path.join("."), value.kind, value.text().into_owned()))
        })
        .ok()?;
        Some(out)
    }

    #[test]
    fn leaves_keep_order_spelling_duplicates_and_raw_numbers() {
        let got = leaves(
            r#"{"b":1.10,"a_b":"x","user":{"id":7,"role":{"name":"owner"}},"b":null,"tags":["a",{"k":"]"}],"e":{},"ok":true}"#,
        )
        .unwrap();
        let expected = [
            ("b", ValueKind::Number, "1.10"),
            ("a_b", ValueKind::String, "x"),
            ("user.id", ValueKind::Number, "7"),
            ("user.role.name", ValueKind::String, "owner"),
            ("b", ValueKind::Null, "null"),
            ("tags", ValueKind::Array, r#"["a",{"k":"]"}]"#),
            ("e", ValueKind::Object, "{}"),
            ("ok", ValueKind::Bool, "true"),
        ];
        assert_eq!(got.len(), expected.len());
        for (got, (key, kind, text)) in got.iter().zip(expected) {
            assert_eq!((got.0.as_str(), got.1, got.2.as_str()), (key, kind, text));
        }
    }

    #[test]
    fn escapes_decode_like_serde_json() {
        let cases = [
            r#"plain"#,
            r#"quote \" slash \/ back \\ nl \n tab \t"#,
            r#"é中😀"#,
            r#"lone \ud800 surrogate"#,
        ];
        for case in cases {
            let json = format!("\"{case}\"");
            let expected: String =
                serde_json::from_str(&json).unwrap_or_else(|_| case.replace(r"\ud800", "\u{FFFD}"));
            assert_eq!(unescape(case.as_bytes()), expected, "{case}");
        }
        let got = leaves(r#"{"k\"ey":"vA"}"#).unwrap();
        assert_eq!(got[0].0, "k\"ey");
        assert_eq!(got[0].2, "vA");
    }

    #[test]
    fn non_objects_and_broken_lines_are_rejected() {
        for line in [
            "",
            "[1,2]",
            "\"text\"",
            "{",
            r#"{"a":1"#,
            r#"{"a" 1}"#,
            r#"{"a":1,}"#,
            r#"{"a":tru}"#,
            r#"{"a":1} trailing"#,
            r#"{"a":"unterminated}"#,
            r#"{"a":-}"#,
            "not json at all",
        ] {
            assert!(leaves(line).is_none(), "{line:?}");
        }
        // Positive control: the same scanner accepts the valid neighbours.
        assert!(leaves(" {} ").unwrap().is_empty());
        assert_eq!(leaves(r#"{"a":-1e3}"#).unwrap()[0].2, "-1e3");
    }

    #[test]
    fn deep_nesting_stops_flattening_without_recursing_further() {
        let deep = format!(
            "{}{}",
            "{\"k\":".repeat(40),
            "1".to_owned() + &"}".repeat(40)
        );
        let got = leaves(&deep).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0.split('.').count(), MAX_FLATTEN_DEPTH);
        assert_eq!(got[0].1, ValueKind::Object);
    }
}
