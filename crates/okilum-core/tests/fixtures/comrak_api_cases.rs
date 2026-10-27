pub fn cases() -> Vec<String> {
    vec![
        "".into(),
        "# title\n\n**bold** *italic* ~~gone~~ [[note|label]]\n".into(),
        "[unused]: a.md \"title\"\n".into(),
        "[forward][id]\n\n[id]: <a&amp;b.md> 'a &amp; b'\n".into(),
        "[a b]: /first\n[a\u{a0}b]: /second\n\n[x][a B]\n".into(),
        "[Straẞe]: /first\n[STRASSE]: /second\n\n[x][strasse]\n".into(),
        "---\n[id]: /frontmatter\n---\n\n[id]: /real\n\n[x][id]\n".into(),
        "+++\n[id]: /frontmatter\n+++\n\n[x][id]\n".into(),
        "```md\n[id]: /fence\n```\n\n    [code]: /indented\n\n[x][id]\n".into(),
        "> [id]: /quote\n>\n> [inside][id]\n\n[outside][id]\n".into(),
        "- [id]: /list\n\n  [inside][id]\n\n[outside][id]\n".into(),
        "[id]: /first\n\n- [id]: /second\n\n  [inside][id]\n".into(),
        "[id]: /a\n  \"multiline title\"\n\n[x][id]\n".into(),
        "[id]: /a\nheading\n---\n\n[x][id]\n".into(),
        "\u{feff}# e\u{301} 👩‍💻 🇮🇱\r\n\r\n[x][id]\r\n\r\n[id]: /emoji 🧠\r\n".into(),
        "[escaped\\]]: /a\\*b \"&amp; title\"\n\n[x][escaped\\]]\n".into(),
        "| a | b |\n|---|---|\n| [x][id] | **b** |\n\n[id]: /table\n".into(),
        "<div>\n[id]: /html\n</div>\n\n[x][id]\n".into(),
        "![alt][id]\n\n[id]: /image 'title'\n".into(),
        "[missing] [x][missing]\n".into(),
        format!("[r]: /{}\n\n{}\n\n# boundary\n\n{}", "x".repeat(999), "[r] ".repeat(50), "[r] ".repeat(51)),
    ]
}

pub fn options(source: &str) -> comrak::Options<'static> {
    let mut options = comrak::Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.extension.front_matter_delimiter = Some(if source.starts_with("+++") { "+++" } else { "---" }.into());
    options
}

pub fn output<'a>(root: &'a comrak::nodes::AstNode<'a>, options: &comrak::Options<'_>) -> Vec<String> {
    // XML includes AST source positions; also preserve exact formatter results.
    let mut xml = String::new();
    let mut html = String::new();
    let mut commonmark = String::new();
    comrak::format_xml(root, options, &mut xml).unwrap();
    comrak::format_html(root, options, &mut html).unwrap();
    comrak::format_commonmark(root, options, &mut commonmark).unwrap();
    vec![xml, html, commonmark]
}
