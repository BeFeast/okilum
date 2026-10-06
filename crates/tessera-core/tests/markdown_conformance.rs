//! CommonMark 0.31.2 and GFM 0.29 spec conformance of Tessera's Markdown
//! parsing (#650).
//!
//! Two parsers read a note, and each is measured with the input Tessera
//! actually gives it:
//!
//! - **comrak**: [`render::preprocess`] then Comrak with
//!   [`render::comrak_options`]. This is the HTML reader mode and every core
//!   analysis (links, tasks, outline IR, export, source classification).
//! - **reader**: [`render::reader_document_from_source`] then markdown-rs with
//!   `ParseOptions::gfm()`, the options gpui-component's Markdown view parses
//!   with. This is the default reader. What is measured is markdown-rs's parse,
//!   compiled to HTML so it can be compared; how gpui-component draws that
//!   tree is not covered here.
//!
//! Comrak's URL resolution and syntax highlighting run after the parse and
//! rewrite URLs and code markup by design, so they are left out. The reader
//! resolves link destinations in the source, before its parse; an example
//! whose output only differs in `href`/`src` values that became `tessera://`
//! URLs counts as passing, and is reported as such.
//!
//! A failure is classified, never just counted:
//!
//! - `deviation:<knob>`: the parser passes with the spec's own options, and
//!   putting that one Tessera choice back to the spec value passes too. The
//!   choices are documented in `docs/research/650-conformance.md`.
//! - `deviation:combined`: the parser passes with the spec's own options, but
//!   no single choice explains the failure.
//! - `parser`: the parser fails with the spec's own options as well.
//! - `spec-drift`: a GFM 0.29 example whose CommonMark core changed by 0.31.2;
//!   the parser passes the 0.31.2 example with the same input.
//!
//! The failures are pinned in `fixtures/markdown-spec/known-failures.txt`. A
//! new failure, a new pass or a changed cause all fail the test, so the list
//! only moves on purpose. Run with `TESSERA_CONFORMANCE_REPORT=1` and
//! `--nocapture` to print the per-section report and a fresh list, and with
//! `TESSERA_CONFORMANCE_DEBUG=1` to print each failure's input and output.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use tessera_core::{render, Vault};

const COMMONMARK: &str = include_str!("fixtures/markdown-spec/commonmark-0.31.2.json");
const GFM: &str = include_str!("fixtures/markdown-spec/gfm-0.29.json");
const KNOWN: &str = include_str!("fixtures/markdown-spec/known-failures.txt");

#[derive(Deserialize)]
struct Example {
    markdown: String,
    html: String,
    example: u32,
    section: String,
    /// GFM only: the extensions cmark-gfm enables for this example. Its task
    /// list examples are tagged `disabled` (upstream cannot normalise the
    /// checkbox attributes); here they run with the task list extension.
    #[serde(default)]
    extensions: Vec<String>,
}

impl Example {
    fn uses(&self, extension: &str) -> bool {
        self.extensions.iter().any(|e| e == extension)
            || (extension == "tasklist" && self.extensions.iter().any(|e| e == "disabled"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Suite {
    CommonMark,
    Gfm,
}

impl Suite {
    fn name(self) -> &'static str {
        match self {
            Suite::CommonMark => "commonmark",
            Suite::Gfm => "gfm",
        }
    }

    fn examples(self) -> Vec<Example> {
        serde_json::from_str(match self {
            Suite::CommonMark => COMMONMARK,
            Suite::Gfm => GFM,
        })
        .unwrap()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    Comrak,
    Reader,
}

impl Engine {
    fn name(self) -> &'static str {
        match self {
            Engine::Comrak => "comrak",
            Engine::Reader => "reader",
        }
    }

    /// Every Tessera choice that can differ from a spec run, by name.
    fn knobs(self) -> &'static [&'static str] {
        match self {
            Engine::Comrak => &[
                "embeds",
                "front_matter",
                "wikilinks",
                "footnotes",
                "alerts",
                "strikethrough",
                "table",
                "tasklist",
                "autolink",
                "tagfilter",
            ],
            Engine::Reader => &[
                "front_matter",
                "embeds",
                "source_links",
                "highlights",
                "footnotes",
                "strikethrough",
                "table",
                "tasklist",
                "autolink",
                "tagfilter",
            ],
        }
    }

    /// Render the way Tessera does, optionally with one knob put back to the
    /// value the spec run uses for `example`.
    fn tessera(self, example: &Example, revert: Option<&str>) -> String {
        let spec = |extension: &str| example.uses(extension);
        let reverted = |knob: &str| revert == Some(knob);
        let markdown = example.markdown.as_str();
        match self {
            Engine::Comrak => {
                let mut o = render::comrak_options();
                let e = &mut o.extension;
                if reverted("front_matter") {
                    e.front_matter_delimiter = None;
                }
                if reverted("wikilinks") {
                    e.wikilinks_title_after_pipe = false;
                }
                if reverted("footnotes") {
                    e.footnotes = false;
                }
                if reverted("alerts") {
                    e.alerts = false;
                }
                for (knob, field) in [
                    ("strikethrough", &mut e.strikethrough),
                    ("table", &mut e.table),
                    ("tasklist", &mut e.tasklist),
                    ("autolink", &mut e.autolink),
                    ("tagfilter", &mut e.tagfilter),
                ] {
                    if reverted(knob) {
                        *field = spec(knob);
                    }
                }
                let source = if reverted("embeds") {
                    markdown.to_owned()
                } else {
                    render::preprocess(markdown)
                };
                comrak(&source, &o)
            }
            Engine::Reader => {
                let source = if revert.is_none() {
                    // The real entry point, so the pipeline cannot drift from
                    // the one below without this test noticing.
                    let mut vault = Vault::from_note_paths([]);
                    vault.root = std::env::temp_dir().join("tessera-conformance-empty-vault");
                    render::reader_document_from_source(&vault, NOTE, markdown).rendered
                } else {
                    reader_source(markdown, revert)
                };
                let mut parse = markdown::ParseOptions::gfm();
                let c = &mut parse.constructs;
                for (knob, field) in [
                    ("strikethrough", &mut c.gfm_strikethrough),
                    ("table", &mut c.gfm_table),
                    ("tasklist", &mut c.gfm_task_list_item),
                    ("autolink", &mut c.gfm_autolink_literal),
                ] {
                    if reverted(knob) {
                        *field = spec(knob);
                    }
                }
                if reverted("footnotes") {
                    c.gfm_footnote_definition = false;
                    c.gfm_label_start_footnote = false;
                }
                let mut compile = dangerous();
                if reverted("tagfilter") {
                    compile.gfm_tagfilter = spec("tagfilter");
                }
                markdown::to_html_with_options(&source, &markdown::Options { parse, compile })
                    .unwrap()
            }
        }
    }

    /// Render with the options the spec itself is run with: plain CommonMark,
    /// or the extensions cmark-gfm enables for that GFM example.
    fn reference(self, suite: Suite, example: &Example) -> String {
        let gfm = |extension: &str| suite == Suite::Gfm && example.uses(extension);
        match self {
            Engine::Comrak => {
                let mut o = comrak::Options::default();
                o.render.r#unsafe = true;
                o.extension.strikethrough = gfm("strikethrough");
                o.extension.table = gfm("table");
                o.extension.tasklist = gfm("tasklist");
                o.extension.autolink = gfm("autolink");
                o.extension.tagfilter = gfm("tagfilter");
                comrak(&example.markdown, &o)
            }
            Engine::Reader => {
                let mut parse = markdown::ParseOptions::default();
                parse.constructs.gfm_strikethrough = gfm("strikethrough");
                parse.constructs.gfm_table = gfm("table");
                parse.constructs.gfm_task_list_item = gfm("tasklist");
                parse.constructs.gfm_autolink_literal = gfm("autolink");
                let mut compile = dangerous();
                compile.gfm_tagfilter = gfm("tagfilter");
                markdown::to_html_with_options(
                    &example.markdown,
                    &markdown::Options { parse, compile },
                )
                .unwrap()
            }
        }
    }
}

const NOTE: &str = "note.md";

/// The default reader's source transforms, one at a time, so a deviation can
/// be pinned on the step that causes it. Mirrors
/// `render::reader_document_from_source` for a vault with no notes, where
/// embeds have nothing to expand.
fn reader_source(markdown: &str, revert: Option<&str>) -> String {
    let vault = Vault::from_note_paths([]);
    let keep = |step: &str| revert != Some(step);
    let mut s = if keep("front_matter") {
        render::without_frontmatter(markdown).to_owned()
    } else {
        markdown.to_owned()
    };
    if keep("embeds") {
        s = render::preprocess(&s);
    }
    if keep("source_links") {
        s = render::rewrite_source_images(&s, &vault, NOTE);
        s = render::rewrite_source_links(&s, &vault, NOTE);
    }
    if keep("highlights") {
        s = render::rewrite_highlights(&s);
    }
    s
}

/// Compile options that show the parse as it is: raw HTML and every URL
/// protocol pass through, as they do into the reader's own tree.
fn dangerous() -> markdown::CompileOptions {
    markdown::CompileOptions {
        allow_dangerous_html: true,
        allow_dangerous_protocol: true,
        ..markdown::CompileOptions::default()
    }
}

fn comrak(markdown: &str, options: &comrak::Options) -> String {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, markdown, options);
    let mut out = String::new();
    comrak::format_html(root, options, &mut out).unwrap();
    out
}

/// How Tessera's output compares with the spec's.
#[derive(PartialEq, Eq)]
enum Verdict {
    Pass,
    /// Equal once link and image URLs are set aside. The reader resolves
    /// every link destination in the source into a `tessera://` URL before
    /// parsing, by design; the parse around it still has to match.
    PassResolved,
    Fail,
}

fn verdict(got: &str, expected: &str) -> Verdict {
    let (got, expected) = (normalize(got), normalize(expected));
    if got == expected {
        Verdict::Pass
    } else if got.contains("tessera://") && mask_urls(&got) == mask_urls(&expected) {
        Verdict::PassResolved
    } else {
        Verdict::Fail
    }
}

fn mask_urls(html: &str) -> String {
    // `normalize` writes an empty value as a bare attribute name.
    let url = regex::Regex::new(r#" (href|src)(="[^"]*")?"#).unwrap();
    url.replace_all(html, " $1").into_owned()
}

/// What made one example fail.
fn cause(engine: Engine, suite: Suite, example: &Example) -> String {
    if normalize(&engine.reference(suite, example)) != normalize(&example.html) {
        return if suite == Suite::Gfm && commonmark_passes(engine, &example.markdown) {
            "spec-drift".into()
        } else {
            "parser".into()
        };
    }
    let culprits: Vec<_> = engine
        .knobs()
        .iter()
        .copied()
        .filter(|knob| {
            verdict(&engine.tessera(example, Some(knob)), &example.html) != Verdict::Fail
        })
        .collect();
    if culprits.is_empty() {
        "deviation:combined".into()
    } else {
        format!("deviation:{}", culprits.join("+"))
    }
}

/// Whether the CommonMark 0.31.2 suite has an example with this input that
/// `engine` passes with plain CommonMark options.
fn commonmark_passes(engine: Engine, markdown: &str) -> bool {
    Suite::CommonMark
        .examples()
        .iter()
        .filter(|e| e.markdown == markdown)
        .any(|e| normalize(&engine.reference(Suite::CommonMark, e)) == normalize(&e.html))
}

/// HTML normalisation in the spirit of cmark's `test/normalize.py`, so a
/// difference that does not change the document (attribute order, `<br />`
/// against `<br>`, whitespace around block tags) is not a failure.
fn normalize(html: &str) -> String {
    const BLOCK: &[&str] = &[
        "article",
        "aside",
        "blockquote",
        "body",
        "dd",
        "div",
        "dl",
        "dt",
        "figure",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "html",
        "li",
        "ol",
        "p",
        "pre",
        "section",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "tr",
        "ul",
    ];
    enum Token {
        Text(String),
        Tag { name: String, text: String },
    }
    let mut tokens = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        if let Some((len, name, text)) = rest.starts_with('<').then(|| tag(rest)).flatten() {
            tokens.push(Token::Tag { name, text });
            rest = &rest[len..];
        } else {
            let next = rest
                .char_indices()
                .skip(1)
                .find(|&(_, c)| c == '<')
                .map_or(rest.len(), |(i, _)| i);
            tokens.push(Token::Text(rest[..next].to_owned()));
            rest = &rest[next..];
        }
    }
    let is_block = |name: &str| BLOCK.contains(&name.trim_start_matches('/'));
    let mut out = String::new();
    let mut in_pre = 0usize;
    let mut after_block = true;
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::Tag { name, text } => {
                if name == "pre" {
                    in_pre += 1;
                } else if name == "/pre" {
                    in_pre = in_pre.saturating_sub(1);
                }
                after_block = is_block(name) && in_pre == 0;
                if after_block {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                }
                out.push_str(text);
            }
            Token::Text(text) if in_pre > 0 => {
                out.push_str(text);
                after_block = false;
            }
            Token::Text(text) => {
                let mut collapsed = String::new();
                let mut space = false;
                for c in text.chars() {
                    if c.is_ascii_whitespace() {
                        space = true;
                        continue;
                    }
                    if space && !(collapsed.is_empty() && (after_block || out.ends_with(' '))) {
                        collapsed.push(' ');
                    }
                    collapsed.push(c);
                    space = false;
                }
                let next_is_block = matches!(
                    tokens.get(i + 1),
                    Some(Token::Tag { name, .. }) if is_block(name)
                );
                if space && !next_is_block && !(collapsed.is_empty() && after_block) {
                    collapsed.push(' ');
                }
                if !collapsed.is_empty() {
                    after_block = false;
                }
                out.push_str(&collapsed);
            }
        }
    }
    out.trim().to_owned()
}

/// Parse one tag at the start of `s`: `(byte length, lowercase name with a
/// leading '/' for an end tag, canonical text)`. Comments, declarations and
/// processing instructions come back verbatim with an empty name; anything
/// that is not a well-formed tag is left to be text.
fn tag(s: &str) -> Option<(usize, String, String)> {
    for (open, close) in [
        ("<!--", "-->"),
        ("<?", "?>"),
        ("<![CDATA[", "]]>"),
        ("<!", ">"),
    ] {
        if let Some(rest) = s.strip_prefix(open) {
            let end = rest.find(close)? + open.len() + close.len();
            return Some((end, String::new(), s[..end].to_owned()));
        }
    }
    let bytes = s.as_bytes();
    let closing = bytes.get(1) == Some(&b'/');
    let mut at = if closing { 2 } else { 1 };
    let name_start = at;
    while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'-') {
        at += 1;
    }
    if at == name_start || !bytes[name_start].is_ascii_alphabetic() {
        return None;
    }
    let name = s[name_start..at].to_ascii_lowercase();
    let mut attributes = Vec::new();
    loop {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        match bytes.get(at)? {
            b'>' => {
                at += 1;
                break;
            }
            b'/' if bytes.get(at + 1) == Some(&b'>') => {
                at += 2;
                break;
            }
            _ if closing => return None,
            _ => {}
        }
        let key_start = at;
        while at < bytes.len()
            && !bytes[at].is_ascii_whitespace()
            && !matches!(bytes[at], b'=' | b'>' | b'/' | b'"' | b'\'')
        {
            at += 1;
        }
        if at == key_start {
            return None;
        }
        let key = s[key_start..at].to_ascii_lowercase();
        let mut value = String::new();
        if bytes.get(at) == Some(&b'=') {
            at += 1;
            if let quote @ (b'"' | b'\'') = *bytes.get(at)? {
                let end = s[at + 1..].find(quote as char)? + at + 1;
                value = s[at + 1..end].to_owned();
                at = end + 1;
            } else {
                let start = at;
                while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'>' {
                    at += 1;
                }
                value = s[start..at].to_owned();
            }
        }
        attributes.push((key, value));
    }
    attributes.sort();
    let name = if closing { format!("/{name}") } else { name };
    let mut text = format!("<{name}");
    for (key, value) in attributes {
        text.push(' ');
        text.push_str(&key);
        if !value.is_empty() {
            text.push_str(&format!("=\"{value}\""));
        }
    }
    text.push('>');
    Some((at, name, text))
}

#[derive(Default)]
struct Tally {
    passed: usize,
    resolved: usize,
    total: usize,
}

#[test]
fn spec_examples_match_the_pinned_failure_list() {
    // (engine, suite, example, cause)
    let known: BTreeSet<(String, String, u32, String)> = KNOWN
        .lines()
        .map(|line| line.split('#').next().unwrap().trim())
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let [engine, suite, example, cause] = fields[..] else {
                panic!("malformed known failure: {line:?}");
            };
            (
                engine.to_owned(),
                suite.to_owned(),
                example.parse().unwrap(),
                cause.to_owned(),
            )
        })
        .collect();
    let debug = std::env::var_os("TESSERA_CONFORMANCE_DEBUG").is_some();
    let mut actual = BTreeSet::new();
    let mut report = String::new();
    for engine in [Engine::Comrak, Engine::Reader] {
        for suite in [Suite::CommonMark, Suite::Gfm] {
            let examples = suite.examples();
            let mut sections: Vec<(String, Tally)> = Vec::new();
            let mut causes: BTreeMap<String, usize> = BTreeMap::new();
            for example in &examples {
                if sections
                    .last()
                    .is_none_or(|(name, _)| *name != example.section)
                {
                    sections.push((example.section.clone(), Tally::default()));
                }
                let tally = &mut sections.last_mut().unwrap().1;
                tally.total += 1;
                let got = engine.tessera(example, None);
                match verdict(&got, &example.html) {
                    Verdict::Pass => {
                        tally.passed += 1;
                        continue;
                    }
                    Verdict::PassResolved => {
                        tally.passed += 1;
                        tally.resolved += 1;
                        continue;
                    }
                    Verdict::Fail => {}
                }
                let cause = cause(engine, suite, example);
                if debug {
                    eprintln!(
                        "=== {} {} {} {cause} [{}]\n--- markdown\n{}--- expected\n{}--- got\n{got}",
                        engine.name(),
                        suite.name(),
                        example.example,
                        example.section,
                        example.markdown,
                        example.html,
                    );
                }
                *causes.entry(cause.clone()).or_default() += 1;
                actual.insert((
                    engine.name().to_owned(),
                    suite.name().to_owned(),
                    example.example,
                    cause,
                ));
            }
            let passed: usize = sections.iter().map(|(_, t)| t.passed).sum();
            let resolved: usize = sections.iter().map(|(_, t)| t.resolved).sum();
            report.push_str(&format!(
                "\n### {} — {}: {passed}/{} ({:.1}%), {resolved} with resolved URLs\n\n\
                 | Section | Passed | Resolved URLs | Total | Rate |\n|---|---:|---:|---:|---:|\n",
                engine.name(),
                suite.name(),
                examples.len(),
                100.0 * passed as f64 / examples.len() as f64
            ));
            for (name, tally) in &sections {
                report.push_str(&format!(
                    "| {name} | {} | {} | {} | {:.0}% |\n",
                    tally.passed,
                    tally.resolved,
                    tally.total,
                    100.0 * tally.passed as f64 / tally.total as f64
                ));
            }
            report.push_str("\nFailures by cause:\n\n");
            for (cause, count) in &causes {
                report.push_str(&format!("- `{cause}`: {count}\n"));
            }
        }
    }
    if std::env::var_os("TESSERA_CONFORMANCE_REPORT").is_some() {
        println!("{report}");
        println!("--- known-failures.txt ---");
        for (engine, suite, example, cause) in &actual {
            println!("{engine} {suite} {example} {cause}");
        }
    }
    let new: Vec<_> = actual.difference(&known).collect();
    let fixed: Vec<_> = known.difference(&actual).collect();
    assert!(
        new.is_empty() && fixed.is_empty(),
        "spec conformance changed.\nnew or reclassified failures: {new:#?}\n\
         now passing or reclassified: {fixed:#?}\n\
         Update fixtures/markdown-spec/known-failures.txt and docs/research/650-conformance.md."
    );
}

/// The comparison must still see real differences, or every example would
/// pass (AGENTS.md: a probe that reports an absence needs a positive control).
#[test]
fn normalization_ignores_form_but_not_content() {
    for (a, b) in [
        ("<p>a  <em>b</em>\n</p>\n<br />", "<p>a <em>b</em></p><br>"),
        (
            "<input type=\"checkbox\" disabled=\"\">",
            "<input disabled type=\"checkbox\" />",
        ),
        ("<ul>\n<li>a</li>\n</ul>\n", "<ul><li>a</li></ul>"),
    ] {
        assert_eq!(normalize(a), normalize(b), "{a:?} vs {b:?}");
    }
    for (a, b) in [
        ("<p>a</p>", "<p>b</p>"),
        ("<p>a b</p>", "<p>ab</p>"),
        ("<p><em>a</em> b</p>", "<p><em>a</em>b</p>"),
        ("<pre><code>a\n</code></pre>", "<pre><code>a</code></pre>"),
        (
            "<pre><code>a  b</code></pre>",
            "<pre><code>a b</code></pre>",
        ),
        ("<h1>a</h1>", "<h2>a</h2>"),
        ("<a href=\"x\">a</a>", "<a href=\"y\">a</a>"),
        ("<p>&lt;a&gt;</p>", "<p><a></p>"),
    ] {
        assert_ne!(normalize(a), normalize(b), "{a:?} vs {b:?}");
    }
}

/// The harness must be able to fail: a renderer that differs from the spec
/// on purpose is detected on the examples it touches, and named.
#[test]
fn harness_detects_a_known_deviation() {
    let examples = Suite::CommonMark.examples();
    let bare_url = examples
        .iter()
        .find(|e| e.markdown == "https://example.com\n")
        .expect("CommonMark keeps its bare-URL example");
    for engine in [Engine::Comrak, Engine::Reader] {
        assert_eq!(
            normalize(&engine.reference(Suite::CommonMark, bare_url)),
            normalize(&bare_url.html)
        );
        assert_ne!(
            normalize(&engine.tessera(bare_url, None)),
            normalize(&bare_url.html),
            "Tessera autolinks bare URLs, so this example must fail"
        );
        assert_eq!(
            cause(engine, Suite::CommonMark, bare_url),
            "deviation:autolink"
        );
    }
}
