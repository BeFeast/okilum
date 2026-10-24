//! Plain-text paths: TSV grids, VS Code editor data, code fences, URLs,
//! page titles and insertion context.

use std::path::PathBuf;

use okilum_paste::{
    adapt_to_context, extract_title, fence_code, guess_language, link_over_selection, single_url,
    titled_link, tsv_to_table, vscode_language, InsertContext, VsCodeHint,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fixture_str(name: &str) -> String {
    String::from_utf8(fixture(name)).unwrap()
}

// ---- TSV ------------------------------------------------------------------

#[test]
fn tsv_excel_range_crlf() {
    assert_eq!(
        tsv_to_table(&fixture_str("tsv_excel.txt")).unwrap(),
        "| Name           | Qty |     Price |\n\
         | -------------- | --: | --------: |\n\
         | Apples         |  12 | $1,234.50 |\n\
         | Pears \\| green |   3 |       -7% |"
    );
}

#[test]
fn tsv_quoted_cells_with_newlines_and_quotes() {
    assert_eq!(
        tsv_to_table(&fixture_str("tsv_quoted.txt")).unwrap(),
        "| Task         | Notes                |\n\
         | ------------ | -------------------- |\n\
         | Ship \"paste\" | line one<br>line two |\n\
         | Review       |                      |"
    );
}

#[test]
fn tsv_rejects_non_grids() {
    assert_eq!(tsv_to_table("a\tb"), None, "one row");
    assert_eq!(tsv_to_table("only one\ncolumn"), None, "no tabs");
    assert_eq!(tsv_to_table("a\tb\nc"), None, "inconsistent columns");
    assert_eq!(
        tsv_to_table("fn x() {\n\tlet a = 1;\n\tlet b = 2;\n}"),
        None,
        "tab-indented code"
    );
    assert_eq!(tsv_to_table("x\n\ty\n\tz"), None);
    assert_eq!(tsv_to_table(""), None);
}

#[test]
fn tsv_minimal_grid() {
    assert_eq!(
        tsv_to_table("a\tb\r\nc\td\r\n").unwrap(),
        "| a   | b   |\n| --- | --- |\n| c   | d   |"
    );
}

// ---- VS Code --------------------------------------------------------------

#[test]
fn vscode_editor_data_set() {
    let hint = |name: &str| vscode_language(&fixture_str(name)).unwrap();
    let lang = |l: &str| VsCodeHint {
        language: Some(l.into()),
        is_text: false,
    };
    assert_eq!(hint("vscode_tsx.json"), lang("tsx"));
    assert_eq!(hint("vscode_jsx.json"), lang("jsx"));
    assert_eq!(hint("vscode_shell.json"), lang("bash"));
    assert_eq!(hint("vscode_rust.json"), lang("rust"));
    assert_eq!(hint("vscode_python_multicursor.json"), lang("python"));
    assert!(hint("vscode_markdown.json").is_text);
    assert_eq!(
        hint("vscode_plaintext.json"),
        VsCodeHint {
            language: None,
            is_text: true
        }
    );
    assert_eq!(
        vscode_language(r#"{"mode":"jsonc"}"#).unwrap(),
        lang("json")
    );
    assert_eq!(
        vscode_language(r#"{"mode":"objective-c"}"#).unwrap(),
        lang("objectivec")
    );
    assert_eq!(vscode_language("not json"), None);
    assert_eq!(vscode_language("[1,2]"), None);
}

// ---- Code -----------------------------------------------------------------

#[test]
fn fences() {
    assert_eq!(
        fence_code("fn a() {}\n", Some("rust")),
        "```rust\nfn a() {}\n```"
    );
    assert_eq!(
        fence_code("let s = \"```\";", Some("rust")),
        "````rust\nlet s = \"```\";\n````"
    );
    assert_eq!(
        fence_code("a\r\nb\n\n", None),
        "```\na\nb\n\n```",
        "only one trailing newline trimmed"
    );
}

#[test]
fn guesses_are_conservative() {
    assert_eq!(guess_language("#!/bin/bash\necho hi"), Some("bash"));
    assert_eq!(
        guess_language("#!/usr/bin/env node\nconsole.log(1)"),
        Some("javascript")
    );
    assert_eq!(guess_language("<?php echo 1;"), Some("php"));
    assert_eq!(guess_language("use std::io;\nfn main() {}"), Some("rust"));
    assert_eq!(
        guess_language("#include <stdio.h>\nint main() {}"),
        Some("c")
    );
    assert_eq!(
        guess_language("import React from 'react';\nexport default function App() {}"),
        Some("javascript")
    );
    assert_eq!(
        guess_language("interface A { x: number }\nexport const a: A = { x: 1 };"),
        Some("typescript")
    );
    assert_eq!(guess_language("Dear Anna,\nthanks for the call."), None);
    assert_eq!(guess_language("- item one\n- item two"), None);
    assert_eq!(guess_language("Select the best option from the list"), None);
    assert_eq!(guess_language("With love, from Oleg\nand the team"), None);
    assert_eq!(
        guess_language("select id from users where age > 3"),
        Some("sql")
    );
}

// ---- URLs and titles ------------------------------------------------------

#[test]
fn urls_and_links() {
    assert_eq!(
        single_url("\thttps://example.com/x\n"),
        Some("https://example.com/x")
    );
    assert_eq!(single_url("see https://example.com"), None);
    assert_eq!(single_url("javascript:alert(1)"), None);
    assert_eq!(
        link_over_selection("Rust (lang)", "https://rust-lang.org"),
        "[Rust (lang)](https://rust-lang.org)"
    );
    assert_eq!(
        link_over_selection("a]b", "https://x.org/(a"),
        "[a\\]b](https://x.org/%28a)"
    );
    assert_eq!(
        titled_link("Smart paste", "https://x.org"),
        "[Smart paste](https://x.org)"
    );
}

#[test]
fn title_utf8_whitespace_collapsed() {
    assert_eq!(
        extract_title(&fixture("title_utf8.html"), Some("text/html")).as_deref(),
        Some("Okilum — быстрый Markdown редактор")
    );
}

#[test]
fn title_windows_1251_meta_charset() {
    assert_eq!(
        extract_title(&fixture("title_cp1251.html"), None).as_deref(),
        Some("Новости — Лента дня")
    );
    // The header wins over the meta.
    let utf8 = "<title>Привет</title>".as_bytes();
    assert_eq!(
        extract_title(utf8, Some("text/html; charset=UTF-8")).as_deref(),
        Some("Привет")
    );
}

#[test]
fn title_og_only() {
    assert_eq!(
        extract_title(&fixture("title_og_only.html"), None).as_deref(),
        Some("Smart paste in Okilum")
    );
}

#[test]
fn title_entities_and_truncated_head() {
    assert_eq!(
        extract_title(
            &fixture("title_entities.html"),
            Some("text/html; charset=utf-8")
        )
        .as_deref(),
        Some("Tom & Jerry – \"Cat & Mouse\" <1940> 🐱")
    );
    assert_eq!(
        extract_title(b"<html><head></head><body>no title</body>", None),
        None
    );
    assert_eq!(extract_title(b"", None), None);
}

// ---- Insertion context ----------------------------------------------------

fn ctx<'a>(before: &'a str, after: &'a str) -> InsertContext<'a> {
    InsertContext {
        line_prefix_before_caret: before,
        rest_of_line: after,
        in_code_block: false,
    }
}

#[test]
fn context_inline_and_blocks() {
    // Inline text stays on the caret's line.
    assert_eq!(
        adapt_to_context("word", false, &ctx("Hello ", " tail")),
        "word"
    );
    // A block mid-line gets its own lines on both sides.
    assert_eq!(
        adapt_to_context("| a | b |\n| --- | --- |", true, &ctx("Text ", "more")),
        "\n\n| a | b |\n| --- | --- |\n\n"
    );
    // At an empty line nothing is added.
    assert_eq!(
        adapt_to_context("```\nx\n```", true, &ctx("", "")),
        "```\nx\n```"
    );
    // Inside a fenced code block: verbatim.
    let code = InsertContext {
        line_prefix_before_caret: "    ",
        rest_of_line: "",
        in_code_block: true,
    };
    assert_eq!(adapt_to_context("a\n- b", false, &code), "a\n- b");
}

#[test]
fn context_list_items_and_quotes() {
    assert_eq!(
        adapt_to_context("one\ntwo", false, &ctx("- ", "")),
        "one\n  two"
    );
    assert_eq!(
        adapt_to_context("one\n\ntwo", false, &ctx("1. ", "")),
        "one\n\n   two"
    );
    assert_eq!(
        adapt_to_context("one\n\ntwo", false, &ctx("> ", "")),
        "one\n>\n> two"
    );
    assert_eq!(
        adapt_to_context("```sh\nls\n```", true, &ctx("> - [ ] Task ", "")),
        "\n>\n>   ```sh\n>   ls\n>   ```"
    );
}
