//! #1915: run only against a dedicated, explicitly supplied QA mount directory.
//! No GUI or installed app. Creates/removes only its own tempfile child.
#[cfg(unix)]
#[path = "../src/reader_trash_fs.rs"]
mod trash;

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use okilum_core::{
        file_editor::{FileEditor, Save},
        note_files,
        note_move::{DirectoryMovePlan, DirectorySnapshot, MovePlan},
    };
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("explicit QA root required")?,
    )
    .canonicalize()?;
    ensure!(
        root.file_name().is_some_and(|n| n == "okilum-nfs-qa"),
        "only dedicated okilum-nfs-qa accepted"
    );
    let run = tempfile::Builder::new()
        .prefix("run-1915-")
        .tempdir_in(&root)?;
    let vault = run.path().join("files");
    fs::create_dir(&vault)?;
    let state = tempfile::Builder::new()
        .prefix("okilum-1915-recovery-")
        .tempdir()?;
    let note = vault.join("note.md");
    note_files::create_with_source(&vault, Path::new("note.md"), b"base\r\n")?;
    ensure!(
        note_files::create(&vault, Path::new("note.md")).is_err(),
        "exclusive create failed"
    );
    let mut editor = FileEditor::open(&note, state.path())?;
    editor.set_text("saved\r\n".into())?;
    ensure!(editor.save()? == Save::Saved, "save did not complete");
    ensure!(fs::read(&note)? == b"saved\r\n", "saved bytes differ");
    editor.set_text("draft\r\n".into())?;
    fs::write(&note, b"external\r\n")?;
    let external_stamp = trash::stamp(&note, &fs::metadata(&note)?);
    ensure!(
        editor.save()? == Save::Conflict,
        "external conflict not detected"
    );
    ensure!(
        fs::read(&note)? == b"external\r\n" && editor.text() == "draft\r\n",
        "conflict lost bytes"
    );
    ensure!(
        trash::stamp(&note, &fs::metadata(&note)?) == external_stamp,
        "conflict changed the external file identity or revision"
    );
    drop(editor);
    MovePlan::prepare(&vault, Path::new("note.md"), Path::new("renamed.md"))?.commit()?;
    fs::create_dir(vault.join("Folder"))?;
    MovePlan::prepare(
        &vault,
        Path::new("renamed.md"),
        Path::new("Folder/moved.md"),
    )?
    .commit()?;
    let snapshot = DirectorySnapshot::read(&vault, Path::new("Folder"))?;
    DirectoryMovePlan::prepare(
        &vault,
        Path::new("Folder"),
        Path::new("MovedFolder"),
        &snapshot,
    )?
    .commit()?;
    let relative = Path::new("MovedFolder/moved.md");
    let trashed = trash::move_to_trash(&vault, relative)?;
    ensure!(!vault.join(relative).exists(), "trash retained source");
    trashed.restore()?;
    ensure!(
        fs::read(vault.join(relative))? == b"external\r\n",
        "trash/Undo bytes differ"
    );
    fn no_tails(root: &Path) -> anyhow::Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            ensure!(
                !entry.file_name().to_string_lossy().starts_with(".okilum-"),
                "temporary file retained"
            );
            if entry.file_type()?.is_dir() {
                no_tails(&entry.path())?;
            }
        }
        Ok(())
    }
    no_tails(&vault)?;
    run.close()?;
    println!(
        "PASS create/save/move/rename/trash/Undo/external-conflict/no-tails; own QA child removed"
    );
    Ok(())
}
#[cfg(not(unix))]
fn main() {
    panic!("Unix acceptance only");
}
