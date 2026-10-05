//! Note transclusion (#49): `![[note]]` and `![[note#Heading]]` expand to the
//! target's body for the reader, and never for an agent reading the source.

use std::fs;
use std::path::{Path, PathBuf};

use tessera_core::render::{
    expand_embeds, heading_section, note_source, reader_source, EMBED_LANG, EMBED_MISSING,
    WIKI_SCHEME,
};
use tessera_core::Vault;

fn vault_with(name: &str, files: &[(&str, &str)]) -> (PathBuf, Vault) {
    let root = std::env::temp_dir().join(format!("tessera-embeds-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for (rel, body) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }
    let vault = Vault::scan(Path::new(&root)).unwrap();
    (root, vault)
}

const ALPHA: &str = "---\ntitle: Alpha\n---\n# Alpha\n\nintro [[demo]]\n\n## Setup\n\nstep one\n\n### Detail\n\nfine print\n\n## Teardown\n\nlast\n";

#[test]
fn a_whole_note_embeds_as_a_fence_with_its_links_rewritten() {
    let (_root, v) = vault_with(
        "whole",
        &[
            ("demo.md", "before\n\n![[alpha]]\n\nafter\n"),
            ("alpha.md", ALPHA),
        ],
    );
    let out = reader_source(&v, "demo.md").unwrap();
    let expected = format!(
        "before\n\n\n~~~~{EMBED_LANG} alpha.md\n# Alpha\n\nintro [demo]({WIKI_SCHEME}demo.md)\n\n## Setup\n\nstep one\n\n### Detail\n\nfine print\n\n## Teardown\n\nlast\n~~~~\n\n\nafter\n"
    );
    assert_eq!(out, expected);
}

#[test]
fn a_heading_embed_takes_the_section_up_to_the_next_same_level_heading() {
    let (_root, v) = vault_with(
        "section",
        &[("demo.md", "![[alpha#setup]]\n"), ("alpha.md", ALPHA)],
    );
    let out = reader_source(&v, "demo.md").unwrap();
    assert_eq!(
        out,
        format!("\n~~~~{EMBED_LANG} alpha.md#setup\n## Setup\n\nstep one\n\n### Detail\n\nfine print\n~~~~\n\n")
    );
}

#[test]
fn heading_section_rules() {
    let body = "# T\n\n```\n## Setup\n```\n\n## Setup\n\nx\n\n# Top\n";
    assert_eq!(
        heading_section(body, "Setup"),
        Some("## Setup\n\nx\n\n".into())
    );
    assert_eq!(heading_section(body, "top"), Some("# Top\n".into()));
    assert_eq!(heading_section(body, "nope"), None);
    // Heading text trailing `#`s and spaces do not count.
    assert_eq!(
        heading_section("## A ##  \nb\n", "a"),
        Some("## A ##  \nb\n".into())
    );
}

#[test]
fn depth_is_one_and_a_cycle_ends_in_a_link() {
    let (_root, v) = vault_with(
        "cycle",
        &[
            ("a.md", "A says\n\n![[b]]\n"),
            ("b.md", "B says\n\n![[a]]\n"),
        ],
    );
    let out = reader_source(&v, "a.md").unwrap();
    // b is spliced in; the `![[a]]` inside it is a link back, not another embed.
    assert!(
        out.contains(&format!(
            "~~~~{EMBED_LANG} b.md\nB says\n\n[a]({WIKI_SCHEME}a.md)\n~~~~"
        )),
        "{out}"
    );
    assert_eq!(out.matches(&format!("~~~~{EMBED_LANG}")).count(), 1);
}

#[test]
fn a_self_embed_and_a_missing_target_render_as_missing() {
    let (_root, v) = vault_with(
        "missing",
        &[
            ("a.md", "![[a]]\n\n![[ghost]]\n\n![[b#no such heading]]\n"),
            ("b.md", "# B\n"),
        ],
    );
    let out = reader_source(&v, "a.md").unwrap();
    assert!(
        out.contains(&format!("~~~~{EMBED_LANG} {EMBED_MISSING} a\n~~~~")),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "~~~~{EMBED_LANG} {EMBED_MISSING} b#no such heading\n~~~~"
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!("~~~~{EMBED_LANG} {EMBED_MISSING} ghost\n~~~~")),
        "{out}"
    );
    assert!(!out.contains("~~ghost~~"), "{out}");
}

#[test]
fn images_code_and_inline_embeds_are_not_expanded() {
    let (_root, v) = vault_with(
        "skip",
        &[
            (
                "demo.md",
                "![[pic.png]]\n\n`![[alpha]]`\n\n```\n![[alpha]]\n```\n\nsee ![[alpha]] inline\n",
            ),
            ("alpha.md", "# A\n"),
            ("pic.png", "png"),
        ],
    );
    let out = reader_source(&v, "demo.md").unwrap();
    assert!(!out.contains(&format!("~~~~{EMBED_LANG}")), "{out}");
    assert!(out.starts_with("![](file://"), "{out}");
    assert!(out.contains("`![[alpha]]`"), "{out}");
    assert!(
        out.contains(&format!("see [alpha]({WIKI_SCHEME}alpha.md) inline")),
        "{out}"
    );
}

#[test]
fn the_body_cannot_close_the_fence() {
    let (_root, v) = vault_with(
        "fence",
        &[
            ("demo.md", "![[alpha]]\n"),
            ("alpha.md", "~~~~\ncode\n~~~~\n"),
        ],
    );
    let out = expand_embeds("![[alpha]]\n", &v, "demo.md");
    assert_eq!(
        out,
        format!("\n~~~~~{EMBED_LANG} alpha.md\n~~~~\ncode\n~~~~\n~~~~~\n\n")
    );
}

#[test]
fn note_source_keeps_embeds_as_written() {
    let (_root, v) = vault_with(
        "agent",
        &[("demo.md", "![[alpha]]\n"), ("alpha.md", "# A\n")],
    );
    let out = note_source(&v, "demo.md").unwrap();
    assert_eq!(out, format!("[alpha]({WIKI_SCHEME}alpha.md)\n"));
}
