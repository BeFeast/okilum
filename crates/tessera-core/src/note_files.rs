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

#[cfg(test)]
mod tests {
    use super::*;
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
