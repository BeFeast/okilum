//! Release-mode timings for big clipboard HTML. Ignored by default:
//! `cargo test -p okilum-paste --release --test perf -- --ignored --nocapture`
//! Targets (#1611): 100 KB ≤ 5 ms, 1 MB ≤ 40 ms on one core.
//! `html5ever_parse_baseline` times the parse alone (the floor for any
//! html5ever-based converter); `by_content_kind` splits 1 MB by block type.

use std::time::{Duration, Instant};

use okilum_paste::{html_to_markdown, HtmlInput};

/// Mixed content the way a long web page or doc copies: headings,
/// styled paragraphs with links, lists, a table, code and images.
fn mixed_html(target: usize) -> String {
    let mut html = String::from(
        "<html><head><style>p.p1{font-weight:bold} span.s1{font-family:Menlo}</style></head><body>",
    );
    let mut i = 0usize;
    while html.len() < target {
        html.push_str(&format!(
            "<h2 id=\"s{i}\">Section {i}</h2>\
             <p class=\"p1\">Intro with <a href=\"/wiki/Page_{i}\">a link</a>, <b>bold</b>, <i>italic</i>, \
             <span style=\"font-weight:700;font-style:italic\">styled span</span> and <code>inline_{i}()</code>.</p>\
             <p>Plain text paragraph that goes on for a while so the size is dominated by prose, \
             as it is on real pages; it has &amp; entities, &nbsp;spaces and <span class=\"s1\">mono</span>.</p>\
             <ul><li>First item<ul><li>Nested <em>item</em></li><li><input type=checkbox checked> done</li></ul></li><li>Second</li></ul>\
             <table><thead><tr><th>Name</th><th>Value</th></tr></thead><tbody>\
             <tr><td>alpha</td><td>{i}</td></tr><tr><td>beta | gamma</td><td>{}.5</td></tr></tbody></table>\
             <pre><code class=\"language-rust\">fn f{i}() -&gt; u32 {{\n    {i}\n}}\n</code></pre>\
             <blockquote><p>Quote {i}</p></blockquote>\
             <p><img src=\"https://example.com/img/{i}.png\" alt=\"pic {i}\"></p>",
            i * 3
        ));
        i += 1;
    }
    html.push_str("</body></html>");
    html
}

fn time(html: &str) -> Duration {
    let input = HtmlInput {
        html,
        base_url: Some("https://en.wikipedia.org/wiki/Rust"),
    };
    // Warm up, then best of 5.
    let _ = html_to_markdown(&input);
    (0..5)
        .map(|_| {
            let t = Instant::now();
            let c = html_to_markdown(&input);
            let d = t.elapsed();
            assert!(c.markdown.len() > html.len() / 4);
            d
        })
        .min()
        .unwrap()
}

#[test]
#[ignore]
fn timings() {
    for (label, size, budget_ms) in [("100 KB", 100 * 1024, 5u128), ("1 MB", 1024 * 1024, 40)] {
        let html = mixed_html(size);
        let d = time(&html);
        println!(
            "{label}: {} bytes in {:.2} ms (target {budget_ms} ms)",
            html.len(),
            d.as_secs_f64() * 1000.0
        );
    }
}

#[test]
#[ignore]
fn html5ever_parse_baseline() {
    use html5ever::tendril::TendrilSink;
    for size in [100 * 1024, 1024 * 1024] {
        let html = mixed_html(size);
        let t = Instant::now();
        let dom =
            html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default())
                .one(&*html);
        println!("parse {size}: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
        drop(dom);
    }
}

#[test]
#[ignore]
fn by_content_kind() {
    let kinds = [
        ("para", "<p>Plain text paragraph that goes on for a while so the size is dominated by prose, as it is on real pages; it has &amp; entities and spaces.</p>"),
        ("styled", "<p class=\"p1\">Intro with <a href=\"/wiki/Page\">a link</a>, <b>bold</b>, <i>italic</i>, <span style=\"font-weight:700;font-style:italic\">styled span</span> and <code>inline()</code>.</p>"),
        ("list", "<ul><li>First item<ul><li>Nested <em>item</em></li><li><input type=checkbox checked> done</li></ul></li><li>Second</li></ul>"),
        ("table", "<table><thead><tr><th>Name</th><th>Value</th></tr></thead><tbody><tr><td>alpha</td><td>3</td></tr><tr><td>beta | gamma</td><td>4.5</td></tr></tbody></table>"),
        ("pre", "<pre><code class=\"language-rust\">fn f() -&gt; u32 {\n    1\n}\n</code></pre>"),
        ("quote", "<blockquote><p>Quote text here</p></blockquote>"),
        ("img", "<p><img src=\"https://example.com/img/1.png\" alt=\"pic\"></p>"),
    ];
    for (name, piece) in kinds {
        let mut html =
            String::from("<html><head><style>p.p1{font-weight:bold}</style></head><body>");
        while html.len() < 1024 * 1024 {
            html.push_str(piece);
        }
        let input = HtmlInput {
            html: &html,
            base_url: Some("https://en.wikipedia.org/wiki/Rust"),
        };
        let _ = html_to_markdown(&input);
        let t = Instant::now();
        let _ = html_to_markdown(&input);
        println!("KIND {name}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
}
