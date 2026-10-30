//! Hostile input: deep nesting and random garbage must neither overflow the
//! stack nor panic.

use okilum_paste::{html_to_markdown, HtmlInput};

fn convert(html: &str) -> String {
    html_to_markdown(&HtmlInput {
        html,
        base_url: Some("https://example.com/a/b"),
    })
    .markdown
}

/// Runs on a 512 KB stack (debug build): a quarter of a 2 MB worker thread,
/// a sixteenth of the 8 MB main thread, so recursion that scales with nesting fails loudly here.
fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .expect("converter overflowed or panicked");
}

#[test]
fn ten_thousand_nested_elements() {
    for (open, close) in [
        ("<div>", "</div>"),
        ("<span>", "</span>"),
        ("<b>", "</b>"),
        ("<blockquote>", "</blockquote>"),
        ("<ul><li>", "</li></ul>"),
        ("<table><tr><td>", "</td></tr></table>"),
        ("<a href=x>", "</a>"),
        ("<pre><code>", "</code></pre>"),
    ] {
        let html = format!("{}deep text{}", open.repeat(10_000), close.repeat(10_000));
        on_small_stack(move || {
            let md = convert(&html);
            assert!(md.contains("deep text"), "{md:.200}");
        });
    }
}

/// HTML ignores the slash of `<div/>`: it opens an element like `<div>`,
/// so the depth guard must count it (only SVG/MathML elements self-close).
#[test]
fn self_closing_slash_on_html_elements_still_nests() {
    for open in ["<div/>", "<span/>", "<blockquote/>"] {
        let html = format!("{}deep text", open.repeat(10_000));
        on_small_stack(move || {
            let md = convert(&html);
            assert!(md.contains("deep text"), "{md:.200}");
        });
    }
    // Foreign self-closing elements do not nest and are not flattened.
    let svg = format!(
        "<p>a</p><svg>{}</svg><p>b</p>",
        "<path d='M0 0'/>".repeat(1_000)
    );
    on_small_stack(move || {
        let md = convert(&svg);
        assert!(md.contains('a') && md.contains('b'), "{md:.200}");
    });
}

/// Sibling lists nest by `data-indent` (Slack), with no DOM nesting: a
/// steadily rising indent must not recurse once per level.
#[test]
fn rising_data_indent_lists() {
    let html: String = (0..10_000)
        .map(|i| format!("<ul data-indent=\"{i}\"><li>item {i}</li></ul>"))
        .collect();
    on_small_stack(move || {
        let md = convert(&html);
        assert!(md.contains("item 9999"), "{md:.200}");
    });
}

#[test]
fn unclosed_tags_and_mixed_nesting() {
    let mut html = String::new();
    for i in 0..10_000 {
        html.push_str(["<div>", "<p>", "<i>", "<li>", "<td>", "<h2>", "<span>"][i % 7]);
        html.push('x');
    }
    on_small_stack(move || {
        let _ = convert(&html);
    });
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

#[test]
fn seeded_random_input_never_panics() {
    const PIECES: [&str; 60] = [
        "<p>",
        "</p>",
        "<div>",
        "</div>",
        "<b>",
        "</b>",
        "<i>",
        "<em>",
        "</em>",
        "<s>",
        "<code>",
        "</code>",
        "<pre>",
        "</pre>",
        "<ul>",
        "<ol start=7>",
        "<li>",
        "</li>",
        "</ul>",
        "<table>",
        "<tr>",
        "<td colspan=3>",
        "<th>",
        "</table>",
        "<a href=\"/x?y=1&z\">",
        "<a href=\"javascript:alert(1)\">",
        "</a>",
        "<img src=\"data:image/png;base64,AAA\">",
        "<img src=//cdn.example.com/i.png alt='[x]'>",
        "<img src=\"file:///C:/Temp/msohtmlclip1/01/clip_image001.png\">",
        "<br>",
        "<hr>",
        "<blockquote>",
        "</blockquote>",
        "<h1>",
        "</h1>",
        "<mark>",
        "<span style=\"font-weight:700;font-family:Menlo\">",
        "<span style='mso-list:Ignore'>",
        "<p class=MsoListParagraph style='mso-list:l0 level3 lfo1'>",
        "<style>p.p1{font-weight:bold}</style>",
        "<p class=p1>",
        "<!--[if !supportLists]-->",
        "<![if !vml]>",
        "<![endif]>",
        "<input type=checkbox checked>",
        "<math><annotation encoding=\"application/x-tex\">x^2</annotation></math>",
        " ",
        "\n",
        "\t",
        "&nbsp;",
        "&amp;",
        "&#x1F600;",
        "*",
        "_",
        "`",
        "|",
        "#",
        "1. ",
        "text ",
    ];
    const RAW: [&str; 12] = [
        "\u{200b}", "\u{feff}", "é", "😀", "<", ">", "&", "\"", "\\", "[", "]", "\r\n",
    ];
    for seed in 1..=400u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let len = 1 + rng.next() % 300;
        let mut html = String::new();
        for _ in 0..len {
            if rng.next().is_multiple_of(5) {
                html.push_str(rng.pick(&RAW));
            } else {
                html.push_str(rng.pick(&PIECES));
            }
        }
        let result = std::panic::catch_unwind(|| convert(&html));
        assert!(result.is_ok(), "seed {seed} panicked on {html:?}");
        // Byte garbage, decoded lossily.
        let bytes: Vec<u8> = (0..len * 3).map(|_| rng.next() as u8).collect();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let result = std::panic::catch_unwind(|| convert(&text));
        assert!(result.is_ok(), "seed {seed} panicked on bytes");
        let _ = okilum_paste::extract_title(&bytes, None);
        let _ = okilum_paste::tsv_to_table(&text);
        let _ = okilum_paste::guess_language(&text);
    }
}

/// Wraps `inner` in `depth` divs.
fn nested(depth: usize, inner: &str) -> String {
    format!("{}{inner}{}", "<div>".repeat(depth), "</div>".repeat(depth))
}

/// Past the flattening depth, raw-text elements keep their shallow meaning:
/// script, style, title and textarea bodies are dropped, xmp and plaintext
/// bodies are literal text.
#[test]
fn raw_text_elements_nested_deeply() {
    for depth in [3, 300, 10_000] {
        for t in ["script", "style", "title", "textarea"] {
            let html = nested(
                depth,
                &format!("<p>before</p><{t}>leaked {t} body</{t}><p>after</p>"),
            );
            on_small_stack(move || {
                let md = convert(&html);
                assert!(md.contains("before") && md.contains("after"), "{md:.200}");
                assert!(!md.contains("leaked"), "{depth} {t}: {md:.200}");
            });
        }
        for t in ["xmp", "plaintext"] {
            let html = nested(
                depth,
                &format!("<p>before</p><{t}><b>lit</b> &amp; x</{t}>"),
            );
            on_small_stack(move || {
                let md = convert(&html);
                assert!(md.contains("before"), "{md:.200}");
                assert!(
                    md.contains("\\<b>lit\\</b> \\&amp; x"),
                    "{depth} {t}: {md:.200}"
                );
            });
        }
    }
}

/// 1 000 literal `<div>` strings in a deep `<xmp>` stay text: reparsed as
/// tags, they would nest far past the flattening depth.
#[test]
fn literal_tags_in_deep_xmp_stay_text() {
    let html = nested(300, &format!("<xmp>{}</xmp>after", "<div>".repeat(1_000)));
    on_small_stack(move || {
        let md = convert(&html);
        assert_eq!(md.matches("\\<div>").count(), 1_000, "{md:.200}");
        assert!(md.contains("after"), "{md:.200}");
    });
}
