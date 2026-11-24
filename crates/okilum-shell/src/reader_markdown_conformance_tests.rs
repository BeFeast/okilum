//! Rendered-model conformance of the Reader (#3259): CommonMark 0.31.2, GFM
//! 0.29 and the Obsidian syntax Okilum claims, measured on what the Reader
//! draws rather than on what a parser produced.
//!
//! Each input goes through the Reader's own path: `reader_document_from_source`
//! (core source transforms), then a `TextView` with [`markdown_plugins`] (the
//! Markdown extensions every Reader surface installs), parsed into the node
//! tree gpui-component renders and written out by `render_model_html`. The
//! Reader's task-query and agent-import parsers are Okilum features, not
//! Markdown syntax, and are left out.
//!
//! The comparison is semantic. Both sides are reduced to the same canonical
//! form: block tags with the attributes that change rendering, inline
//! formatting flattened to the set of marks on each stretch of text, entities
//! decoded and HTML whitespace collapsed. A difference that only moves a link
//! or image destination into an `okilum://` URL counts as passing, as in
//! `okilum-core/tests/markdown_conformance.rs` (which measures the parser
//! alone).
//!
//! This is a ratchet: the pass counts and the Obsidian gap count are pinned
//! below. A change that lowers a pass count, or raises it, fails until the pin
//! is moved on purpose. `OKILUM_CONFORMANCE_REPORT=1` prints the per-section
//! report and every failure with its canonical forms.

use super::*;
use ::core::prelude::v1::test;
use std::collections::BTreeMap;

/// Pinned results; see the module docs. The gap matrix behind these numbers
/// is `Dev/Areas/okilum/specs/2026-11-24-markdown-conformance.md`.
const PINNED_COMMONMARK_PASS: usize = 614;
const PINNED_GFM_PASS: usize = 633;
const PINNED_OBSIDIAN_GAPS: usize = 0;
const PINNED_EXTRA_GAPS: usize = 0;

const COMMONMARK: &str =
    include_str!("../../okilum-core/tests/fixtures/markdown-spec/commonmark-0.31.2.json");
const GFM: &str = include_str!("../../okilum-core/tests/fixtures/markdown-spec/gfm-0.29.json");

#[derive(serde::Deserialize)]
struct Example {
    markdown: String,
    html: String,
    example: u32,
    section: String,
}

fn reader_model(markdown: &str) -> String {
    let mut vault = okilum_core::Vault::from_note_paths([]);
    vault.root = std::env::temp_dir().join("okilum-render-conformance-empty-vault");
    let rendered =
        okilum_core::render::reader_document_from_source(&vault, "note.md", markdown).rendered;
    markdown_plugins(
        TextView::markdown("conformance", rendered),
        Arc::new(|_, _, _, _| {}),
        Arc::new(|_| None),
        SelectionFormat::default(),
    )
    .render_model_html()
    .unwrap_or_else(|| "<x-parse-error></x-parse-error>".into())
}

const BLOCK_TAGS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "ul",
    "ol",
    "li",
    "pre",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "hr",
];

/// Inline formatting that is flattened into marks; `code` only outside `pre`.
fn inline_mark(name: &str) -> Option<&'static str> {
    Some(match name {
        "em" | "i" => "em",
        "strong" | "b" => "strong",
        "del" | "s" | "strike" => "del",
        "code" => "code",
        "mark" => "mark",
        "u" => "u",
        "a" => "a",
        _ => return None,
    })
}

fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let window = rest.char_indices().nth(12).map_or(rest.len(), |(at, _)| at);
        let Some(end) = rest[..window].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "nbsp" => Some('\u{a0}'),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `(name, attributes)` of a tag; the name is lowercase, `/name` for a closer.
fn parse_tag(tag: &str) -> (String, BTreeMap<String, String>) {
    let inner = tag
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim_end_matches('/');
    let name_end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    let mut attrs = BTreeMap::new();
    let attr = regex::Regex::new(
        r#"([A-Za-z_:][-A-Za-z0-9_:.]*)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?"#,
    )
    .unwrap();
    for cap in attr.captures_iter(&inner[name_end..]) {
        let value = cap
            .get(2)
            .or(cap.get(3))
            .or(cap.get(4))
            .map_or("", |m| m.as_str());
        attrs.insert(cap[1].to_ascii_lowercase(), decode(value));
    }
    (name, attrs)
}

/// A link or image destination, percent-decoded (`%5C` and `\\` are the same
/// link), with the canonical form's own separators escaped.
fn link_value(url: &str) -> String {
    let bytes = url.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(byte) = url
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                decoded.push(byte);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded)
        .replace('%', "%25")
        .replace(',', "%2C")
        .replace('}', "%7D")
        .replace('"', "%22")
}

/// The canonical form described in the module docs.
fn canonical(html: &str) -> String {
    // `<!-->` and `<!--->` are complete comments (CommonMark 0.31); a tag
    // left open at the end of the input is dropped by browsers.
    let token = regex::Regex::new(
        r"(?s)<!--->|<!-->|<!--.*?-->|<[/!?A-Za-z][^>]*>|<[A-Za-z][^>]*$|[^<]+|<",
    )
    .unwrap();
    let mut hidden: Option<String> = None;
    let mut out = String::new();
    let mut marks: Vec<(String, String)> = Vec::new();
    let mut shown_marks = String::new();
    let mut in_pre = 0usize;
    let mut pending_space = false;
    let push_text = |out: &mut String,
                     shown: &mut String,
                     marks: &[(String, String)],
                     pending: &mut bool,
                     text: &str| {
        let mut current: Vec<String> = marks
            .iter()
            .map(|(m, a)| {
                if a.is_empty() {
                    m.clone()
                } else {
                    format!("{m}={a}")
                }
            })
            .collect();
        current.sort();
        current.dedup();
        let label = current.join(",");
        if *pending && !out.is_empty() && !out.ends_with('>') && !out.ends_with('⏎') {
            out.push(' ');
        }
        *pending = false;
        if label != *shown {
            out.push_str(&format!("{{{label}}}"));
            *shown = label;
        }
        out.push_str(text);
    };
    for m in token.find_iter(html) {
        let raw = m.as_str();
        // Browsers show no `script`/`style` content.
        if let Some(name) = &hidden {
            if raw.to_ascii_lowercase().starts_with(&format!("</{name}")) {
                hidden = None;
            }
            continue;
        }
        if raw.starts_with('<') && raw.len() > 1 && !raw.ends_with('>') {
            continue;
        }
        if raw.starts_with("<!--") || (raw.starts_with('<') && raw.len() > 1) {
            // Comments, declarations and processing instructions are invisible.
            if raw.starts_with("<!--") || raw.starts_with("<!") || raw.starts_with("<?") {
                continue;
            }
            let (name, attrs) = parse_tag(raw);
            let bare = name.trim_start_matches('/');
            let closing = name.starts_with('/');
            if !closing && matches!(bare, "script" | "style") {
                hidden = Some(bare.to_owned());
                continue;
            }
            if bare == "br" {
                pending_space = false;
                out.push('⏎');
                continue;
            }
            if let Some(mark) = inline_mark(bare).filter(|_| !(bare == "code" && in_pre > 0)) {
                if closing {
                    if let Some(at) = marks.iter().rposition(|(m, _)| m == mark) {
                        marks.remove(at);
                    }
                } else {
                    let value = match mark {
                        "a" => link_value(attrs.get("href").map_or("", String::as_str)),
                        _ => String::new(),
                    };
                    marks.push((mark.to_owned(), value));
                }
                continue;
            }
            let keep: &[&str] = match bare {
                "ol" => &["start"],
                "th" | "td" => &["align"],
                "code" => &["class"],
                "img" => &["src", "alt", "title"],
                "input" => &["type", "checked"],
                _ if BLOCK_TAGS.contains(&bare) || bare.starts_with("x-") => &[],
                // Other raw HTML: a viewer sees its content, not the tag.
                _ => continue,
            };
            if in_pre > 0 && closing && (bare == "code" || bare == "pre") && out.ends_with('\n') {
                out.pop();
            }
            if bare == "pre" {
                if closing {
                    in_pre = in_pre.saturating_sub(1);
                } else {
                    in_pre += 1;
                }
            }
            if BLOCK_TAGS.contains(&bare) || bare.starts_with("x-") {
                pending_space = false;
                while out.ends_with(' ') {
                    out.pop();
                }
                shown_marks.clear();
            }
            out.push_str(&format!("<{name}"));
            for key in keep {
                let value = match (bare, *key) {
                    ("img", "alt") => Some(attrs.get("alt").cloned().unwrap_or_default()),
                    ("img", "src") => attrs.get("src").map(|v| link_value(v)),
                    _ => attrs.get(*key).cloned(),
                };
                if let Some(v) = value {
                    out.push_str(&format!(" {key}=\"{v}\""));
                }
            }
            out.push('>');
            if bare == "img" && !marks.is_empty() {
                // An image inside a link: keep the link it sits in.
                push_text(&mut out, &mut shown_marks, &marks, &mut pending_space, "");
            }
            continue;
        }
        let text = decode(raw);
        if in_pre > 0 {
            out.push_str(&text);
            continue;
        }
        // HTML whitespace (not U+00A0) collapses to one space between words
        // and disappears next to block boundaries and line breaks.
        let mut words = text.split([' ', '\t', '\n', '\r', '\x0c']).peekable();
        if text.starts_with([' ', '\t', '\n', '\r', '\x0c']) {
            pending_space = true;
        }
        while let Some(word) = words.next() {
            if !word.is_empty() {
                push_text(&mut out, &mut shown_marks, &marks, &mut pending_space, word);
            }
            if words.peek().is_some() {
                pending_space = true;
            }
        }
    }
    out.trim().to_owned()
}

fn mask_urls(canonical: &str) -> String {
    let url = regex::Regex::new(r#"(src|href)="[^"]*"|\{([^}]*)\}"#).unwrap();
    let link = regex::Regex::new(r"a=[^,}]*").unwrap();
    url.replace_all(canonical, |c: &regex::Captures| match c.get(1) {
        Some(attr) => format!("{}=\"\"", attr.as_str()),
        None => format!("{{{}}}", link.replace_all(&c[2], "a")),
    })
    .into_owned()
}

#[derive(PartialEq, Eq)]
enum Verdict {
    Pass,
    PassResolved,
    Fail,
}

fn verdict(got: &str, expected: &str) -> Verdict {
    if got == expected {
        Verdict::Pass
    } else if got.contains("okilum") && mask_urls(got) == mask_urls(expected) {
        Verdict::PassResolved
    } else {
        Verdict::Fail
    }
}

/// A coarse, automatic label for one failure, so the report groups them.
fn failure_class(got: &str, expected: &str, expected_html: &str) -> &'static str {
    let raw_html = regex::Regex::new(r"<(/?)([a-z][a-z0-9-]*)").unwrap();
    let known = |name: &str| {
        BLOCK_TAGS.contains(&name)
            || matches!(name, "code" | "img" | "input" | "br")
            || name.starts_with("x-")
    };
    if raw_html
        .captures_iter(expected_html)
        .any(|c| !known(&c[2]) && !matches!(&c[2], "em" | "strong" | "del" | "a" | "p"))
        || expected_html.contains("<!--")
    {
        return "raw-html";
    }
    let without_start = regex::Regex::new(r#" start="\d+""#).unwrap();
    if without_start.replace_all(expected, "") == without_start.replace_all(got, "") {
        return "ordered-list-start";
    }
    let without_p = |s: &str| s.replace("<p>", "").replace("</p>", "");
    if without_p(expected) == without_p(got) {
        return "list-tightness";
    }
    if mask_urls(expected) == mask_urls(got) {
        return "link-destination";
    }
    let text = |s: &str| {
        regex::Regex::new(r"<[^>]*>|\{[^}]*\}|⏎")
            .unwrap()
            .replace_all(s, "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    if text(expected) == text(got) {
        return "inline-marks-or-structure";
    }
    "text-differs"
}

struct SuiteResult {
    total: usize,
    pass: usize,
    resolved: usize,
    sections: BTreeMap<String, (usize, usize)>,
    classes: BTreeMap<&'static str, Vec<u32>>,
}

fn run_suite(name: &str, json: &str, report: bool) -> SuiteResult {
    let examples: Vec<Example> = serde_json::from_str(json).unwrap();
    let mut result = SuiteResult {
        total: examples.len(),
        pass: 0,
        resolved: 0,
        sections: BTreeMap::new(),
        classes: BTreeMap::new(),
    };
    for example in &examples {
        let got = canonical(&reader_model(&example.markdown));
        let expected = canonical(&example.html);
        let section = result.sections.entry(example.section.clone()).or_default();
        section.1 += 1;
        match verdict(&got, &expected) {
            Verdict::Pass => {
                result.pass += 1;
                section.0 += 1;
            }
            Verdict::PassResolved => {
                result.pass += 1;
                result.resolved += 1;
                section.0 += 1;
            }
            Verdict::Fail => {
                let class = failure_class(&got, &expected, &example.html);
                result
                    .classes
                    .entry(class)
                    .or_default()
                    .push(example.example);
                if report {
                    println!(
                        "FAIL {name} {} [{}] {class}\n  md:       {:?}\n  expected: {expected}\n  got:      {got}",
                        example.example, example.section, example.markdown
                    );
                }
            }
        }
    }
    if report {
        println!(
            "\n== {name}: {}/{} pass ({} after link resolution)",
            result.pass, result.total, result.resolved
        );
        for (section, (pass, total)) in &result.sections {
            println!("  {section}: {pass}/{total}");
        }
        for (class, examples) in &result.classes {
            println!("  class {class}: {} {:?}", examples.len(), examples);
        }
    }
    result
}

/// Obsidian-flavoured syntax Okilum claims: input, and what the Reader must
/// show for it, checked on the canonical rendered model.
struct Probe {
    /// `true` for Obsidian syntax; `false` for CommonMark/HTML a note
    /// commonly uses, probed here because the spec suites under-weight it.
    obsidian: bool,
    name: &'static str,
    markdown: &'static str,
    expect: fn(&str) -> bool,
}

const PROBES: &[Probe] = &[
    Probe {
        obsidian: true,
        name: "wikilink",
        markdown: "See [[Some Note]] here",
        expect: |m| m.contains("{a=") && m.contains("Some Note") && !m.contains("[["),
    },
    Probe {
        obsidian: true,
        name: "wikilink alias",
        markdown: "See [[Some Note|the alias]] here",
        expect: |m| m.contains("}the alias") && m.contains("{a=") && !m.contains('|'),
    },
    Probe {
        obsidian: true,
        name: "wikilink heading",
        markdown: "See [[Some Note#Part]] here",
        expect: |m| m.contains("{a=") && !m.contains("[["),
    },
    Probe {
        obsidian: true,
        name: "note embed",
        markdown: "![[Some Note]]",
        expect: |m| m.contains("<x-embed>") || m.contains("<x-local-image>"),
    },
    Probe {
        obsidian: true,
        name: "image embed",
        markdown: "![[picture.png]]",
        expect: |m| m.contains("<img") || m.contains("<x-local-image>") || m.contains("<x-embed>"),
    },
    Probe {
        obsidian: true,
        name: "callout",
        markdown: "> [!note] Title\n> Body text",
        expect: |m| m.contains("<x-callout>") && m.contains("Body text") && !m.contains("[!note]"),
    },
    Probe {
        obsidian: true,
        name: "foldable callout",
        markdown: "> [!tip]- Folded\n> Hidden body",
        expect: |m| m.contains("<x-callout>") && !m.contains("[!tip]"),
    },
    Probe {
        obsidian: true,
        name: "nested callout",
        markdown: "> [!note] Outer\n> > [!warning] Inner\n> > Deep",
        expect: |m| {
            m.matches("<x-callout>").count() >= 2
                || (m.contains("<x-callout>") && !m.contains("[!warning]"))
        },
    },
    Probe {
        obsidian: true,
        name: "highlight",
        markdown: "Some ==marked== text",
        expect: |m| m.contains("{mark}marked") && !m.contains("=="),
    },
    Probe {
        obsidian: true,
        name: "highlight inside bold",
        markdown: "**bold ==both== bold**",
        expect: |m| m.contains("{mark,strong}both") && !m.contains("=="),
    },
    Probe {
        obsidian: true,
        name: "highlight in list item",
        markdown: "- a ==marked== item",
        expect: |m| m.contains("{mark}marked") && !m.contains("=="),
    },
    Probe {
        obsidian: true,
        name: "highlight in table cell",
        markdown: "| a |\n| - |\n| ==marked== |",
        expect: |m| m.contains("{mark}marked") && !m.contains("=="),
    },
    Probe {
        obsidian: true,
        name: "highlight in callout",
        markdown: "> [!note] Title\n> a ==marked== line",
        expect: |m| m.contains("{mark}marked") && !m.contains("=="),
    },
    Probe {
        obsidian: true,
        name: "inline comment",
        markdown: "Visible %%hidden%% text",
        expect: |m| m.contains("Visible") && !m.contains("hidden") && !m.contains("%%"),
    },
    Probe {
        obsidian: true,
        name: "block comment",
        markdown: "Before\n\n%%\nhidden block\n%%\n\nAfter",
        expect: |m| m.contains("After") && !m.contains("hidden") && !m.contains("%%"),
    },
    Probe {
        obsidian: true,
        name: "inline math",
        markdown: "Energy $E=mc^2$ here",
        expect: |m| !m.contains('$') && m.contains("Energy"),
    },
    Probe {
        obsidian: true,
        name: "block math",
        markdown: "$$\n\\frac{a}{b}\n$$",
        expect: |m| m.contains("<x-math>"),
    },
    Probe {
        obsidian: true,
        name: "frontmatter",
        markdown: "---\ntitle: Hidden\ntags: [a]\n---\n\nBody",
        expect: |m| m.contains("Body") && !m.contains("title:"),
    },
    Probe {
        obsidian: true,
        name: "footnote",
        markdown: "Text[^1]\n\n[^1]: The note.",
        expect: |m| m.contains("The note.") && !m.contains("[^1]:"),
    },
    Probe {
        obsidian: true,
        name: "inline footnote",
        markdown: "Text^[inline note] end",
        expect: |m| !m.contains("^[") && m.contains("inline note"),
    },
    Probe {
        obsidian: true,
        name: "tag",
        markdown: "A #project/tag here",
        expect: |m| m.contains("#project/tag"),
    },
    Probe {
        obsidian: true,
        name: "block id",
        markdown: "A paragraph ^block-1",
        expect: |m| m.contains("A paragraph") && !m.contains("^block-1"),
    },
    Probe {
        obsidian: false,
        name: "task list",
        markdown: "- [x] done\n- [ ] open",
        expect: |m| m.contains("checked=\"\"") && m.matches("type=\"checkbox\"").count() == 2,
    },
    Probe {
        obsidian: true,
        name: "mermaid",
        markdown: "```mermaid\ngraph TD; A-->B\n```",
        expect: |m| m.contains("<x-mermaid>"),
    },
    Probe {
        obsidian: true,
        name: "wikilink in table",
        markdown: "| a | b |\n| - | - |\n| [[Note\\|Alias]] | x |",
        expect: |m| m.contains("Alias") && m.matches("<td>").count() == 2,
    },
    Probe {
        obsidian: false,
        name: "ordered list start",
        markdown: "3. three\n4. four",
        expect: |m| m.contains("start=\"3\""),
    },
    Probe {
        obsidian: false,
        name: "loose list between items",
        markdown: "- a\n\n- b",
        expect: |m| m.contains("<li><p>"),
    },
    Probe {
        obsidian: false,
        name: "inline html span",
        markdown: "a <span style=\"color:red\">red</span> b",
        expect: |m| m.contains("red") && !m.contains("<span"),
    },
    Probe {
        obsidian: false,
        name: "inline html bold",
        markdown: "a <b>bold</b> and <strong>strong</strong> b",
        expect: |m| m.contains("{strong}bold") && m.contains("{strong}strong"),
    },
    Probe {
        obsidian: false,
        name: "inline html italic and underline",
        markdown: "a <i>it</i> <u>under</u> b",
        expect: |m| m.contains("{em}it") && m.contains("{u}under"),
    },
    Probe {
        obsidian: false,
        name: "inline html kbd",
        markdown: "Press <kbd>Ctrl</kbd>+<kbd>C</kbd>",
        expect: |m| m.contains("{code}Ctrl"),
    },
    Probe {
        obsidian: false,
        name: "inline html link",
        markdown: "a <a href=\"https://example.com\">site</a> b",
        expect: |m| m.contains("{a=https://example.com}site"),
    },
    Probe {
        obsidian: false,
        name: "inline html mark",
        markdown: "a <mark>marked</mark> b",
        expect: |m| m.contains("{mark}marked"),
    },
    Probe {
        obsidian: false,
        name: "code span with line ending",
        markdown: "`code\nspan`",
        expect: |m| m.contains("{code}code span"),
    },
    Probe {
        obsidian: false,
        name: "html details",
        markdown: "<details>\n<summary>More</summary>\n\nHidden\n\n</details>",
        expect: |m| m.contains("More") && m.contains("Hidden"),
    },
    Probe {
        obsidian: false,
        name: "nbsp paragraph",
        markdown: "a\n\n&nbsp;\n\nb",
        expect: |m| m.contains("<p>\u{a0}</p>"),
    },
    Probe {
        obsidian: false,
        name: "br lines",
        markdown: "a\n\n<br />\n<br />\n\nb",
        expect: |m| m.matches('⏎').count() == 2,
    },
];

#[test]
fn reader_render_model_conformance_ratchet() {
    let report = std::env::var_os("OKILUM_CONFORMANCE_REPORT").is_some();
    let commonmark = run_suite("commonmark", COMMONMARK, report);
    let gfm = run_suite("gfm", GFM, report);
    let (mut gaps, mut extra_gaps) = (Vec::new(), Vec::new());
    for probe in PROBES {
        let model = canonical(&reader_model(probe.markdown));
        let ok = (probe.expect)(&model);
        let kind = if probe.obsidian { "obsidian" } else { "extra" };
        if report {
            println!(
                "{} {kind} {}: {:?}\n  model: {model}",
                if ok { "PASS" } else { "GAP " },
                probe.name,
                probe.markdown
            );
        }
        if !ok {
            if probe.obsidian {
                gaps.push(probe.name);
            } else {
                extra_gaps.push(probe.name);
            }
        }
    }
    let obsidian_total = PROBES.iter().filter(|p| p.obsidian).count();
    let summary = format!(
        "commonmark {}/{}, gfm {}/{}, obsidian gaps {}/{obsidian_total} {gaps:?}, \
         extra gaps {}/{} {extra_gaps:?}",
        commonmark.pass,
        commonmark.total,
        gfm.pass,
        gfm.total,
        gaps.len(),
        extra_gaps.len(),
        PROBES.len() - obsidian_total
    );
    println!("render conformance: {summary}");
    assert_eq!(
        (commonmark.pass, gfm.pass, gaps.len(), extra_gaps.len()),
        (
            PINNED_COMMONMARK_PASS,
            PINNED_GFM_PASS,
            PINNED_OBSIDIAN_GAPS,
            PINNED_EXTRA_GAPS
        ),
        "Reader render conformance moved ({summary}). Lower pass or more gaps: a \
         regression, fix it. Better: move the pins in \
         reader_markdown_conformance_tests.rs on purpose."
    );
}

#[test]
fn canonical_form_ignores_presentation_only_differences() {
    // Positive controls: equivalent HTML canonicalises the same.
    assert_eq!(
        canonical("<p><em>a <strong>b</strong></em></p>\n"),
        canonical("<p><em>a </em><strong><em>b</em></strong></p>")
    );
    assert_eq!(canonical("<p>a<br />\nb</p>"), canonical("<p>a<br>b</p>"));
    assert_eq!(canonical("<p>a &amp; b</p>"), canonical("<p>a & b</p>"));
    assert_eq!(
        canonical("<p>a</p><script>x = 1;</script>"),
        canonical("<p>a</p>")
    );
    assert_eq!(canonical("<p>a <!-- c --> b</p>"), canonical("<p>a b</p>"));
    assert_eq!(
        canonical("<p>a <!--> b --></p>"),
        canonical("<p>a b --></p>")
    );
    assert_eq!(
        canonical("<pre><code>a\n</code></pre>"),
        canonical("<pre><code>a</code></pre>")
    );
    // Negative controls: different documents do not.
    assert_ne!(canonical("<p><em>a</em></p>"), canonical("<p>a</p>"));
    assert_ne!(
        canonical("<ol start=\"3\"><li>a</li></ol>"),
        canonical("<ol><li>a</li></ol>")
    );
    assert_ne!(canonical("<p>a</p><p>b</p>"), canonical("<p>a b</p>"));
    assert_ne!(
        canonical("<pre><code>a  b</code></pre>"),
        canonical("<pre><code>a b</code></pre>")
    );
}
