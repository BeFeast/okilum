//! Descriptor-relative, no-follow traversal; names never become absolute reads.
use super::*;
use rustix::fs::{open, openat, Dir, FileType, Mode, OFlags};
use std::{fs::Metadata, io::Read, os::unix::fs::MetadataExt, path::Component};
pub(super) type Root = File;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl From<Metadata> for Stamp {
    fn from(m: Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode(),
            size: m.len(),
            links: m.nlink(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}
pub(super) struct Tree {
    pub files: BTreeMap<String, Vec<u8>>,
    pub stamps: BTreeMap<String, Stamp>,
    pub policy: String,
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
}
pub(super) fn open_root(path: &Path) -> Result<File> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut fd = open("/", flags() | OFlags::DIRECTORY, Mode::empty())?;
    for part in path.components() {
        match part {
            Component::RootDir => (),
            Component::Normal(name) => {
                fd = openat(&fd, name, flags() | OFlags::DIRECTORY, Mode::empty())?
            }
            _ => anyhow::bail!("inspection requires a real directory without parent traversal"),
        }
    }
    Ok(File::from(fd))
}
pub(super) fn identity(root: &File) -> Result<(u64, u64)> {
    let m = root.metadata()?;
    Ok((m.dev(), m.ino()))
}
pub(super) fn stable_identity(
    root: &Root,
) -> Result<okilum_write_authority::filesystem_identity::Identity> {
    okilum_write_authority::filesystem_identity::from_file(root)
}
pub(super) fn verify_root(root: &File, path: &Path) -> Result<()> {
    let current = open_root(path)?;
    ensure!(
        identity(root)? == identity(&current)?,
        "inspection root replaced"
    );
    Ok(())
}
pub(super) fn validate_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.len() <= 1024
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains(':')),
        "unsafe or unsupported inventory path"
    );
    Ok(())
}
pub(super) fn excluded(path: &str) -> bool {
    path == "worktrees"
        || path == ".claude/worktrees"
        || matches!(path, ".stfolder" | ".stversions" | ".stignore")
        || path.split('/').any(|p| {
            p == ".okilum-index"
                || p.starts_with(".okilum-save-")
                || p.starts_with(".okilum-create-")
        })
}
/// This slice deliberately refuses custom glob/include/negation policies rather
/// than approximating Syncthing's matcher and exposing excluded user content.
/// Missing policy on an unenrolled destination means the same canonical rules.
pub(super) fn policy(root: &File) -> Result<String> {
    let expected = source::IGNORES;
    let fd = match openat(root, ".stignore", flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return digest(&expected),
        Err(e) => return Err(e.into()),
    };
    let mut file = File::from(fd);
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.nlink() == 1 && before.len() <= 65536,
        "unsafe ignore policy"
    );
    let mut bytes = vec![];
    (&mut file).take(65537).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536 && Stamp::from(before) == Stamp::from(file.metadata()?),
        "ignore policy changed"
    );
    let text = std::str::from_utf8(&bytes)?;
    let lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect();
    ensure!(
        lines == expected,
        "custom ignore policy is unsupported for inspection; explicit review required"
    );
    digest(&expected)
}
impl Tree {
    pub fn collect(root: &File, path: &Path, content: bool) -> Result<Self> {
        verify_root(root, path)?;
        let first_policy = policy(root)?;
        let mut scan = Scan {
            files: BTreeMap::new(),
            stamps: BTreeMap::new(),
            total: 0,
            content,
            device: root.metadata()?.dev(),
            deadline: Instant::now() + Duration::from_secs(2),
        };
        scan.walk(root, "", 0)?;
        verify_root(root, path)?;
        ensure!(
            policy(root)? == first_policy,
            "ignore policy changed during collection"
        );
        let tree = Self {
            files: scan.files,
            stamps: scan.stamps,
            policy: first_policy,
        };
        if content {
            let after = Self::collect(root, path, false)?;
            ensure!(
                tree.stamps == after.stamps && tree.policy == after.policy,
                "inventory changed during collection"
            );
        }
        Ok(tree)
    }
}
struct Scan {
    files: BTreeMap<String, Vec<u8>>,
    stamps: BTreeMap<String, Stamp>,
    total: usize,
    content: bool,
    device: u64,
    deadline: Instant,
}
impl Scan {
    fn walk(&mut self, dir: &File, relative: &str, depth: usize) -> Result<()> {
        ensure!(
            depth <= 64 && self.stamps.len() <= MAX_FILES * 2 && Instant::now() < self.deadline,
            "inventory traversal exceeds bound"
        );
        let before = Stamp::from(dir.metadata()?);
        ensure!(
            before.dev == self.device,
            "mounted filesystem inside vault is unsupported"
        );
        self.stamps.insert(relative.into(), before.clone());
        for entry in Dir::read_from(dir)? {
            ensure!(
                self.stamps.len() <= MAX_FILES * 2 && Instant::now() < self.deadline,
                "inventory traversal exceeds bound"
            );
            let entry = entry?;
            let name = entry.file_name().to_str()?;
            if name == "." || name == ".." {
                continue;
            }
            let path = if relative.is_empty() {
                name.to_owned()
            } else {
                format!("{relative}/{name}")
            };
            validate_path(&path)?;
            ensure!(
                relative.is_empty() || !matches!(name, ".stfolder" | ".stversions" | ".stignore"),
                "nested runtime metadata has unsupported canonical semantics"
            );
            // Never follow excluded entries, including symlinks. .stignore is
            // separately checked no-follow and included in revision metadata.
            if excluded(&path) && path != ".stignore" {
                continue;
            }
            match entry.file_type() {
                FileType::Directory if path != ".stignore" => {
                    let child = File::from(openat(
                        dir,
                        entry.file_name(),
                        flags() | OFlags::DIRECTORY,
                        Mode::empty(),
                    )?);
                    self.walk(&child, &path, depth + 1)?;
                }
                FileType::RegularFile => {
                    let mut file =
                        File::from(openat(dir, entry.file_name(), flags(), Mode::empty())?);
                    let meta = file.metadata()?;
                    ensure!(
                        meta.is_file()
                            && meta.nlink() == 1
                            && meta.dev() == self.device
                            && meta.len() <= MAX_BYTES as u64,
                        "unsafe file, hard link or file exceeds inspection bound"
                    );
                    let stamp = Stamp::from(meta);
                    if path != ".stignore" {
                        ensure!(
                            self.files.len() < MAX_FILES,
                            "file count exceeds inspection bound"
                        );
                        self.total = self
                            .total
                            .checked_add(stamp.size as usize)
                            .context("inventory size overflow")?;
                        ensure!(
                            self.total <= MAX_BYTES,
                            "inventory bytes exceed inspection bound"
                        );
                        let mut bytes = vec![];
                        if self.content {
                            (&mut file).take(stamp.size + 1).read_to_end(&mut bytes)?;
                            ensure!(
                                bytes.len() == stamp.size as usize,
                                "file changed during read"
                            );
                        }
                        self.files.insert(path.clone(), bytes);
                    }
                    ensure!(
                        stamp == Stamp::from(file.metadata()?),
                        "file changed during read"
                    );
                    self.stamps.insert(path, stamp);
                }
                _ => anyhow::bail!("symlink, special file or unknown entry in inventory"),
            }
        }
        ensure!(
            before == Stamp::from(dir.metadata()?),
            "directory changed during collection"
        );
        Ok(())
    }
}
