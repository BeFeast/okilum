//! Explicit ordinary-note filesystem operations; never part of the reader protocol.
use anyhow::{bail, Context, Result};
use rustix::fs::{open, openat, Mode, OFlags};
use std::{
    fs::File,
    path::{Component, Path},
};

/// Create an empty Markdown note exclusively in an existing real folder.
/// Walk relative directories through descriptors: symlink substitution cannot
/// redirect creation, and O_EXCL rejects even dangling destination symlinks.
/// On a sync error the created file is retained, never unlinked behind a writer.
pub fn create(root: &Path, relative: &Path) -> Result<()> {
    create_with_source(root, relative, b"")
}

/// Publish a complete recovered source with NOREPLACE; no partial destination.
pub fn create_with_source(root: &Path, relative: &Path, source: &[u8]) -> Result<()> {
    if crate::vault::service_path(relative) {
        bail!("Service files are not note creation targets");
    }
    let parts: Vec<_> = relative.components().collect();
    if parts.is_empty() || parts.iter().any(|p| !matches!(p, Component::Normal(_))) {
        bail!("Choose a filename inside the open folder");
    }
    if !relative
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
    {
        bail!("Use a Markdown (.md) filename");
    }
    let mut folder = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    for part in &parts[..parts.len() - 1] {
        folder = openat(
            &folder,
            part.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .context("Choose an existing real folder, not a symbolic link")?;
    }
    let temporary = format!(".tessera-create-{}", uuid::Uuid::new_v4());
    let file = openat(
        &folder,
        temporary.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o644),
    )
    .context("Cannot create note: the name may already exist or the folder is not writable")?;
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut file = File::from(file);
        file.write_all(source)?;
        file.sync_all()?;
        rustix::fs::renameat_with(
            &folder,
            temporary.as_str(),
            &folder,
            parts.last().unwrap().as_os_str(),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .context("Cannot create note: destination already exists or cannot be written")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(&folder, temporary.as_str(), rustix::fs::AtFlags::empty());
    }
    result?;
    File::from(folder)
        .sync_all()
        .context("The new file exists, but its folder could not be synced")?;
    Ok(())
}

/// Validate typed sidebar paths before any filesystem writes. Empty, absolute,
/// dot and parent segments are errors rather than silently normalized names.
pub fn typed_path(parent: &Path, name: &str, directory: bool) -> Result<std::path::PathBuf> {
    if name.is_empty()
        || name.split('/').any(|p| {
            p.is_empty()
                || p == "."
                || p == ".."
                || p.trim() != p
                || p.chars().any(|c| c.is_control() || c == '\\')
        })
    {
        bail!("Enter a name, or folders/name, without empty, dot or parent segments");
    }
    if parent
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        bail!("Choose a folder inside the vault");
    }
    let mut relative = parent.join(name);
    if !directory
        && !relative
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
    {
        let filename = relative
            .file_name()
            .context("Enter a filename")?
            .to_string_lossy();
        relative.set_file_name(format!("{filename}.md"));
    }
    Ok(relative)
}

/// Create intermediate real directories via descriptors. Existing final folders
/// are an error for New Folder; intermediate existing folders are allowed.
pub fn create_folders(root: &Path, relative: &Path, exclusive_last: bool) -> Result<()> {
    if relative
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        bail!("Choose a folder inside the vault");
    }
    let mut folder = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let parts: Vec<_> = relative.components().collect();
    for (index, part) in parts.iter().enumerate() {
        match rustix::fs::mkdirat(&folder, part.as_os_str(), Mode::from_bits_truncate(0o755)) {
            Ok(()) => {
                rustix::fs::fsync(&folder)?;
            }
            Err(rustix::io::Errno::EXIST) if !exclusive_last || index + 1 != parts.len() => {}
            Err(error) => {
                return Err(error).context("Cannot create folder: the name may already exist")
            }
        }
        folder = openat(
            &folder,
            part.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .context("Choose a real folder, not a symbolic link")?;
    }
    Ok(())
}

/// Create-only publication with an explicit vault template and draft protection.
/// Intermediate folders may remain if later publication fails.
pub fn create_from_template(
    root: &Path,
    relative: &Path,
    now: time::OffsetDateTime,
    drafts: &Path,
    catalog: &crate::note_templates::Catalog,
    selected: Option<&str>,
) -> Result<String> {
    let name = relative.to_str().context("Use a UTF-8 filename")?;
    let checked = typed_path(Path::new(""), name, false)?;
    if checked != relative {
        bail!("Use a Markdown (.md) filename");
    }
    if catalog.contains_target(relative) {
        bail!("Choose a note destination outside the templates folder");
    }
    let current = crate::note_templates::Catalog::load(root)?;
    if &current != catalog {
        bail!("Templates changed. Cancel and choose New File again");
    }
    let title = relative
        .file_stem()
        .and_then(|s| s.to_str())
        .context("Use a UTF-8 title")?;
    let source = catalog.source(root, selected, title, now)?;
    let parent = relative.parent().context("Choose a note filename")?;
    create_folders(root, parent, false)?;
    let _reservation =
        crate::file_editor::FileEditor::reserve_destination(&root.join(relative), drafts)?;
    create_with_source(root, relative, source.as_bytes())?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn create_note(root: &Path, relative: &Path, state: &Path) -> Result<String> {
        let catalog = crate::note_templates::Catalog::load(root)?;
        let selected = catalog.default_file();
        create_from_template(
            root,
            relative,
            time::macros::datetime!(2026-10-05 09:07 UTC),
            state,
            &catalog,
            selected.as_deref(),
        )
    }
    #[test]
    fn configured_template_publication_is_lossless_and_create_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let root = dir.path();
        let rel = typed_path(Path::new(""), "A/B/Привет 🧠", false).unwrap();
        let source = create_note(root, &rel, state.path()).unwrap();
        assert_eq!(
            source,
            "---\ntype: Note\ncreated: 2026-10-05\n---\n\n# Привет 🧠\n"
        );
        std::fs::create_dir_all(root.join("_Assets/Templates")).unwrap();
        std::fs::write(
            root.join("_Assets/Templates/Note.md"),
            "\u{feff}# {{title}}\r\n{{date:DD.MM.YYYY}} {{time:HH:mm}} {{unknown}}\r\n",
        )
        .unwrap();
        std::fs::write(
            root.join("A/_template.md"),
            "old mechanism must not be used",
        )
        .unwrap();
        assert_eq!(
            create_note(root, Path::new("A/{{date}}.md"), state.path()).unwrap(),
            "\u{feff}# {{date}}\r\n05.10.2026 09:07 {{unknown}}\r\n"
        );
        assert!(create_note(root, &rel, state.path()).is_err());
        assert_eq!(std::fs::read_to_string(root.join(&rel)).unwrap(), source);
        assert!(create_note(root, Path::new("_Assets/Templates/new.md"), state.path()).is_err());
        std::fs::write(root.join("_Assets/Templates/Note.md"), [0xff]).unwrap();
        assert!(create_note(root, Path::new("invalid.md"), state.path()).is_err());
        assert!(!root.join("invalid.md").exists());
        for name in [
            "",
            "/abs",
            "../escape",
            "a/../escape",
            "a//b",
            "a/./b",
            "bad\nname",
        ] {
            assert!(typed_path(Path::new(""), name, false).is_err(), "{name:?}");
        }
        assert_eq!(
            typed_path(Path::new(""), "Meeting 1.2", false).unwrap(),
            Path::new("Meeting 1.2.md")
        );
        create_folders(root, Path::new("Empty/Nested"), true).unwrap();
        assert!(create_folders(root, Path::new("Empty/Nested"), true).is_err());
    }
    #[test]
    fn templates_and_nested_creation_reject_symlinks_and_orphan_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        assert!(create_note(root, Path::new("link/new/note.md"), state.path()).is_err());
        assert!(!outside.path().join("new").exists());
        std::fs::create_dir_all(root.join("_Assets/Templates")).unwrap();
        std::fs::write(root.join("_Assets/Templates/Note.md"), "template").unwrap();
        let catalog = crate::note_templates::Catalog::load(root).unwrap();
        std::fs::remove_file(root.join("_Assets/Templates/Note.md")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("absent.md"),
            root.join("_Assets/Templates/Note.md"),
        )
        .unwrap();
        assert!(create_from_template(
            root,
            Path::new("new.md"),
            time::macros::datetime!(2026-10-05 09:07 UTC),
            state.path(),
            &catalog,
            Some("Note.md")
        )
        .is_err());
        std::fs::remove_file(root.join("_Assets/Templates/Note.md")).unwrap();
        std::fs::write(root.join("note.md"), "base").unwrap();
        let mut editor =
            crate::file_editor::FileEditor::open(&root.join("note.md"), state.path()).unwrap();
        editor.set_text("recover me".into()).unwrap();
        drop(editor);
        std::fs::remove_file(root.join("note.md")).unwrap();
        assert!(create_note(root, Path::new("note.md"), state.path()).is_err());
        assert!(!root.join("note.md").exists());
    }

    #[test]
    fn unicode_creation_collision_and_first_save_recovery() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Notes")).unwrap();
        let rel = Path::new("Notes/Привет 🧠e\u{301}.md");
        create(dir.path(), rel).unwrap();
        let path = dir.path().join(rel);
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        let state = dir.path().join("drafts");
        let mut editor = crate::file_editor::FileEditor::open(&path, &state).unwrap();
        editor.set_text("first unsaved 🧠".into()).unwrap();
        drop(editor);
        assert!(crate::file_editor::FileEditor::has_unsaved_draft(&path, &state).unwrap());
        let mut editor = crate::file_editor::FileEditor::open(&path, &state).unwrap();
        assert_eq!(editor.text(), "first unsaved 🧠");
        assert_eq!(editor.save().unwrap(), crate::file_editor::Save::Saved);
        assert!(create(dir.path(), rel).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first unsaved 🧠");
    }
    #[test]
    fn concurrent_creation_has_one_winner_and_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| create(root, Path::new("race.md")));
            let b = scope.spawn(|| create(root, Path::new("race.md")));
            assert_eq!(
                usize::from(a.join().unwrap().is_ok()) + usize::from(b.join().unwrap().is_ok()),
                1
            );
        });
        for rel in [
            "../escape.md",
            "/absolute.md",
            "absent/note.md",
            "wrong.txt",
        ] {
            assert!(create(root, Path::new(rel)).is_err());
        }
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        assert!(create(root, Path::new("link/note.md")).is_err());
        std::os::unix::fs::symlink(outside.path().join("absent.md"), root.join("dangling.md"))
            .unwrap();
        assert!(create(root, Path::new("dangling.md")).is_err());
        assert!(!outside.path().join("note.md").exists());
        assert!(!outside.path().join("absent.md").exists());
    }
}
