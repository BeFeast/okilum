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
    parse_up_to(source, usize::MAX)
}

fn parse_up_to(source: &str, max_imports: usize) -> Vec<Import> {
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
}

/// Implementations must resolve symlinks before returning a canonical absolute path.
/// `read` must enforce `max_bytes` before allocating an unbounded source.
pub trait Source {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, Failure>;
    fn read(&self, canonical: &Path, max_bytes: usize) -> Result<String, Failure>;
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
    let candidate = normalize(&source.parent().ok_or(Failure::InvalidPath)?.join(target))?;
    if !candidate.starts_with(&root) {
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
        for import in parse_up_to(
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
            Ok(path.into())
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
