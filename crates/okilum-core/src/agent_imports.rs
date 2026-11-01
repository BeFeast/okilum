//! Read-only agent-file imports. All offsets refer to original UTF-8 source bytes.
use std::{
    ops::Range,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub line: Range<usize>,
    pub target_bytes: Range<usize>,
    pub target: String,
}

/// Recognize standalone @path lines, excluding fenced and indented code.
/// Whitespace in a path is literal; shell quoting and interpolation are never evaluated.
pub fn parse(source: &str) -> Vec<Import> {
    parse_bounded(source, usize::MAX)
}

/// Parse at most `max_imports` directives without allocating for the remainder.
pub fn parse_bounded(source: &str, max_imports: usize) -> Vec<Import> {
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
            if !target.is_empty() && !target.chars().any(char::is_control) {
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
/// Other platforms fail closed until an equivalent rooted handle reader exists.
pub struct RootedSource {
    root: PathBuf,
    #[cfg(unix)]
    directory: std::fs::File,
}

fn io_failure(error: std::io::Error) -> Failure {
    match error.kind() {
        std::io::ErrorKind::NotFound => Failure::Missing,
        _ => Failure::Unreadable,
    }
}

impl RootedSource {
    pub fn new(root: &Path) -> Result<Self, Failure> {
        let root = std::fs::canonicalize(root).map_err(io_failure)?;
        #[cfg(unix)]
        {
            let directory = open_directory(&root)?;
            Ok(Self { root, directory })
        }
        #[cfg(not(unix))]
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
        #[cfg(not(unix))]
        {
            let _ = (canonical, max_bytes);
            Err(Failure::Unreadable)
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
