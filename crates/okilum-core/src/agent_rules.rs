//! Local instruction evidence for #1590. Discovery order is not evidence that a
//! remote executor loaded these files. No writes or provider calls occur here.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
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
    /// Wording overlap only, not semantic equivalence or a contradiction verdict.
    pub similar_lines: Vec<SimilarLine>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SimilarLine {
    pub line: usize,
    pub other_layer: usize,
    pub other_line: usize,
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
    /// Vault-relative `CLAUDE.md` files in the vault-to-project folder chain,
    /// outermost first (#2970). Claude Code reads `AGENTS.md` only when no
    /// `CLAUDE.md` exists in the working directory or any ancestor, so each
    /// entry switches that fallback off for Claude Code.
    pub claude_md: Vec<PathBuf>,
}

impl RuleStack {
    /// One warning per `CLAUDE.md` in the folder chain, paired with the file
    /// it names so the Rules tab can open it (#2970).
    pub fn agents_fallback_warnings(&self) -> Vec<(PathBuf, String)> {
        self.claude_md
            .iter()
            .map(|path| {
                let folder = path
                    .parent()
                    .filter(|folder| !folder.as_os_str().is_empty())
                    .map_or_else(
                        || "the vault root".to_owned(),
                        |folder| folder.display().to_string(),
                    );
                (
                    path.clone(),
                    format!("CLAUDE.md in {folder} switches off AGENTS.md for Claude Code"),
                )
            })
            .collect()
    }
}

impl RuleStack {
    pub fn total_bytes(&self) -> usize {
        self.layers
            .iter()
            .filter(|l| l.provenance != Provenance::Alternative)
            .map(Layer::bytes)
            .sum()
    }
    pub fn approximate_tokens(&self) -> usize {
        self.layers
            .iter()
            .filter(|l| l.provenance != Provenance::Alternative)
            .map(Layer::approximate_tokens)
            .sum()
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
            names.insert(0, "@AgentContract.md");
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
            // Recorded before the layer limit: the fallback warning (#2970)
            // depends only on the file existing, not on room for its layer.
            if name == "CLAUDE.md" && fs::symlink_metadata(root.join(&path)).is_ok() {
                stack.claude_md.push(path.clone());
            }
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
                similar_lines: vec![],
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

fn import_path(root: &Path, from: &Path, reference: &str) -> Result<PathBuf, String> {
    let absolute = crate::agent_imports::reference_path(&root.join(from), reference)
        .map_err(|error| format!("Invalid import: {error:?}"))?;
    absolute
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| "Import leaves this vault".into())
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
    let (source, error) = if active.contains(&identity) {
        (None, Some("Import cycle; expansion stopped".into()))
    } else if let Some(first) = seen.get(&identity) {
        (
            None,
            Some(format!(
                "Repeated source, first seen at {}; expansion stopped",
                first.display()
            )),
        )
    } else {
        read_source(root, &path)
    };
    let expand_imports = error.is_none();
    let text = source.as_deref().unwrap_or_default();
    let numbered = |imports: Vec<crate::agent_imports::Import>| -> Vec<(usize, String)> {
        imports
            .into_iter()
            .map(|import| {
                (
                    text[..import.line.start]
                        .bytes()
                        .filter(|b| *b == b'\n')
                        .count()
                        + 1,
                    import.target,
                )
            })
            .collect()
    };
    let imports = if expand_imports {
        numbered(crate::agent_imports::parse_bounded(text, MAX_LAYERS + 1))
    } else {
        vec![]
    };
    // An agent never loads `@@name` (#2939): no layer, bytes or tokens, but
    // the file that contains it says how to write a real import.
    if expand_imports {
        for (line, target) in numbered(crate::agent_imports::ignored_bounded(text, MAX_LAYERS)) {
            stack.diagnostics.push(format!(
                "{}:{line}: {}",
                path.display(),
                crate::agent_imports::ignored_message(&target)
            ));
        }
    }
    stack.layers.push(Layer {
        path: path.clone(),
        provenance,
        source,
        error,
        duplicate_lines: vec![],
        similar_lines: vec![],
    });
    if !expand_imports {
        return;
    }
    seen.insert(identity.clone(), path.clone());
    active.push(identity.clone());
    if imports.len() > MAX_LAYERS {
        stack.diagnostics.push(format!(
            "{}: import count exceeds the {MAX_LAYERS} reference limit",
            path.display()
        ));
    }
    for (line, reference) in imports.into_iter().take(MAX_LAYERS) {
        match import_path(root, &identity, &reference) {
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
        if layer.provenance == Provenance::Alternative {
            continue;
        }
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
    mark_similar(stack);
}

/// Bound both storage and work independently of file size. This is a local
/// wording hint; unknown/omitted matches remain explicit instead of implying
/// that all rules have been checked for agreement.
fn mark_similar(stack: &mut RuleStack) {
    const MAX_LINES: usize = 4096;
    const MAX_COMPARISONS: usize = 100_000;
    const MAX_EDIT_CELLS: usize = 2_000_000;
    const MAX_CHARS: usize = 512;
    let mut candidates: Vec<(usize, usize, String, BTreeSet<String>)> = Vec::new();
    let mut words: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut matches = Vec::new();
    let mut comparisons = 0;
    let mut edit_cells = 0;
    let mut limited = false;
    'layers: for (layer, entry) in stack.layers.iter().enumerate() {
        if entry.provenance == Provenance::Alternative {
            continue;
        }
        for (line, text) in entry
            .source
            .as_deref()
            .unwrap_or_default()
            .lines()
            .enumerate()
        {
            let heading = text.trim_start().starts_with('#');
            let count = text.chars().take(MAX_CHARS + 1).count();
            if count > MAX_CHARS {
                limited = true;
                continue;
            }
            let tokens: BTreeSet<String> = text
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect();
            let letters: usize = tokens.iter().map(|word| word.chars().count()).sum();
            if letters < if heading { 6 } else { 12 } || text.trim_start().starts_with('@') {
                continue;
            }
            if candidates.len() == MAX_LINES {
                limited = true;
                break 'layers;
            }
            let normalized = text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            let previous: BTreeSet<usize> = tokens
                .iter()
                .filter_map(|word| words.get(word))
                .flatten()
                .copied()
                .collect();
            for prior in previous {
                let (other_layer, other_line, other_text, other_tokens) = &candidates[prior];
                if *other_layer == layer || *other_text == normalized {
                    continue;
                }
                if comparisons == MAX_COMPARISONS {
                    limited = true;
                    break 'layers;
                }
                comparisons += 1;
                let common = tokens.intersection(other_tokens).count();
                let union = tokens.len() + other_tokens.len() - common;
                // At least three shared words for prose; short headings use
                // a bounded edit distance below after sharing an anchor word.
                let overlap = common >= 3 && common * 4 >= union * 3;
                let near_heading =
                    !overlap && heading && other_text.trim_start().starts_with('#') && {
                        let cost = count * other_text.chars().count();
                        if edit_cells + cost > MAX_EDIT_CELLS {
                            limited = true;
                            false
                        } else {
                            edit_cells += cost;
                            near_heading(text, other_text)
                        }
                    };
                if overlap || near_heading {
                    matches.push((layer, line + 1, *other_layer, *other_line));
                }
            }
            let index = candidates.len();
            for word in &tokens {
                words.entry(word.clone()).or_default().push(index);
            }
            candidates.push((layer, line + 1, normalized, tokens));
        }
    }
    // A large repeated corpus must not allocate quadratic peer annotations.
    for (left, line, right, other_line) in matches {
        for (owner, hint) in [
            (
                left,
                SimilarLine {
                    line,
                    other_layer: right,
                    other_line,
                },
            ),
            (
                right,
                SimilarLine {
                    line: other_line,
                    other_layer: left,
                    other_line: line,
                },
            ),
        ] {
            if stack.layers[owner].similar_lines.len() < 256 {
                stack.layers[owner].similar_lines.push(hint);
            } else {
                limited = true;
            }
        }
    }
    for layer in &mut stack.layers {
        layer.similar_lines.sort();
        layer.similar_lines.dedup();
    }
    if limited {
        stack.diagnostics.push("Similar-wording analysis is partial (line, length or comparison limit); source files and totals remain complete.".into());
    }
}

fn near_heading(left: &str, right: &str) -> bool {
    let normalize = |text: &str| {
        text.trim_start()
            .trim_start_matches('#')
            .trim()
            .to_lowercase()
            .chars()
            .collect::<Vec<_>>()
    };
    let left = normalize(left);
    let right = normalize(right);
    let limit = left.len().max(right.len()) / 5;
    if left.len().abs_diff(right.len()) > limit {
        return false;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut row = vec![i + 1; right.len() + 1];
        for (j, b) in right.iter().enumerate() {
            row[j + 1] = (previous[j] + usize::from(a != b))
                .min(row[j] + 1)
                .min(previous[j + 1] + 1);
        }
        if *row.iter().min().unwrap() > limit {
            return false;
        }
        previous = row;
    }
    previous[right.len()] <= limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_1590_repeated_sources_are_references_without_text_totals_or_false_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("Project")).unwrap();
        let contract = "# Contract\nKeep canonical files unchanged.\n";
        // A file whose name starts with `@` is imported as `@./@name`; a bare
        // `@@name` line is not an import (#2939) and would add no layer.
        let agents = "@./@AgentContract.md\n";
        let project = "@../@AgentContract.md\nProject-specific guidance only.\n";
        fs::write(root.join("@AgentContract.md"), contract).unwrap();
        fs::write(root.join("AGENTS.md"), agents).unwrap();
        fs::write(root.join("Project/AGENTS.md"), project).unwrap();
        fs::write(root.join("Project/CLAUDE.md"), contract).unwrap();
        let stack = discover(root, &root.join("Project"));
        assert_eq!(stack.layers.len(), 6);
        assert!(
            stack
                .diagnostics
                .iter()
                .all(|d| !d.contains("Not an import")),
            "both references must be real imports: {:?}",
            stack.diagnostics
        );
        for (index, from) in [(2, "AGENTS.md"), (4, "Project/AGENTS.md")] {
            assert_eq!(stack.layers[index].path, Path::new("@AgentContract.md"));
            assert_eq!(
                stack.layers[index].provenance,
                Provenance::Import {
                    from: PathBuf::from(from),
                    line: 1
                }
            );
        }
        for index in [2, 4] {
            let repeated = &stack.layers[index];
            assert!(repeated.source.is_none());
            assert!(repeated
                .error
                .as_deref()
                .unwrap()
                .contains("Repeated source"));
            assert_eq!(repeated.bytes(), 0);
            assert_eq!(repeated.approximate_tokens(), 0);
            assert!(repeated.duplicate_lines.is_empty() && repeated.similar_lines.is_empty());
        }
        assert_eq!(stack.layers[0].source.as_deref(), Some(contract));
        assert!(
            stack.layers[0].duplicate_lines.is_empty(),
            "references and provider candidates cannot make every contract line a duplicate"
        );
        assert_eq!(
            stack.total_bytes(),
            contract.len() + agents.len() + project.len()
        );
        assert_eq!(
            stack.approximate_tokens(),
            [contract, agents, project]
                .iter()
                .map(|s| s.chars().count().div_ceil(4))
                .sum::<usize>()
        );
        assert_eq!(
            fs::read_to_string(root.join("@AgentContract.md")).unwrap(),
            contract
        );
    }

    #[test]
    fn rules_1590_similar_lines_and_headings_keep_peer_coordinates_and_exact_flags() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("@AgentContract.md"), "# Memory usage\nTreat every imported file as read-only evidence.\nKeep canonical files unchanged.\n").unwrap();
        fs::write(temp.path().join("AGENTS.md"), "## Memory use\nTreat every imported document as read-only evidence.\nKeep   canonical files unchanged.\nUnrelated instructions about garden plants.\n").unwrap();
        let stack = discover(temp.path(), temp.path());
        assert_eq!(stack.layers[0].duplicate_lines, [3]);
        assert_eq!(stack.layers[1].duplicate_lines, [3]);
        for (layer, peer) in [(0, 1), (1, 0)] {
            assert_eq!(
                stack.layers[layer].similar_lines,
                [
                    SimilarLine {
                        line: 1,
                        other_layer: peer,
                        other_line: 1
                    },
                    SimilarLine {
                        line: 2,
                        other_layer: peer,
                        other_line: 2
                    },
                ]
            );
        }
        assert!(stack.diagnostics.is_empty());
    }

    #[test]
    fn rules_1590_analysis_limits_are_visible_without_truncating_source_or_totals() {
        let temp = tempfile::tempdir().unwrap();
        let source: String = (0..4100)
            .map(|n| format!("IndependentInstruction{n:05}\n"))
            .collect();
        fs::write(temp.path().join("AGENTS.md"), &source).unwrap();
        let stack = discover(temp.path(), temp.path());
        assert!(stack
            .diagnostics
            .iter()
            .any(|d| d.contains("analysis is partial")));
        assert_eq!(stack.total_bytes(), source.len());
        assert_eq!(stack.layers[0].source.as_deref(), Some(source.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn rules_1590_canonical_alias_is_counted_once_but_distinct_equal_files_are_flagged() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let rule = "Keep canonical files unchanged.\n";
        fs::write(temp.path().join("@AgentContract.md"), rule).unwrap();
        fs::write(temp.path().join("AGENTS.md"), "@alias.md\n@copy.md\n").unwrap();
        fs::write(temp.path().join("copy.md"), rule).unwrap();
        symlink("@AgentContract.md", temp.path().join("alias.md")).unwrap();
        let stack = discover(temp.path(), temp.path());
        assert!(stack.layers[2].source.is_none());
        assert_eq!(stack.layers[0].duplicate_lines, [1]);
        assert_eq!(stack.layers[3].duplicate_lines, [1]);
        assert_eq!(
            stack.total_bytes(),
            rule.len() * 2 + "@alias.md\n@copy.md\n".len()
        );
    }
    #[test]
    fn rules_share_import_parser_and_ignore_fenced_examples() {
        let root = std::env::temp_dir().join(format!("okilum-rules-parser-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        fs::write(
            root.join("AGENTS.md"),
            format!(
                "```md\n@fenced.md\n```\n    @indented.md\n@{}\n",
                root.join("actual.md").display()
            ),
        )
        .unwrap();
        fs::write(root.join("actual.md"), "Actual imported rule\n").unwrap();
        let stack = discover(&root, &root);
        assert_eq!(
            stack.layers.len(),
            2,
            "examples never become missing imports"
        );
        assert_eq!(
            stack.layers[1].source.as_deref(),
            Some("Actual imported rule\n")
        );
        assert_eq!(
            stack.layers[1].provenance,
            Provenance::Import {
                from: "AGENTS.md".into(),
                line: 5
            }
        );
        fs::remove_dir_all(root).unwrap();
    }

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
                "@AgentContract.md",
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
                "@AgentContract.md",
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

#[cfg(test)]
mod underscore_tests {
    #[test]
    fn issue2847_underscore_contract_has_no_implicit_role() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("_AgentContract.md"), "# Ordinary note\n").unwrap();
        let stack = super::discover(root.path(), root.path());
        assert!(stack.layers.is_empty());
        assert!(stack.diagnostics.is_empty());
        std::fs::write(root.path().join("@AgentContract.md"), "# Rules\n").unwrap();
        let stack = super::discover(root.path(), root.path());
        assert_eq!(stack.layers.len(), 1);
        assert_eq!(
            stack.layers[0].path,
            std::path::Path::new("@AgentContract.md")
        );
    }
}

#[cfg(test)]
mod claude_fallback_tests {
    use std::{fs, path::PathBuf};

    #[test]
    fn issue2970_claude_md_in_folder_chain_warns_agents_fallback_is_off() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        fs::create_dir_all(root.join("Dev/App/sub")).unwrap();
        fs::create_dir_all(root.join("Other")).unwrap();
        fs::write(root.join("AGENTS.md"), "# Rules\n").unwrap();
        fs::write(root.join("Dev/App/AGENTS.md"), "# App\n").unwrap();
        let stack = super::discover(root, &root.join("Dev/App/sub"));
        assert!(
            stack.claude_md.is_empty() && stack.agents_fallback_warnings().is_empty(),
            "control: AGENTS.md alone keeps the fallback"
        );
        // Outside the chain: a sibling folder and a child of the project.
        fs::write(root.join("Other/CLAUDE.md"), "x\n").unwrap();
        fs::create_dir_all(root.join("Dev/App/sub/deeper")).unwrap();
        fs::write(root.join("Dev/App/sub/deeper/CLAUDE.md"), "x\n").unwrap();
        assert!(super::discover(root, &root.join("Dev/App/sub"))
            .claude_md
            .is_empty());
        fs::write(root.join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
        fs::write(root.join("Dev/App/CLAUDE.md"), "Claude only\n").unwrap();
        let stack = super::discover(root, &root.join("Dev/App/sub"));
        assert_eq!(
            stack.claude_md,
            [
                PathBuf::from("CLAUDE.md"),
                PathBuf::from("Dev/App/CLAUDE.md")
            ]
        );
        assert_eq!(
            stack.agents_fallback_warnings(),
            [
                (
                    PathBuf::from("CLAUDE.md"),
                    "CLAUDE.md in the vault root switches off AGENTS.md for Claude Code".to_owned()
                ),
                (
                    PathBuf::from("Dev/App/CLAUDE.md"),
                    "CLAUDE.md in Dev/App switches off AGENTS.md for Claude Code".to_owned()
                ),
            ]
        );
        assert_eq!(
            fs::read_to_string(root.join("CLAUDE.md")).unwrap(),
            "@AGENTS.md\n"
        );
    }
}
