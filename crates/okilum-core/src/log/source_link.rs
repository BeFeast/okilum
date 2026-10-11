//! Source locations in log records (#1165): `file:line[:col]` in a field or
//! a stack frame, backward-cpp's and Python's `"file", line N`, and quill's
//! `file_name` + `line` fields, plus how to open one in an editor.
//!
//! Detection is conservative: a path counts only with a source-file
//! extension, so `10.0.0.1:8080`, `12:30:45` and `host:443` are not links.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::Record;

/// Extensions of files a stack frame or a logging call site points at.
const SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "inl", "ipp", "m", "mm", "rs", "go",
    "py", "pyx", "js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx", "java", "kt", "kts",
    "scala", "cs", "fs", "vb", "rb", "php", "swift", "dart", "ex", "exs", "erl", "hrl", "clj",
    "lua", "pl", "pm", "sh", "zig", "nim", "d", "hs", "ml", "vue", "svelte",
];

/// A source location found in log text. `span` covers `path:line[:col]` (or
/// the quoted form) in the text it was found in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub path: String,
    pub line: u32,
    pub column: Option<u32>,
    pub span: Range<usize>,
}

/// `path:line[:col]`: the path is a run without spaces, quotes or brackets
/// ending in `.ext`; a leading drive letter (`C:\`) is part of the path.
static COLON_FORM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        (?P<path>(?:[A-Za-z]:[\\/])?[^\s:"'()<>\[\]{},;=]*\.(?P<ext>[A-Za-z0-9+]+))
        :(?P<line>[0-9]{1,7})
        (?::(?P<col>[0-9]{1,5}))?
        (?P<after>[0-9A-Za-z_]?)
        "#,
    )
    .expect("source location pattern")
});

/// backward-cpp `Source "path", line 64` and Python `File "path", line 12`.
static QUOTED_FORM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""(?P<path>[^"\n]+\.(?P<ext>[A-Za-z0-9+]+))", line (?P<line>[0-9]{1,7})"#)
        .expect("quoted source location pattern")
});

fn is_source_extension(extension: &str) -> bool {
    SOURCE_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(extension))
}

/// Every source location in `text`, in order, none overlapping.
pub fn find(text: &str) -> Vec<SourceLocation> {
    let mut found: Vec<SourceLocation> = Vec::new();
    for captures in QUOTED_FORM.captures_iter(text) {
        if !is_source_extension(&captures["ext"]) {
            continue;
        }
        let Ok(line) = captures["line"].parse() else {
            continue;
        };
        let whole = captures.get(0).expect("match");
        found.push(SourceLocation {
            path: captures["path"].to_owned(),
            line,
            column: None,
            span: whole.range(),
        });
    }
    for captures in COLON_FORM.captures_iter(text) {
        // `file.cpp:12a` or `v1.2:34x` is not a location.
        if !captures["after"].is_empty() || !is_source_extension(&captures["ext"]) {
            continue;
        }
        let path = captures.name("path").expect("path");
        let end = captures
            .name("col")
            .or_else(|| captures.name("line"))
            .expect("line")
            .end();
        let span = path.start()..end;
        if found
            .iter()
            .any(|known| known.span.start < span.end && span.start < known.span.end)
        {
            continue;
        }
        let Ok(line) = captures["line"].parse() else {
            continue;
        };
        found.push(SourceLocation {
            path: path.as_str().to_owned(),
            line,
            column: captures
                .name("col")
                .and_then(|col| col.as_str().parse().ok()),
            span,
        });
    }
    found.sort_by_key(|location| location.span.start);
    found
}

/// A location in a record's fields: the field's index and, for a link in
/// its value, the location with a span into that value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldLocation {
    pub field: usize,
    pub location: SourceLocation,
}

/// Locations in `record`'s field values, plus quill's `file_name` + `line`
/// pair, which is linked on the `file_name` value.
pub fn in_record(record: &Record) -> Vec<FieldLocation> {
    let mut found: Vec<FieldLocation> = record
        .fields
        .iter()
        .enumerate()
        .flat_map(|(field, value)| {
            find(&value.value)
                .into_iter()
                .map(move |location| FieldLocation { field, location })
        })
        .collect();
    let named = |names: &[&str]| {
        record.fields.iter().position(|field| {
            names
                .iter()
                .any(|name| field.key.eq_ignore_ascii_case(name))
        })
    };
    if let (Some(file), Some(line)) = (
        named(&["file_name", "filename", "file", "source_file"]),
        named(&["line", "lineno", "line_number", "source_line"]),
    ) {
        let path = &record.fields[file].value;
        let extension = Path::new(path).extension().and_then(|e| e.to_str());
        if let (Some(extension), Ok(number)) = (extension, record.fields[line].value.trim().parse())
        {
            if is_source_extension(extension) && !found.iter().any(|known| known.field == file) {
                found.push(FieldLocation {
                    field: file,
                    location: SourceLocation {
                        path: path.clone(),
                        line: number,
                        column: None,
                        span: 0..path.len(),
                    },
                });
            }
        }
    }
    found
}

/// Where a click on a location goes (#1165, Settings > Logs).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Editor {
    #[default]
    VsCode,
    Cursor,
    Zed,
    /// JetBrains IDEs through their command-line launcher.
    JetBrains,
    /// A command line with `{file}`, `{line}` and `{col}` placeholders.
    Custom(String),
}

/// How to open a location: a URL for the system to hand to the editor, or a
/// program and its arguments, run without a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    Url(String),
    Command(Vec<String>),
}

/// The command used for the JetBrains choice; `clion` is the CLion launcher
/// that the Toolbox installs on the path.
pub const JETBRAINS_COMMAND: &str = "clion --line {line} --column {col} {file}";

/// How to open `path` at `line` (and `column`) in `editor`.
pub fn open(editor: &Editor, path: &Path, line: u32, column: Option<u32>) -> Result<Open, String> {
    let column = column.unwrap_or(1);
    let url =
        |scheme: &str| Open::Url(format!("{scheme}://file{}:{line}:{column}", url_path(path)));
    match editor {
        Editor::VsCode => Ok(url("vscode")),
        Editor::Cursor => Ok(url("cursor")),
        Editor::Zed => Ok(url("zed")),
        Editor::JetBrains => command(JETBRAINS_COMMAND, path, line, column),
        Editor::Custom(template) => command(template, path, line, column),
    }
}

fn command(template: &str, path: &Path, line: u32, column: u32) -> Result<Open, String> {
    let words = split_command(template)?;
    if words.is_empty() {
        return Err("The editor command is empty".into());
    }
    let file = path.to_string_lossy();
    Ok(Open::Command(
        words
            .into_iter()
            .map(|word| {
                word.replace("{file}", &file)
                    .replace("{line}", &line.to_string())
                    .replace("{col}", &column.to_string())
            })
            .collect(),
    ))
}

/// Splits a command line into words: whitespace separates, double or single
/// quotes group, a backslash escapes the next character outside single
/// quotes. Nothing else is interpreted; the result never reaches a shell.
pub fn split_command(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') => match chars.next() {
                Some(next) => {
                    word.push(next);
                    in_word = true;
                }
                None => return Err("The editor command ends with a backslash".into()),
            },
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("The editor command has an unclosed quote".into());
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// `path` for an editor URL: forward slashes, a Windows drive kept after the
/// first slash (`/C:/src/a.cpp`), and characters outside the URL-safe path
/// set percent-encoded.
fn url_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::with_capacity(text.len() + 1);
    if !text.starts_with('/') {
        out.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One path mapping for builds made elsewhere (#1165): a location under
/// `from` (as the log writes it) opens under `to` (a local checkout).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PathMap {
    pub from: String,
    pub to: PathBuf,
}

/// `path` with the longest matching `from` prefix replaced by its `to`.
/// A prefix matches whole path components only (`/src/ord` does not match
/// `/src/ordgw/a.cpp`), and `/` and `\` are the same separator. No match
/// leaves the path as written.
pub fn map(path: &str, maps: &[PathMap]) -> Option<PathBuf> {
    let normal = |text: &str| text.replace('\\', "/");
    let written = normal(path);
    maps.iter()
        .filter_map(|map| {
            let from = normal(&map.from);
            let from = from.trim_end_matches('/');
            if from.is_empty() {
                return None;
            }
            let rest = written.strip_prefix(from)?;
            (rest.is_empty() || rest.starts_with('/')).then(|| (from.len(), map, rest.to_owned()))
        })
        .max_by_key(|(length, _, _)| *length)
        .map(|(_, map, rest)| {
            let mut local = map.to.clone();
            for part in rest.split('/').filter(|part| !part.is_empty()) {
                local.push(part);
            }
            local
        })
}

/// Resolves a location's path: absolute paths as they are, relative ones
/// against `base` (the log's folder) when that file exists there.
pub fn resolve(path: &str, base: &Path) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() || base.as_os_str().is_empty() {
        return candidate.to_owned();
    }
    let joined = base.join(candidate);
    if joined.exists() {
        joined
    } else {
        candidate.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Format;

    fn paths(text: &str) -> Vec<(String, u32, Option<u32>)> {
        find(text)
            .into_iter()
            .map(|location| {
                assert!(text.get(location.span.clone()).is_some());
                (location.path, location.line, location.column)
            })
            .collect()
    }

    #[test]
    fn stack_frames_and_call_sites_are_found() {
        // AddressSanitizer frame from the sample pack (#1155).
        let asan = "    #1 0x55e2b1c4f1e2 in okl::OrderBook::replace(okl::OrderId) /src/ordgw/src/md/order_book.cpp:212:23";
        assert_eq!(
            paths(asan),
            [("/src/ordgw/src/md/order_book.cpp".into(), 212, Some(23))]
        );
        let found = find(asan);
        assert_eq!(
            &asan[found[0].span.clone()],
            "/src/ordgw/src/md/order_book.cpp:212:23"
        );
        // spdlog `src` field, Go panic, Node and Python frames, backward-cpp.
        assert_eq!(
            paths("order_book.cpp:97"),
            [("order_book.cpp".into(), 97, None)]
        );
        assert_eq!(
            paths("\t/app/cmd/checkout/main.go:42 +0x1d"),
            [("/app/cmd/checkout/main.go".into(), 42, None)]
        );
        assert_eq!(
            paths("    at handler (/srv/app/routes/pay.ts:10:5)"),
            [("/srv/app/routes/pay.ts".into(), 10, Some(5))]
        );
        assert_eq!(
            paths(r#"  File "/srv/app/jobs.py", line 12, in run"#),
            [("/srv/app/jobs.py".into(), 12, None)]
        );
        assert_eq!(
            paths(r#"#5    Source "/src/ordgw/src/risk/risk_thread.cpp", line 64, in run()"#),
            [("/src/ordgw/src/risk/risk_thread.cpp".into(), 64, None)]
        );
        assert_eq!(
            paths(r"C:\work\ordgw\src\fix.cpp:7:2 and src/a.rs:3"),
            [
                (r"C:\work\ordgw\src\fix.cpp".into(), 7, Some(2)),
                ("src/a.rs".into(), 3, None)
            ]
        );
    }

    #[test]
    fn addresses_times_and_versions_are_not_links() {
        for text in [
            "10.0.0.1:8080",
            "upstream checkout-api.shop.svc:8080 refused",
            "at 12:30:45.120",
            "listening on [::]:443",
            "image ordgw:1.42.0",
            "libc.so.6:12",
            "order_book.cpp:12abc",
            "(/build/asan/tests/ordgw_tests+0x1a2e3d)",
        ] {
            assert!(find(text).is_empty(), "{text:?} -> {:?}", find(text));
        }
    }

    #[test]
    fn quill_file_name_and_line_fields_are_one_location() {
        let line = br#"{"timestamp":"1791612242497259677","file_name":"risk_engine.cpp","line":"118","message":"accepted"}"#;
        let record = Record::parse(line, Format::JsonLines).unwrap();
        let found = in_record(&record);
        assert_eq!(found.len(), 1);
        assert_eq!(record.fields[found[0].field].key, "file_name");
        assert_eq!(found[0].location.path, "risk_engine.cpp");
        assert_eq!(found[0].location.line, 118);
        let spd = br#"{"level":"warning","src":"order_book.cpp:97","msg":"crossed book"}"#;
        let record = Record::parse(spd, Format::JsonLines).unwrap();
        let found = in_record(&record);
        assert_eq!(record.fields[found[0].field].key, "src");
        assert_eq!(found[0].location.line, 97);
    }

    #[test]
    fn mapped_paths_take_the_longest_whole_component_prefix() {
        let maps = [
            PathMap {
                from: "/src/ordgw".into(),
                to: PathBuf::from("/home/me/work/ordgw"),
            },
            PathMap {
                from: "/src/ordgw/third_party/".into(),
                to: PathBuf::from("/opt/vendor"),
            },
            PathMap {
                from: r"C:\build\agent\ordgw".into(),
                to: PathBuf::from("/home/me/work/ordgw"),
            },
        ];
        assert_eq!(
            map("/src/ordgw/src/md/order_book.cpp", &maps),
            Some(PathBuf::from("/home/me/work/ordgw/src/md/order_book.cpp"))
        );
        assert_eq!(
            map("/src/ordgw/third_party/gtest/gtest.cc", &maps),
            Some(PathBuf::from("/opt/vendor/gtest/gtest.cc")),
            "the longer prefix wins"
        );
        assert_eq!(
            map(r"C:\build\agent\ordgw\src\fix.cpp", &maps),
            Some(PathBuf::from("/home/me/work/ordgw/src/fix.cpp"))
        );
        // Whole components only, and no match leaves the path alone.
        assert_eq!(map("/src/ordgw2/a.cpp", &maps), None);
        assert_eq!(map("/usr/include/c++/vector", &maps), None);
        assert_eq!(map("/src/ordgw/a.cpp", &[]), None);
    }

    #[test]
    fn editors_get_urls_or_shell_free_commands() {
        let file = Path::new("/src/ordgw/src/md/order_book.cpp");
        assert_eq!(
            open(&Editor::VsCode, file, 212, Some(23)).unwrap(),
            Open::Url("vscode://file/src/ordgw/src/md/order_book.cpp:212:23".into())
        );
        assert_eq!(
            open(&Editor::Cursor, file, 212, None).unwrap(),
            Open::Url("cursor://file/src/ordgw/src/md/order_book.cpp:212:1".into())
        );
        assert_eq!(
            open(
                &Editor::Zed,
                Path::new(r"C:\work\My Book\a.cpp"),
                7,
                Some(2)
            )
            .unwrap(),
            Open::Url("zed://file/C:/work/My%20Book/a.cpp:7:2".into())
        );
        assert_eq!(
            open(&Editor::JetBrains, file, 212, Some(23)).unwrap(),
            Open::Command(
                [
                    "clion",
                    "--line",
                    "212",
                    "--column",
                    "23",
                    "/src/ordgw/src/md/order_book.cpp"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        // A custom command: placeholders inside quoted words, no shell.
        let custom = Editor::Custom(r#"nvim-remote "+call cursor({line}, {col})" '{file}'"#.into());
        assert_eq!(
            open(&custom, Path::new("/a b/x.rs; rm -rf /"), 3, None).unwrap(),
            Open::Command(
                ["nvim-remote", "+call cursor(3, 1)", "/a b/x.rs; rm -rf /"]
                    .map(String::from)
                    .to_vec()
            )
        );
        assert!(open(&Editor::Custom("  ".into()), file, 1, None).is_err());
        assert!(split_command(r#"code "unclosed"#).is_err());
    }
}
