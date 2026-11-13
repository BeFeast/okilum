//! Copy as cURL (#2836): a request as a command for bash, Windows cmd or
//! PowerShell. Every value is quoted for exactly one shell so that no value,
//! however hostile, becomes shell syntax or a curl option.
//!
//! - The URL is always passed with `--url`: a capture whose URL starts with
//!   `-` must not become a curl option such as `-o`.
//! - Header names must be HTTP tokens (so `@file` cannot make curl read a
//!   file); CR/LF in values are replaced with spaces (no header injection).
//! - HTTP/2 pseudo-headers and `Content-Length` are left to curl.
//! - NUL cannot be passed in any argument and is dropped.
//! - cmd: no `!` reaches the command line, so delayed expansion (`/V:ON`)
//!   has nothing to expand: the URL's `!` is percent-encoded (`%21`), and a
//!   header, method or body containing `!` is left out ([`Command::left_out`]).
//!   Every `%` that something follows is written `^%^`, so whatever name
//!   cmd reads between two `%` starts with `^` and is never a variable.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Bash,
    Cmd,
    PowerShell,
}

impl Dialect {
    pub(crate) const ALL: [Dialect; 3] = [Dialect::Bash, Dialect::Cmd, Dialect::PowerShell];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Dialect::Bash => "Copy as cURL (bash)",
            Dialect::Cmd => "Copy as cURL (cmd)",
            Dialect::PowerShell => "Copy as PowerShell",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Dialect::Bash => "har-copy-curl-bash",
            Dialect::Cmd => "har-copy-curl-cmd",
            Dialect::PowerShell => "har-copy-powershell",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// A text body; binary bodies are left out (`body_omitted`).
    pub body: Option<String>,
    pub body_omitted: bool,
}

/// RFC 9110 token characters.
fn token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn header_value(value: &str) -> String {
    value.replace(['\r', '\n'], " ").replace('\0', "")
}

/// Headers curl should send: valid names, no pseudo-headers or length.
pub(crate) fn headers(request: &Request) -> Vec<(&str, String)> {
    request
        .headers
        .iter()
        .filter(|(name, _)| token(name) && !name.eq_ignore_ascii_case("content-length"))
        .map(|(name, value)| (name.as_str(), header_value(value)))
        .collect()
}

fn method(request: &Request) -> Option<&str> {
    let method = request.method.as_str();
    let implied = match &request.body {
        Some(_) => "POST",
        None => "GET",
    };
    (token(method) && !method.eq_ignore_ascii_case(implied)).then_some(method)
}

/// A command, and whether a part of the request had to be left out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Command {
    pub text: String,
    /// cmd only: headers, a method or a body containing `!`.
    pub left_out: bool,
}

pub(crate) fn command(request: &Request, shell: Dialect) -> Command {
    match shell {
        Dialect::Bash => Command {
            text: posix_like(request, bash, " \\\n  "),
            left_out: false,
        },
        Dialect::Cmd => {
            let (safe, left_out) = without_bang(request);
            Command {
                text: posix_like(&safe, cmd, " ^\n  "),
                left_out,
            }
        }
        Dialect::PowerShell => Command {
            text: powershell(request),
            left_out: false,
        },
    }
}

/// The request with no `!` anywhere: delayed expansion is then harmless.
fn without_bang(request: &Request) -> (Request, bool) {
    let mut left_out = false;
    let mut safe = Request {
        url: request.url.replace('!', "%21"),
        body_omitted: request.body_omitted,
        ..Default::default()
    };
    safe.method = if request.method.contains('!') {
        left_out = true;
        String::new()
    } else {
        request.method.clone()
    };
    for (name, value) in &request.headers {
        if name.contains('!') || value.contains('!') {
            left_out = true;
        } else {
            safe.headers.push((name.clone(), value.clone()));
        }
    }
    safe.body = match &request.body {
        Some(body) if body.contains('!') => {
            left_out = true;
            None
        }
        body => body.clone(),
    };
    (safe, left_out)
}

fn posix_like(request: &Request, quote: fn(&str) -> String, newline: &str) -> String {
    let mut parts = vec![format!("curl --url {}", quote(&request.url))];
    if let Some(method) = method(request) {
        parts.push(format!("-X {}", quote(method)));
    }
    for (name, value) in headers(request) {
        parts.push(format!("-H {}", quote(&format!("{name}: {value}"))));
    }
    if let Some(body) = &request.body {
        parts.push(format!("--data-raw {}", quote(body)));
    }
    parts.join(newline)
}

/// bash: single quotes are fully literal; a quote is closed, escaped, reopened.
pub(crate) fn bash(value: &str) -> String {
    format!("'{}'", value.replace('\0', "").replace('\'', r"'\''"))
}

/// cmd.exe + curl.exe: MSVCRT argument quoting first (backslashes doubled
/// only before a quote), then every character cmd could interpret is
/// escaped with `^` inside `^"…^"`, `%` cannot start a variable and a line
/// break continues the argument.
pub(crate) fn cmd(value: &str) -> String {
    let value = value
        .replace('\0', "")
        .replace("\r\n", "\n")
        .replace('\r', "");
    let mut argv = String::new();
    let mut backslashes = 0usize;
    for c in value.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                argv.push_str(&"\\".repeat(backslashes * 2 + 1));
                argv.push('"');
                backslashes = 0;
            }
            c => {
                argv.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                argv.push(c);
            }
        }
    }
    argv.push_str(&"\\".repeat(backslashes * 2));
    let safe = |c: char| c.is_ascii_alphanumeric() || " _-:=+~'/.,?;()*`\t".contains(c);
    let mut out = String::from("^\"");
    let mut chars = argv.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => out.push_str("^\n\n"),
            '%' => {
                out.push_str("^%");
                // A caret after every `%` that something follows: any
                // `%name%` cmd could read then starts with `^`.
                if chars.peek().is_some() {
                    out.push('^');
                }
            }
            c if safe(c) || !c.is_ascii() => out.push(c),
            c => {
                out.push('^');
                out.push(c);
            }
        }
    }
    out.push_str("^\"");
    out
}

/// PowerShell single-quoted strings are literal except for quote characters,
/// including the typographic ones PowerShell also accepts; each is doubled.
pub(crate) fn powershell_quote(value: &str) -> String {
    let mut out = String::from("'");
    for c in value.chars() {
        match c {
            '\0' => {}
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => {
                out.push(c);
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// `Invoke-WebRequest` takes every value as a PowerShell string, so nothing
/// passes through native argument parsing. User-Agent and Content-Type have
/// their own parameters in Windows PowerShell 5.1.
fn powershell(request: &Request) -> String {
    let q = powershell_quote;
    let mut lines = vec![format!(
        "Invoke-WebRequest -UseBasicParsing -Uri {} -Method {}",
        q(&request.url),
        q(if token(&request.method) {
            &request.method
        } else {
            "GET"
        })
    )];
    let mut table = Vec::new();
    for (name, value) in headers(request) {
        if name.eq_ignore_ascii_case("user-agent") {
            lines.push(format!("-UserAgent {}", q(&value)));
        } else if name.eq_ignore_ascii_case("content-type") {
            lines.push(format!("-ContentType {}", q(&value)));
        } else if !name.eq_ignore_ascii_case("host") && !name.eq_ignore_ascii_case("connection") {
            table.push(format!("  {} = {}", q(name), q(&value)));
        }
    }
    if !table.is_empty() {
        lines.push(format!("-Headers @{{\n{}\n}}", table.join("\n")));
    }
    if let Some(body) = &request.body {
        lines.push(format!("-Body {}", q(body)));
    }
    lines.join(" `\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    const HOSTILE: [&str; 8] = [
        "it's",
        "$(touch /tmp/okilum-pwned) `id` $HOME !! \\",
        "a\"b\\\"c\\",
        "100% %PATH% ^&|<>()",
        "line1\nline2\r\nline3",
        "‘curly’ ‚low‛",
        "-o /tmp/x",
        "nul\0byte",
    ];

    fn request(value: &str) -> Request {
        Request {
            method: "PUT".into(),
            url: format!("https://e.test/?q={value}"),
            headers: vec![
                (":authority".into(), "e.test".into()),
                ("x-token".into(), value.into()),
                ("@/etc/passwd".into(), "read me".into()),
                ("Content-Length".into(), "3".into()),
                ("x-inject".into(), "a\r\nEvil: 1".into()),
            ],
            body: Some(value.into()),
            body_omitted: false,
        }
    }

    /// bash quoting round-trips through a real bash for every hostile value.
    #[cfg(unix)]
    #[test]
    fn bash_arguments_round_trip_through_bash() {
        for value in HOSTILE {
            let script = format!("printf '%s\\0' {}", bash(value));
            let output = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .output()
                .unwrap();
            assert!(output.status.success());
            let expected = format!("{}\0", value.replace('\0', ""));
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                expected,
                "{value:?}"
            );
        }
        // The whole command parses into exactly the intended argv.
        let command = command(&request(HOSTILE[1]), Dialect::Bash).text;
        let script = format!(
            "set -- {}; printf '%s\\0' \"$@\"",
            &command["curl ".len()..]
        );
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();
        let argv: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_owned)
            .collect();
        assert_eq!(
            argv,
            [
                "--url".to_owned(),
                format!("https://e.test/?q={}", HOSTILE[1]),
                "-X".into(),
                "PUT".into(),
                "-H".into(),
                format!("x-token: {}", HOSTILE[1]),
                "-H".into(),
                "x-inject: a  Evil: 1".into(),
                "--data-raw".into(),
                HOSTILE[1].into(),
            ]
        );
    }

    #[test]
    fn unsafe_headers_and_options_never_reach_curl() {
        for shell in Dialect::ALL {
            let text = command(&request("-o /tmp/x"), shell).text;
            assert!(!text.contains("authority"), "{shell:?}: pseudo-header");
            assert!(!text.contains("passwd"), "{shell:?}: @file header");
            assert!(!text.contains("Content-Length"), "{shell:?}");
            assert!(!text.contains("\nEvil"), "{shell:?}: header injection");
        }
        let bash = command(&request("-o /tmp/x"), Dialect::Bash).text;
        assert!(bash.starts_with("curl --url 'https://e.test/?q=-o /tmp/x'"));
    }

    #[test]
    fn cmd_escapes_metacharacters_and_msvcrt_quotes() {
        assert_eq!(cmd("plain value"), "^\"plain value^\"");
        // Quotes become \" for curl's argv parser, then ^-escaped for cmd.
        assert_eq!(cmd("a\"b"), "^\"a^\\^\"b^\"");
        // Backslashes are doubled only before a quote or at the end.
        assert_eq!(cmd("a\\b"), "^\"a^\\b^\"");
        assert_eq!(cmd("a\\"), "^\"a^\\^\\^\"");
        assert_eq!(cmd("50%"), "^\"50^%^\"");
        assert_eq!(cmd("%PATH%"), "^\"^%^PATH^%^\"");
        assert_eq!(cmd("a&b|c<d>e^f!g"), "^\"a^&b^|c^<d^>e^^f^!g^\"");
        assert_eq!(cmd("%%x"), "^\"^%^^%^x^\"");
        assert_eq!(cmd("%-x%"), "^\"^%^-x^%^\"");
        assert_eq!(cmd("a\r\nb"), "^\"a^\n\nb^\"");
        for value in HOSTILE {
            let quoted = cmd(value);
            // No unescaped cmd metacharacter remains.
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                if c == '^' {
                    chars.next();
                    continue;
                }
                assert!(!"&|<>\"%!".contains(c), "{value:?} -> {quoted}");
            }
        }
    }

    #[test]
    fn powershell_strings_are_literal() {
        assert_eq!(powershell_quote("it's $x `n"), "'it''s $x `n'");
        assert_eq!(powershell_quote("‘a’"), "'‘‘a’’'");
        let text = command(&request("it's"), Dialect::PowerShell).text;
        assert!(text.starts_with(
            "Invoke-WebRequest -UseBasicParsing -Uri 'https://e.test/?q=it''s' -Method 'PUT'"
        ));
        assert!(text.contains("'x-token' = 'it''s'"));
        assert!(text.contains("-Body 'it''s'"));
        // Every single-quoted run is balanced (no value closes its string).
        for value in HOSTILE {
            let quoted = powershell_quote(value);
            let inner = &quoted[1..quoted.len() - 1];
            let quotes = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];
            let mut chars = inner.chars().peekable();
            while let Some(c) = chars.next() {
                if quotes.contains(&c) {
                    assert_eq!(chars.next(), Some(c), "{value:?}");
                }
            }
        }
    }

    #[test]
    fn method_is_implied_when_curl_would_choose_it() {
        let mut get = Request {
            method: "GET".into(),
            url: "https://e.test/".into(),
            ..Default::default()
        };
        assert_eq!(
            command(&get, Dialect::Bash).text,
            "curl --url 'https://e.test/'"
        );
        get.method = "POST".into();
        get.body = Some("x".into());
        assert_eq!(
            command(&get, Dialect::Bash).text,
            "curl --url 'https://e.test/' \\\n  --data-raw 'x'"
        );
    }

    /// Review of PR 2904 (P1-1): cmd output holds no `!` in any state of
    /// delayed expansion, and every `%name%` cmd could read starts with `^`.
    #[test]
    fn review2904_cmd_is_safe_with_delayed_expansion_on_and_off() {
        let request = Request {
            method: "POST".into(),
            url: "https://attacker.invalid/?key=!AWS_SECRET_ACCESS_KEY!&p=%PATH%".into(),
            headers: vec![
                ("x-ok".into(), "100%".into()),
                ("x-bang".into(), "hi!".into()),
            ],
            body: Some("pass!word".into()),
            body_omitted: false,
        };
        let command = command(&request, Dialect::Cmd);
        assert!(!command.text.contains('!'), "{}", command.text);
        assert!(command.left_out, "the header and body with ! are left out");
        assert!(command.text.contains("key=^%^21AWS_SECRET_ACCESS_KEY^%^21"));
        assert!(!command.text.contains("hi"));
        assert!(!command.text.contains("pass"));
        // Phase-1 % expansion: between any two % the name starts with ^.
        let text = &command.text;
        let percents: Vec<usize> = text.match_indices('%').map(|(i, _)| i).collect();
        for pair in percents.windows(2) {
            let name = &text[pair[0] + 1..pair[1]];
            assert!(name.starts_with('^'), "%{name}% could expand in {text}");
        }
        // Control: bash keeps everything.
        let bash = super::command(&request, Dialect::Bash);
        assert!(bash.text.contains("!AWS_SECRET_ACCESS_KEY!") && !bash.left_out);
    }
}
