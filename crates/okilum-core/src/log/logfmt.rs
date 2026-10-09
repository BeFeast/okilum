//! logfmt: space-separated `key=value` pairs, values optionally quoted.
//!
//! Every token must be a pair. Bare words are rejected, so a prose line such
//! as `Starting server at port=8080` is an unparsed line rather than a record
//! with three invented boolean keys.

use std::borrow::Cow;

use super::record::{Escape, RawValue, ValueKind};

pub(crate) fn scan<'a>(
    line: &'a [u8],
    visit: &mut dyn FnMut(&[Cow<'a, str>], RawValue<'a>),
) -> Result<(), ()> {
    let mut i = 0;
    let mut pairs = 0;
    let space = |c: u8| c == b' ' || c == b'\t';
    loop {
        while line.get(i).copied().is_some_and(space) {
            i += 1;
        }
        if i == line.len() {
            break;
        }
        let start = i;
        while line
            .get(i)
            .is_some_and(|&c| !space(c) && c != b'=' && c != b'"')
        {
            i += 1;
        }
        if i == start || line.get(i) != Some(&b'=') {
            return Err(());
        }
        let key = std::str::from_utf8(&line[start..i]).map_err(|_| ())?;
        i += 1;
        let value = if line.get(i) == Some(&b'"') {
            i += 1;
            let begin = i;
            let mut escaped = false;
            loop {
                let at = memchr::memchr2(b'"', b'\\', &line[i..]).ok_or(())?;
                i += at;
                if line[i] == b'"' {
                    break;
                }
                escaped = true;
                i += 2;
                if i > line.len() {
                    return Err(());
                }
            }
            let raw = &line[begin..i];
            i += 1;
            if line.get(i).is_some_and(|&c| !space(c)) {
                return Err(());
            }
            RawValue {
                kind: ValueKind::String,
                raw,
                escape: if escaped {
                    Escape::Logfmt
                } else {
                    Escape::None
                },
            }
        } else {
            let begin = i;
            while line.get(i).is_some_and(|&c| !space(c)) {
                i += 1;
            }
            let raw = &line[begin..i];
            RawValue {
                kind: bare_kind(raw),
                raw,
                escape: Escape::None,
            }
        };
        visit(std::slice::from_ref(&Cow::Borrowed(key)), value);
        pairs += 1;
    }
    if pairs == 0 {
        return Err(());
    }
    Ok(())
}

fn bare_kind(raw: &[u8]) -> ValueKind {
    match raw {
        b"true" | b"false" => ValueKind::Bool,
        b"null" => ValueKind::Null,
        _ if !raw.is_empty()
            && std::str::from_utf8(raw).is_ok_and(|s| s.parse::<f64>().is_ok())
            && raw.iter().any(u8::is_ascii_digit) =>
        {
            ValueKind::Number
        }
        _ => ValueKind::String,
    }
}

pub(crate) fn unescape(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied();
    while let Some(c) = bytes.next() {
        if c != b'\\' {
            out.push(c);
            continue;
        }
        match bytes.next() {
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'r') => out.push(b'\r'),
            Some(b'"') => out.push(b'"'),
            Some(b'\\') => out.push(b'\\'),
            Some(other) => out.extend_from_slice(&[b'\\', other]),
            None => out.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(line: &str) -> Option<Vec<(String, ValueKind, String)>> {
        let mut out = Vec::new();
        scan(line.as_bytes(), &mut |path, value| {
            out.push((path.join("."), value.kind, value.text().into_owned()))
        })
        .ok()?;
        Some(out)
    }

    #[test]
    fn pairs_keep_spelling_quotes_and_order() {
        let got = pairs(
            r#"ts=2025-10-01T00:00:00Z level=info msg="scan \"done\"" request_id=97c0 elapsed_ms=8.50 empty= url=/a?b=c ok=true"#,
        )
        .unwrap();
        let keys: Vec<_> = got.iter().map(|p| p.0.as_str()).collect();
        assert_eq!(
            keys,
            [
                "ts",
                "level",
                "msg",
                "request_id",
                "elapsed_ms",
                "empty",
                "url",
                "ok"
            ]
        );
        assert_eq!(got[2].2, "scan \"done\"");
        assert_eq!(
            got[4],
            ("elapsed_ms".into(), ValueKind::Number, "8.50".into())
        );
        assert_eq!(got[5].2, "");
        assert_eq!(got[6].2, "/a?b=c");
        assert_eq!(got[7].1, ValueKind::Bool);
    }

    #[test]
    fn prose_and_broken_quotes_are_not_records() {
        for line in [
            "",
            "   ",
            "Starting server at port=8080",
            "goroutine 1 [running]:",
            r#"msg="unterminated"#,
            r#"msg="a"b=1"#,
            "=value",
            "\"k\"=v",
        ] {
            assert!(pairs(line).is_none(), "{line:?}");
        }
        assert_eq!(pairs("a=1").unwrap().len(), 1, "positive control");
    }
}
