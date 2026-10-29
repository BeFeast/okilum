//! #1915: run only against a dedicated, explicitly supplied QA mount directory.
//! No GUI or installed app. Creates/removes only its own tempfile child.
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
    let trashed = stages.run("trash", 30, || trash::move_to_trash(&vault, relative))?;
    stages.run("verify-trash", 30, || {
        ensure!(!vault.join(relative).exists(), "trash retained source");
        Ok(())
    })?;
    stages.run("undo", 30, || trashed.restore())?;
    stages.run("verify-undo", 30, || {
        ensure!(
            fs::read(vault.join(relative))? == b"external\r\n",
            "trash/Undo bytes differ"
        );
        Ok(())
    })?;
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
    stages.run("cleanup-own-child", 30, || Ok(fs::remove_dir_all(&run)?))?;
    stages.line(
        "PASS create/save/move/rename/trash/Undo/external-conflict/no-tails; own QA child removed",
    )?;
    Ok(())
}
#[cfg(not(unix))]
fn main() {
    panic!("Unix acceptance only");
}
