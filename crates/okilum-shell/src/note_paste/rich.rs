//! Rich text (RTF, RTFD) as simple HTML for the Markdown converter (#1611).
//!
//! A platform reads its attributed string into [`Run`]s (macOS walks
//! NSAttributedString's attribute runs); this module writes the HTML, so the
//! same output comes from every platform: `p`, `br`, `b`, `i`, `s`, `code`,
//! `a`, and `ul`/`ol` lists with nesting. Platform-neutral and pure.

/// One stretch of text with the same attributes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// A fixed-pitch font: code.
    pub mono: bool,
    pub strike: bool,
    pub link: Option<String>,
    /// The paragraph's list, if it is a list item.
    pub list: Option<ListLevel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListLevel {
    pub ordered: bool,
    /// 1 for a top-level item.
    pub depth: usize,
}

/// The HTML for `runs`. Paragraphs end at `\n` (or U+2029); U+2028 is a
/// line break. A list item's text marker (`\t•\t`, `\t1.\t`), which AppKit
/// keeps in the string, is dropped.
pub fn runs_to_html(runs: &[Run]) -> String {
    let mut paragraphs: Vec<(Option<ListLevel>, Vec<Run>)> = vec![(None, Vec::new())];
    for run in runs {
        let mut pieces = run.text.split(['\n', '\u{2029}']).peekable();
        while let Some(piece) = pieces.next() {
            let current = paragraphs.last_mut().expect("one paragraph");
            if current.1.is_empty() {
                current.0 = run.list;
            }
            if !piece.is_empty() {
                current.1.push(Run {
                    text: piece.to_string(),
                    ..run.clone()
                });
            }
            if pieces.peek().is_some() {
                paragraphs.push((None, Vec::new()));
            }
        }
    }
    let mut html = String::new();
    let mut open: Vec<bool> = Vec::new();
    for (list, mut runs) in paragraphs {
        if list.is_some() {
            strip_marker(&mut runs);
        }
        let content = inline(&runs);
        match list {
            Some(level) => {
                let depth = level.depth.max(1);
                while open.len() > depth {
                    close_list(&mut html, &mut open);
                }
                if open.len() == depth && open.last() != Some(&level.ordered) {
                    close_list(&mut html, &mut open);
                }
                while open.len() < depth {
                    html.push_str(if level.ordered { "<ol>" } else { "<ul>" });
                    open.push(level.ordered);
                }
                html.push_str("<li>");
                html.push_str(&content);
                html.push_str("</li>");
            }
            None => {
                while !open.is_empty() {
                    close_list(&mut html, &mut open);
                }
                if !content.trim().is_empty() {
                    html.push_str("<p>");
                    html.push_str(&content);
                    html.push_str("</p>");
                }
            }
        }
    }
    while !open.is_empty() {
        close_list(&mut html, &mut open);
    }
    html
}

fn close_list(html: &mut String, open: &mut Vec<bool>) {
    if let Some(ordered) = open.pop() {
        html.push_str(if ordered { "</ol>" } else { "</ul>" });
    }
}

/// Drop `\t<marker>\t` (or `<marker>\t`) from the start of a list item.
fn strip_marker(runs: &mut [Run]) {
    let Some(first) = runs.first_mut() else {
        return;
    };
    let text = first.text.trim_start_matches('\t');
    if let Some(tab) = text.find('\t') {
        if tab <= 12 && !text[..tab].contains(' ') {
            first.text = text[tab + 1..].to_string();
        }
    }
}

fn inline(runs: &[Run]) -> String {
    let mut out = String::new();
    for run in runs {
        let mut text = escape(&run.text).replace('\u{2028}', "<br>");
        if run.mono {
            text = format!("<code>{text}</code>");
        } else {
            if run.strike {
                text = format!("<s>{text}</s>");
            }
            if run.italic {
                text = format!("<i>{text}</i>");
            }
            if run.bold {
                text = format!("<b>{text}</b>");
            }
        }
        if let Some(link) = &run.link {
            text = format!("<a href=\"{}\">{text}</a>", escape(link));
        }
        out.push_str(&text);
    }
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Run {
        Run {
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn inline_styles_links_and_paragraphs() {
        let runs = [
            run("Plain "),
            Run {
                bold: true,
                ..run("bold")
            },
            run(" and "),
            Run {
                italic: true,
                strike: true,
                ..run("gone")
            },
            run("\nSecond "),
            Run {
                mono: true,
                ..run("a<b")
            },
            run(" "),
            Run {
                link: Some("https://x.org/?a=1&b=2".into()),
                ..run("link")
            },
            run("\u{2028}break\n\n"),
        ];
        assert_eq!(
            runs_to_html(&runs),
            "<p>Plain <b>bold</b> and <i><s>gone</s></i></p>\
             <p>Second <code>a&lt;b</code> <a href=\"https://x.org/?a=1&amp;b=2\">link</a><br>break</p>"
        );
    }

    #[test]
    fn lists_nest_and_drop_their_text_markers() {
        let item = |text: &str, ordered, depth| Run {
            list: Some(ListLevel { ordered, depth }),
            ..run(text)
        };
        let runs = [
            run("Intro\n"),
            item("\t•\tone\n", false, 1),
            item("\t◦\tinner\n", false, 2),
            item("\t•\ttwo\n", false, 1),
            item("\t1.\tfirst\n", true, 1),
            run("After"),
        ];
        assert_eq!(
            runs_to_html(&runs),
            "<p>Intro</p><ul><li>one</li><ul><li>inner</li></ul><li>two</li></ul>\
             <ol><li>first</li></ol><p>After</p>"
        );
    }
}
