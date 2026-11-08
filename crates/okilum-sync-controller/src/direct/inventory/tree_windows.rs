//! NTFS inspection reuses the Reader's retained ancestors and no-reparse reads.
use super::*;
use okilum_core::windows_files;
use okilum_write_authority::filesystem_identity::Identity;
use std::{
    ffi::OsStr,
    fs::{File, Metadata},
    io::Read,
    os::windows::fs::MetadataExt,
};
pub(super) type Root = windows_files::Directory;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Stamp {
    identity: Option<Identity>,
    attributes: u32,
    size: u64,
    links: Option<u64>,
    created: u64,
    modified: u64,
}
impl From<Metadata> for Stamp {
    fn from(meta: Metadata) -> Self {
        Self {
            identity: None,
            attributes: meta.file_attributes(),
            size: meta.len(),
            links: None,
            created: meta.creation_time(),
            modified: meta.last_write_time(),
        }
    }
}
impl Stamp {
    fn file(file: &File) -> Result<Self> {
        let mut result = Self::from(file.metadata()?);
        result.identity = Some(okilum_write_authority::filesystem_identity::from_file(
            file,
        )?);
        result.links = Some(windows_files::information(file)?.nNumberOfLinks as u64);
        Ok(result)
    }
    fn directory(directory: &Root) -> Result<Self> {
        let mut result = Self::from(directory.metadata()?);
        result.identity = Some(directory.filesystem_identity()?);
        Ok(result)
    }
}
pub(super) struct Tree {
    pub files: BTreeMap<String, Vec<u8>>,
    pub stamps: BTreeMap<String, Stamp>,
    pub policy: String,
}
pub(super) fn open_root(path: &Path) -> Result<Root> {
    let root = Root::open(path)?;
    root.filesystem_identity()?;
    Ok(root)
}
pub(super) fn identity(root: &Root) -> Result<(u64, u64)> {
    let info = root.information()?;
    Ok((
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ))
}
pub(super) fn verify_root(root: &Root, path: &Path) -> Result<()> {
    let current = open_root(path)?;
    ensure!(
        root.filesystem_identity()? == current.filesystem_identity()?,
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
                .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(':')),
        "unsafe or unsupported inventory path"
    );
    for part in path.split('/') {
        windows_files::validate_file_name(OsStr::new(part))?;
    }
    Ok(())
}
pub(super) fn excluded(path: &str) -> bool {
    path == "worktrees"
        || path == ".claude/worktrees"
        || matches!(path, ".stfolder" | ".stversions" | ".stignore")
        || path.split('/').any(|part| {
            part == ".okilum-index"
                || part.starts_with(".okilum-save-")
                || part.starts_with(".okilum-create-")
        })
}
fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
pub(super) fn policy(root: &Root) -> Result<String> {
    let mut file = match root.open_file(OsStr::new(".stignore")) {
        Ok(file) => file,
        Err(error) if missing(&error) => return digest(&source::IGNORES),
        Err(error) => return Err(error),
    };
    let before = Stamp::file(&file)?;
    ensure!(
        before.links == Some(1) && before.size <= 65536,
        "unsafe ignore policy"
    );
    let mut bytes = vec![];
    (&mut file).take(65537).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536 && before == Stamp::file(&file)?,
        "ignore policy changed"
    );
    let text = std::str::from_utf8(&bytes)?;
    let lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect();
    ensure!(
        lines == source::IGNORES,
        "custom ignore policy is unsupported for inspection; explicit review required"
    );
    digest(&source::IGNORES)
}
impl Tree {
    pub fn collect(root: &Root, path: &Path, content: bool) -> Result<Self> {
        verify_root(root, path)?;
        let policy_before = policy(root)?;
        let mut scan = Scan {
            files: BTreeMap::new(),
            stamps: BTreeMap::new(),
            total: 0,
            content,
            volume: identity(root)?.0,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        scan.walk(root, "", 0)?;
        verify_root(root, path)?;
        ensure!(
            policy(root)? == policy_before,
            "ignore policy changed during collection"
        );
        let result = Self {
            files: scan.files,
            stamps: scan.stamps,
            policy: policy_before,
        };
        if content {
            let after = Self::collect(root, path, false)?;
            ensure!(
                result.stamps == after.stamps && result.policy == after.policy,
                "inventory changed during collection"
            );
        }
        Ok(result)
    }
}
struct Scan {
    files: BTreeMap<String, Vec<u8>>,
    stamps: BTreeMap<String, Stamp>,
    total: usize,
    content: bool,
    volume: u64,
    deadline: Instant,
}
impl Scan {
    fn bound(&self, depth: usize) -> Result<()> {
        ensure!(
            depth <= 64 && self.stamps.len() <= MAX_FILES * 2 && Instant::now() < self.deadline,
            "inventory traversal exceeds bound"
        );
        Ok(())
    }
    fn walk(&mut self, directory: &Root, relative: &str, depth: usize) -> Result<()> {
        self.bound(depth)?;
        ensure!(
            identity(directory)?.0 == self.volume,
            "mounted filesystem inside vault is unsupported"
        );
        let before = Stamp::directory(directory)?;
        self.stamps.insert(relative.into(), before.clone());
        for entry in std::fs::read_dir(directory.path())? {
            self.bound(depth)?;
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().context("inventory name is not Unicode")?;
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
            if excluded(&path) && path != ".stignore" {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() && path != ".stignore" {
                let child = open_root(&directory.child(OsStr::new(name))?)?;
                self.walk(&child, &path, depth + 1)?;
            } else if kind.is_file() {
                let mut file = directory.open_file(OsStr::new(name))?;
                let stamp = Stamp::file(&file)?;
                let info = windows_files::information(&file)?;
                ensure!(
                    stamp.links == Some(1)
                        && info.dwVolumeSerialNumber as u64 == self.volume
                        && stamp.size <= MAX_BYTES as u64,
                    "unsafe file, hard link or file exceeds inspection bound"
                );
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
                ensure!(stamp == Stamp::file(&file)?, "file changed during read");
                self.stamps.insert(path, stamp);
            } else {
                anyhow::bail!("symlink, special file or unknown entry in inventory");
            }
        }
        ensure!(
            before == Stamp::directory(directory)?,
            "directory changed during collection"
        );
        Ok(())
    }
}
