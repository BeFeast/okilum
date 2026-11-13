//! Semantic rows and honest verdicts for YAML, TOML and XML (#2813).
use super::*;
use ::core::prelude::v1::test;

fn doc(language: &str, text: &str) -> Document {
    crate::source_presentation::code_languages::install();
    Document::for_file(text.to_owned(), language, false)
}

fn labels(doc: &Document, index: usize) -> Vec<String> {
    doc.children(index).map(|i| doc.row_label(i)).collect()
}

fn find_key(doc: &Document, key: &str) -> usize {
    (0..doc.structure.len())
        .find(|&i| doc.key_text(i) == Some(key))
        .unwrap_or_else(|| panic!("row {key}"))
}

const FEED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:xml="http://www.w3.org/XML/1998/namespace">
  <!-- generated -->
  <title>Release notes</title>
  <entry id="r-0.9.4" name="ignored"><title>0.9.4</title><link href="https://example.com"/></entry>
  <entry xml:id="r-0.9.3"/>
  <item name="named" />
  <atom:link rel="self"/>
</feed>
"#;

#[test]
fn xml2813_elements_take_id_then_name_and_attributes_are_not_rows() {
    let d = doc("xml", FEED);
    assert!(d.semantic && d.invalid.is_none() && d.syntax.is_none());
    assert_eq!(d.format, Some("XML"));
    assert_eq!(labels(&d, 0), ["feed 5 elements"], "the root element");
    assert_eq!(
        labels(&d, 1),
        [
            "title Release notes",
            "entry r-0.9.4",
            "entry r-0.9.3",
            "item named",
            "atom:link ",
        ],
        "id before name, namespaced xml:id, self-closing; comments and attributes are not rows"
    );
    let entry = d.children(1).nth(1).unwrap();
    assert_eq!(labels(&d, entry), ["title 0.9.4", "link "]);
    assert_eq!(d.container(1), Some((ContainerKind::Element, 5)));
    assert_eq!(d.key_text(1), Some("feed"));
    assert_eq!(&d.text[d.structure[entry].range.clone()][..6], "<entry");
}

#[test]
fn xml2813_root_stays_open_and_fold_all_keeps_it_open() {
    let d = doc("xml", FEED);
    let mut tree = super::super::structure::Structure::default();
    tree.reset(&d);
    assert!(!tree.folded(1), "the root element is open on arrival");
    assert!(tree.visible.len() > 1, "its children are visible");
    tree.all(true, &d);
    assert!(!tree.folded(1), "Fold all keeps the only top row open");
}

#[test]
fn toml2813_tables_keys_and_merged_array_of_tables() {
    let d = doc(
        "toml",
        "title = \"top\"\n\n[package]\nname = \"okilum-shell\"\nversion = \"0.9.4\"\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n\n[[bin]]\nname = \"a\"\npath = \"a.rs\"\n\n[[bin]]\nname = \"b\"\npath = \"b.rs\"\n\n[[example]]\nname = \"c\"\n",
    );
    assert!(d.semantic && d.syntax.is_none());
    assert_eq!(
        labels(&d, 0),
        [
            "title \"top\"",
            "package 2 keys",
            "dependencies 1 key",
            "bin 2 items",
            "example 1 item",
        ]
    );
    let bin = find_key(&d, "bin");
    assert_eq!(labels(&d, bin), ["[0] 2 keys", "[1] 2 keys"]);
    assert_eq!(d.container(bin), Some((ContainerKind::Array, 2)));
    let serde = find_key(&d, "serde");
    assert_eq!(labels(&d, serde), ["version \"1\"", "features 1 item"]);
    let first = d.children(bin).next().unwrap();
    assert_eq!(labels(&d, first), ["name \"a\"", "path \"a.rs\""]);
    assert!(
        d.structure[bin].range.start <= d.structure[first].range.start
            && d.structure[bin].range.end
                >= d.structure[d.structure[bin].subtree_end - 1].range.end,
        "the merged row encloses its blocks, so the sorted lookup stays valid"
    );
}

#[test]
fn yaml2813_keys_sequences_anchors_and_documents() {
    let compose = "version: \"3.9\"\nservices:\n  gateway:\n    image: okilum/gateway\n    ports:\n      - \"8080:80\"\n      - \"8443:443\"\n  accounts: &svc\n    image: okilum/accounts\n    restart: unless-stopped\n  billing:\n    <<: *svc\n    replicas: 2\nvolumes: {}\nlogs: []\nempty:\n";
    let d = doc("yaml", compose);
    assert!(d.semantic && d.invalid.is_none(), "{:?}", d.invalid);
    assert_eq!(d.format, Some("YAML"));
    assert_eq!(
        labels(&d, 0),
        [
            "version \"3.9\"",
            "services 3 keys",
            "volumes { }",
            "logs [ ]",
            "empty ",
        ]
    );
    let ports = find_key(&d, "ports");
    assert_eq!(d.row_label(ports), "ports 2 items");
    assert_eq!(labels(&d, ports), ["[0] \"8080:80\"", "[1] \"8443:443\""]);
    let replicas = find_key(&d, "replicas");
    assert_eq!(d.scalar(replicas).unwrap().0, ScalarKind::Number);
    assert_eq!(
        d.scalar(find_key(&d, "restart")).unwrap().0,
        ScalarKind::String
    );
    let multi = doc(
        "yaml",
        "kind: Service\n---\nkind: Deployment\nspec:\n  replicas: 1\n",
    );
    assert!(multi.invalid.is_none());
    assert_eq!(labels(&multi, 0), ["[0] 1 key", "[1] 2 keys"]);
}

#[test]
fn verdicts2813_yaml_strict_toml_and_xml_syntax_only() {
    let bad = doc("yaml", "a: [1, 2\nb: 3\n");
    let error = bad.invalid.as_ref().expect("serde_yaml rejects");
    assert!(error.line >= 1 && error.column >= 1);
    assert!(
        !bad.semantic && bad.structure.is_empty(),
        "no Tree for a rejected YAML"
    );
    assert!(bad.syntax.is_none());
    let unknown_alias = doc("yaml", "a: *missing\n");
    assert!(
        unknown_alias.invalid.is_some(),
        "an unknown alias is an error"
    );
    let toml = doc("toml", "[a]\nb = 1\nc = \n[d]\ne = 2\n");
    let syntax = toml.syntax.as_ref().expect("grammar error");
    assert_eq!(syntax.line, 3);
    assert!(toml.invalid.is_none(), "no verdict for TOML");
    assert!(toml.semantic && find_key(&toml, "d") > 0, "Tree stays");
    let xml = doc("xml", "<root>\n  <a>one</a\n  <c/>\n</root>\n");
    let syntax = xml.syntax.as_ref().expect("grammar error");
    // tree-sitter reports where recovery starts: the broken tag or the next one.
    assert!((2..=3).contains(&syntax.line), "{syntax:?}");
    assert!(xml.invalid.is_none() && xml.semantic, "no verdict for XML");
    assert!(xml.structure.len() > 1);
}

#[test]
fn corpus2813_real_valid_files_get_no_badge() {
    let corpus: &[(&str, &str, &str)] = &[
        (
            "yaml",
            "docker-compose.yml",
            include_str!("corpus/docker-compose.yml"),
        ),
        (
            "yaml",
            "data-view-native.yml (this repo)",
            include_str!("../../../../../.github/workflows/data-view-native.yml"),
        ),
        (
            "yaml",
            "workflow-anchors.yml",
            include_str!("corpus/workflow-anchors.yml"),
        ),
        (
            "yaml",
            "kubernetes.yaml",
            include_str!("corpus/kubernetes.yaml"),
        ),
        (
            "toml",
            "Cargo.toml (okilum-shell)",
            include_str!("../../../Cargo.toml"),
        ),
        (
            "toml",
            "Cargo.toml (workspace)",
            include_str!("../../../../../Cargo.toml"),
        ),
        (
            "toml",
            "pyproject.toml",
            include_str!("corpus/pyproject.toml"),
        ),
        ("xml", "atom.xml", include_str!("corpus/atom.xml")),
        (
            "xml",
            "symbol-reversed.svg (this repo)",
            include_str!("../../../assets/brand/symbol-reversed.svg"),
        ),
        ("xml", "pom.xml", include_str!("corpus/pom.xml")),
        ("xml", "schema.xsd", include_str!("corpus/schema.xsd")),
    ];
    for (language, name, text) in corpus {
        let d = doc(language, text);
        assert!(d.invalid.is_none(), "{name}: {:?}", d.invalid);
        assert!(d.syntax.is_none(), "{name}: {:?}", d.syntax);
        assert!(
            d.semantic && d.children(0).next().is_some(),
            "{name}: Tree rows"
        );
    }
    let cargo = doc("toml", include_str!("../../../Cargo.toml"));
    let bin = find_key(&cargo, "bin");
    assert!(
        cargo
            .container(bin)
            .is_some_and(|(kind, n)| kind == ContainerKind::Array && n >= 1),
        "[[bin]] is one array row"
    );
}

#[test]
fn data2813_deep_and_multi_megabyte_files_index_iteratively_in_bounded_memory() {
    // tree-sitter-xml's external scanner keeps a bounded tag stack, so depth
    // is exercised with TOML arrays, which the grammar nests without one.
    let depth = 50_000;
    let deep = format!("a = {}1{}\n", "[".repeat(depth), "]".repeat(depth));
    let d = doc("toml", &deep);
    assert!(d.syntax.is_none(), "positive: the deep file parses");
    assert_eq!(
        d.structure.len(),
        depth + 2,
        "root, key, nested arrays and the scalar: no recursion limit, no stack overflow"
    );
    let entries = 40_000;
    let mut large = String::from("<feed>");
    for i in 0..entries {
        large.push_str(&format!(
            "<entry id=\"r-{i}\"><title>Release {i}</title><summary>{}</summary></entry>",
            "x".repeat(60)
        ));
    }
    large.push_str("</feed>");
    assert!(large.len() > 4_000_000);
    let started = std::time::Instant::now();
    let d = doc("xml", &large);
    let elapsed = started.elapsed();
    assert_eq!(d.structure.len(), 2 + entries * 3);
    assert_eq!(d.row_label(1), "feed 40,000 elements");
    // Rows hold ranges, never node text: index memory is bounded by the row
    // count, well under the text itself.
    let index_bytes = d.structure.len() * std::mem::size_of::<StructureRow>();
    assert!(
        index_bytes < large.len() * 4,
        "{index_bytes} bytes for {}",
        large.len()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "a multi-megabyte XML parses and indexes on a worker in {elapsed:?}"
    );
}

#[test]
fn toml2813_trailing_comments_never_replace_values() {
    let d = doc(
        "toml",
        "a = [1,2] # ports\nb = { x = 1 } # inline\nc = \"s\" # note\n",
    );
    assert_eq!(labels(&d, 0), ["a 2 items", "b 1 key", "c \"s\""]);
    assert_eq!(labels(&d, find_key(&d, "a")), ["[0] 1", "[1] 2"]);
    assert_eq!(labels(&d, find_key(&d, "b")), ["x 1"]);
}

#[test]
fn yaml2813_comments_after_anchor_tag_or_dash_keep_the_content() {
    let d = doc(
        "yaml",
        "x: &ref # comment\n  a: 1\n  b: 2\nlist:\n  - # note\n    k: v\n",
    );
    assert!(d.invalid.is_none(), "{:?}", d.invalid);
    assert_eq!(labels(&d, 0), ["x 2 keys", "list 1 item"]);
    assert_eq!(labels(&d, find_key(&d, "x")), ["a 1", "b 2"]);
    assert_eq!(labels(&d, find_key(&d, "list")), ["[0] 1 key"]);
}

#[test]
fn toml2813_sub_tables_between_array_blocks_stay_in_one_array_row() {
    let text = "[[bin]]\nname = \"a\"\n[bin.meta]\nk = 1\n[[bin]]\nname = \"b\"\n";
    let d = doc("toml", text);
    assert_eq!(
        labels(&d, 0),
        ["bin 2 items"],
        "one row, not two restarting at 0"
    );
    let bin = find_key(&d, "bin");
    assert_eq!(labels(&d, bin), ["[0] 2 keys", "[1] 1 key"]);
    let first = d.children(bin).next().unwrap();
    assert_eq!(labels(&d, first), ["name \"a\"", "meta 1 key"]);
    // Rows still enclose their children, so the sorted lookup finds `k`.
    let at = text.find("k = 1").unwrap();
    assert_eq!(d.row_at(&(at..at + 1)), Some(find_key(&d, "k")));
}

#[test]
fn xml2813_root_chevron_and_left_cannot_close_the_root() {
    let d = doc("xml", FEED);
    let mut tree = super::super::structure::Structure::default();
    tree.reset(&d);
    // Chevron clicks and the Left key both go through `toggle`.
    tree.toggle(1, &d);
    assert!(!tree.folded(1), "the root stays open");
    assert!(tree.visible.len() > 1);
    let child = d.children(1).nth(1).unwrap();
    tree.toggle(child, &d);
    assert!(
        !tree.folded(child),
        "positive: other rows still open and close"
    );
}
