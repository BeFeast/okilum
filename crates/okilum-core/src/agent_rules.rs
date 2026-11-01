//! Local instruction evidence for #1590. Discovery order is not evidence that a
//! remote executor loaded these files. No writes or provider calls occur here.
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_LAYERS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance {
    Ancestor,
    Import {
        from: PathBuf,
        line: usize,
    },
    /// Provider-specific files are visible without inventing their precedence.
    Alternative,
}

#[derive(Clone, Debug)]
pub struct Layer {
    /// Vault-relative source identity, including when reading failed.
    pub path: PathBuf,
    pub provenance: Provenance,
    pub source: Option<String>,
    pub error: Option<String>,
    pub duplicate_lines: Vec<usize>,
}

impl Layer {
    pub fn bytes(&self) -> usize {
        self.source.as_ref().map_or(0, String::len)
    }
    /// A character-based estimate, never a provider tokenizer measurement.
    pub fn approximate_tokens(&self) -> usize {
        self.source
            .as_ref()
            .map_or(0, |s| s.chars().count().div_ceil(4))
    }
}

#[derive(Clone, Debug, Default)]
pub struct RuleStack {
    pub layers: Vec<Layer>,
    pub diagnostics: Vec<String>,
}

impl RuleStack {
    pub fn total_bytes(&self) -> usize {
        self.layers.iter().map(Layer::bytes).sum()
    }
    pub fn approximate_tokens(&self) -> usize {
        self.layers.iter().map(Layer::approximate_tokens).sum()
    }
}

/// Pure single-flight refresh coordinator. Events invalidate a running read;
/// they never publish its obsolete snapshot or start parallel discovery jobs.
#[derive(Debug, Default)]
pub struct Refresh {
    target: Option<(PathBuf, PathBuf)>,
    generation: u64,
    running: Option<DiscoveryTicket>,
    dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryTicket {
    generation: u64,
    pub vault: PathBuf,
    pub project: PathBuf,
}

impl Refresh {
    pub fn bind(&mut self, vault: PathBuf, project: PathBuf) {
        self.target = Some((vault, project));
        self.request();
    }

    /// A validated/coalesced filesystem write batch, explicit refresh, or rescan.
    /// No disk access is needed to invalidate a snapshot on the UI thread.
    pub fn request(&mut self) {
        if self.target.is_some() {
            self.advance();
            self.dirty = true;
        }
    }

    pub fn deactivate(&mut self) {
        self.target = None;
        self.supersede();
    }

    /// Installing an owner-provided snapshot also invalidates older worker reads.
    pub fn supersede(&mut self) {
        self.advance();
        self.dirty = false;
    }

    fn advance(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("Rules generation exhausted");
    }

    pub fn is_bound(&self) -> bool {
        self.target.is_some()
    }
    pub fn is_pending(&self) -> bool {
        self.is_bound() && (self.dirty || self.running.is_some())
    }

    pub fn begin(&mut self) -> Option<DiscoveryTicket> {
        if self.running.is_some() || !self.dirty {
            return None;
        }
        let (vault, project) = self.target.as_ref()?;
        let ticket = DiscoveryTicket {
            generation: self.generation,
            vault: vault.clone(),
            project: project.clone(),
        };
        self.running = Some(ticket.clone());
        self.dirty = false;
        Some(ticket)
    }

    /// Always drain the matching job, even when invalidated. The caller then
    /// invokes begin() once to launch any coalesced follow-up.
    pub fn finish(&mut self, ticket: &DiscoveryTicket) -> bool {
        if self.running.as_ref() != Some(ticket) {
            return false;
        }
        self.running = None;
        self.generation == ticket.generation
            && self.target.as_ref().is_some_and(|(vault, project)| {
                vault == &ticket.vault && project == &ticket.project
            })
    }
}

/// Rebuild on a background executor. Only ancestors inside this explicit vault
/// are inspected. Existing exact-name discovery is shared with search/Tree.
pub fn discover(vault: &Path, project: &Path) -> RuleStack {
    let mut stack = RuleStack::default();
    let root = match vault.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            stack
                .diagnostics
                .push(format!("Cannot read vault: {error}"));
            return stack;
        }
    };
    let project = match project.canonicalize() {
        Ok(project) if project.starts_with(&root) && project.is_dir() => project,
        _ => {
            stack
                .diagnostics
                .push("Project is unavailable or outside this vault".into());
            return stack;
        }
    };
    let mut directories: Vec<_> = project
        .ancestors()
        .take_while(|p| p.starts_with(&root))
        .collect();
    directories.reverse();
    let mut active = Vec::new();
    let mut seen = BTreeMap::new();
    for directory in directories {
        let relative = directory.strip_prefix(&root).expect("bounded ancestor");
        let mut names = vec!["AGENTS.md"];
        if relative.as_os_str().is_empty() {
            names.insert(0, "_AgentContract.md");
        }
        for name in names {
            let path = relative.join(name);
            // Absence is normal only for optional ancestor candidates. Imports
            // are explicit references and always retain missing-file errors.
            match fs::symlink_metadata(root.join(&path)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => expand(
                    &root,
                    path,
                    Provenance::Ancestor,
                    &mut stack,
                    &mut active,
                    &mut seen,
                ),
            }
        }
        for name in ["CLAUDE.md", "GEMINI.md", "CONTEXT.md", "ANTIGRAVITY.md"] {
            if !crate::quick_open::is_agent_file(name) {
                continue;
            }
            let path = relative.join(name);
            if stack.layers.len() >= MAX_LAYERS {
                stack
                    .diagnostics
                    .push("Provider candidate discovery stopped at the layer limit".into());
                break;
            }
            match fs::symlink_metadata(root.join(&path)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                _ => {}
            }
            stack.diagnostics.push(format!(
                "{}: provider-specific candidate; precedence and loading are unknown",
                path.display()
            ));
            // Show alternatives without merging their imports into the ordered
            // AGENTS stack or silently selecting a provider.
            let (source, error) = read_source(&root, &path);
            stack.layers.push(Layer {
                path,
                provenance: Provenance::Alternative,
                source,
                error,
                duplicate_lines: vec![],
            });
        }
    }
    mark_duplicates(&mut stack);
    stack
}

fn read_source(root: &Path, relative: &Path) -> (Option<String>, Option<String>) {
    let result = (|| -> Result<String, String> {
        let path = root
            .join(relative)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !path.starts_with(root) {
            return Err("Source resolves outside this vault".into());
        }
        let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("Source is not a regular file".into());
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err("Source exceeds the 1 MiB limit".into());
        }
        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("Source exceeds the 1 MiB limit".into());
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    })();
    match result {
        Ok(source) => (Some(source), None),
        Err(error) => (None, Some(error)),
    }
}

fn import_path(from: &Path, reference: &str) -> Result<PathBuf, String> {
    if reference.is_empty() || Path::new(reference).is_absolute() {
        return Err("Import must name a file relative to its source".into());
    }
    let mut path = from.parent().unwrap_or(Path::new("")).to_path_buf();
    for component in Path::new(reference).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir if path.pop() => {}
            _ => return Err("Import leaves this vault".into()),
        }
    }
    Ok(path)
}

fn expand(
    root: &Path,
    path: PathBuf,
    provenance: Provenance,
    stack: &mut RuleStack,
    active: &mut Vec<PathBuf>,
    seen: &mut BTreeMap<PathBuf, PathBuf>,
) {
    if stack.layers.len() >= MAX_LAYERS {
        stack.diagnostics.push(format!(
            "{}: instruction expansion stopped at {MAX_LAYERS} layers",
            path.display()
        ));
        return;
    }
    // Canonical identities catch symlink cycles and duplicate aliases too.
    let identity = root
        .join(&path)
        .canonicalize()
        .unwrap_or_else(|_| root.join(&path));
    let repeated = seen.get(&identity).cloned();
    let (source, mut error) = read_source(root, &path);
    if active.contains(&identity) {
        error = Some("Import cycle; expansion stopped".into());
    } else if let Some(first) = repeated {
        error = Some(format!(
            "Repeated source, first seen at {}; expansion stopped",
            first.display()
        ));
    }
    let expand_imports = error.is_none();
    let imports: Vec<_> = if expand_imports {
        source
            .as_deref()
            .unwrap_or_default()
            .lines()
            .enumerate()
            .filter_map(|(i, line)| line.strip_prefix('@').map(|s| (i + 1, s.trim().to_owned())))
            .collect()
    } else {
        vec![]
    };
    stack.layers.push(Layer {
        path: path.clone(),
        provenance,
        source,
        error,
        duplicate_lines: vec![],
    });
    if !expand_imports {
        return;
    }
    seen.insert(identity.clone(), path.clone());
    active.push(identity);
    for (line, reference) in imports {
        match import_path(&path, &reference) {
            Ok(target) => expand(
                root,
                target,
                Provenance::Import {
                    from: path.clone(),
                    line,
                },
                stack,
                active,
                seen,
            ),
            Err(error) => stack
                .diagnostics
                .push(format!("{}:{line}: {error}: {reference}", path.display())),
        }
    }
    active.pop();
}

fn mark_duplicates(stack: &mut RuleStack) {
    let mut lines: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for (index, layer) in stack.layers.iter().enumerate() {
        for (line, text) in layer
            .source
            .as_deref()
            .unwrap_or_default()
            .lines()
            .enumerate()
        {
            let normalized = text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            // Ignore punctuation and tiny fragments; flag repeated headings or
            // substantive rules. This is an overlap hint, not contradiction AI.
            if normalized.chars().filter(|c| c.is_alphanumeric()).count() >= 12
                && !normalized.starts_with('@')
            {
                lines.entry(normalized).or_default().push((index, line + 1));
            }
        }
    }
    for occurrences in lines.values() {
        if occurrences
            .iter()
            .any(|(index, _)| *index != occurrences[0].0)
        {
            for &(index, line) in occurrences {
                stack.layers[index].duplicate_lines.push(line);
            }
        }
    }
    for layer in &mut stack.layers {
        layer.duplicate_lines.sort_unstable();
        layer.duplicate_lines.dedup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_coalesces_writes_and_rejects_old_project_and_disabled_results() {
        let mut refresh = Refresh::default();
        assert!(refresh.begin().is_none(), "unbound views do no work");
        refresh.bind("vault".into(), "vault/A".into());
        let initial = refresh.begin().unwrap();
        assert!(refresh.begin().is_none());
        for _ in 0..20 {
            refresh.request();
        }
        assert!(refresh.begin().is_none(), "at most one worker");
        assert!(
            !refresh.finish(&initial),
            "external writes supersede old read"
        );
        let latest = refresh.begin().unwrap();
        assert!(
            refresh.finish(&latest),
            "positive current-generation control"
        );
        assert!(!refresh.is_pending());
        assert!(refresh.begin().is_none(), "no polling or idle reads");
        refresh.request();
        let old_project = refresh.begin().unwrap();
        refresh.bind("vault".into(), "vault/B".into());
        assert!(!refresh.finish(&old_project));
        let new_project = refresh.begin().unwrap();
        assert_eq!(new_project.project, Path::new("vault/B"));
        assert!(
            !refresh.finish(&old_project),
            "duplicate completion cannot drain a newer job"
        );
        refresh.deactivate();
        assert!(!refresh.finish(&new_project));
        assert!(!refresh.is_pending());
        assert!(refresh.begin().is_none());
        refresh.bind("vault".into(), "vault/B".into());
        let resumed = refresh.begin().unwrap();
        refresh.supersede();
        assert!(!refresh.finish(&resumed), "explicit replacement wins");
        assert!(refresh.begin().is_none());
    }

    #[test]
    fn refresh_discovers_external_import_edits_creates_and_deletes() {
        let root =
            std::env::temp_dir().join(format!("okilum-rules-refresh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Project")).unwrap();
        fs::write(root.join("AGENTS.md"), "@import.md\n").unwrap();
        let mut refresh = Refresh::default();
        refresh.bind(root.clone(), root.join("Project"));
        let first = refresh.begin().unwrap();
        let before = discover(&first.vault, &first.project);
        assert!(refresh.finish(&first));
        assert!(
            before.layers[1].error.is_some(),
            "missing import is visible"
        );
        fs::write(root.join("import.md"), "First external rule\n").unwrap();
        refresh.request();
        let pending = refresh.begin().unwrap();
        let stale = discover(&pending.vault, &pending.project);
        fs::write(root.join("import.md"), "Second external rule\n").unwrap();
        fs::write(root.join("Project/AGENTS.md"), "New project layer\n").unwrap();
        refresh.request();
        assert!(!refresh.finish(&pending));
        assert_eq!(
            stale.layers[1].source.as_deref(),
            Some("First external rule\n")
        );
        let latest = refresh.begin().unwrap();
        let updated = discover(&latest.vault, &latest.project);
        assert!(refresh.finish(&latest));
        assert_eq!(
            updated.layers[1].source.as_deref(),
            Some("Second external rule\n")
        );
        assert_eq!(updated.layers[2].path, Path::new("Project/AGENTS.md"));
        fs::remove_file(root.join("import.md")).unwrap();
        fs::remove_file(root.join("Project/AGENTS.md")).unwrap();
        refresh.request();
        let deleted = refresh.begin().unwrap();
        let after = discover(&deleted.vault, &deleted.project);
        assert!(refresh.finish(&deleted));
        assert_eq!(after.layers.len(), 2);
        assert!(after.layers[1].source.is_none() && after.layers[1].error.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_visible_and_never_read() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("okilum-rules-boundary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("vault")).unwrap();
        fs::write(root.join("secret.md"), "outside canary").unwrap();
        fs::write(root.join("vault/AGENTS.md"), "@linked.md\n@AGENTS.md\n").unwrap();
        symlink(root.join("secret.md"), root.join("vault/linked.md")).unwrap();
        let stack = discover(&root.join("vault"), &root.join("vault"));
        assert_eq!(stack.layers.len(), 3);
        assert!(stack.layers[0].source.is_some(), "positive read control");
        assert!(stack.layers[1].source.is_none());
        assert!(stack.layers[1]
            .error
            .as_deref()
            .unwrap()
            .contains("outside"));
        assert!(stack.layers[2].error.as_deref().unwrap().contains("cycle"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordered_imports_errors_and_duplicates_are_visible_without_writes() {
        let root = std::env::temp_dir().join(format!("okilum-rules-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Dev/App")).unwrap();
        for (name, source) in [
            (
                "_AgentContract.md",
                "# Shared instructions\nKeep canonical source intact\n",
            ),
            ("AGENTS.md", "@shared.md\n@missing.md\n@../outside.md\n"),
            ("shared.md", "@AGENTS.md\nKeep canonical source intact\n"),
            ("Dev/AGENTS.md", "Domain instructions\n"),
            ("Dev/App/AGENTS.md", "Project instructions\n"),
            ("Dev/App/CLAUDE.md", "@AGENTS.md\n"),
        ] {
            fs::write(root.join(name), source).unwrap();
        }
        let stack = discover(&root, &root.join("Dev/App"));
        assert_eq!(
            stack
                .layers
                .iter()
                .map(|l| l.path.to_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "_AgentContract.md",
                "AGENTS.md",
                "shared.md",
                "AGENTS.md",
                "missing.md",
                "Dev/AGENTS.md",
                "Dev/App/AGENTS.md",
                "Dev/App/CLAUDE.md"
            ]
        );
        assert!(stack.layers[3].error.as_deref().unwrap().contains("cycle"));
        assert!(stack.layers[4].error.is_some());
        assert_eq!(
            stack.layers[2].provenance,
            Provenance::Import {
                from: "AGENTS.md".into(),
                line: 1
            }
        );
        assert_eq!(stack.layers[0].duplicate_lines, [2]);
        assert!(stack
            .diagnostics
            .iter()
            .any(|s| s.contains("leaves this vault")));
        assert!(stack
            .diagnostics
            .iter()
            .any(|s| s.contains("loading are unknown")));
        assert!(stack.total_bytes() > 0 && stack.approximate_tokens() > 0);
        assert_eq!(
            fs::read_to_string(root.join("shared.md")).unwrap(),
            "@AGENTS.md\nKeep canonical source intact\n"
        );
        assert!(discover(&root, &root.join("absent")).layers.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
