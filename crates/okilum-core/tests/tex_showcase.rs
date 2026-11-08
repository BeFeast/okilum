//! #2741: independent expected glyphs and exact original screenshot inputs.
use okilum_core::tex::{self, Class, Node};

#[test]
fn math_2741_showcase_symbols_are_native_glyphs_with_their_tex_classes() {
    for (source, expected, class) in [
        (r"\Alpha", "A", Class::Ord),
        (r"\varkappa", "ϰ", Class::Ord),
        (r"\beth", "ℶ", Class::Ord),
        (r"\lessgtr", "≶", Class::Rel),
        (r"\boxplus", "⊞", Class::Bin),
        (r"\updownarrow", "↕", Class::Rel),
        (r"\lmoustache", "⎰", Class::Open),
        (r"\N", "ℕ", Class::Ord),
        (r"\Complex", "ℂ", Class::Ord),
    ] {
        let nodes = tex::parse(source).unwrap_or_else(|e| panic!("{source}: {e}"));
        assert!(
            matches!(&nodes[..], [Node::Atom { text, class: found, .. }]
            if text == expected && *found == class),
            "{source}: {nodes:?}"
        );
        assert_eq!(tex::linear(source).unwrap().text, expected, "{source}");
    }
}

#[test]
fn math_2741_exact_screenshot_tags_belong_to_display_and_preserve_the_equation() {
    let expected = tex::parse("x+y^{2x}").unwrap();
    for (source, label) in [
        (r"\tag{hi} x+y^{2x}", "(hi)"),
        (r"\tag*{hi} x+y^{2x}", "hi"),
        (r"x+y^{2x}\tag{hi}", "(hi)"),
    ] {
        let formula = tex::parse_display(source).unwrap();
        assert_eq!(formula.nodes, expected, "{source}");
        assert_eq!(formula.tag.as_deref(), Some(label));
        assert_eq!(
            tex::parse(source).unwrap_err().0,
            "Equation tags require display math ($$…$$)"
        );
    }
    for source in [r"\tag{a}\tag{b}x", r"{\tag{a}x}", r"\tag x", r"\tag{hi}"] {
        assert!(tex::parse_display(source).is_err(), "{source}");
    }
    assert!(tex::parse_display(&"x".repeat(tex::MAX_FORMULA_BYTES + 1)).is_err());
    assert!(tex::parse_display(r"\href{https://example.org}{x}").is_err());
    assert!(tex::parse(r"\def\x{y}\x").is_err());
}

#[test]
fn math_2741_additional_accents_share_inline_and_display_parsing() {
    for (source, mark) in [
        (r"\dddot{a}", '\u{20db}'),
        (r"\ddddot{a}", '\u{20dc}'),
        (r"\mathring{g}", '\u{30a}'),
        (r"\widecheck{ac}", '\u{30c}'),
        (r"\overleftarrow{AB}", '\u{20d6}'),
        (r"\overleftrightarrow{AB}", '\u{20e1}'),
    ] {
        let nodes = tex::parse(source).unwrap();
        assert!(matches!(&nodes[..], [Node::Accent { mark: found, .. }] if *found == mark));
        assert_eq!(tex::parse_display(source).unwrap().nodes, nodes);
        assert!(tex::linear(source).unwrap().text.contains(mark));
    }
}
