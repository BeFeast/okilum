//! Native NTFS publication retains displaced inodes and uses the Reader's
//! no-replace primitives and source DACL/EFS preparation, never blind Replace.
use super::*;
use okilum_core::windows_files::Directory;
use std::{ffi::OsStr, fs::File};

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
fn read_file(mut file: File) -> Result<Vec<u8>> {
    okilum_write_authority::filesystem_identity::from_file(&file)?;
    let info = okilum_core::windows_files::information(&file)?;
    ensure!(
        info.nNumberOfLinks == 1 && file.metadata()?.len() <= MAX_BYTES as u64,
        "unsafe retained first Sync preimage"
    );
    let mut bytes = vec![];
    (&mut file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "retained first Sync file exceeds bound"
    );
    Ok(bytes)
}
fn parent(root: &Directory, path: &str, create: bool) -> Result<(Directory, String)> {
    tree::validate_path(path)?;
    let mut parts = path.split('/').peekable();
    let mut directory = tree::open_root(root.path())?;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return Ok((directory, part.into()));
        }
        let child = directory.child(OsStr::new(part))?;
        directory = match tree::open_root(&child) {
            Ok(child) => child,
            Err(error) if create && missing(&error) => {
                directory.create_directory(OsStr::new(part))?;
                tree::open_root(&child)?
            }
            Err(error) => return Err(error),
        };
    }
    anyhow::bail!("empty merge file path")
}
pub(super) fn read_retained(root: &Directory, path: &str, retained: &str) -> Result<Vec<u8>> {
    let (directory, _) = parent(root, path, false)?;
    read_file(directory.open_file(OsStr::new(retained))?)
}
impl Store {
    pub fn publish(&mut self, validate: impl Fn() -> Result<()>) -> Result<()> {
        ensure!(
            matches!(
                self.binding.phase,
                Phase::Prepared | Phase::Publishing | Phase::Published
            ),
            "first Sync not prepared"
        );
        validate()?;
        self.compatible(self.binding.phase != Phase::Prepared)?;
        self.binding.phase = Phase::Publishing;
        self.save()?;
        let plan = self.plan()?;
        let root = tree::open_root(&self.binding.source.path)?;
        let originals = if self.binding.remote_side {
            &plan.originals().1.files
        } else {
            &plan.originals().0.files
        };
        for (path, bytes) in plan.merged_files() {
            validate()?;
            tree::verify_root(&root, &self.binding.source.path)?;
            let (directory, name) = parent(&root, path, true)?;
            let retained = self.retained(path);
            let source = match directory.open_file(OsStr::new(&name)) {
                Ok(file) => {
                    let current = read_file(file)?;
                    if current == *bytes {
                        continue;
                    }
                    ensure!(
                        originals.get(path) == Some(&current),
                        "third version stops first Sync publication"
                    );
                    directory.rename(
                        OsStr::new(&name),
                        &directory,
                        OsStr::new(&retained),
                        &current,
                    )?;
                    checkpoint(
                        if self.binding.remote_side {
                            "source-captured"
                        } else {
                            "destination-captured"
                        },
                        self.binding.operation,
                    );
                    #[cfg(feature = "first-sync-fixture")]
                    if self.binding.remote_side && path == "different.md" {
                        checkpoint("source-note-captured", self.binding.operation);
                    }
                    ensure!(
                        read_retained(&root, path, &retained)? == current,
                        "writer changed captured first Sync preimage; recovery required"
                    );
                    Some(OsStr::new(&retained))
                }
                Err(error) if missing(&error) => match directory.open_file(OsStr::new(&retained)) {
                    Ok(file) => {
                        let captured = read_file(file)?;
                        ensure!(
                            originals.get(path) == Some(&captured),
                            "third version in retained first Sync preimage; recovery stopped"
                        );
                        Some(OsStr::new(&retained))
                    }
                    Err(error) if missing(&error) => None,
                    Err(error) => return Err(error),
                },
                Err(error) => return Err(error),
            };
            // Directory pins every ancestor; the staged bytes inherit exactly
            // the captured source DACL/encryption before no-replace publication.
            directory.create_preserving(OsStr::new(&name), bytes, source)?;
        }
        validate()?;
        ensure!(
            Tree::collect(&root, &self.binding.source.path, true)?.files == *plan.merged_files(),
            "first Sync readback differs; recovery required"
        );
        self.binding.phase = Phase::Published;
        self.save()?;
        checkpoint(
            if self.binding.remote_side {
                "source-published"
            } else {
                "destination-published"
            },
            self.binding.operation,
        );
        Ok(())
    }
}
