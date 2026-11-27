//! Two-way Jira Server/Data Center wiki markup converter (#3291).
//!
//! Pure functions with no GPUI, clipboard, filesystem or network access:
//! - [`markdown_to_jira`] converts Markdown (GFM) to Jira Server/DC wiki markup.
//! - [`jira_to_markdown`] converts Jira Server/DC wiki markup to clean Markdown.
//! - [`looks_like_jira_wiki`] conservatively detects unmistakable Jira wiki
//!   markup for smart paste auto-conversion without false positives on normal
//!   Markdown, plain text, or source code.

use crate::render::fence_code;
use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{parse_document, Arena, Options};

// ---------------------------------------------------------------------------
// Markdown -> Jira wiki markup
// ---------------------------------------------------------------------------

/// Convert Markdown (CommonMark + GFM tables/strikethrough/autolink/tasklist)
/// into Jira Server/Data Center wiki markup.
pub fn markdown_to_jira(markdown: &str) -> String {
    let normalized = normalize_newlines(markdown);
    if normalized.trim().is_empty() {
        return String::new();
    }
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;

    let root = parse_document(&arena, &normalized, &options);
    render_blocks(root, "").trim_end_matches('\n').to_owned()
}

fn normalize_newlines(s: &str) -> String {
    if !s.contains('\r') {
        return s.to_owned();
    }
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn render_blocks<'a>(parent: &'a AstNode<'a>, list_prefix: &str) -> String {
    let mut blocks = Vec::new();
    for child in parent.children() {
        let rendered = render_block(child, list_prefix);
        if !rendered.is_empty() {
            blocks.push(rendered);
        }
    }
    blocks.join("\n\n")
}

fn render_block<'a>(node: &'a AstNode<'a>, list_prefix: &str) -> String {
    match &node.data.borrow().value {
        NodeValue::Heading(h) => {
            let level = h.level.clamp(1, 6);
            let inline = render_inlines(node, false);
            format!("h{level}. {inline}")
        }
        NodeValue::Paragraph => render_inlines(node, false),
        NodeValue::ThematicBreak => "----".to_owned(),
        NodeValue::CodeBlock(cb) => {
            let lang = cb.info.split_whitespace().next().unwrap_or("");
            let body = cb.literal.strip_suffix('\n').unwrap_or(&cb.literal);
            let header = if lang.is_empty() {
                "{code}".to_owned()
            } else {
                format!("{{code:{lang}}}")
            };
            if body.is_empty() {
                format!("{header}\n{{code}}")
            } else {
                format!("{header}\n{body}\n{{code}}")
            }
        }
        NodeValue::BlockQuote => {
            let inner = render_blocks(node, "");
            if inner.is_empty() {
                "{quote}\n{quote}".to_owned()
            } else {
                format!("{{quote}}\n{inner}\n{{quote}}")
            }
        }
        NodeValue::List(list) => {
            let marker = match list.list_type {
                ListType::Bullet => '*',
                ListType::Ordered => '#',
            };
            let mut next_prefix = String::with_capacity(list_prefix.len() + 1);
            next_prefix.push_str(list_prefix);
            next_prefix.push(marker);

            let mut lines = Vec::new();
            for item in node.children() {
                render_list_item(item, &next_prefix, &mut lines);
            }
            lines.join("\n")
        }
        NodeValue::Table(_) => render_table(node),
        NodeValue::HtmlBlock(hb) => hb.literal.trim_end_matches('\n').to_owned(),
        _ => render_blocks(node, list_prefix),
    }
}

fn render_list_item<'a>(item: &'a AstNode<'a>, prefix: &str, out: &mut Vec<String>) {
    let task_prefix = match &item.data.borrow().value {
        NodeValue::TaskItem(Some(_)) => "[x] ",
        NodeValue::TaskItem(None) => "[ ] ",
        _ => "",
    };

    let mut emitted_first = false;
    for child in item.children() {
        match &child.data.borrow().value {
            NodeValue::Paragraph if !emitted_first => {
                let text = render_inlines(child, false);
                out.push(format!("{prefix} {task_prefix}{text}"));
                emitted_first = true;
            }
            NodeValue::List(_) => {
                if !emitted_first {
                    out.push(format!("{prefix} {task_prefix}").trim_end().to_owned());
                    emitted_first = true;
                }
                let nested = render_block(child, prefix);
                if !nested.is_empty() {
                    out.push(nested);
                }
            }
            _ => {
                let rendered = render_block(child, "");
                if !rendered.is_empty() {
                    if !emitted_first {
                        out.push(format!("{prefix} {task_prefix}{rendered}"));
                        emitted_first = true;
                    } else {
                        out.push(rendered);
                    }
                }
            }
        }
    }
    if !emitted_first {
        out.push(format!("{prefix} {task_prefix}").trim_end().to_owned());
    }
}

fn render_table<'a>(table: &'a AstNode<'a>) -> String {
    let mut rows = Vec::new();
    for row in table.children() {
        let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
        let sep = if is_header { "||" } else { "|" };
        let mut cells = Vec::new();
        for cell in row.children() {
            let content = render_inlines(cell, true);
            if content.trim().is_empty() {
                cells.push(" ".to_owned());
            } else {
                cells.push(content);
            }
        }
        let mut line = String::new();
        line.push_str(sep);
        line.push_str(&cells.join(sep));
        line.push_str(sep);
        rows.push(line);
    }
    rows.join("\n")
}

fn render_inlines<'a>(parent: &'a AstNode<'a>, in_cell: bool) -> String {
    let mut out = String::new();
    for child in parent.children() {
        render_inline(child, in_cell, &mut out);
    }
    out
}

fn render_inline<'a>(node: &'a AstNode<'a>, in_cell: bool, out: &mut String) {
    match &node.data.borrow().value {
        NodeValue::Text(text) => {
            if in_cell {
                out.push_str(&text.replace('\n', " "));
            } else {
                out.push_str(text);
            }
        }
        NodeValue::SoftBreak => {
            if in_cell {
                out.push(' ');
            } else {
                out.push('\n');
            }
        }
        NodeValue::LineBreak => {
            if in_cell {
                out.push_str(r" \\ ");
            } else {
                out.push_str("\\\\\n");
            }
        }
        NodeValue::Code(code) => {
            out.push_str("{{");
            out.push_str(&code.literal.replace("}}", r"\}\}"));
            out.push_str("}}");
        }
        NodeValue::Strong => {
            out.push('*');
            for child in node.children() {
                render_inline(child, in_cell, out);
            }
            out.push('*');
        }
        NodeValue::Emph => {
            out.push('_');
            for child in node.children() {
                render_inline(child, in_cell, out);
            }
            out.push('_');
        }
        NodeValue::Strikethrough => {
            out.push('-');
            for child in node.children() {
                render_inline(child, in_cell, out);
            }
            out.push('-');
        }
        NodeValue::Link(link) => {
            let label = render_inlines(node, in_cell);
            if label.is_empty() || label == link.url {
                out.push('[');
                out.push_str(&link.url);
                out.push(']');
            } else {
                out.push('[');
                out.push_str(&label);
                out.push('|');
                out.push_str(&link.url);
                out.push(']');
            }
        }
        NodeValue::Image(img) => {
            out.push('!');
            out.push_str(&img.url);
            out.push('!');
        }
        NodeValue::HtmlInline(html) => {
            out.push_str(html);
        }
        _ => {
            for child in node.children() {
                render_inline(child, in_cell, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Jira wiki markup -> Markdown
// ---------------------------------------------------------------------------

/// Convert Jira Server/Data Center wiki markup to clean GFM Markdown.
///
/// Unknown Jira macros (`{toc}`, `{status:...}`, `{color:...}`, `{anchor:...}`,
/// custom `{foo:...}`) are preserved verbatim.
pub fn jira_to_markdown(jira: &str) -> String {
    let normalized = normalize_newlines(jira);
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut blocks: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // 1. {code} / {code:...} or {noformat} / {noformat:...}
        if let Some((tag, lang, inline_rest)) = parse_code_macro_open(trimmed) {
            let closing = format!("{{{tag}}}");
            if let Some(before_close) = inline_rest.strip_suffix(&closing) {
                let lang_opt = lang.as_deref().filter(|s| !s.is_empty());
                blocks.push(fence_code(before_close, lang_opt));
                i += 1;
                continue;
            }
            let mut body_lines: Vec<&str> = Vec::new();
            if !inline_rest.is_empty() {
                body_lines.push(inline_rest);
            }
            i += 1;
            while i < lines.len() {
                let cur = lines[i];
                let cur_trim = cur.trim();
                if cur_trim == closing {
                    i += 1;
                    break;
                }
                if let Some(before) = cur_trim.strip_suffix(&closing) {
                    if !before.is_empty() {
                        body_lines.push(before);
                    }
                    i += 1;
                    break;
                }
                body_lines.push(cur);
                i += 1;
            }
            let body = body_lines.join("\n");
            let lang_opt = lang.as_deref().filter(|s| !s.is_empty());
            blocks.push(fence_code(&body, lang_opt));
            continue;
        }

        // 2. {quote}
        if let Some(inline_rest) = parse_simple_macro_open(trimmed, "quote") {
            let closing = "{quote}";
            if let Some(before_close) = inline_rest.strip_suffix(closing) {
                let inner = jira_to_markdown(before_close);
                blocks.push(prefix_blockquote(&inner, None));
                i += 1;
                continue;
            }
            let mut inner_lines = Vec::new();
            if !inline_rest.is_empty() {
                inner_lines.push(inline_rest);
            }
            i += 1;
            while i < lines.len() {
                let cur = lines[i];
                let cur_trim = cur.trim();
                if cur_trim == closing {
                    i += 1;
                    break;
                }
                if let Some(before) = cur_trim.strip_suffix(closing) {
                    if !before.is_empty() {
                        inner_lines.push(before);
                    }
                    i += 1;
                    break;
                }
                inner_lines.push(cur);
                i += 1;
            }
            let inner = jira_to_markdown(&inner_lines.join("\n"));
            blocks.push(prefix_blockquote(&inner, None));
            continue;
        }

        // 3. {panel} / {panel:...}
        if let Some((title, inline_rest)) = parse_panel_macro_open(trimmed) {
            let closing = "{panel}";
            if let Some(before_close) = inline_rest.strip_suffix(closing) {
                let inner = jira_to_markdown(before_close);
                blocks.push(prefix_blockquote(&inner, title.as_deref()));
                i += 1;
                continue;
            }
            let mut inner_lines = Vec::new();
            if !inline_rest.is_empty() {
                inner_lines.push(inline_rest);
            }
            i += 1;
            while i < lines.len() {
                let cur = lines[i];
                let cur_trim = cur.trim();
                if cur_trim == closing {
                    i += 1;
                    break;
                }
                if let Some(before) = cur_trim.strip_suffix(closing) {
                    if !before.is_empty() {
                        inner_lines.push(before);
                    }
                    i += 1;
                    break;
                }
                inner_lines.push(cur);
                i += 1;
            }
            let inner = jira_to_markdown(&inner_lines.join("\n"));
            blocks.push(prefix_blockquote(&inner, title.as_deref()));
            continue;
        }

        // 4. Single-line blockquote: `bq. ...`
        if let Some(rest) = parse_bq_line(trimmed) {
            let mut quote_lines = vec![format!("> {}", convert_inlines(rest, false))];
            i += 1;
            while i < lines.len() {
                if let Some(next_rest) = parse_bq_line(lines[i].trim()) {
                    quote_lines.push(format!("> {}", convert_inlines(next_rest, false)));
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(quote_lines.join("\n"));
            continue;
        }

        // 5. Headings `h1.` .. `h6.`
        if let Some((level, rest)) = parse_heading_line(trimmed) {
            let hashes = "#".repeat(level);
            blocks.push(format!("{hashes} {}", convert_inlines(rest, false)));
            i += 1;
            continue;
        }

        // 6. Horizontal rule `----` (4 or more `-`)
        if is_jira_hr(trimmed) {
            blocks.push("---".to_owned());
            i += 1;
            continue;
        }

        // 7. Table (`||H1||H2||` / `|C1|C2|`)
        if is_jira_table_row(trimmed) {
            let mut table_rows = Vec::new();
            while i < lines.len() && is_jira_table_row(lines[i].trim()) {
                table_rows.push(parse_jira_table_row(lines[i].trim()));
                i += 1;
            }
            blocks.push(format_gfm_table(&table_rows));
            continue;
        }

        // 8. Nested lists (`*`, `**`, `#`, `##`, `*#`, `#*`)
        if parse_jira_list_line(line).is_some() {
            let mut list_lines = Vec::new();
            while i < lines.len() {
                if let Some((markers, rest)) = parse_jira_list_line(lines[i]) {
                    list_lines.push(format_markdown_list_item(markers, rest));
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(list_lines.join("\n"));
            continue;
        }

        // 9. Paragraph (group contiguous plain lines until blank or next block)
        let mut para_lines = Vec::new();
        while i < lines.len() {
            let cur = lines[i];
            let cur_trim = cur.trim();
            if cur_trim.is_empty()
                || parse_code_macro_open(cur_trim).is_some()
                || parse_simple_macro_open(cur_trim, "quote").is_some()
                || parse_panel_macro_open(cur_trim).is_some()
                || parse_bq_line(cur_trim).is_some()
                || parse_heading_line(cur_trim).is_some()
                || is_jira_hr(cur_trim)
                || is_jira_table_row(cur_trim)
                || parse_jira_list_line(cur).is_some()
            {
                break;
            }
            para_lines.push(convert_inlines(cur, false));
            i += 1;
        }
        if !para_lines.is_empty() {
            blocks.push(para_lines.join("\n"));
        }
    }

    blocks.join("\n\n")
}

fn parse_code_macro_open(trimmed: &str) -> Option<(&'static str, Option<String>, &str)> {
    for tag in ["code", "noformat"] {
        let exact = format!("{{{tag}}}");
        if let Some(rest) = trimmed.strip_prefix(&exact) {
            return Some((tag, None, rest));
        }
        let with_params = format!("{{{tag}:");
        if let Some(after_colon) = trimmed.strip_prefix(&with_params) {
            let close_brace = after_colon.find('}')?;
            let params = &after_colon[..close_brace];
            let rest = &after_colon[close_brace + 1..];
            let lang = if tag == "code" {
                extract_code_language(params)
            } else {
                None
            };
            return Some((tag, lang, rest));
        }
    }
    None
}

fn extract_code_language(params: &str) -> Option<String> {
    let mut positional = None;
    for part in params.split('|') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((key, val)) = part.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            let val = val.trim();
            if matches!(key.as_str(), "language" | "lang") && !val.is_empty() {
                return Some(val.to_owned());
            }
        } else if positional.is_none() {
            positional = Some(part.to_owned());
        }
    }
    positional
}

fn parse_simple_macro_open<'a>(trimmed: &'a str, name: &str) -> Option<&'a str> {
    let exact = format!("{{{name}}}");
    trimmed.strip_prefix(&exact)
}

fn parse_panel_macro_open(trimmed: &str) -> Option<(Option<String>, &str)> {
    if let Some(rest) = trimmed.strip_prefix("{panel}") {
        return Some((None, rest));
    }
    if let Some(after_colon) = trimmed.strip_prefix("{panel:") {
        let close_brace = after_colon.find('}')?;
        let params = &after_colon[..close_brace];
        let rest = &after_colon[close_brace + 1..];
        let mut title = None;
        for part in params.split('|') {
            if let Some((k, v)) = part.split_once('=') {
                if k.trim().eq_ignore_ascii_case("title") && !v.trim().is_empty() {
                    title = Some(v.trim().to_owned());
                }
            }
        }
        return Some((title, rest));
    }
    None
}

fn prefix_blockquote(inner: &str, title: Option<&str>) -> String {
    let mut out = Vec::new();
    if let Some(title) = title {
        out.push(format!("> **{}**", convert_inlines(title, false)));
        if !inner.is_empty() {
            out.push(">".to_owned());
        }
    }
    if !inner.is_empty() {
        for line in inner.lines() {
            if line.is_empty() {
                out.push(">".to_owned());
            } else {
                out.push(format!("> {line}"));
            }
        }
    }
    if out.is_empty() {
        ">".to_owned()
    } else {
        out.join("\n")
    }
}

fn parse_bq_line(trimmed: &str) -> Option<&str> {
    trimmed
        .strip_prefix("bq. ")
        .or_else(|| trimmed.strip_prefix("bq.\t"))
}

fn parse_heading_line(trimmed: &str) -> Option<(usize, &str)> {
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'h'
        && (b'1'..=b'6').contains(&bytes[1])
        && bytes[2] == b'.'
        && (bytes[3] == b' ' || bytes[3] == b'\t')
    {
        let level = (bytes[1] - b'0') as usize;
        let rest = trimmed[3..].trim_start();
        if !rest.is_empty() {
            return Some((level, rest));
        }
    }
    None
}

fn is_jira_hr(trimmed: &str) -> bool {
    trimmed.len() >= 4 && trimmed.bytes().all(|b| b == b'-')
}

fn is_jira_table_row(trimmed: &str) -> bool {
    trimmed.len() >= 2
        && trimmed.starts_with('|')
        && trimmed.ends_with('|')
        && trimmed.contains(|c: char| c != '|' && !c.is_whitespace())
}

fn parse_jira_table_row(trimmed: &str) -> (bool, Vec<String>) {
    let is_header = trimmed.starts_with("||") && trimmed.ends_with("||") && trimmed.len() >= 4;
    let inner = if is_header {
        &trimmed[2..trimmed.len() - 2]
    } else {
        &trimmed[1..trimmed.len() - 1]
    };

    let mut cells = Vec::new();
    let mut current = String::new();
    let bytes = inner.as_bytes();
    let mut i = 0;
    let mut bracket_depth: usize = 0;
    let mut brace_depth: usize = 0;
    let mut in_mono = false;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            current.push(bytes[i] as char);
            let ch = inner[i + 1..].chars().next().unwrap();
            current.push(ch);
            i += 1 + ch.len_utf8();
            continue;
        }
        if !in_mono && inner[i..].starts_with("{{") {
            in_mono = true;
            current.push_str("{{");
            i += 2;
            continue;
        }
        if in_mono && inner[i..].starts_with("}}") {
            in_mono = false;
            current.push_str("}}");
            i += 2;
            continue;
        }
        if !in_mono {
            match bytes[i] {
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b'|' if bracket_depth == 0 && brace_depth == 0 => {
                    cells.push(convert_inlines(current.trim(), true));
                    current.clear();
                    if bytes.get(i + 1) == Some(&b'|') {
                        i += 2;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }
        let ch = inner[i..].chars().next().unwrap();
        current.push(ch);
        i += ch.len_utf8();
    }
    cells.push(convert_inlines(current.trim(), true));
    (is_header, cells)
}

fn format_gfm_table(rows: &[(bool, Vec<String>)]) -> String {
    let cols = rows.iter().map(|(_, c)| c.len()).max().unwrap_or(0).max(1);
    let fmt_row = |cells: &[String]| -> String {
        let mut out = String::from("|");
        for idx in 0..cols {
            let cell = cells.get(idx).map(String::as_str).unwrap_or("");
            let escaped = escape_table_pipes(cell);
            out.push(' ');
            out.push_str(&escaped);
            out.push_str(" |");
        }
        out
    };
    let sep_row = || -> String {
        let mut out = String::from("|");
        for _ in 0..cols {
            out.push_str(" --- |");
        }
        out
    };

    let mut lines = Vec::new();
    if let Some((first_is_header, first_cells)) = rows.first() {
        if *first_is_header {
            lines.push(fmt_row(first_cells));
            lines.push(sep_row());
            for (_, cells) in &rows[1..] {
                lines.push(fmt_row(cells));
            }
        } else {
            let empty = vec![String::new(); cols];
            lines.push(fmt_row(&empty));
            lines.push(sep_row());
            for (_, cells) in rows {
                lines.push(fmt_row(cells));
            }
        }
    }
    lines.join("\n")
}

fn escape_table_pipes(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    let mut prev_backslash = false;
    for ch in cell.chars() {
        if ch == '|' && !prev_backslash {
            out.push_str(r"\|");
        } else {
            out.push(ch);
        }
        prev_backslash = ch == '\\' && !prev_backslash;
    }
    out
}

fn parse_jira_list_line(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let marker_len = trimmed
        .bytes()
        .take_while(|&b| b == b'*' || b == b'#')
        .count();
    if marker_len == 0 {
        return None;
    }
    let after = &trimmed[marker_len..];
    if !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    let rest = after.trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((&trimmed[..marker_len], rest))
}

fn format_markdown_list_item(markers: &str, rest: &str) -> String {
    let bytes = markers.as_bytes();
    let mut indent = 0usize;
    for &b in &bytes[..bytes.len() - 1] {
        indent += if b == b'#' { 3 } else { 2 };
    }
    let prefix = if *bytes.last().unwrap() == b'#' {
        "1. "
    } else {
        "- "
    };
    let content = convert_inlines(rest, false);
    format!("{}{prefix}{content}", " ".repeat(indent))
}

// ---------------------------------------------------------------------------
// Inline Jira -> Markdown converter
// ---------------------------------------------------------------------------

fn convert_inlines(input: &str, in_cell: bool) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // 1. Forced line break `\\`
        if input[i..].starts_with(r"\\") {
            if in_cell {
                out.push_str("<br>");
            } else {
                out.push_str("  \n");
            }
            i += 2;
            continue;
        }

        // 2. Inline monospace `{{...}}`
        if input[i..].starts_with("{{") {
            if let Some((code, next_i)) = parse_jira_monospace(input, i) {
                out.push_str(&format_markdown_code_span(&code));
                i = next_i;
                continue;
            }
        }

        // 3. Unknown inline macros `{name}` or `{name:...}` — preserve verbatim!
        if bytes[i] == b'{' {
            if let Some(end) = match_jira_macro_tag(&input[i..]) {
                out.push_str(&input[i..i + end]);
                i += end;
                continue;
            }
        }

        // 4. Links & mentions `[...]`
        if bytes[i] == b'[' {
            if let Some((rendered, next_i)) = parse_jira_bracket(input, i, in_cell) {
                out.push_str(&rendered);
                i = next_i;
                continue;
            }
        }

        // 5. Images `!url!` or `!url|params!`
        if bytes[i] == b'!' {
            if let Some((rendered, next_i)) = parse_jira_image(input, i) {
                out.push_str(&rendered);
                i = next_i;
                continue;
            }
        }

        // 6. Bare URLs `http://...` / `https://...` — do not mangle `_` or `-` inside them
        if input[i..].starts_with("http://") || input[i..].starts_with("https://") {
            let end = scan_bare_url(&input[i..]);
            out.push_str(&input[i..i + end]);
            i += end;
            continue;
        }

        // 7. Inline emphasis: `*bold*`, `_italic_`, `-strike-`
        if matches!(bytes[i], b'*' | b'_' | b'-') {
            let delim = bytes[i];
            if let Some((inner, next_i)) = parse_jira_emphasis(input, i, delim) {
                let converted = convert_inlines(inner, in_cell);
                match delim {
                    b'*' => {
                        out.push_str("**");
                        out.push_str(&converted);
                        out.push_str("**");
                    }
                    b'_' => {
                        out.push('*');
                        out.push_str(&converted);
                        out.push('*');
                    }
                    b'-' => {
                        out.push_str("~~");
                        out.push_str(&converted);
                        out.push_str("~~");
                    }
                    _ => unreachable!(),
                }
                i = next_i;
                continue;
            }
        }

        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

fn parse_jira_monospace(input: &str, start: usize) -> Option<(String, usize)> {
    let mut i = start + 2;
    let bytes = input.as_bytes();
    let mut content = String::new();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && matches!(bytes[i + 1], b'{' | b'}') {
            content.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if input[i..].starts_with("}}") {
            return Some((content, i + 2));
        }
        let ch = input[i..].chars().next().unwrap();
        content.push(ch);
        i += ch.len_utf8();
    }
    None
}

fn format_markdown_code_span(code: &str) -> String {
    let mut max_run = 0usize;
    let mut cur_run = 0usize;
    for b in code.bytes() {
        if b == b'`' {
            cur_run += 1;
            max_run = max_run.max(cur_run);
        } else {
            cur_run = 0;
        }
    }
    let ticks = "`".repeat(max_run + 1);
    let needs_pad = code.starts_with('`')
        || code.ends_with('`')
        || (code.starts_with(' ') && code.ends_with(' ') && !code.trim().is_empty());
    if needs_pad {
        format!("{ticks} {code} {ticks}")
    } else {
        format!("{ticks}{code}{ticks}")
    }
}

/// Match `{macro}` or `{macro:params}` starting at `s[0] == '{'`.
/// Returns byte length of the full `{...}` tag.
fn match_jira_macro_tag(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'{' {
        return None;
    }
    if !bytes[1].is_ascii_alphabetic() {
        return None;
    }
    let mut i = 2;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
    {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'}' {
        return Some(i + 1);
    }
    if bytes[i] == b':' {
        let rest = &s[i + 1..];
        let close = rest.find('}')?;
        if rest[..close].contains('\n') {
            return None;
        }
        return Some(i + 1 + close + 1);
    }
    None
}

fn parse_jira_bracket(input: &str, start: usize, in_cell: bool) -> Option<(String, usize)> {
    let rest = &input[start + 1..];
    let close = rest.find(']')?;
    let inner = &rest[..close];
    if inner.is_empty() || inner.contains('\n') || inner.contains('[') {
        return None;
    }
    let next_i = start + 1 + close + 1;

    // Mention: `[~username]`
    if let Some(user) = inner.strip_prefix('~') {
        if !user.is_empty()
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Some((format!("@{user}"), next_i));
        }
    }

    // Link with label: `[text|url]` or `[text|url|title]`
    if let Some((label, after_pipe)) = inner.split_once('|') {
        let target = after_pipe.split('|').next().unwrap_or("").trim();
        let label = label.trim();
        if !target.is_empty() {
            if label.is_empty() {
                return Some((format!("<{target}>"), next_i));
            }
            let rendered_label = convert_inlines(label, in_cell);
            return Some((format!("[{rendered_label}]({target})"), next_i));
        }
    }

    // Bare URL link: `[https://example.com]` / `[mailto:user@example.com]`
    if inner.starts_with("http://")
        || inner.starts_with("https://")
        || inner.starts_with("mailto:")
        || inner.starts_with("ftp://")
    {
        return Some((format!("<{inner}>"), next_i));
    }

    None
}

fn parse_jira_image(input: &str, start: usize) -> Option<(String, usize)> {
    // Word boundary before `!`
    if let Some(prev) = input[..start].chars().next_back() {
        if prev.is_alphanumeric() {
            return None;
        }
    }
    let rest = &input[start + 1..];
    let close = rest.find('!')?;
    let inner = &rest[..close];
    if inner.is_empty() || inner.contains('\n') || inner.contains(' ') {
        return None;
    }
    let target = inner.split('|').next().unwrap_or("");
    let looks_like_image = target.starts_with("http://")
        || target.starts_with("https://")
        || target.ends_with(".png")
        || target.ends_with(".jpg")
        || target.ends_with(".jpeg")
        || target.ends_with(".gif")
        || target.ends_with(".svg")
        || target.ends_with(".webp");
    if !looks_like_image {
        return None;
    }
    Some((format!("![]({target})"), start + 1 + close + 1))
}

fn scan_bare_url(s: &str) -> usize {
    let mut end = 0;
    for (idx, ch) in s.char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | ']') {
            break;
        }
        end = idx + ch.len_utf8();
    }
    // Trim trailing sentence punctuation.
    while end > 0 && matches!(s.as_bytes()[end - 1], b'.' | b',' | b';' | b')') {
        end -= 1;
    }
    end
}

fn parse_jira_emphasis(input: &str, start: usize, delim: u8) -> Option<(&str, usize)> {
    let bytes = input.as_bytes();
    // Opening boundary: preceding char must not be alphanumeric or the same delimiter.
    if let Some(prev) = input[..start].chars().next_back() {
        if prev.is_alphanumeric() || prev == delim as char {
            return None;
        }
    }
    // Character right after opening delimiter must exist, not be whitespace, not be same delimiter.
    let after_open = &input[start + 1..];
    let first_ch = after_open.chars().next()?;
    if first_ch.is_whitespace() || first_ch == delim as char {
        return None;
    }

    // Scan for matching closing delimiter on the same line.
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            return None;
        }
        if input[i..].starts_with("{{") {
            if let Some((_, next_i)) = parse_jira_monospace(input, i) {
                i = next_i;
                continue;
            }
        }
        if bytes[i] == delim && i > start + 1 {
            let prev_ch = input[..i].chars().next_back()?;
            let next_ch = input[i + 1..].chars().next();
            let valid_prev = !prev_ch.is_whitespace() && prev_ch != delim as char;
            let valid_next = next_ch.is_none_or(|c| !c.is_alphanumeric() && c != delim as char);
            if valid_prev && valid_next {
                return Some((&input[start + 1..i], i + 1));
            }
        }
        let ch = input[i..].chars().next().unwrap();
        i += ch.len_utf8();
    }
    None
}

// ---------------------------------------------------------------------------
// Conservative auto-detect heuristic for Smart Paste
// ---------------------------------------------------------------------------

/// Return `true` only when `text` contains unmistakable Jira Server/DC wiki
/// markup signals and does not already look like a Markdown document with
/// fenced code blocks.
pub fn looks_like_jira_wiki(text: &str) -> bool {
    let normalized = normalize_newlines(text);
    if normalized.trim().is_empty() {
        return false;
    }

    // If the text already contains Markdown code fences, do not auto-convert.
    for line in normalized.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            return false;
        }
    }

    // Check line-level unmistakable Jira block markers.
    for line in normalized.lines() {
        let trimmed = line.trim();
        if parse_heading_line(trimmed).is_some()
            || parse_code_macro_open(trimmed).is_some()
            || parse_simple_macro_open(trimmed, "quote").is_some()
            || parse_panel_macro_open(trimmed).is_some()
            || parse_bq_line(trimmed).is_some()
        {
            return true;
        }
        // Jira table header row: `||Header 1||Header 2||`
        if trimmed.len() >= 5
            && trimmed.starts_with("||")
            && trimmed.ends_with("||")
            && trimmed[2..trimmed.len() - 2].contains(|c: char| c != '|' && !c.is_whitespace())
        {
            return true;
        }
    }

    // Check inline unmistakable Jira signals: `[text|http(s)://...]` or `[~user]`.
    if has_jira_external_link(&normalized) || has_jira_mention(&normalized) {
        return true;
    }

    false
}

fn has_jira_external_link(s: &str) -> bool {
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find(']') else {
            break;
        };
        let inner = &rest[..close];
        if !inner.contains('\n') && !inner.contains('[') {
            if let Some((label, after_pipe)) = inner.split_once('|') {
                let target = after_pipe.split('|').next().unwrap_or("").trim();
                if !label.trim().is_empty()
                    && (target.starts_with("http://")
                        || target.starts_with("https://")
                        || target.starts_with("mailto:"))
                {
                    return true;
                }
            }
        }
        rest = &rest[close + 1..];
    }
    false
}

fn has_jira_mention(s: &str) -> bool {
    let mut rest = s;
    while let Some(open) = rest.find("[~") {
        rest = &rest[open + 2..];
        let Some(close) = rest.find(']') else {
            break;
        };
        let user = &rest[..close];
        if !user.is_empty()
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return true;
        }
        rest = &rest[close + 1..];
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_to_jira_headings_and_emphasis() {
        let md =
            "# Title\n\n## Subtitle\n\nPlain **bold** and *italic* and ~~strike~~ and `code_val`.";
        let jira = markdown_to_jira(md);
        assert_eq!(
            jira,
            "h1. Title\n\nh2. Subtitle\n\nPlain *bold* and _italic_ and -strike- and {{code_val}}."
        );
    }

    #[test]
    fn jira_to_markdown_headings_and_emphasis_word_boundaries() {
        let jira = "h1. Title\n\nh3. Sub\n\nUse *bold*, _italic_, -strike-, {{inline_code}} and keep foo_bar_baz and foo-bar-baz intact.";
        let md = jira_to_markdown(jira);
        assert_eq!(
            md,
            "# Title\n\n### Sub\n\nUse **bold**, *italic*, ~~strike~~, `inline_code` and keep foo_bar_baz and foo-bar-baz intact."
        );
    }

    #[test]
    fn nested_and_mixed_lists_round_trip() {
        let md = "\
- Alpha
  - Beta
  1. Gamma
     - Delta
- Epsilon

1. First
   1. Second
   - Third";
        let jira = markdown_to_jira(md);
        assert_eq!(
            jira,
            "\
* Alpha
** Beta
*# Gamma
*#* Delta
* Epsilon

# First
## Second
#* Third"
        );
        let back = jira_to_markdown(&jira);
        assert_eq!(back, md);
    }

    #[test]
    fn links_bare_urls_and_mentions() {
        let jira = "See [Design Doc|https://example.invalid/doc_v1] and [https://example.invalid/raw_path] cc [~jdoe].";
        let md = jira_to_markdown(jira);
        assert_eq!(
            md,
            "See [Design Doc](https://example.invalid/doc_v1) and <https://example.invalid/raw_path> cc @jdoe."
        );

        let jira_back =
            markdown_to_jira("See [Design Doc](https://example.invalid/doc_v1) and <https://example.invalid/raw_path>.");
        assert_eq!(
            jira_back,
            "See [Design Doc|https://example.invalid/doc_v1] and [https://example.invalid/raw_path]."
        );
    }

    #[test]
    fn code_blocks_with_language_title_and_noformat() {
        let jira = "\
{code:title=Example.rs|rust}
fn main() {
    println!(\"hi\");
}
{code}

{noformat}
raw *not bold* _not italic_
{noformat}";
        let md = jira_to_markdown(jira);
        assert_eq!(
            md,
            "\
```rust
fn main() {
    println!(\"hi\");
}
```

```
raw *not bold* _not italic_
```"
        );

        let md_code = "```python\ndef f():\n    return 1\n```";
        let jira_code = markdown_to_jira(md_code);
        assert_eq!(jira_code, "{code:python}\ndef f():\n    return 1\n{code}");
        assert_eq!(jira_to_markdown(&jira_code), md_code);
    }

    #[test]
    fn blockquotes_bq_and_panel() {
        let jira = "\
{quote}
Quoted *line*
{quote}

bq. Short quote

{panel:title=Heads up|bgColor=#eee}
Panel _body_
{panel}";
        let md = jira_to_markdown(jira);
        assert_eq!(
            md,
            "\
> Quoted **line**

> Short quote

> **Heads up**
>
> Panel *body*"
        );
    }

    #[test]
    fn tables_with_links_round_trip() {
        let jira = "\
||Key||Summary||Link||
|DEMO-1|Fix *bug*|[Spec|https://example.invalid/spec]|
|DEMO-2|Keep `code`|{{val}}|";
        let md = jira_to_markdown(jira);
        assert_eq!(
            md,
            "\
| Key | Summary | Link |
| --- | --- | --- |
| DEMO-1 | Fix **bug** | [Spec](https://example.invalid/spec) |
| DEMO-2 | Keep `code` | `val` |"
        );

        let back = markdown_to_jira(&md);
        assert_eq!(
            back,
            "\
||Key||Summary||Link||
|DEMO-1|Fix *bug*|[Spec|https://example.invalid/spec]|
|DEMO-2|Keep {{code}}|{{val}}|"
        );
    }

    #[test]
    fn horizontal_rule_round_trip() {
        assert_eq!(
            markdown_to_jira("Above\n\n---\n\nBelow"),
            "Above\n\n----\n\nBelow"
        );
        assert_eq!(
            jira_to_markdown("Above\n\n----\n\nBelow"),
            "Above\n\n---\n\nBelow"
        );
    }

    #[test]
    fn unknown_macros_are_preserved_verbatim() {
        let jira = "\
{toc:maxLevel=3}

Status is {status:colour=Green|title=IN_PROGRESS} and {color:#ff0000}red{color}.

{anchor:section_one}

Custom {my-macro:foo_bar=1|baz=2} stays intact.";
        let md = jira_to_markdown(jira);
        assert!(md.contains("{toc:maxLevel=3}"));
        assert!(md.contains("{status:colour=Green|title=IN_PROGRESS}"));
        assert!(md.contains("{color:#ff0000}red{color}"));
        assert!(md.contains("{anchor:section_one}"));
        assert!(md.contains("{my-macro:foo_bar=1|baz=2}"));

        // And Markdown -> Jira preserves unknown macros verbatim too.
        let back = markdown_to_jira(&md);
        assert!(back.contains("{toc:maxLevel=3}"));
        assert!(back.contains("{status:colour=Green|title=IN_PROGRESS}"));
        assert!(back.contains("{anchor:section_one}"));
        assert!(back.contains("{my-macro:foo_bar=1|baz=2}"));
    }

    #[test]
    fn full_document_round_trip_markdown_jira_markdown() {
        let md = "\
# Release Checklist

Verify **core** behavior, _edge cases_, ~~deprecated~~ paths, and `token_id`.

- Prepare build
  - Run tests
  1. Check logs
- Publish

See [Release Notes](https://example.invalid/notes) or <https://example.invalid/status>.

```rust
fn check(x: usize) -> bool {
    x > 0
}
```

> Important reminder

---

| Area | Owner |
| --- | --- |
| Parser | Team A |
| Shell | Team B |";

        let jira = markdown_to_jira(md);
        let round_tripped = jira_to_markdown(&jira);
        // Normalize `_edge cases_` -> `*edge cases*` for comparison.
        let expected = md.replace("_edge cases_", "*edge cases*");
        assert_eq!(round_tripped, expected);
    }

    #[test]
    fn auto_detect_positive_cases() {
        assert!(looks_like_jira_wiki("h1. Project Overview\n\nDetails"));
        assert!(looks_like_jira_wiki("h2. Steps to reproduce"));
        assert!(looks_like_jira_wiki("{code:java}\nclass A {}\n{code}"));
        assert!(looks_like_jira_wiki("{noformat}\nstack trace\n{noformat}"));
        assert!(looks_like_jira_wiki("{quote}\nHello\n{quote}"));
        assert!(looks_like_jira_wiki("{panel:title=Note}\nBody\n{panel}"));
        assert!(looks_like_jira_wiki("bq. Single line quote"));
        assert!(looks_like_jira_wiki("||Col A||Col B||\n|1|2|"));
        assert!(looks_like_jira_wiki(
            "See [Docs|https://example.invalid/docs]"
        ));
        assert!(looks_like_jira_wiki("Assigned to [~alice.smith]"));
    }

    #[test]
    fn auto_detect_negative_cases() {
        assert!(!looks_like_jira_wiki(""));
        assert!(!looks_like_jira_wiki(
            "Just a plain sentence with no markup."
        ));
        assert!(!looks_like_jira_wiki(
            "# Markdown Heading\n\n**bold** and *italic*"
        ));
        assert!(!looks_like_jira_wiki(
            "# Doc\n\n```rust\nfn main() {}\n```\n\nMentions {code} in prose."
        ));
        assert!(!looks_like_jira_wiki(
            "fn test(a: bool, b: bool) { if a || b { return; } }"
        ));
        assert!(!looks_like_jira_wiki(
            "{\"key\": \"value\", \"arr\": [1, 2, 3]}"
        ));
        assert!(!looks_like_jira_wiki("Template {{ user.name }} in Jinja"));
        assert!(!looks_like_jira_wiki(
            "https://example.invalid/path_with_underscores"
        ));
        assert!(!looks_like_jira_wiki(
            "| Col A | Col B |\n| --- | --- |\n| 1 | 2 |"
        ));
        assert!(!looks_like_jira_wiki("a\tb\n1\t2"));
    }
}
