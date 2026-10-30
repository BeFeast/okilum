//! Filesystem assertions shared by the bounded network-write example and its
//! hosted unit tests. This module is absent from the shipping application.
use anyhow::{ensure, Context as _, Result};
use std::{ffi::OsStr, fs, io::Read, path::Path};

/// macOS can materialize xattrs as AppleDouble on a remote filesystem. A name
/// alone is insufficient: never hide an ordinary file or an Okilum recovery tail.
fn is_appledouble(entry: &fs::DirEntry) -> Result<bool> {
    let name = entry.file_name();
    let bytes = name.as_encoded_bytes();
    if !bytes.starts_with(b"._") || bytes.len() <= 2 || !entry.file_type()?.is_file() {
        return Ok(false);
    }
    let mut header = [0; 4];
    match fs::File::open(entry.path())?.read_exact(&mut header) {
        Ok(()) => Ok(header == [0x00, 0x05, 0x16, 0x07]),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn check_note_only(vault: &Path, expected: &OsStr) -> Result<()> {
    let mut names = Vec::new();
    let mut unexpected = Vec::new();
    let mut found = false;
    for entry in fs::read_dir(vault)? {
        let entry = entry?;
        let name = entry.file_name();
        names.push(name.clone());
        if name == expected && entry.file_type()?.is_file() {
            found = true;
        } else if !is_appledouble(&entry)
            .with_context(|| format!("Inspect extra encoding QA entry {name:?}"))?
        {
            unexpected.push(name);
        }
    }
    names.sort();
    unexpected.sort();
    ensure!(
        found && unexpected.is_empty(),
        "encoding transaction left vault tails: expected={expected:?}, note_present={found}, unexpected={unexpected:?}, entries={names:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appledouble_needs_both_the_name_and_magic_but_never_hides_save_tails() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let note = root.join("Note.md");
        let original = b"Exact \xff bytes\r\n";
        fs::write(&note, original).unwrap();
        check_note_only(root, OsStr::new("Note.md")).unwrap();
        let header = [0x00, 0x05, 0x16, 0x07, 0xff];
        fs::write(root.join("._Note.md"), header).unwrap();
        check_note_only(root, OsStr::new("Note.md")).unwrap();
        for (name, bytes) in [
            ("._NotAppleDouble.md", b"ordinary source".as_slice()),
            ("._Short.md", &[0x00, 0x05, 0x16][..]),
            ("._WrongMagic.md", &[0x00, 0x05, 0x16, 0x00][..]),
            ("._", header.as_slice()),
            ("Unexpected.md", header.as_slice()),
            (".okilum-save-synthetic", header.as_slice()),
        ] {
            let extra = root.join(name);
            fs::write(&extra, bytes).unwrap();
            let message = check_note_only(root, OsStr::new("Note.md"))
                .unwrap_err()
                .to_string();
            assert!(
                message.contains(name),
                "the offending filename is retained: {message}"
            );
            assert!(
                message.contains("entries="),
                "the census is printed: {message}"
            );
            fs::remove_file(extra).unwrap();
            check_note_only(root, OsStr::new("Note.md")).unwrap();
        }
        assert_eq!(
            fs::read(note).unwrap(),
            original,
            "the checker never rewrites the note"
        );
    }

    #[test]
    fn a_missing_note_and_appledouble_named_directory_are_not_accepted() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        fs::write(root.join("._Note.md"), [0x00, 0x05, 0x16, 0x07]).unwrap();
        let error = check_note_only(root, OsStr::new("Note.md")).unwrap_err();
        assert!(error.to_string().contains("note_present=false"));
        fs::write(root.join("Note.md"), "# Note").unwrap();
        fs::create_dir(root.join("._directory")).unwrap();
        assert!(check_note_only(root, OsStr::new("Note.md"))
            .unwrap_err()
            .to_string()
            .contains("._directory"));
    }

    #[cfg(unix)]
    #[test]
    fn appledouble_named_symlinks_are_reported_without_reading_their_target() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        fs::write(root.join("Note.md"), "# Note").unwrap();
        std::os::unix::fs::symlink(root.join("absent-target"), root.join("._link")).unwrap();
        let error = check_note_only(root, OsStr::new("Note.md")).unwrap_err();
        assert!(error.to_string().contains("._link"));
    }
}
