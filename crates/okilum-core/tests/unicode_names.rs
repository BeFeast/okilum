//! Names stored decomposed (NFD) on disk resolve from composed (NFC) link text.
//!
//! macOS and Syncthing copies often keep a Cyrillic `й` as `и` + U+0306 while
//! the Markdown that links to it is composed. Directory listings return the
//! stored bytes, so a byte-keyed index left those links unresolved. These tests
//! write the NFD bytes themselves, so they exercise the same mismatch on Linux.

use std::fs;
use std::path::Path;

use okilum_core::{
    document_links::{self, IndexedResolver},
    quick_open, Resolution, Vault,
};
use unicode_normalization::UnicodeNormalization;

const NAME: &str = "похудеешь-ёлка";

fn nfd(text: &str) -> String {
    text.nfd().collect()
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn nfd_file_names_resolve_from_nfc_links_and_keep_their_disk_spelling() {
    assert_ne!(nfd(NAME), NAME, "the fixture name must have an NFD form");
    let root = tempfile::tempdir().unwrap();
    let stored = format!("sources/2026/{}.md", nfd(NAME));
    write(root.path(), &stored, "# Target\n");
    write(root.path(), &format!("img/{}.png", nfd("фото-й")), "png");
    write(
        root.path(),
        "digest.md",
        &format!(
            "[[sources/2026/{NAME}]] [[{NAME}]] [md](sources/2026/{NAME}.md) ![[фото-й.png]]\n"
        ),
    );
    let vault = Vault::scan(root.path()).unwrap();

    for target in [format!("sources/2026/{NAME}"), NAME.to_owned()] {
        assert_eq!(
            vault.resolve_from(&target, "digest.md"),
            Resolution::Resolved {
                path: stored.clone()
            },
            "{target}"
        );
    }
    assert_eq!(
        vault.resolve_markdown(&format!("sources/2026/{NAME}.md"), "digest.md"),
        Resolution::Resolved {
            path: stored.clone()
        }
    );

    let resolver = IndexedResolver::new(&vault);
    let source = fs::read_to_string(root.path().join("digest.md")).unwrap();
    let links = resolver.parse(&source, "digest.md");
    // The `![[…]]` embed is not a navigable link; `resolve_asset` covers it below.
    assert_eq!(
        links.iter().map(|link| link.wiki).collect::<Vec<_>>(),
        [true, true, false]
    );
    for link in links {
        let resolved = resolver.resolve(&link.target, link.wiki, "digest.md");
        assert!(
            matches!(resolved.status, "resolved" | "attachment"),
            "{} -> {} {}",
            link.target,
            resolved.status,
            resolved.url
        );
    }
    assert_eq!(
        document_links::resolve(&format!("sources/2026/{NAME}"), true, &vault, "digest.md").status,
        "resolved"
    );
    assert!(
        vault.resolve_asset("фото-й.png", "digest.md").is_some(),
        "an NFC embed finds the NFD attachment"
    );
    // Backlinks are keyed by the stored identity the resolver returned.
    assert!(!vault.backlinks(&stored).is_empty());
}

#[test]
fn nfc_and_nfd_files_with_one_name_are_ambiguous_not_a_silent_pick() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("{NAME}.md"), "# Composed\n");
    write(root.path(), &format!("{}.md", nfd(NAME)), "# Decomposed\n");
    write(root.path(), "from.md", &format!("[[{NAME}]]\n"));
    let vault = Vault::scan(root.path()).unwrap();
    for target in [NAME.to_owned(), nfd(NAME)] {
        match vault.resolve_from(&target, "from.md") {
            Resolution::Ambiguous { candidates } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
    assert!(matches!(
        vault.resolve_markdown(&format!("{NAME}.md"), "from.md"),
        Resolution::Ambiguous { .. }
    ));
}

#[test]
fn relative_links_inside_an_nfd_folder_resolve() {
    let root = tempfile::tempdir().unwrap();
    let folder = nfd("здоровье-й");
    write(
        root.path(),
        &format!("{folder}/a.md"),
        "[[./b]] [b](b.md)\n",
    );
    write(root.path(), &format!("{folder}/b.md"), "# B\n");
    let vault = Vault::scan(root.path()).unwrap();
    let from = format!("{folder}/a.md");
    let expected = Resolution::Resolved {
        path: format!("{folder}/b.md"),
    };
    assert_eq!(vault.resolve_from("./b", &from), expected);
    assert_eq!(vault.resolve_markdown("b.md", &from), expected);
}

#[test]
fn quick_open_matches_an_nfd_title_from_an_nfc_query() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("{}.md", nfd(NAME)), "# T\n");
    let vault = Vault::scan(root.path()).unwrap();
    let found = quick_open::search(&vault.notes, "похудеешь-ёл", &[], 10);
    assert_eq!(found.len(), 1, "{found:?}");
}

/// Manual probe: unresolved links in a real vault. Run with
/// `OKILUM_LINK_CENSUS_VAULT=<dir> cargo test -p okilum-core --test unicode_names -- --ignored --nocapture`.
#[test]
#[ignore]
fn link_census() {
    let Some(root) = std::env::var_os("OKILUM_LINK_CENSUS_VAULT") else {
        return;
    };
    let root = Path::new(&root);
    let vault = Vault::scan(root).unwrap();
    let resolver = IndexedResolver::new(&vault);
    let (mut links, mut unresolved, mut non_ascii, mut ambiguous) = (0, 0, 0, 0);
    for note in &vault.notes {
        let Ok(source) = fs::read_to_string(root.join(&note.path)) else {
            continue;
        };
        for link in resolver.parse(&source, &note.path) {
            links += 1;
            match resolver.resolve(&link.target, link.wiki, &note.path).status {
                "unresolved" => {
                    unresolved += 1;
                    if !link.target.is_ascii() {
                        non_ascii += 1;
                    }
                }
                "ambiguous" => ambiguous += 1,
                _ => {}
            }
        }
    }
    println!(
        "link-census notes={} links={links} unresolved={unresolved} unresolved_non_ascii={non_ascii} ambiguous={ambiguous}",
        vault.notes.len()
    );
}
