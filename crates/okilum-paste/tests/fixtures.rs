//! Clipboard HTML from real apps → exact Markdown.
//!
//! Each `tests/fixtures/<name>.html` has its expected `<name>.md` next to
//! it. `OKILUM_PASTE_BLESS=1 cargo test -p okilum-paste --test fixtures`
//! rewrites the `.md` files (review the diff before committing).

use std::path::PathBuf;

use okilum_paste::{html_to_markdown, AssetSource, Converted, HtmlInput};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn convert(name: &str, base_url: Option<&str>) -> Converted {
    let html = std::fs::read_to_string(dir().join(format!("{name}.html"))).unwrap();
    let converted = html_to_markdown(&HtmlInput {
        html: &html,
        base_url,
    });
    let expected_path = dir().join(format!("{name}.md"));
    if std::env::var_os("OKILUM_PASTE_BLESS").is_some() {
        std::fs::write(&expected_path, format!("{}\n", converted.markdown)).unwrap();
    }
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|_| panic!("missing {name}.md; bless first"));
    assert_eq!(
        converted.markdown,
        expected.strip_suffix('\n').unwrap_or(&expected),
        "fixture {name}"
    );
    for line in converted.markdown.lines() {
        let trailing = line.len() - line.trim_end().len();
        assert!(
            trailing == 0 || (trailing == 2 && line.ends_with("  ")),
            "{name}: stray trailing whitespace in {line:?}"
        );
    }
    assert!(
        !converted.markdown.contains("\n\n\n"),
        "{name}: double blank line"
    );
    converted
}

#[test]
fn chrome_wikipedia() {
    let c = convert(
        "chrome_wikipedia",
        Some("https://en.wikipedia.org/wiki/Rust_(programming_language)"),
    );
    assert!(!c.plain && c.block);
    assert_eq!(c.assets.len(), 1);
    assert_eq!(
        c.assets[0].source,
        AssetSource::Remote(
            "https://upload.wikimedia.org/wikipedia/commons/thumb/d/d5/Rust_programming_language_black_logo.svg/120px-Rust_programming_language_black_logo.svg.png".into()
        )
    );
    assert!(c.markdown.contains(&c.assets[0].placeholder));
}

#[test]
fn safari_page() {
    let c = convert("safari_page", None);
    assert!(c.block && !c.plain);
}

#[test]
fn google_docs() {
    let c = convert("google_docs", None);
    assert!(c.block);
    assert_eq!(c.assets.len(), 1);
    assert!(matches!(c.assets[0].source, AssetSource::Remote(_)));
}

#[test]
fn word_mac() {
    let c = convert("word_mac", None);
    assert_eq!(c.assets.len(), 1);
    assert_eq!(
        c.assets[0].source,
        AssetSource::LocalFile(
            "/Users/oleg/Library/Group Containers/UBF8T346G9.Office/TemporaryItems/msohtmlclip/clip_image002.png".into()
        )
    );
    assert_eq!(
        c.assets[0].placeholder,
        "![Sales chart](okilum-paste-asset:0)"
    );
    assert_eq!(c.assets[0].alt, "Sales chart");
}

#[test]
fn notion() {
    let c = convert("notion", None);
    assert!(c.block);
    assert_eq!(c.code_language, None);
}

#[test]
fn github_readme() {
    convert("github_readme", Some("https://github.com/BeFeast/okilum"));
}

#[test]
fn chatgpt() {
    convert("chatgpt", None);
}

#[test]
fn claude() {
    convert("claude", None);
}

#[test]
fn stackoverflow() {
    convert(
        "stackoverflow",
        Some("https://stackoverflow.com/questions/1/x"),
    );
}

#[test]
fn gmail() {
    let c = convert("gmail", None);
    assert!(c.assets.is_empty(), "tracking pixel dropped");
}

#[test]
fn slack() {
    convert("slack", None);
}

#[test]
fn excel() {
    let c = convert("excel", None);
    assert!(c.block && !c.plain);
}

#[test]
fn excel_single_cell() {
    let c = convert("excel_single_cell", None);
    assert!(c.plain && !c.block);
}

#[test]
fn google_sheets() {
    convert("google_sheets", None);
}

#[test]
fn apple_textedit() {
    let c = convert("apple_textedit", None);
    assert!(!c.plain);
}

#[test]
fn plain_only() {
    let c = convert("plain_only", None);
    assert!(c.plain, "only paragraphs and line breaks");
    assert!(!c.block);
}

#[test]
fn xcode_rtf_is_one_code_block() {
    let c = convert("xcode_rtf", None);
    assert!(c.monospace);
    assert_eq!(c.code_language.as_deref(), Some("swift"));
}

#[test]
fn jetbrains_pre() {
    let c = convert("jetbrains", None);
    assert!(c.monospace);
    assert_eq!(c.code_language.as_deref(), Some("kotlin"));
}

#[test]
fn image_only() {
    let c = convert("image_only", None);
    assert!(c.is_single_image());
    assert!(!c.plain);
}

#[test]
fn data_image() {
    let c = convert("data_image", None);
    assert_eq!(c.assets.len(), 2);
    assert_eq!(
        c.assets[0].source,
        AssetSource::Data {
            mime: "image/png".into(),
            bytes: vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10]
        }
    );
    assert_eq!(
        c.assets[0].placeholder,
        "![dot \\[1\\]](okilum-paste-asset:0)"
    );
    assert!(!c.is_single_image());
}

fn md(html: &str, base_url: Option<&str>) -> String {
    html_to_markdown(&HtmlInput { html, base_url }).markdown
}

#[test]
fn inline_edge_cases() {
    // Adjacent code elements stay separate spans; delimiters fit the content.
    assert_eq!(
        md(
            "<p>x<code>a`b</code> <kbd>Ctrl</kbd> <code>``</code></p>",
            None
        ),
        "x``a`b`` `Ctrl` ``` `` ```"
    );
    // <base> resolves against the page URL; javascript: links lose the
    // link; link text equal to the href becomes a bare URL.
    assert_eq!(
        md(
            "<base href='/docs/'><a href='x.html'>X</a> <a href='javascript:alert(1)'>bad</a> <a href='https://a.org/'>https://a.org/</a>",
            Some("https://ex.com/a/b")
        ),
        "[X](https://ex.com/docs/x.html) bad https://a.org/"
    );
    // An image inside a link is just the image.
    assert_eq!(
        md(
            "<a href='https://a.org'><img src='https://a.org/i.png' alt='i'></a>",
            None
        ),
        "![i](https://a.org/i.png)"
    );
    // Only what would become formatting is escaped.
    assert_eq!(
        md("<p># not heading</p><p>1. not list</p><p>a *b* _c_ snake_case [e](f) &lt;div&gt; x|y</p>", None),
        "\\# not heading\n\n1\\. not list\n\na \\*b\\* \\_c\\_ snake_case \\[e](f) \\<div> x|y"
    );
    // Hidden and chrome elements are dropped, tracking pixels too.
    let c = html_to_markdown(&HtmlInput {
        html: "<p>a<span style='display:none'>H</span><span aria-hidden=true>H</span><script>s</script><button>B</button>b</p><img src='https://t.co/p.gif' width=1 height=1>",
        base_url: None,
    });
    assert_eq!(c.markdown, "ab");
    assert!(c.plain && c.assets.is_empty());
    // Google Docs' wrapper <b style="font-weight:normal"> is not bold.
    assert_eq!(
        md("<b style='font-weight:normal' id='docs-internal-guid-abc'><span style='font-weight:700'>B</span> <span>n</span></b>", None),
        "**B** n"
    );
    assert_eq!(
        md(
            "<h3>H <em>i</em></h3><p><mark>m</mark> <del>d</del></p>",
            None
        ),
        "### H *i*\n\n==m== ~~d~~"
    );
}
