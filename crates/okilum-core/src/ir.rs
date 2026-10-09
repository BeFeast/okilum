//! Document IR for slices without an HTML renderer (Slint builds native
//! widgets from these blocks).

use crate::render::{comrak_options, preprocess, rewrite_links};
use crate::vault::Vault;
use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{parse_document, Arena};
use serde::Serialize;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

#[derive(Debug, Clone, Serialize)]
pub struct Doc {
    pub title: String,
    pub path: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    CodeBlock {
        lang: String,
        lines: Vec<Vec<CodeSpan>>,
    },
    Quote {
        blocks: Vec<Block>,
    },
    Callout {
        kind: String,
        title: String,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        items: Vec<ListItem>,
    },
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Image {
        path: String,
        alt: String,
    },
    Rule,
    RawHtml {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ListItem {
    /// None = plain item; Some(done) = task item
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeSpan {
    pub text: String,
    /// #rrggbb foreground
    pub color: String,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Inline {
    Text {
        text: String,
        bold: bool,
        italic: bool,
        strike: bool,
        code: bool,
    },
    /// href carries the tessera:// scheme for wikilinks or a real URL.
    Link {
        text: String,
        href: String,
        external: bool,
        unresolved: bool,
    },
    Image {
        path: String,
        alt: String,
    },
}

struct Ctx {
    ss: SyntaxSet,
    theme: syntect::highlighting::Theme,
}

pub fn build_doc(vault: &Vault, note_rel: &str) -> anyhow::Result<Doc> {
    let text = preprocess(&vault.read_note(note_rel)?);
    let arena = Arena::new();
    let opts = comrak_options();
    let root = parse_document(&arena, &text, &opts);
    rewrite_links(root, vault, note_rel, &text);

    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let ctx = Ctx {
        ss,
        theme: ts.themes["InspiredGitHub"].clone(),
    };

    let mut blocks = Vec::new();
    for child in root.children() {
        blocks.extend(block_from(child, &ctx));
    }
    Ok(Doc {
        title: vault.note_title(note_rel),
        path: note_rel.to_string(),
        blocks,
    })
}

fn block_from<'a>(node: &'a AstNode<'a>, ctx: &Ctx) -> Vec<Block> {
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Heading(h) => vec![Block::Heading {
            level: h.level,
            inlines: inlines_from(node),
        }],
        NodeValue::Paragraph => {
            let inlines = inlines_from(node);
            // paragraph that is exactly one image -> image block
            if inlines.len() == 1 {
                if let Inline::Image { path, alt } = &inlines[0] {
                    return vec![Block::Image {
                        path: path.clone(),
                        alt: alt.clone(),
                    }];
                }
            }
            vec![Block::Paragraph { inlines }]
        }
        NodeValue::CodeBlock(cb) => {
            vec![Block::CodeBlock {
                lang: cb.info.clone(),
                lines: highlight(&cb.literal, &cb.info, ctx),
            }]
        }
        NodeValue::BlockQuote => {
            let blocks: Vec<Block> = node.children().flat_map(|c| block_from(c, ctx)).collect();
            vec![Block::Quote { blocks }]
        }
        NodeValue::Alert(a) => {
            let blocks: Vec<Block> = node.children().flat_map(|c| block_from(c, ctx)).collect();
            let kind = format!("{:?}", a.alert_type).to_lowercase();
            let title = a.title.clone().unwrap_or_else(|| {
                let mut k = kind.clone();
                if let Some(f) = k.get_mut(0..1) {
                    f.make_ascii_uppercase();
                }
                k
            });
            vec![Block::Callout {
                kind,
                title,
                blocks,
            }]
        }
        NodeValue::List(l) => {
            let ordered = matches!(l.list_type, ListType::Ordered);
            let mut items = Vec::new();
            for item in node.children() {
                let task = match &item.data.borrow().value {
                    NodeValue::TaskItem(sym) => Some(sym.is_some()),
                    _ => None,
                };
                let blocks: Vec<Block> = item.children().flat_map(|c| block_from(c, ctx)).collect();
                items.push(ListItem { task, blocks });
            }
            vec![Block::List { ordered, items }]
        }
        NodeValue::Table(_) => {
            let mut header = Vec::new();
            let mut rows = Vec::new();
            for row in node.children() {
                let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
                let cells: Vec<Vec<Inline>> = row.children().map(inlines_from).collect();
                if is_header {
                    header = cells;
                } else {
                    rows.push(cells);
                }
            }
            vec![Block::Table { header, rows }]
        }
        NodeValue::ThematicBreak => vec![Block::Rule],
        NodeValue::HtmlBlock(h) => vec![Block::RawHtml { text: h.literal }],
        NodeValue::FrontMatter(_) => vec![],
        _ => {
            // fallthrough: flatten children (footnote defs etc.)
            node.children().flat_map(|c| block_from(c, ctx)).collect()
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
}

fn inlines_from<'a>(node: &'a AstNode<'a>) -> Vec<Inline> {
    let mut out = Vec::new();
    for child in node.children() {
        collect_inline(child, Style::default(), &mut out);
    }
    out
}

fn collect_inline<'a>(node: &'a AstNode<'a>, style: Style, out: &mut Vec<Inline>) {
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Text(t) => out.push(Inline::Text {
            text: t.to_string(),
            bold: style.bold,
            italic: style.italic,
            strike: style.strike,
            code: false,
        }),
        NodeValue::Code(c) => out.push(Inline::Text {
            text: c.literal.to_string(),
            bold: style.bold,
            italic: style.italic,
            strike: style.strike,
            code: true,
        }),
        NodeValue::SoftBreak => out.push(Inline::Text {
            text: " ".into(),
            bold: false,
            italic: false,
            strike: false,
            code: false,
        }),
        NodeValue::LineBreak => out.push(Inline::Text {
            text: "\n".into(),
            bold: false,
            italic: false,
            strike: false,
            code: false,
        }),
        NodeValue::Emph => {
            for c in node.children() {
                collect_inline(
                    c,
                    Style {
                        italic: true,
                        ..style
                    },
                    out,
                );
            }
        }
        NodeValue::Strong => {
            for c in node.children() {
                collect_inline(
                    c,
                    Style {
                        bold: true,
                        ..style
                    },
                    out,
                );
            }
        }
        NodeValue::Strikethrough => {
            for c in node.children() {
                collect_inline(
                    c,
                    Style {
                        strike: true,
                        ..style
                    },
                    out,
                );
            }
        }
        NodeValue::Link(link) => {
            let text = plain_text(node);
            out.push(Inline::Link {
                text,
                href: link.url.clone(),
                external: link.url.contains("://") && !link.url.starts_with("tessera://"),
                unresolved: false,
            });
        }
        NodeValue::WikiLink(w) => {
            let text = plain_text(node);
            let unresolved = w.url.starts_with(crate::render::UNRESOLVED_SCHEME);
            out.push(Inline::Link {
                text,
                href: w.url.clone(),
                external: false,
                unresolved,
            });
        }
        NodeValue::Image(img) => {
            out.push(Inline::Image {
                path: img.url.trim_start_matches("file://").to_string(),
                alt: plain_text(node),
            });
        }
        _ => {
            for c in node.children() {
                collect_inline(c, style, out);
            }
        }
    }
}

fn plain_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        match &d.data.borrow().value {
            NodeValue::Text(t) => s.push_str(t),
            NodeValue::Code(c) => s.push_str(&c.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => s.push(' '),
            _ => {}
        }
    }
    if s.is_empty() {
        "?".into()
    } else {
        s
    }
}

fn highlight(code: &str, info: &str, ctx: &Ctx) -> Vec<Vec<CodeSpan>> {
    let lang = info.split_whitespace().next().unwrap_or("");
    let syntax = ctx
        .ss
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| ctx.ss.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, &ctx.theme);
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code) {
        let mut spans = Vec::new();
        if let Ok(ranges) = h.highlight_line(line, &ctx.ss) {
            for (style, text) in ranges {
                let t = text.trim_end_matches('\n');
                let c = style.foreground;
                spans.push(CodeSpan {
                    text: t.to_string(),
                    color: format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
                    bold: style
                        .font_style
                        .contains(syntect::highlighting::FontStyle::BOLD),
                    italic: style
                        .font_style
                        .contains(syntect::highlighting::FontStyle::ITALIC),
                });
            }
        } else {
            spans.push(CodeSpan {
                text: line.trim_end_matches('\n').to_string(),
                color: "#333333".into(),
                bold: false,
                italic: false,
            });
        }
        lines.push(spans);
    }
    lines
}
