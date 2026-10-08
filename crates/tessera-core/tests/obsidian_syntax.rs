//! The Reader's Obsidian syntax corpus (#651): the fixture note rendered
//! through the same pipeline the desktop Reader uses.

use tessera_core::document_links::HeadingInventory;
use tessera_core::obsidian::{footnote_block, footnote_reference_block, FOOTNOTE_SCHEME};
use tessera_core::render::{note_source, reader_document_from_source, WIKI_SCHEME};
use tessera_core::Vault;

const CORPUS: &str = include_str!("../../../fixtures/reader/obsidian-syntax.md");

fn corpus() -> String {
    let vault = Vault::from_note_paths(["obsidian-syntax.md".into()]);
    reader_document_from_source(&vault, "obsidian-syntax.md", CORPUS).rendered
}

#[test]
fn comments_are_hidden_and_code_stays_literal() {
    let out = corpus();
    assert!(!out.contains("an inline comment"));
    assert!(!out.contains("A block comment."));
    assert!(out.contains("`%%not a comment%%`"), "positive control");
    assert!(out.contains("`==not a highlight==`"));
    assert!(out.contains("<mark>a highlight</mark>"));
}

#[test]
fn callouts_keep_their_fold_markers_for_the_shell() {
    let out = corpus();
    for header in [
        "> [!warning]- Folded warning",
        "> [!tip]+ Open tip",
        "> [!faq] Default question style",
    ] {
        assert!(out.contains(header), "{header}");
    }
}

#[test]
fn footnotes_are_numbered_and_land_both_ways() {
    let out = corpus();
    assert!(out.contains(&format!("[¹]({FOOTNOTE_SCHEME}1)")));
    assert!(out.contains(&format!("[²]({FOOTNOTE_SCHEME}long)")));
    assert!(out.contains(&format!("[³]({FOOTNOTE_SCHEME}inline-3)")));
    assert!(out.contains("A literal `[^1]` in code"));
    assert!(out.contains("~~~~footnote 2 long\nA footnote with\na continuation line.\n~~~~"));
    assert!(!out.contains("[^long]:"), "definitions leave the body");
    let reference = footnote_reference_block(&out, "1").unwrap();
    let definition = footnote_block(&out, "1").unwrap();
    assert!(definition > reference);
    assert!(footnote_block(&out, "inline-3").unwrap() > definition);
}

#[test]
fn math_renders_as_source() {
    let out = corpus();
    assert!(out.contains(r"Inline `e^{i\pi} + 1 = 0` next to money: $5 and $10."));
    assert!(out.contains("~~~~math\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n~~~~"));
    assert!(out.contains("Literal: `$x$`."));
}

#[test]
fn block_ids_are_hidden_and_land() {
    let out = corpus();
    assert!(!out.contains(" ^para-1"));
    assert!(out.contains("`^not-an-id` inside code."));
    assert!(out.contains(&format!("{WIKI_SCHEME}obsidian-syntax.md#%5Epara-1")));
    let inventory = HeadingInventory::new(&out);
    let table = inventory.locate("^table-1").unwrap().block;
    let item = inventory.locate("^item-2").unwrap().block;
    let para = inventory.locate("^para-1").unwrap().block;
    assert!(para < item && item < table);
    assert!(inventory.locate("^not-an-id").is_err());
    assert_eq!(
        inventory.locate("Block references").unwrap().block + 1,
        para,
        "the paragraph follows its heading"
    );
}

#[test]
fn mcp_source_keeps_the_note_as_written() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("obsidian-syntax.md"), CORPUS).unwrap();
    let vault = Vault::scan(dir.path()).unwrap();
    let source = note_source(&vault, "obsidian-syntax.md").unwrap();
    assert!(source.contains("%%an inline comment%%"));
    assert!(source.contains("[^1]: The source of the claim."));
    assert!(source.contains("A paragraph that can be linked. ^para-1"));
}

#[test]
fn embeds_take_a_heading_section_or_one_block() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("obsidian-syntax.md"), CORPUS).unwrap();
    std::fs::write(
        dir.path().join("host.md"),
        "![[obsidian-syntax#Math]]\n\n![[obsidian-syntax#^item-2]]\n\n![[obsidian-syntax#^missing]]\n",
    )
    .unwrap();
    let vault = Vault::scan(dir.path()).unwrap();
    let raw = std::fs::read_to_string(dir.path().join("host.md")).unwrap();
    let out = reader_document_from_source(&vault, "host.md", &raw).rendered;
    assert!(
        out.contains("embed obsidian-syntax.md#Math\n## Math\n"),
        "{out}"
    );
    assert!(out.contains("~~~~math"), "math inside the embedded section");
    assert!(
        out.contains("embed obsidian-syntax.md#^item-2\n- Second item<!--^item-2-->\n"),
        "{out}"
    );
    assert!(!out.contains("First item"), "only the block is embedded");
    assert!(out.contains("embed missing obsidian-syntax#^missing"));
}

#[test]
fn block_embeds_refuse_ambiguous_ids_without_picking_a_winner() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("target.md"),
        "First candidate. ^shared\n\nSecond candidate. ^SHARED\n\nUnique candidate. ^unique\n\n```\nCode candidate. ^unique\n```\n",
    )
    .unwrap();
    let vault = Vault::scan(dir.path()).unwrap();
    let out = reader_document_from_source(
        &vault,
        "host.md",
        "![[target#^shared]]\n\n![[target#^UNIQUE]]\n",
    )
    .rendered;
    assert!(out.contains("embed missing target#^shared"), "{out}");
    assert!(
        !out.contains("First candidate"),
        "an ambiguous embed must not guess"
    );
    assert!(
        !out.contains("Second candidate"),
        "an ambiguous embed must not guess"
    );
    assert!(
        out.contains("Unique candidate"),
        "positive control: a unique ID embeds"
    );
}
