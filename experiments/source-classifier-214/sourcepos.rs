use comrak::{parse_document, Arena, Options};
fn main() {
    let cases = [
        ("nul", "**a\0b** [x](d)"),
        ("edge-inline", "### **bold** ###\n\n***nested*** ~~strike~~ ~single~ __under__ _italic_ [ a ](dest) [[Target]] [[Target|alias]]"),
        ("unsupported", "---\na: **x**\n---\n\n**bold** ![[embed]]\n\n| a | b |\n|---|---|\n|**x**|y|\n\n> **quote**\n\n- **list**"),
        ("cr-only", "**one**\r\r# two\r"),
        ("unicode-crlf", "\u{feff}# כותרת 🧠\r\n\r\né **жир** and *e\u{301}*\r\n"),
        ("tabs", "#\tTitle\n\n\t**indented**\n\ntext\t**bold**"),
        ("escapes-entities", "escaped \\*star\\* and **b&amp;d** and [a\\]b](target)"),
        ("code", "`` a ` b `` and ` x ` and `a\nb`"),
        ("links", "[label](target \"title\") [ref][id] [[Target|label]] ![[embed]]\n\n[id]: /url"),
        ("multiline", "**one\r\ntwo** and [multi\r\nlabel](dest)"),
    ];
    for (name, text) in cases {
        let arena = Arena::new();
        let mut options = Options::default();
        options.extension.strikethrough = true;
        options.extension.table = true;
        options.extension.wikilinks_title_after_pipe = true;
        options.extension.front_matter_delimiter = Some("---".into());
        let root = parse_document(&arena, text, &options);
        println!("CASE {name} {:?}", text);
        for node in root.descendants() {
            let data = node.data.borrow();
            let lines: Vec<_> = std::iter::once(0)
                .chain(text.match_indices('\n').map(|(n, _)| n + 1))
                .collect();
            let p = data.sourcepos;
            let range = p
                .start
                .line
                .checked_sub(1)
                .and_then(|l| lines.get(l))
                .and_then(|s| p.start.column.checked_sub(1).map(|c| s + c))
                .zip(
                    p.end
                        .line
                        .checked_sub(1)
                        .and_then(|l| lines.get(l))
                        .map(|s| s + p.end.column),
                );
            let raw = range.and_then(|(a, b)| text.get(a..b));
            println!("{:?} {:?} naive_raw={:?}", data.value, p, raw);
        }
    }
}
