//! Read-only agent-file imports. All offsets refer to original UTF-8 source bytes.
use std::{
    ops::Range,
    path::{Component, Path, PathBuf},
};

/// Document behavior is independent of tree/search visibility. The root contract
/// participates in Rules, while remaining an ordinary vault inventory entry.
pub fn is_agent_document(path: &str) -> bool {
    path == "@AgentContract.md" || crate::quick_open::is_agent_file(path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub line: Range<usize>,
    pub target_bytes: Range<usize>,
    pub target: String,
}

/// Recognize standalone @path lines, excluding fenced and indented code.
/// Whitespace in a path is literal; shell quoting and interpolation are never evaluated.
/// A line whose target itself starts with `@` (`@@name`) is not an import:
/// Claude Code silently ignores it (#2939). See [`ignored`].
pub fn parse(source: &str) -> Vec<Import> {
    parse_bounded(source, usize::MAX)
}

/// Parse at most `max_imports` directives without allocating for the remainder.
pub fn parse_bounded(source: &str, max_imports: usize) -> Vec<Import> {
    scan(source, max_imports, false)
}

/// Standalone `@@name` lines (#2939). Claude Code imports a file whose name
/// starts with `@` only as `@./@name`, `@~/@name` or `@/…`; it ignores `@@name`
/// without a message. These lines are never imports: no link, expansion or
/// Rules layer. `target` keeps the text after the first `@`, e.g. `@name`.
pub fn ignored(source: &str) -> Vec<Import> {
    ignored_bounded(source, usize::MAX)
}

/// [`ignored`] with the same allocation bound as [`parse_bounded`].
pub fn ignored_bounded(source: &str, max: usize) -> Vec<Import> {
    scan(source, max, true)
}

/// The user-facing explanation for an [`ignored`] line; `target` is its
/// [`Import::target`] (it starts with `@`).
pub fn ignored_message(target: &str) -> String {
    format!("Not an import: Claude Code ignores `@{target}`. Write `@./{target}`.")
}

fn scan(source: &str, max_imports: usize, ignored: bool) -> Vec<Import> {
    let mut result = Vec::new();
    let mut offset = 0;
    let mut fence: Option<(u8, usize)> = None;
    for line in source.split_inclusive('\n') {
        if result.len() >= max_imports {
            break;
        }
        let text = line.trim_end_matches(['\r', '\n']);
        let leading = text.len() - text.trim_start_matches(' ').len();
        let body = &text[leading..];
        let marker = body.as_bytes().first().copied();
        let count = marker.map_or(0, |m| body.bytes().take_while(|b| *b == m).count());
        if leading <= 3 && matches!(marker, Some(b'`' | b'~')) && count >= 3 {
            if let Some((m, n)) = fence {
                if marker == Some(m) && count >= n && body[count..].trim().is_empty() {
                    fence = None;
                }
            } else if marker != Some(b'`') || !body[count..].contains('`') {
                fence = Some((marker.unwrap(), count));
            }
        } else if fence.is_none() && leading <= 3 && body.starts_with('@') {
            let target = body[1..].trim_end();
            if !target.is_empty()
                && !target.chars().any(char::is_control)
                && target.starts_with('@') == ignored
            {
                let start = offset + leading + 1;
                result.push(Import {
                    line: offset..offset + text.len(),
                    target_bytes: start..start + target.len(),
                    target: target.into(),
                });
            }
        }
        offset += line.len();
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    OutsideRoot,
    InvalidPath,
    Missing,
    Unreadable,
    Cycle,
    Depth,
    Budget,
    /// Source changed while it was being read, or its root was replaced.
    Changed,
}

/// Implementations must resolve symlinks before returning a canonical absolute path.
/// `read` must enforce `max_bytes` before allocating an unbounded source.
pub trait Source {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, Failure>;
    fn read(&self, canonical: &Path, max_bytes: usize) -> Result<String, Failure>;
}

/// Concrete filesystem reader bound to one canonical vault root. No writes,
/// interpolation, shell execution, or unbounded whole-file reads are performed.
/// Unix reads walk directory descriptors with NOFOLLOW after resolution, so a
/// replaced symlink cannot redirect an already resolved import outside the root.
/// Windows reuses the local-NTFS pinned-ancestry reader and refuses reparse points,
/// offline files, network locations and unsupported names. Other platforms fail closed.
pub struct RootedSource {
    root: PathBuf,
    #[cfg(unix)]
    directory: std::fs::File,
    #[cfg(windows)]
    windows: windows_rooted::Root,
}

fn io_failure(error: std::io::Error) -> Failure {
    match error.kind() {
        std::io::ErrorKind::NotFound => Failure::Missing,
        _ => Failure::Unreadable,
    }
}

impl RootedSource {
    pub fn new(root: &Path) -> Result<Self, Failure> {
        #[cfg(unix)]
        let root = std::fs::canonicalize(root).map_err(io_failure)?;
        #[cfg(unix)]
        {
            let directory = open_directory(&root)?;
            Ok(Self { root, directory })
        }
        #[cfg(windows)]
        {
            let (root, windows) = windows_rooted::Root::new(root)?;
            Ok(Self { root, windows })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = root;
            Err(Failure::Unreadable)
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(unix)]
    fn validate_root(&self) -> Result<(), Failure> {
        use std::os::unix::fs::MetadataExt;
        let current = open_directory(&self.root)?.metadata().map_err(io_failure)?;
        let held = self.directory.metadata().map_err(io_failure)?;
        if (current.dev(), current.ino()) != (held.dev(), held.ino()) {
            return Err(Failure::Changed);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Result<std::fs::File, Failure> {
    use rustix::fs::{openat, Mode, OFlags, CWD};
    if !path.is_absolute() {
        return Err(Failure::InvalidPath);
    }
    let mut directory = std::fs::File::from(
        openat(
            CWD,
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io_failure(e.into()))?,
    );
    for part in path.components() {
        match part {
            Component::RootDir => (),
            Component::Normal(name) => {
                directory = std::fs::File::from(
                    openat(
                        &directory,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(|e| io_failure(e.into()))?,
                );
            }
            _ => return Err(Failure::InvalidPath),
        }
    }
    Ok(directory)
}

impl Source for RootedSource {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, Failure> {
        #[cfg(windows)]
        {
            return self.windows.canonicalize(&self.root, path);
        }
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            self.validate_root()?;
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.root.join(path)
            };
            if !normalize(&path)?.starts_with(&self.root) {
                return Err(Failure::OutsideRoot);
            }
            let canonical = std::fs::canonicalize(path).map_err(io_failure)?;
            if !canonical.starts_with(&self.root) {
                return Err(Failure::OutsideRoot);
            }
            Ok(canonical)
        }
    }

    fn read(&self, canonical: &Path, max_bytes: usize) -> Result<String, Failure> {
        #[cfg(unix)]
        {
            use rustix::fs::{openat, Mode, OFlags};
            use std::{io::Read, os::unix::fs::MetadataExt};
            self.validate_root()?;
            let relative = canonical
                .strip_prefix(&self.root)
                .map_err(|_| Failure::OutsideRoot)?;
            let parts: Vec<_> = relative.components().collect();
            if parts.is_empty() || parts.iter().any(|p| !matches!(p, Component::Normal(_))) {
                return Err(Failure::InvalidPath);
            }
            let mut directory = self.directory.try_clone().map_err(io_failure)?;
            for (index, part) in parts.iter().enumerate() {
                let Component::Normal(name) = part else {
                    unreachable!()
                };
                let last = index + 1 == parts.len();
                let mut flags =
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
                if !last {
                    flags |= OFlags::DIRECTORY;
                }
                let file = std::fs::File::from(
                    openat(&directory, *name, flags, Mode::empty())
                        .map_err(|e| io_failure(e.into()))?,
                );
                if !last {
                    directory = file;
                    continue;
                }
                let before = file.metadata().map_err(io_failure)?;
                if !before.is_file() {
                    return Err(Failure::Unreadable);
                }
                if before.len() > max_bytes as u64 {
                    return Err(Failure::Budget);
                }
                // The extra byte detects growth even if metadata was initially small.
                let mut bytes = Vec::new();
                (&file)
                    .take((max_bytes as u64).saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(io_failure)?;
                if bytes.len() > max_bytes {
                    return Err(Failure::Budget);
                }
                let after = file.metadata().map_err(io_failure)?;
                let revision = |m: &std::fs::Metadata| {
                    (
                        m.len(),
                        m.mtime(),
                        m.mtime_nsec(),
                        m.ctime(),
                        m.ctime_nsec(),
                    )
                };
                if revision(&before) != revision(&after) || after.len() != bytes.len() as u64 {
                    return Err(Failure::Changed);
                }
                self.validate_root()?;
                return String::from_utf8(bytes).map_err(|_| Failure::Unreadable);
            }
            Err(Failure::InvalidPath)
        }
        #[cfg(windows)]
        {
            self.windows.read(&self.root, canonical, max_bytes)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (canonical, max_bytes);
            Err(Failure::Unreadable)
        }
    }
}

// This read/revision policy is shared by the Windows adapter and host-side mocks.
// Frozen public APIs deliberately keep unsupported locations as Unreadable.
#[cfg(any(windows, test))]
mod windows_policy {
    use super::Failure;
    use std::io::Read;
    // Win32 REPARSE_POINT, OFFLINE, RECALL_ON_OPEN, RECALL_ON_DATA_ACCESS.
    pub(super) const DENIED_ATTRIBUTES: u32 = 0x400 | 0x1000 | 0x40000 | 0x400000;
    #[cfg(windows)]
    pub(super) type Identity = crate::windows_files::Identity;
    #[cfg(not(windows))]
    pub(super) type Identity = (u32, u32, u32);
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Revision {
        pub identity: Identity,
        pub length: u64,
        pub write_time: u64,
        pub change_time: i64,
        pub creation_time: u64,
        pub attributes: u32,
        pub links: u32,
        pub regular: bool,
    }
    pub(super) fn validate(revision: &Revision) -> Result<(), Failure> {
        if !revision.regular || revision.links != 1 || revision.attributes & DENIED_ATTRIBUTES != 0
        {
            return Err(Failure::Unreadable);
        }
        Ok(())
    }
    pub(super) fn ancestry(expected: &[Identity], actual: &[Identity]) -> Result<(), Failure> {
        if !actual.starts_with(expected) {
            return Err(Failure::Changed);
        }
        Ok(())
    }
    pub(super) fn read(
        mut reader: impl Read,
        max_bytes: usize,
        mut revision: impl FnMut() -> Result<Revision, Failure>,
        mut location: impl FnMut() -> Result<(), Failure>,
    ) -> Result<String, Failure> {
        let before = revision()?;
        validate(&before)?;
        location()?;
        if before.length > max_bytes as u64 {
            return Err(Failure::Budget);
        }
        let mut bytes = Vec::new();
        reader
            .by_ref()
            .take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(super::io_failure)?;
        if bytes.len() > max_bytes {
            return Err(Failure::Budget);
        }
        let after = revision()?;
        validate(&after)?;
        if before != after || after.length != bytes.len() as u64 {
            return Err(Failure::Changed);
        }
        location()?;
        String::from_utf8(bytes).map_err(|_| Failure::Unreadable)
    }
}

#[cfg(windows)]
mod windows_rooted {
    use super::{
        windows_policy::{self, Identity, Revision},
        *,
    };
    use crate::windows_files::{self, Directory};
    use std::{
        ffi::OsString,
        fs::{File, OpenOptions},
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
        path::Prefix,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
        FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
        FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
        FILE_SHARE_DELETE, FILE_SHARE_READ, VOLUME_NAME_DOS,
    };
    const _: () = assert!(
        windows_policy::DENIED_ATTRIBUTES
            == FILE_ATTRIBUTE_REPARSE_POINT
                | FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
    );

    pub(super) struct Root {
        identities: Vec<Identity>,
    }

    fn failure(error: anyhow::Error) -> Failure {
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Failure::Missing;
            }
        }
        Failure::Unreadable
    }
    // Only drive-qualified local names reach native primitives. UNC, device,
    // drive-relative and rooted-without-drive paths cannot trigger network I/O.
    fn local(path: &Path) -> Result<(u8, Vec<Component<'_>>), Failure> {
        let mut parts = path.components();
        let drive = match parts.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive.to_ascii_uppercase(),
                _ => return Err(Failure::Unreadable),
            },
            _ => return Err(Failure::InvalidPath),
        };
        if parts.next() != Some(Component::RootDir) {
            return Err(Failure::InvalidPath);
        }
        Ok((drive, parts.collect()))
    }
    fn same_name(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
        // NTFS can enable case-sensitive directories. Refuse noncanonical root
        // spelling instead of guessing whether case folding would name its sibling.
        a == b
    }

    fn relative(root: &Path, path: &Path) -> Result<Vec<OsString>, Failure> {
        let (root_drive, root_parts) = local(root)?;
        let (drive, parts) = local(path)?;
        if drive != root_drive || parts.len() < root_parts.len() {
            return Err(Failure::OutsideRoot);
        }
        for (a, b) in root_parts.iter().zip(&parts) {
            match (a, b) {
                (Component::Normal(a), Component::Normal(b)) if same_name(a, b) => (),
                _ => return Err(Failure::OutsideRoot),
            }
        }
        parts[root_parts.len()..]
            .iter()
            .map(|part| match part {
                Component::Normal(name) => Ok(name.to_os_string()),
                Component::ParentDir => Ok(OsString::from("..")),
                Component::CurDir => Ok(OsString::from(".")),
                _ => Err(Failure::InvalidPath),
            })
            .collect()
    }
    // Reuse the approved no-DELETE-sharing ancestry pins. Inspect every directory
    // before opening its child, refusing even cloud reparse directories here.
    fn pin(path: &Path) -> Result<Directory, Failure> {
        let (_, parts) = local(path)?;
        if parts.iter().any(|p| !matches!(p, Component::Normal(_))) {
            return Err(Failure::InvalidPath);
        }
        let mut current = path
            .ancestors()
            .last()
            .ok_or(Failure::InvalidPath)?
            .to_path_buf();
        let mut directory = Directory::open(&current).map_err(failure)?;
        check_directory(&directory)?;
        for part in parts {
            let Component::Normal(name) = part else {
                unreachable!()
            };
            current = directory.child(name).map_err(failure)?;
            let next = Directory::open(&current).map_err(failure)?;
            check_directory(&next)?;
            directory = next;
        }
        Ok(directory)
    }
    fn check_directory(directory: &Directory) -> Result<(), Failure> {
        if directory.information().map_err(failure)?.dwFileAttributes
            & windows_policy::DENIED_ATTRIBUTES
            != 0
        {
            return Err(Failure::Unreadable);
        }
        Ok(())
    }
    fn open_import(directory: &Directory, name: &std::ffi::OsStr) -> Result<File, Failure> {
        let path = directory.child(name).map_err(failure)?;
        // The pinned parent/name validation is shared with Windows source editing.
        // Add OPEN_NO_RECALL for imports: unsupported placeholders must never be
        // hydrated by merely expanding a reference. Omit WRITE sharing during read.
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL)
            .open(path)
            .map_err(io_failure)
    }
    fn final_path(file: &File) -> Result<PathBuf, Failure> {
        use std::os::windows::ffi::OsStringExt;
        // A local extended-length Windows path is bounded at 32K UTF-16 units.
        let mut buffer = vec![0u16; 32768];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(Failure::Unreadable);
        }
        let path = PathBuf::from(OsString::from_wide(&buffer[..length]));
        local(&path)?;
        Ok(path)
    }
    fn revision(file: &File) -> Result<Revision, Failure> {
        let info = windows_files::information(file).map_err(failure)?;
        let mut basic = FILE_BASIC_INFO::default();
        // The owned file handle remains alive and basic has the exact Win32 layout.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(Failure::Unreadable);
        }
        Ok(Revision {
            identity: windows_files::identity(&info),
            length: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            write_time: ((info.ftLastWriteTime.dwHighDateTime as u64) << 32)
                | info.ftLastWriteTime.dwLowDateTime as u64,
            creation_time: ((info.ftCreationTime.dwHighDateTime as u64) << 32)
                | info.ftCreationTime.dwLowDateTime as u64,
            change_time: basic.ChangeTime,
            attributes: info.dwFileAttributes,
            links: info.nNumberOfLinks,
            regular: file.metadata().map_err(io_failure)?.is_file(),
        })
    }
    impl Root {
        pub(super) fn new(path: &Path) -> Result<(PathBuf, Self), Failure> {
            // Pin before canonicalization: no arbitrary path-following is allowed.
            let directory = pin(path)?;
            let identities = directory.identities().map_err(failure)?;
            let canonical = std::fs::canonicalize(directory.path()).map_err(io_failure)?;
            let canonical_directory = pin(&canonical)?;
            if canonical_directory.identities().map_err(failure)? != identities {
                return Err(Failure::Changed);
            }
            Ok((canonical, Self { identities }))
        }
        fn root(&self, root: &Path) -> Result<Directory, Failure> {
            let directory = pin(root)?;
            let identities = directory.identities().map_err(failure)?;
            if identities != self.identities {
                return Err(Failure::Changed);
            }
            Ok(directory)
        }
        fn descendant(&self, path: &Path) -> Result<Directory, Failure> {
            let directory = pin(path)?;
            windows_policy::ancestry(&self.identities, &directory.identities().map_err(failure)?)?;
            Ok(directory)
        }
        pub(super) fn canonicalize(&self, root: &Path, path: &Path) -> Result<PathBuf, Failure> {
            // A plain relative path is source/root-relative; a Windows prefix or
            // leading separator without an absolute drive is never reinterpreted.
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else if path.has_root()
                || matches!(path.components().next(), Some(Component::Prefix(_)))
            {
                return Err(Failure::InvalidPath);
            } else {
                root.join(path)
            };
            let parts = relative(root, &path)?;
            let mut directories = vec![self.root(root)?];
            for (index, name) in parts.iter().enumerate() {
                if name == ".." {
                    if directories.len() == 1 {
                        return Err(Failure::OutsideRoot);
                    }
                    directories.pop();
                    continue;
                }
                if name == "." {
                    continue;
                }
                let directory = directories.last().unwrap();
                let child = directory.child(name).map_err(failure)?;
                match self.descendant(&child) {
                    Ok(next) => {
                        directories.push(next);
                    }
                    Err(_) if index + 1 == parts.len() => {
                        let file = open_import(directory, name)?;
                        let info = revision(&file)?;
                        windows_policy::validate(&info)?;
                        let final_path = final_path(&file)?;
                        relative(root, &final_path)?;
                        self.verify_directory(directory)?;
                        return Ok(final_path);
                    }
                    Err(error) => return Err(error),
                }
            }
            // Only checked, pinned directories reach this path-based operation.
            let canonical =
                std::fs::canonicalize(directories.last().unwrap().path()).map_err(io_failure)?;
            relative(root, &canonical)?;
            self.verify_directory(directories.last().unwrap())?;
            Ok(canonical)
        }
        fn verify_directory(&self, directory: &Directory) -> Result<(), Failure> {
            let current = self.descendant(directory.path())?;
            if current.identities().map_err(failure)? != directory.identities().map_err(failure)? {
                return Err(Failure::Changed);
            }
            Ok(())
        }
        pub(super) fn read(
            &self,
            root: &Path,
            canonical: &Path,
            max_bytes: usize,
        ) -> Result<String, Failure> {
            let parts = relative(root, canonical)?;
            if parts.is_empty() || parts.iter().any(|name| name == "." || name == "..") {
                return Err(Failure::InvalidPath);
            }
            let _root = self.root(root)?;
            let parent = canonical.parent().ok_or(Failure::InvalidPath)?;
            let directory = self.descendant(parent)?;
            let file = open_import(&directory, parts.last().unwrap())?;
            windows_policy::read(
                &file,
                max_bytes,
                || revision(&file),
                || {
                    self.verify_directory(&directory)?;
                    let current = final_path(&file)?;
                    let actual = relative(root, &current)?;
                    if parts.len() != actual.len()
                        || !parts.iter().zip(actual).all(|(a, b)| same_name(a, &b))
                    {
                        return Err(Failure::Changed);
                    }
                    Ok(())
                },
            )
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub depth: usize,
    pub imports: usize,
    pub bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            depth: 8,
            imports: 128,
            bytes: 256 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    pub source: PathBuf,
    pub import: Import,
    pub result: Result<Imported, Failure>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub path: PathBuf,
    /// Unmodified source; descendants carry their own source coordinates.
    pub text: String,
    pub children: Vec<Expansion>,
}

fn normalize(path: &Path) -> Result<PathBuf, Failure> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !out.pop() {
                    return Err(Failure::OutsideRoot);
                }
            }
            Component::CurDir => (),
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// Lexical dependency identity, also available when a target does not exist yet.
/// Use canonical source context and track the resolved path separately for symlinks.
pub fn reference_path(source: &Path, target: &str) -> Result<PathBuf, Failure> {
    if target.is_empty() || target.chars().any(char::is_control) {
        return Err(Failure::InvalidPath);
    }
    normalize(&source.parent().ok_or(Failure::InvalidPath)?.join(target))
}

/// Resolve a literal source-relative import; absolute paths must remain inside root.
/// Canonical containment is checked before source is read (including symlink escapes).
pub fn resolve(
    store: &impl Source,
    root: &Path,
    source: &Path,
    target: &str,
) -> Result<PathBuf, Failure> {
    if target.is_empty() || target.chars().any(char::is_control) {
        return Err(Failure::InvalidPath);
    }
    let root = store.canonicalize(root)?;
    let source = store.canonicalize(source)?;
    if !root.is_absolute() || !source.starts_with(&root) {
        return Err(Failure::OutsideRoot);
    }
    let candidate = source.parent().ok_or(Failure::InvalidPath)?.join(target);
    if !normalize(&candidate)?.starts_with(&root) {
        return Err(Failure::OutsideRoot);
    }
    let canonical = store.canonicalize(&candidate)?;
    if !canonical.starts_with(&root) {
        return Err(Failure::OutsideRoot);
    }
    Ok(canonical)
}

/// Expand a snapshot without rewriting it. A shared budget bounds breadth and bytes;
/// only the active ancestor stack detects cycles, so repeated sibling imports work.
pub fn expand(
    store: &impl Source,
    root: &Path,
    source: &Path,
    text: &str,
    limits: Limits,
) -> Result<Vec<Expansion>, Failure> {
    let source = store.canonicalize(source)?;
    let canonical_root = store.canonicalize(root)?;
    if !canonical_root.is_absolute() || !source.starts_with(&canonical_root) {
        return Err(Failure::OutsideRoot);
    }
    let mut state = State {
        store,
        root: canonical_root,
        limits,
        remaining: limits.bytes,
        count: 0,
        stack: vec![source.clone()],
    };
    Ok(state.walk(&source, text))
}
struct State<'a, S> {
    store: &'a S,
    root: PathBuf,
    limits: Limits,
    remaining: usize,
    count: usize,
    stack: Vec<PathBuf>,
}
impl<S: Source> State<'_, S> {
    fn walk(&mut self, source: &Path, text: &str) -> Vec<Expansion> {
        // Include at most one terminal Budget diagnostic rather than allocate for
        // every remaining directive in an arbitrarily large source snapshot.
        let mut nodes = Vec::new();
        for import in parse_bounded(
            text,
            self.limits
                .imports
                .saturating_sub(self.count)
                .saturating_add(1),
        ) {
            if self.count >= self.limits.imports {
                nodes.push(Expansion {
                    source: source.into(),
                    import,
                    result: Err(Failure::Budget),
                });
                break;
            }
            self.count += 1;
            let result = self.load(source, &import.target);
            nodes.push(Expansion {
                source: source.into(),
                import,
                result,
            });
        }
        nodes
    }
    fn load(&mut self, source: &Path, target: &str) -> Result<Imported, Failure> {
        if self.stack.len() > self.limits.depth {
            return Err(Failure::Depth);
        }
        let path = resolve(self.store, &self.root, source, target)?;
        if self.stack.contains(&path) {
            return Err(Failure::Cycle);
        }
        let text = self.store.read(&path, self.remaining)?;
        if text.len() > self.remaining {
            return Err(Failure::Budget);
        }
        self.remaining -= text.len();
        self.stack.push(path.clone());
        let children = self.walk(&path, &text);
        self.stack.pop();
        Ok(Imported {
            path,
            text,
            children,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::BTreeMap};
    struct Mock {
        files: BTreeMap<PathBuf, String>,
        reads: RefCell<Vec<PathBuf>>,
    }
    impl Source for Mock {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf, Failure> {
            if path == Path::new("/vault/escape") {
                return Ok("/outside/secret".into());
            }
            normalize(path)
        }
        fn read(&self, path: &Path, max: usize) -> Result<String, Failure> {
            self.reads.borrow_mut().push(path.into());
            let text = self.files.get(path).ok_or(Failure::Missing)?;
            if text.len() > max {
                return Err(Failure::Budget);
            }
            Ok(text.clone())
        }
    }
    #[test]
    fn exact_utf8_crlf_mapping_and_code_exclusion() {
        let text = "é\r\n  @child/日本.md  \r\n```\n@hidden\n```\n    @code\n\t@code\n@last";
        let imports = parse(text);
        assert_eq!(imports.len(), 2);
        assert_eq!(&text[imports[0].target_bytes.clone()], "child/日本.md");
        assert_eq!(&text[imports[0].line.clone()], "  @child/日本.md  ");
        assert_eq!(imports[1].target, "last");
    }
    #[test]
    fn issue2939_double_at_is_not_an_import_but_dot_slash_is() {
        let text = "@@AgentContract.md\n@./@AgentContract.md\n```\n@@fenced.md\n```\n  @@x.md \n";
        let imports = parse(text);
        assert_eq!(
            imports
                .iter()
                .map(|i| i.target.as_str())
                .collect::<Vec<_>>(),
            ["./@AgentContract.md"]
        );
        let ignored = ignored(text);
        assert_eq!(
            ignored
                .iter()
                .map(|i| i.target.as_str())
                .collect::<Vec<_>>(),
            ["@AgentContract.md", "@x.md"]
        );
        assert_eq!(&text[ignored[0].line.clone()], "@@AgentContract.md");
        assert_eq!(&text[ignored[1].target_bytes.clone()], "@x.md");
        assert_eq!(
            ignored_message(&ignored[0].target),
            "Not an import: Claude Code ignores `@@AgentContract.md`. Write `@./@AgentContract.md`."
        );
        // Expansion never reads an ignored target.
        let store = Mock {
            files: [("/vault/@AgentContract.md".into(), "rules".into())].into(),
            reads: RefCell::new(vec![]),
        };
        let nodes = expand(
            &store,
            Path::new("/vault"),
            Path::new("/vault/AGENTS.md"),
            "@@AgentContract.md\n",
            Limits::default(),
        )
        .unwrap();
        assert!(nodes.is_empty());
        assert!(store.reads.borrow().is_empty());
    }
    #[test]
    fn nested_context_cycle_and_escape_never_read_outside() {
        let store = Mock {
            files: [("/vault/sub/a", "@../root\n"), ("/vault/root", "@sub/a\n")]
                .into_iter()
                .map(|(p, t)| (p.into(), t.into()))
                .collect(),
            reads: RefCell::new(vec![]),
        };
        let nodes = expand(
            &store,
            Path::new("/vault"),
            Path::new("/vault/root"),
            "@sub/a\n@escape\n@../secret",
            Limits::default(),
        )
        .unwrap();
        let a = nodes[0].result.as_ref().unwrap();
        assert_eq!(a.path, Path::new("/vault/sub/a"));
        assert_eq!(a.children[0].result, Err(Failure::Cycle));
        assert_eq!(nodes[1].result, Err(Failure::OutsideRoot));
        assert_eq!(nodes[2].result, Err(Failure::OutsideRoot));
        assert_eq!(*store.reads.borrow(), vec![PathBuf::from("/vault/sub/a")]);
    }
    #[test]
    fn depth_and_shared_byte_and_import_budgets() {
        let store = Mock {
            files: [
                ("/vault/a".into(), "@b".into()),
                ("/vault/b".into(), "ok".into()),
            ]
            .into(),
            reads: RefCell::new(vec![]),
        };
        let run = |limits| {
            expand(
                &store,
                Path::new("/vault"),
                Path::new("/vault/root"),
                "@a\n@a\n@a",
                limits,
            )
            .unwrap()
        };
        let nodes = run(Limits {
            depth: 1,
            imports: 10,
            bytes: 4,
        });
        assert_eq!(
            nodes[0].result.as_ref().unwrap().children[0].result,
            Err(Failure::Depth)
        );
        assert_eq!(nodes[2].result, Err(Failure::Budget));
        let nodes = run(Limits {
            depth: 8,
            imports: 1,
            bytes: 100,
        });
        assert_eq!(
            nodes[0].result.as_ref().unwrap().children[0].result,
            Err(Failure::Budget)
        );
        assert_eq!(nodes.len(), 2);
    }
}

#[cfg(all(test, unix))]
mod rooted_tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
    };
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "okilum-imports-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::create_dir(path.join("vault")).unwrap();
            fs::create_dir(path.join("outside")).unwrap();
            Self(path)
        }
        fn root(&self) -> PathBuf {
            self.0.join("vault")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn real_rooted_reads_are_bounded_lossless_and_regular_only() {
        let f = Fixture::new();
        let root = f.root();
        fs::write(root.join("AGENTS.md"), "é\r\n").unwrap();
        fs::write(root.join("invalid"), [0xff]).unwrap();
        let store = RootedSource::new(&root).unwrap();
        let path = store.canonicalize(&root.join("AGENTS.md")).unwrap();
        assert_eq!(store.read(&path, 4).unwrap(), "é\r\n");
        assert_eq!(store.read(&path, 3), Err(Failure::Budget));
        assert_eq!(
            store.read(&root.join("invalid"), 10),
            Err(Failure::Unreadable)
        );
        assert_eq!(store.read(store.root(), 10), Err(Failure::InvalidPath));
        assert_eq!(
            store.canonicalize(&root.join("missing")),
            Err(Failure::Missing)
        );
        assert_eq!(
            store.read(&f.0.join("outside/secret"), 10),
            Err(Failure::OutsideRoot)
        );
    }

    #[test]
    fn symlinks_resolve_inside_root_but_cannot_redirect_a_resolved_read() {
        let f = Fixture::new();
        let root = f.root();
        fs::create_dir_all(root.join("real/deep")).unwrap();
        fs::write(root.join("AGENTS.md"), "@alias/../target").unwrap();
        fs::write(root.join("target"), "wrong lexical target").unwrap();
        fs::write(root.join("real/target"), "correct target").unwrap();
        fs::write(f.0.join("outside/secret"), "must never be imported").unwrap();
        symlink("real/deep", root.join("alias")).unwrap();
        symlink(f.0.join("outside"), root.join("escape")).unwrap();
        let store = RootedSource::new(&root).unwrap();
        let target = resolve(
            &store,
            store.root(),
            &root.join("AGENTS.md"),
            "alias/../target",
        )
        .unwrap();
        assert_eq!(store.read(&target, 100).unwrap(), "correct target");
        assert_eq!(
            resolve(
                &store,
                store.root(),
                &root.join("AGENTS.md"),
                "escape/secret"
            ),
            Err(Failure::OutsideRoot)
        );
        fs::remove_file(&target).unwrap();
        symlink(f.0.join("outside/secret"), &target).unwrap();
        assert_eq!(store.read(&target, 100), Err(Failure::Unreadable));
        // Replacing an ancestor after resolution is rejected as well.
        fs::rename(root.join("real"), root.join("old-real")).unwrap();
        symlink(f.0.join("outside"), root.join("real")).unwrap();
        assert!(store.read(&target, 100).is_err());
    }

    #[test]
    fn replaced_root_requires_a_new_reader() {
        let f = Fixture::new();
        let root = f.root();
        fs::write(root.join("AGENTS.md"), "old").unwrap();
        let store = RootedSource::new(&root).unwrap();
        fs::rename(&root, f.0.join("old-vault")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("AGENTS.md"), "replacement").unwrap();
        assert_eq!(
            store.canonicalize(&root.join("AGENTS.md")),
            Err(Failure::Changed)
        );
        assert_eq!(
            store.read(&root.join("AGENTS.md"), 100),
            Err(Failure::Changed)
        );
    }
}

#[cfg(test)]
mod windows_policy_tests {
    use super::{
        windows_policy::{self, Revision},
        Failure,
    };
    use std::{
        cell::Cell,
        io::{self, Read},
    };
    fn id(volume: u32, high: u32, low: u32) -> windows_policy::Identity {
        #[cfg(windows)]
        {
            let mut file = [0; 16];
            file[..4].copy_from_slice(&low.to_le_bytes());
            file[8..12].copy_from_slice(&high.to_le_bytes());
            crate::windows_files::Identity::Full {
                volume: volume.into(),
                file,
            }
        }
        #[cfg(not(windows))]
        {
            (volume, high, low)
        }
    }
    fn stable() -> Revision {
        Revision {
            identity: id(1, 2, 3),
            length: 4,
            write_time: 10,
            change_time: 20,
            creation_time: 1,
            attributes: 0x20,
            links: 1,
            regular: true,
        }
    }
    struct Counted<'a> {
        input: &'a [u8],
        read: &'a Cell<usize>,
    }
    impl Read for Counted<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let count = self.input.read(out)?;
            self.read.set(self.read.get() + count);
            Ok(count)
        }
    }
    #[test]
    fn real_read_policy_preserves_utf8_crlf_and_caps_growth_at_budget_plus_one() {
        let bytes = Cell::new(0);
        let checks = Cell::new(0);
        let input = Counted {
            input: "é\r\n".as_bytes(),
            read: &bytes,
        };
        let text = windows_policy::read(
            input,
            4,
            || Ok(stable()),
            || {
                checks.set(checks.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(text, "é\r\n");
        assert_eq!(bytes.get(), 4);
        assert_eq!(checks.get(), 2);
        bytes.set(0);
        let input = Counted {
            input: b"1234567890",
            read: &bytes,
        };
        assert_eq!(
            windows_policy::read(input, 4, || Ok(stable()), || Ok(())),
            Err(Failure::Budget)
        );
        assert_eq!(bytes.get(), 5, "growth cannot become an unbounded read");
        bytes.set(0);
        let input = Counted {
            input: b"12345",
            read: &bytes,
        };
        assert_eq!(
            windows_policy::read(input, 3, || Ok(stable()), || Ok(())),
            Err(Failure::Budget)
        );
        assert_eq!(
            bytes.get(),
            0,
            "oversized metadata stops before source bytes"
        );
    }
    #[test]
    fn unsupported_attributes_types_links_and_outside_locations_never_read_bytes() {
        for bit in [0x400, 0x1000, 0x40000, 0x400000] {
            let bytes = Cell::new(0);
            let mut state = stable();
            state.attributes |= bit;
            assert_eq!(
                windows_policy::read(
                    Counted {
                        input: b"data",
                        read: &bytes
                    },
                    4,
                    || Ok(state.clone()),
                    || Ok(())
                ),
                Err(Failure::Unreadable)
            );
            assert_eq!(bytes.get(), 0);
        }
        for (regular, links) in [(false, 1), (true, 2), (true, 0)] {
            let mut state = stable();
            state.regular = regular;
            state.links = links;
            let bytes = Cell::new(0);
            assert_eq!(
                windows_policy::read(
                    Counted {
                        input: b"data",
                        read: &bytes
                    },
                    4,
                    || Ok(state.clone()),
                    || Ok(())
                ),
                Err(Failure::Unreadable)
            );
            assert_eq!(bytes.get(), 0);
        }
        let bytes = Cell::new(0);
        assert_eq!(
            windows_policy::read(
                Counted {
                    input: b"data",
                    read: &bytes
                },
                4,
                || Ok(stable()),
                || Err(Failure::OutsideRoot)
            ),
            Err(Failure::OutsideRoot)
        );
        assert_eq!(bytes.get(), 0);
    }
    #[test]
    fn identity_size_and_times_are_revision_bound_and_ancestry_cannot_change() {
        let before = stable();
        let mut variants = Vec::new();
        let mut after = before.clone();
        after.identity = id(1, 2, 4);
        variants.push(after);
        let mut after = before.clone();
        after.length += 1;
        variants.push(after);
        let mut after = before.clone();
        after.write_time += 1;
        variants.push(after);
        let mut after = before.clone();
        after.change_time += 1;
        variants.push(after);
        let mut after = before.clone();
        after.creation_time += 1;
        variants.push(after);
        for after in variants {
            let mut calls = 0;
            assert_eq!(
                windows_policy::read(
                    &b"data"[..],
                    4,
                    || {
                        calls += 1;
                        Ok(if calls == 1 {
                            before.clone()
                        } else {
                            after.clone()
                        })
                    },
                    || Ok(())
                ),
                Err(Failure::Changed)
            );
        }
        let mut location_checks = 0;
        assert_eq!(
            windows_policy::read(
                &b"data"[..],
                4,
                || Ok(stable()),
                || {
                    location_checks += 1;
                    if location_checks == 1 {
                        Ok(())
                    } else {
                        Err(Failure::Changed)
                    }
                }
            ),
            Err(Failure::Changed)
        );
        assert_eq!(
            windows_policy::read(&[0xff; 4][..], 4, || Ok(stable()), || Ok(())),
            Err(Failure::Unreadable)
        );
        let root = [id(1, 2, 3), id(1, 2, 4)];
        assert_eq!(
            windows_policy::ancestry(&root, &[id(1, 2, 3), id(1, 2, 4), id(1, 2, 5)]),
            Ok(())
        );
        assert_eq!(
            windows_policy::ancestry(&root, &[id(1, 2, 3), id(1, 9, 9)]),
            Err(Failure::Changed)
        );
        assert_eq!(
            windows_policy::ancestry(&root, &[id(1, 2, 3)]),
            Err(Failure::Changed)
        );
    }
}

#[cfg(test)]
mod document_classification_tests {
    #[test]
    fn root_contract_is_an_agent_document_without_changing_browser_visibility() {
        assert!(super::is_agent_document("@AgentContract.md"));
        assert!(!super::is_agent_document("_AgentContract.md"));
        assert!(!super::is_agent_document("project/_AgentContract.md"));
        assert!(super::is_agent_document("project/AGENTS.md"));
        assert!(!super::is_agent_document("project/not-AGENTS.md"));
        assert!(crate::quick_open::FileFilter::Default.admits("_AgentContract.md"));
        assert!(!crate::quick_open::FileFilter::AgentFiles.admits("_AgentContract.md"));
    }
}
