//! #1915: run only against a dedicated, explicitly supplied QA mount directory.
//! No GUI or installed app. Creates/removes only its own tempfile child.
#[cfg(unix)]
#[path = "../src/network_write_qa.rs"]
mod network_write_qa;
#[cfg(unix)]
#[path = "../src/reader_trash_fs.rs"]
mod trash;

#[cfg(unix)]
mod diagnostics {
    use std::{
        fs::File,
        io::Write,
        sync::{mpsc, Arc, Mutex},
        time::{Duration, Instant},
    };
    pub struct Stages {
        log: Arc<Mutex<File>>,
        started: Instant,
    }
    impl Stages {
        pub fn new() -> anyhow::Result<Self> {
            // Explicit local tmp; never trust TMPDIR to point outside the mount.
            let (file, path) = tempfile::Builder::new()
                .prefix("okilum-1915-stages-")
                .suffix(".log")
                .tempfile_in("/tmp")?
                .keep()?;
            let this = Self {
                log: Arc::new(Mutex::new(file)),
                started: Instant::now(),
            };
            this.line(&format!("LOG {}", path.display()))?;
            Ok(this)
        }
        fn emit(log: &Mutex<File>, line: &str) -> anyhow::Result<()> {
            // Write/flush local evidence first, then the runner's stdout pipe.
            let mut file = log
                .lock()
                .map_err(|_| anyhow::anyhow!("diagnostic log poisoned"))?;
            writeln!(file, "{line}")?;
            file.flush()?;
            file.sync_data()?;
            drop(file);
            let mut out = std::io::stdout().lock();
            writeln!(out, "{line}")?;
            out.flush()?;
            Ok(())
        }
        pub fn line(&self, text: &str) -> anyhow::Result<()> {
            Self::emit(
                &self.log,
                &format!(
                    "pid={} elapsed_ms={} {text}",
                    std::process::id(),
                    self.started.elapsed().as_millis()
                ),
            )
        }
        pub fn run<T>(
            &self,
            name: &'static str,
            seconds: u64,
            operation: impl FnOnce() -> anyhow::Result<T>,
        ) -> anyhow::Result<T> {
            self.line(&format!("BEGIN {name} timeout_s={seconds}"))?;
            let (done, wait) = mpsc::channel();
            let log = self.log.clone();
            let watchdog = std::thread::spawn(move || {
                if matches!(
                    wait.recv_timeout(Duration::from_secs(seconds)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let message = format!(
                        "pid={} stage {name} timed out; cleanup unverified",
                        std::process::id()
                    );
                    let _ = Self::emit(&log, &message);
                    eprintln!("{message}");
                    // A blocked filesystem syscall cannot be cancelled safely.
                    // Exit the entire example without NFS destructors or retry.
                    std::process::exit(124);
                }
            });
            let result = operation();
            let _ = done.send(());
            watchdog
                .join()
                .map_err(|_| anyhow::anyhow!("stage watchdog panicked"))?;
            match &result {
                Ok(_) => self.line(&format!("END {name} PASS"))?,
                Err(e) => self.line(&format!("END {name} ERROR {e:#}"))?,
            }
            result
        }
    }
}

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
    let stages = diagnostics::Stages::new()?;
    let supplied = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("explicit QA root required")?,
    );
    let encoding_repair = match std::env::args_os().nth(2) {
        None => false,
        Some(option) if option == "--encoding-repair" => true,
        Some(_) => anyhow::bail!("unknown acceptance mode"),
    };
    ensure!(std::env::args_os().nth(3).is_none(), "unexpected arguments");
    ensure!(
        supplied.is_absolute()
            && supplied.components().all(|c| !matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            ))
            && supplied.file_name().is_some_and(|n| n == "okilum-nfs-qa"),
        "only dedicated QA root accepted"
    );
    #[cfg(target_os = "macos")]
    ensure!(
        supplied == Path::new("/Volumes/Maestro/okilum-nfs-qa"),
        "unexpected Mac QA root"
    );
    // No canonicalize/open/stat of NFS precedes this logged, bounded probe.
    stages.run("probe", 10, || {
        ensure!(
            fs::metadata(&supplied)?.is_dir(),
            "QA root is not a directory"
        );
        let mut entries = fs::read_dir(&supplied)?;
        if let Some(entry) = entries.next() {
            entry?;
        }
        Ok(())
    })?;
    let root = stages.run("canonicalize", 10, || Ok(supplied.canonicalize()?))?;
    // Keep instead of a TempDir destructor: every NFS cleanup is an explicit
    // bounded stage. Errors/timeouts retain only our QA child for inspection.
    let run = stages.run("mkdir-run", 30, || {
        Ok(tempfile::Builder::new()
            .prefix("run-1915-")
            .tempdir_in(&root)?
            .keep())
    })?;
    stages.line(&format!("OWN_CHILD {}", run.display()))?;
    let vault = run.join("files");
    stages.run("mkdir-files", 30, || Ok(fs::create_dir(&vault)?))?;
    let state = stages.run("local-recovery", 30, || {
        Ok(tempfile::Builder::new()
            .prefix("okilum-1915-recovery-")
            .tempdir_in("/tmp")?)
    })?;
    if encoding_repair {
        encoding_repair_acceptance(&stages, &vault, state.path())?;
        stages.run("cleanup-encoding-child", 30, || {
            Ok(fs::remove_dir_all(&run)?)
        })?;
        stages.line("PASS encoding Repair/Edit/byte-exact History Restore/conflict/recovery failure/no-tails; own QA child removed")?;
        return Ok(());
    }
    let note = vault.join("note.md");
    stages.run("create", 30, || {
        note_files::create_with_source(&vault, Path::new("note.md"), b"base\r\n")
    })?;
    stages.run("create-collision", 30, || {
        ensure!(
            note_files::create(&vault, Path::new("note.md")).is_err(),
            "exclusive create failed"
        );
        Ok(())
    })?;
    let mut editor = stages.run("editor-open", 30, || FileEditor::open(&note, state.path()))?;
    stages.run("draft-write", 30, || editor.set_text("saved\r\n".into()))?;
    stages.run("save", 30, || {
        ensure!(editor.save()? == Save::Saved, "save did not complete");
        Ok(())
    })?;
    stages.run("verify-save", 30, || {
        ensure!(fs::read(&note)? == b"saved\r\n", "saved bytes differ");
        Ok(())
    })?;
    stages.run("conflict-draft", 30, || editor.set_text("draft\r\n".into()))?;
    let external_stamp = stages.run("external-write", 30, || {
        fs::write(&note, b"external\r\n")?;
        Ok(trash::stamp(&note, &fs::metadata(&note)?))
    })?;
    stages.run("conflict-save", 30, || {
        ensure!(
            editor.save()? == Save::Conflict,
            "external conflict not detected"
        );
        Ok(())
    })?;
    stages.run("verify-conflict", 30, || {
        ensure!(
            fs::read(&note)? == b"external\r\n" && editor.text() == "draft\r\n",
            "conflict lost bytes"
        );
        ensure!(
            trash::stamp(&note, &fs::metadata(&note)?) == external_stamp,
            "conflict changed external revision"
        );
        Ok(())
    })?;
    stages.run("editor-close", 30, || {
        drop(editor);
        Ok(())
    })?;
    stages.run("rename", 30, || {
        MovePlan::prepare(&vault, Path::new("note.md"), Path::new("renamed.md"))?.commit()?;
        Ok(())
    })?;
    stages.run("mkdir-folder", 30, || {
        Ok(fs::create_dir(vault.join("Folder"))?)
    })?;
    stages.run("move-file", 30, || {
        MovePlan::prepare(
            &vault,
            Path::new("renamed.md"),
            Path::new("Folder/moved.md"),
        )?
        .commit()?;
        Ok(())
    })?;
    let snapshot = stages.run("folder-inventory", 30, || {
        DirectorySnapshot::read(&vault, Path::new("Folder"))
    })?;
    stages.run("move-folder", 30, || {
        DirectoryMovePlan::prepare(
            &vault,
            Path::new("Folder"),
            Path::new("MovedFolder"),
            &snapshot,
        )?
        .commit()?;
        Ok(())
    })?;
    let relative = Path::new("MovedFolder/moved.md");
    let before_trash = stages.run("trash-preimage", 30, || {
        Ok((
            fs::read(vault.join(relative))?,
            trash::stamp(&vault.join(relative), &fs::metadata(vault.join(relative))?),
        ))
    })?;
    let trashed = stages.run("trash-with-vault-fallback", 30, || {
        trash::move_to_trash(&vault, relative)
    })?;
    stages.line(if trashed.in_vault_trash() {
        "VAULT_TRASH fallback selected"
    } else {
        "SYSTEM_TRASH selected"
    })?;
    #[cfg(target_os = "macos")]
    ensure!(
        trashed.in_vault_trash(),
        "NFS QA expected vault Trash fallback"
    );
    stages.run("verify-trash", 30, || {
        ensure!(!vault.join(relative).exists(), "trash retained source");
        Ok(())
    })?;
    stages.run("undo", 30, || trashed.restore())?;
    stages.run("verify-undo", 30, || {
        ensure!(
            fs::read(vault.join(relative))? == before_trash.0,
            "Undo changed source bytes"
        );
        ensure!(
            trash::stamp(&vault.join(relative), &fs::metadata(vault.join(relative))?)
                == before_trash.1,
            "Undo changed source identity or revision"
        );
        Ok(())
    })?;
    let redone = stages.run("redo-trash", 30, || trash::move_to_trash(&vault, relative))?;
    stages.run("redo-undo", 30, || redone.restore())?;
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
    stages.run("no-tails", 30, || no_tails(&vault))?;
    stages.run("cleanup-r2-child", 30, || {
        let prior = root.join("run-1915-HXNtBM");
        match fs::symlink_metadata(&prior) {
            Ok(meta) => {
                ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "prior QA child replaced"
                );
                fs::remove_dir_all(prior)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    })?;
    stages.run("cleanup-own-child", 30, || Ok(fs::remove_dir_all(&run)?))?;
    stages.line(
        "PASS create/save/move/rename/trash/Undo/external-conflict/no-tails; own QA child removed",
    )?;
    stages.run("cleanup-local-stage-logs", 30, || {
        use std::os::unix::fs::MetadataExt;
        for entry in fs::read_dir("/tmp")? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("okilum-1915-stages-") && name.ends_with(".log") {
                let metadata = fs::symlink_metadata(entry.path())?;
                ensure!(
                    metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.uid() == unsafe { libc::geteuid() },
                    "foreign QA log retained"
                );
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    })?;
    // END is flushed through the still-open unlinked log descriptor and stdout.
    Ok(())
}

/// Reuse the existing bounded NFS admission/stages, touching only this run's
/// synthetic child. Native unit tests cover injected post-publication failures.
#[cfg(unix)]
fn encoding_repair_acceptance(
    stages: &diagnostics::Stages,
    vault: &std::path::Path,
    state: &std::path::Path,
) -> anyhow::Result<()> {
    use anyhow::ensure;
    use okilum_core::file_editor::{FileEditor, Save};
    use std::{fs, os::unix::fs::MetadataExt};

    let note = vault.join("Synthetic.md");
    let original = b"Before \xff After\r\n";
    let repaired = String::from_utf8_lossy(original);
    let clean = |expected: &[u8]| -> anyhow::Result<()> {
        ensure!(fs::read(&note)? == expected, "encoding bytes changed");
        ensure!(fs::metadata(&note)?.nlink() == 1, "preimage link retained");
        network_write_qa::check_note_only(vault, std::ffi::OsStr::new("Synthetic.md"))
    };
    stages.run("encoding-create", 30, || Ok(fs::write(&note, original)?))?;
    stages.run("encoding-no-click", 30, || {
        ensure!(
            FileEditor::open(&note, state).is_err(),
            "damaged note editable"
        );
        clean(original)
    })?;
    stages.run("encoding-repair", 30, || {
        ensure!(
            FileEditor::repair_encoding(&note, state)? == Save::Saved,
            "Repair refused"
        );
        clean(repaired.as_bytes())
    })?;
    stages.run("encoding-edit-unlocked", 30, || {
        let editor = FileEditor::open(&note, state)?;
        ensure!(editor.text() == repaired, "editable text differs");
        drop(editor);
        clean(repaired.as_bytes())
    })?;
    let retained = stages.run("encoding-history", 30, || {
        let history = okilum_core::source_history::list(state, vault)?;
        ensure!(history.warnings.is_empty(), "History contains warnings");
        let versions: Vec<_> = history.versions.iter().filter(|v| v.note == note).collect();
        ensure!(versions.len() == 1, "expected one original History version");
        ensure!(
            versions[0].bytes() == original,
            "History lost original bytes"
        );
        Ok(versions[0].bytes().to_vec())
    })?;
    stages.run("encoding-history-restore", 30, || {
        ensure!(
            FileEditor::replace_bytes(&note, state, repaired.as_bytes(), &retained)? == Save::Saved,
            "History Restore refused"
        );
        ensure!(
            FileEditor::open(&note, state).is_err(),
            "restored note editable"
        );
        clean(original)
    })?;
    stages.run("encoding-stale-revision", 30, || {
        ensure!(
            FileEditor::replace_bytes(&note, state, b"stale reviewed bytes", repaired.as_bytes())?
                == Save::Conflict,
            "stale revision was accepted"
        );
        clean(original)
    })?;
    stages.run("encoding-recovery-failure", 30, || {
        let blocked = state.join("blocked");
        fs::create_dir(&blocked)?;
        fs::write(blocked.join("source-history"), b"blocks History directory")?;
        ensure!(
            FileEditor::repair_encoding(&note, &blocked).is_err(),
            "blocked History was accepted"
        );
        clean(original)
    })?;
    Ok(())
}
#[cfg(not(unix))]
fn main() {
    panic!("Unix acceptance only");
}
