//! Plain-text converters: TSV grids, code languages, insertion context.

use crate::render::{plain_cell, render_table};
use crate::{InsertContext, VsCodeHint};

/// Labels code blocks carry in chat UIs and editors, lowercase.
pub const KNOWN_LANGUAGES: &[&str] = &[
    "bash",
    "c",
    "c#",
    "c++",
    "clojure",
    "cpp",
    "cs",
    "csharp",
    "css",
    "dart",
    "diff",
    "docker",
    "dockerfile",
    "elixir",
    "erlang",
    "go",
    "golang",
    "graphql",
    "groovy",
    "haskell",
    "html",
    "ini",
    "java",
    "javascript",
    "js",
    "json",
    "jsonc",
    "jsx",
    "julia",
    "kotlin",
    "kt",
    "latex",
    "less",
    "lua",
    "makefile",
    "markdown",
    "matlab",
    "md",
    "nginx",
    "nix",
    "objc",
    "objective-c",
    "objectivec",
    "ocaml",
    "perl",
    "php",
    "powershell",
    "proto",
    "protobuf",
    "py",
    "python",
    "r",
    "rb",
    "ruby",
    "rust",
    "scala",
    "scss",
    "sh",
    "shell",
    "sql",
    "svelte",
    "swift",
    "tex",
    "toml",
    "ts",
    "tsx",
    "typescript",
    "vb",
    "vue",
    "xml",
    "yaml",
    "zsh",
];

/// Canonical fence language for an editor/highlighter id, or `None` for
/// plain text and ids that are not a language.
pub fn normalize_language(id: &str) -> Option<String> {
    let id = id.trim().trim_end_matches(';').to_ascii_lowercase();
    let id = id.strip_prefix("source-").unwrap_or(&id).to_string();
    let mapped = match id.as_str() {
        "" | "plaintext" | "plain" | "text" | "txt" | "none" | "nohighlight" | "plain text"
        | "plain-text" | "markdown-plain" => return None,
        "typescriptreact" => "tsx",
        "javascriptreact" => "jsx",
        "shellscript" | "shell" | "sh" | "shell-session" | "console" => "bash",
        "js" | "node" => "javascript",
        "ts" => "typescript",
        "py" | "python3" => "python",
        "rs" => "rust",
        "rb" => "ruby",
        "yml" => "yaml",
        "c++" => "cpp",
        "c#" | "cs" => "csharp",
        "objective-c" | "objc" => "objectivec",
        "objective-cpp" => "objectivecpp",
        "md" | "text-md" | "gfm" => "markdown",
        "golang" => "go",
        "kt" => "kotlin",
        "ps1" | "pwsh" => "powershell",
        "text-html-basic" | "text-html" | "htm" => "html",
        "jsonc" | "json5" => "json",
        "docker" => "dockerfile",
        "protobuf" => "proto",
        "tex" => "latex",
        other => other,
    };
    let ok = mapped.len() <= 24
        && mapped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | '_' | '.'))
        && mapped
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic());
    ok.then(|| mapped.to_string())
}

/// Reads VS Code's `vscode-editor-data` clipboard JSON
/// (`{"version":1,"isFromEmptySelection":false,"mode":"rust"}`).
pub fn vscode_language(editor_data_json: &str) -> Option<VsCodeHint> {
    let value: serde_json::Value =
        serde_json::from_str(editor_data_json.trim_matches('\0').trim()).ok()?;
    let object = value.as_object()?;
    let mode = object.get("mode").and_then(|m| m.as_str()).unwrap_or("");
    let lower = mode.to_ascii_lowercase();
    let is_text = matches!(lower.as_str(), "markdown" | "plaintext" | "" | "text");
    let language = if is_text && lower != "markdown" {
        None
    } else {
        normalize_language(mode)
    };
    Some(VsCodeHint { language, is_text })
}

/// A language only when the code leaves no doubt; `None` otherwise.
pub fn guess_language(code: &str) -> Option<&'static str> {
    let trimmed = code.trim_start();
    if let Some(first) = trimmed.lines().next().filter(|l| l.starts_with("#!")) {
        let interp = first.trim_start_matches("#!").trim();
        let interp = interp
            .strip_prefix("/usr/bin/env")
            .map(str::trim)
            .unwrap_or(interp);
        let name = interp
            .split_whitespace()
            .find(|t| !t.starts_with('-'))
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("");
        let name = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        return match name {
            "bash" | "sh" | "zsh" | "dash" | "ksh" => Some("bash"),
            "python" => Some("python"),
            "node" | "deno" | "bun" => Some("javascript"),
            "ruby" => Some("ruby"),
            "perl" => Some("perl"),
            "php" => Some("php"),
            "fish" => Some("fish"),
            "pwsh" => Some("powershell"),
            _ => None,
        };
    }
    if trimmed.starts_with("<?php") {
        return Some("php");
    }
    let lower_head: String = trimmed
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    if lower_head.starts_with("<!doctype html") || lower_head.starts_with("<html") {
        return Some("html");
    }
    if lower_head.starts_with("<?xml") {
        return Some("xml");
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && trimmed.len() < 4_000_000
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return Some("json");
    }
    let has = |s: &str| code.contains(s);
    let line_starts = |s: &str| code.lines().any(|l| l.trim_start().starts_with(s));
    if (has("fn ")
        && (has("let mut ")
            || has("impl ")
            || has("pub fn ")
            || has("-> ")
            || has("&self")
            || has("::new(")))
        || line_starts("use std::")
        || has("fn main() {")
    {
        return Some("rust");
    }
    if line_starts("package main") || (line_starts("func ") && has(":=")) {
        return Some("go");
    }
    if line_starts("#include <") || line_starts("#include \"") {
        return Some(
            if has("std::") || has("<iostream>") || has("class ") || has("template<") {
                "cpp"
            } else {
                "c"
            },
        );
    }
    if line_starts("using System") || (has("namespace ") && has("public class ") && has("{ get;")) {
        return Some("csharp");
    }
    if has("public static void main(")
        || (line_starts("import java.") || line_starts("package ") && has(";")) && has("class ")
    {
        return Some("java");
    }
    if line_starts("fun ") && (has("val ") || has("println(") || has("): ")) && !has(";\n") {
        return Some("kotlin");
    }
    if line_starts("func ") && !has(":=") && (has(") -> ") || has("let ") || has("guard ")) {
        return Some("swift");
    }
    if line_starts("import SwiftUI")
        || line_starts("import UIKit")
        || line_starts("import Foundation") && (has("func ") || has("let ") || has("var "))
    {
        return Some("swift");
    }
    let py_def = code.lines().any(|l| {
        let l = l.trim_start();
        (l.starts_with("def ") || l.starts_with("class ") || l.starts_with("async def "))
            && l.trim_end().ends_with(':')
    });
    let py_import = line_starts("from ") && has(" import ")
        || line_starts("import ") && !has(";") && !has(" from ");
    if py_def && !has("{") || py_def && py_import || line_starts("if __name__ ==") {
        return Some("python");
    }
    let ts_markers = has("interface ") && has(": ")
        || has(": string")
        || has(": number")
        || has(": boolean")
        || line_starts("type ") && has(" = {");
    let js_markers = has("console.log(")
        || has("require(")
        || line_starts("export default")
        || line_starts("export const ")
        || line_starts("import ") && has(" from '")
        || line_starts("import ") && has(" from \"")
        || has("=> {")
        || line_starts("const ") && has(" = (");
    if js_markers || ts_markers && (has("const ") || has("export ") || has("function ")) {
        return Some(if ts_markers {
            "typescript"
        } else {
            "javascript"
        });
    }
    let sql_head = lower_head.trim_start();
    if [
        "select ",
        "insert into ",
        "update ",
        "delete from ",
        "create table ",
        "alter table ",
        "with ",
    ]
    .iter()
    .any(|k| sql_head.starts_with(k))
    {
        let lower = code.to_ascii_lowercase();
        // Prose starts with "Select" too: lowercase keywords need more
        // structure than a "from".
        let keyword_upper = trimmed
            .split_whitespace()
            .next()
            .is_some_and(|w| w.chars().all(|c| c.is_ascii_uppercase()));
        let structured = lower.trim_end().ends_with(';')
            || lower.contains(" where ")
            || lower.contains("\nwhere ")
            || sql_head.starts_with("select *")
            || sql_head.starts_with("select count(");
        if !(keyword_upper || structured) {
            return None;
        }
        if lower.contains(" from ")
            || lower.contains("\nfrom ")
            || lower.contains(" set ")
            || lower.contains(" values")
            || sql_head.starts_with("create table")
            || sql_head.starts_with("alter table")
        {
            return Some("sql");
        }
    }
    // Every line a familiar shell command.
    const COMMANDS: [&str; 34] = [
        "sudo",
        "apt",
        "apt-get",
        "brew",
        "npm",
        "npx",
        "yarn",
        "pnpm",
        "cargo",
        "git",
        "cd",
        "ls",
        "curl",
        "wget",
        "docker",
        "kubectl",
        "pip",
        "pip3",
        "export",
        "echo",
        "mkdir",
        "systemctl",
        "rm",
        "cp",
        "mv",
        "chmod",
        "chown",
        "ssh",
        "scp",
        "make",
        "go",
        "rustup",
        "source",
        "cat",
    ];
    let mut lines = 0;
    let all_shell = code
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .all(|l| {
            lines += 1;
            let l = l.strip_prefix("$ ").unwrap_or(l);
            let first = l.split_whitespace().next().unwrap_or("");
            COMMANDS.contains(&first) && l.contains(' ')
        });
    if all_shell && lines >= 1 {
        return Some("bash");
    }
    None
}

/// Spreadsheet text (rows of tab-separated cells) as a GFM table. `None`
/// unless there are at least two rows and two columns with a consistent
/// column count.
pub fn tsv_to_table(text: &str) -> Option<String> {
    let text = text.replace("\r\n", "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    if !text.contains('\t') {
        return None;
    }
    let rows = parse_tsv(text)?;
    if rows.len() < 2 {
        return None;
    }
    let cols = rows[0].len();
    if cols < 2 || rows.iter().any(|r| r.len() != cols) {
        return None;
    }
    // Tab-indented code is not a grid.
    if rows.iter().all(|r| r[0].trim().is_empty()) {
        return None;
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| r.iter().map(|c| plain_cell(c)).collect())
        .collect();
    Some(render_table(&cells))
}

/// Excel's TSV: a cell holding a tab, newline or quote is quoted, with `""`
/// for a quote inside. `None` at the first row whose column count differs
/// from the first row's (text with tabs that is not a grid is not parsed to
/// the end).
fn parse_tsv(text: &str) -> Option<Vec<Vec<String>>> {
    let mut cols: Option<usize> = None;
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut at_cell_start = true;
    while let Some(c) = chars.next() {
        if at_cell_start && c == '"' {
            // Quoted cell.
            let mut closed = false;
            let mut quoted = String::new();
            while let Some(q) = chars.next() {
                if q == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        quoted.push('"');
                    } else {
                        closed = true;
                        break;
                    }
                } else {
                    quoted.push(q);
                }
            }
            if !closed || !matches!(chars.peek(), None | Some('\t') | Some('\n')) {
                // Not a well-formed quoted cell: keep it literally.
                cell.push('"');
                cell.push_str(&quoted);
                if closed {
                    cell.push('"');
                }
            } else {
                cell.push_str(&quoted);
            }
            at_cell_start = false;
            continue;
        }
        match c {
            '\t' => {
                row.push(std::mem::take(&mut cell));
                at_cell_start = true;
            }
            '\n' => {
                row.push(std::mem::take(&mut cell));
                if *cols.get_or_insert(row.len()) != row.len() {
                    return None;
                }
                rows.push(std::mem::take(&mut row));
                at_cell_start = true;
            }
            c => {
                cell.push(c);
                at_cell_start = false;
            }
        }
    }
    row.push(cell);
    if cols.is_some_and(|cols| cols != row.len()) {
        return None;
    }
    rows.push(row);
    Some(rows)
}

/// Fits converted Markdown to the caret's line: block output on its own
/// lines, continuation lines indented under a list item and prefixed inside
/// a blockquote.
pub fn adapt_to_context(markdown: &str, block: bool, ctx: &InsertContext) -> String {
    if ctx.in_code_block {
        return markdown.to_string();
    }
    let (quote, indent, at_line_start) = container_of(ctx.line_prefix_before_caret);
    let cont = format!("{quote}{indent}");
    let empty_cont = quote.trim_end().to_string();
    let markdown = markdown.trim_end_matches('\n');
    let mut body = String::with_capacity(markdown.len() + 16);
    for (k, line) in markdown.split('\n').enumerate() {
        if k > 0 {
            body.push('\n');
            if line.is_empty() {
                body.push_str(&empty_cont);
            } else {
                body.push_str(&cont);
            }
        }
        body.push_str(line);
    }
    if !block {
        return body;
    }
    let mut out = String::with_capacity(body.len() + 2 * cont.len() + 4);
    if !at_line_start {
        out.push('\n');
        out.push_str(&empty_cont);
        out.push('\n');
        out.push_str(&cont);
    }
    out.push_str(&body);
    if !ctx.rest_of_line.trim().is_empty() {
        out.push('\n');
        out.push_str(&empty_cont);
        out.push('\n');
        out.push_str(&cont);
    }
    out
}

/// `("> > ", "   ", at_line_start)` for the text before the caret.
fn container_of(prefix: &str) -> (String, String, bool) {
    let mut rest = prefix;
    let mut quote = String::new();
    loop {
        let spaces = rest.len() - rest.trim_start_matches(' ').len();
        if spaces <= 3 && rest[spaces..].starts_with('>') {
            rest = &rest[spaces + 1..];
            rest = rest.strip_prefix(' ').unwrap_or(rest);
            quote.push_str("> ");
        } else {
            break;
        }
    }
    let lead_len = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let lead = &rest[..lead_len];
    let after = &rest[lead_len..];
    let marker_len = list_marker_len(after);
    let (indent, content) = match marker_len {
        Some(m) => {
            let spaces = after[m..].len() - after[m..].trim_start_matches(' ').len();
            let spaces = if spaces == 0 || spaces > 4 { 1 } else { spaces };
            let content = &after[(m + spaces).min(after.len())..];
            let content = strip_task(content);
            (format!("{lead}{}", " ".repeat(m + spaces)), content)
        }
        None => (lead.to_string(), after),
    };
    (quote, indent, content.trim().is_empty())
}

fn list_marker_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let followed = |k: usize| k == b.len() || b[k] == b' ' || b[k] == b'\t';
    match b.first() {
        Some(b'-' | b'*' | b'+') if followed(1) => Some(1),
        Some(c) if c.is_ascii_digit() => {
            let d = b.iter().take_while(|c| c.is_ascii_digit()).count();
            (d <= 9 && matches!(b.get(d), Some(b'.' | b')')) && followed(d + 1)).then_some(d + 1)
        }
        _ => None,
    }
}

fn strip_task(s: &str) -> &str {
    for t in ["[ ] ", "[x] ", "[X] ", "[ ]", "[x]", "[X]"] {
        if let Some(rest) = s.strip_prefix(t) {
            return rest;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(before: &'a str, after: &'a str) -> InsertContext<'a> {
        InsertContext {
            line_prefix_before_caret: before,
            rest_of_line: after,
            in_code_block: false,
        }
    }

    #[test]
    fn context_list_and_quote() {
        assert_eq!(
            adapt_to_context("a\nb", false, &ctx("- item ", "")),
            "a\n  b"
        );
        assert_eq!(
            adapt_to_context("a\n\nb", false, &ctx("> ", "")),
            "a\n>\n> b"
        );
        assert_eq!(
            adapt_to_context("a\nb", false, &ctx("  1. ", "")),
            "a\n     b"
        );
        assert_eq!(
            adapt_to_context("| a |\n| --- |", true, &ctx("text ", " tail")),
            "\n\n| a |\n| --- |\n\n tail".replace(" tail", "")
        );
        assert_eq!(adapt_to_context("# H", true, &ctx("", "")), "# H");
        assert_eq!(
            adapt_to_context("```\nx\n```", true, &ctx("> - [ ] ", "")),
            "```\n> x\n> ```"
                .replace("> x", ">   x")
                .replace("> ```", ">   ```")
        );
    }

    #[test]
    fn languages() {
        assert_eq!(
            guess_language("#!/usr/bin/env python3\nprint(1)"),
            Some("python")
        );
        assert_eq!(
            guess_language("fn main() {\n    println!(\"hi\");\n}"),
            Some("rust")
        );
        assert_eq!(guess_language("hello world\nsecond line"), None);
        assert_eq!(guess_language("{\"a\": 1}"), Some("json"));
        assert_eq!(
            guess_language("SELECT a, b\nFROM t WHERE x = 1"),
            Some("sql")
        );
        assert_eq!(guess_language("def f(x):\n    return x"), Some("python"));
        assert_eq!(
            guess_language("brew install ripgrep\ncargo build --release"),
            Some("bash")
        );
        assert_eq!(guess_language("package main\n\nfunc main() {}"), Some("go"));
    }
}
