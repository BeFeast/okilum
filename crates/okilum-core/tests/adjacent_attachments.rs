//! #315 acceptance corpus: ordinary links to files next to a note.
use okilum_core::{
    document_links::prepared::{LinkPreparation, LinkStatus, TargetSnapshot},
    Vault,
};

fn corpus() -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for folder in ["notes/sub", "assets", "attachments", "a", "b"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    std::fs::write(root.join("notes/start.md"), "# Start").unwrap();
    for file in [
        "notes/report.pdf",
        "assets/pic.png",
        "notes/my file.pdf",
        "notes/Отчёт.pdf",
        "notes/dup.png",
        "dup.png",
        "attachments/root.png",
        "a/shared.csv",
        "b/shared.csv",
        "notes/letter.docx",
        "notes/README",
        "gone.png",
        "notes/locked.pdf",
    ] {
        std::fs::write(root.join(file), file.as_bytes()).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join("notes/locked.pdf"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
    }
    let vault = Vault::scan_metadata(root).unwrap();
    (dir, vault)
}

fn attachment(path: &str) -> Option<String> {
    Some(format!(
        "okilum://attachment/{}",
        okilum_core::document_links::encode(path)
    ))
}

#[test]
fn windows_adjacent_attachment_links_open_the_exact_file_or_say_why_not() {
    let (dir, vault) = corpus();
    let mut prep = LinkPreparation::new(
        &vault,
        "notes/start.md",
        |_| -> Result<TargetSnapshot, String> { Err("not a document".into()) },
    )
    .with_local_files();
    // A01/A02 and their spellings: the exact file, previewed in Okilum.
    for (target, file) in [
        ("report.pdf", "notes/report.pdf"),
        ("../assets/pic.png", "assets/pic.png"),
        ("my%20file.pdf", "notes/my file.pdf"),
        ("my file.pdf", "notes/my file.pdf"),
        ("Отчёт.pdf", "notes/Отчёт.pdf"),
        ("%D0%9E%D1%82%D1%87%D1%91%D1%82.pdf", "notes/Отчёт.pdf"),
        // The sibling wins over a root namesake.
        ("dup.png", "notes/dup.png"),
        // Root-compatible `attachments/...` links stay supported.
        ("attachments/root.png", "attachments/root.png"),
        ("letter.docx", "notes/letter.docx"),
    ] {
        let (resolved, state) = prep.link(target, false);
        assert_eq!(state.status, LinkStatus::Resolved, "{target}: {state:?}");
        assert_eq!(resolved.candidates, [file], "{target}");
        assert_eq!(state.action_url, attachment(file), "{target}");
    }
    // Missing: visible, never redirected to a root namesake, and the copied
    // path is the one a person would type.
    let root = okilum_core::vault::display_path(&dir.path().canonicalize().unwrap());
    for (target, file) in [
        ("gone.pdf", "notes/gone.pdf"),
        ("./gone.png", "notes/gone.png"),
    ] {
        let (_, state) = prep.link(target, false);
        assert_eq!(state.status, LinkStatus::MissingFile, "{target}: {state:?}");
        assert_eq!(state.reason, "File not found");
        let copied = okilum_core::document_links::decode(
            state
                .action_url
                .as_deref()
                .and_then(|u| u.strip_prefix("okilum://missing-file/"))
                .unwrap(),
        );
        let expected = std::path::Path::new(&root).join(file);
        assert_eq!(
            std::path::Path::new(&copied),
            expected,
            "{target}: no verbatim prefix, no ./ or ../"
        );
        assert!(!copied.contains(r"\\?\"), "{copied}");
    }
    // Ambiguity is explicit: both files, no silent winner.
    let (resolved, state) = prep.link("shared.csv", false);
    assert_eq!(state.status, LinkStatus::Ambiguous, "{state:?}");
    assert_eq!(resolved.candidates, ["a/shared.csv", "b/shared.csv"]);
    // Folders and extensionless files are refused in plain words.
    for target in ["sub/", "sub", "README"] {
        let (_, state) = prep.link(target, false);
        assert_eq!(state.status, LinkStatus::Unsupported, "{target}: {state:?}");
        assert!(state.reason.starts_with("Links open notes"), "{state:?}");
        assert!(!state.reason.contains("extensionless"), "{state:?}");
    }
    let (_, state) = prep.link("nope.md", false);
    assert_eq!(state.status, LinkStatus::MissingDocument, "{state:?}");
}
