//! Shared, deterministic completion grammar and ranking (#1638).
//! No filesystem or UI work: providers insert ordinary Markdown source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::reader_editor) enum Kind {
    Note,
    Heading(String),
    Emoji,
    Language,
    Block,
}

/// Lower is better. Path fragments match consecutive path segments by prefix;
/// subsequences are a final fallback for all providers.
#[cfg(test)]
pub(super) fn score(query: &str, title: &str, path: &str) -> Option<u8> {
    Matcher::new(query).score(title, path)
}

/// Prepare the query once per search, rather than allocating query/path segment
/// vectors for each note. Matching a path walks borrowed segment iterators.
pub(super) struct Matcher<'a> {
    query: &'a str,
    fragments: Vec<&'a str>,
}

impl<'a> Matcher<'a> {
    pub(super) fn new(query: &'a str) -> Self {
        let fragments = if query.trim_start_matches('/').contains('/') {
            query.trim_start_matches('/').split('/').collect()
        } else {
            Vec::new()
        };
        Self { query, fragments }
    }

    pub(super) fn score(&self, title: &str, path: &str) -> Option<u8> {
        let query = self.query;
        if query.is_empty() || title == query {
            return Some(0);
        }
        if title.starts_with(query) {
            return Some(1);
        }
        if title
            .match_indices(query)
            .any(|(at, _)| title[..at].ends_with([' ', '-', '_', '.', '(']))
        {
            return Some(2);
        }
        if title.contains(query) {
            return Some(3);
        }
        if path.contains(query) {
            return Some(4);
        }
        if !self.fragments.is_empty() {
            let mut segments = path.split('/');
            while segments.clone().next().is_some() {
                let mut parts = segments.clone();
                if self
                    .fragments
                    .iter()
                    .all(|fragment| parts.next().is_some_and(|part| part.starts_with(fragment)))
                {
                    return Some(5);
                }
                segments.next();
            }
        }
        let mut chars = query.chars().filter(|c| *c != '/');
        let mut next = chars.next();
        for ch in path.chars() {
            if next == Some(ch) {
                next = chars.next();
            }
            if next.is_none() {
                return Some(6);
            }
        }
        None
    }
}

/// Syntactic trigger (provider, replacement start, query). The host validates
/// complete Markdown context before publishing rows, including for a line tail.
pub(super) fn trigger(line: &str, emoji: bool) -> Option<(Kind, usize, String)> {
    if let Some(open) = line.rfind("[[") {
        let query = &line[open + 2..];
        if !query.contains(['[', ']', '|', '^']) && query.chars().count() <= 120 {
            if let Some((note, heading)) = query.split_once('#') {
                return (!heading.contains('#'))
                    .then(|| (Kind::Heading(note.into()), open + 2, heading.into()));
            }
            return Some((Kind::Note, open + 2, query.into()));
        }
    }
    // Only a new fence's info string, never a code block's contents.
    if let Some(query) = line.strip_prefix("```") {
        if !query.is_empty()
            && query
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-'))
        {
            return Some((Kind::Language, 3, query.into()));
        }
    }
    if let Some(query) = line.strip_prefix('/') {
        if query.chars().all(|c| c.is_ascii_alphabetic()) {
            return Some((Kind::Block, 0, query.into()));
        }
    }
    if emoji {
        let at = line.rfind(':')?;
        let query = &line[at + 1..];
        if (at == 0 || line[..at].ends_with(|c: char| c.is_whitespace() || matches!(c, '(' | '[')))
            && query
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
        {
            return Some((Kind::Emoji, at, query.into()));
        }
    }
    None
}

/// Backtick runs close only a run of the same length. Escaped backticks in
/// prose are literal; backslashes inside code do not escape its delimiter.
pub(super) fn in_inline_code(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    let mut delimiter = None;
    let mut at = 0;
    while at < bytes.len() {
        if delimiter.is_none() && bytes[at] == b'\\' {
            at += 2;
        } else if bytes[at] == b'`' {
            let start = at;
            while at < bytes.len() && bytes[at] == b'`' {
                at += 1;
            }
            let len = at - start;
            if delimiter == Some(len) {
                delimiter = None;
            } else if delimiter.is_none() {
                delimiter = Some(len);
            }
        } else {
            at += 1;
        }
    }
    delimiter.is_some()
}

/// One replacement, one Undo step. Existing link aliases and closing marks
/// stay authored; emoji consumes its optional closing colon.
pub(super) fn replacement(
    text: &str,
    start: usize,
    caret: usize,
    kind: &Kind,
    target: &str,
) -> (std::ops::Range<usize>, String, usize) {
    if !matches!(kind, Kind::Note | Kind::Heading(_)) {
        let mut end = caret;
        while let Some(ch) = text[end..].chars().next() {
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-' | '#')) {
                break;
            }
            end += ch.len_utf8();
        }
        if matches!(kind, Kind::Emoji) && text[end..].starts_with(':') {
            end += 1;
        }
        let crlf = text
            .find('\n')
            .is_some_and(|at| at > 0 && text.as_bytes()[at - 1] == b'\r');
        let inserted = if matches!(kind, Kind::Block) && crlf {
            target.replace('\n', "\r\n")
        } else {
            target.to_owned()
        };
        let after = start
            + if matches!(kind, Kind::Block) && target == "```\n\n```" {
                if crlf {
                    5
                } else {
                    4
                }
            } else {
                inserted.len()
            };
        return (start..end, inserted, after);
    }
    let tail = text[caret..]
        .find(['[', ']', '|', '#', '^', '\n', '\r'])
        .map_or(text.len(), |at| caret + at);
    let rest = &text[tail..];
    if rest.starts_with("]]") {
        (start..tail, target.into(), start + target.len() + 2)
    } else if rest.starts_with(['|', '#', '^']) {
        (start..tail, target.into(), start + target.len())
    } else if rest.starts_with(']') {
        (start..tail, format!("{target}]"), start + target.len() + 2)
    } else {
        (
            start..caret,
            format!("{target}]]"),
            start + target.len() + 2,
        )
    }
}

/// Keep every candidate selectable while painting at most one page. Keyboard
/// movement brings the selected row into the same popup, including wraparound.
pub(super) fn visible_rows(count: usize, selected: usize, limit: usize) -> std::ops::Range<usize> {
    let end = count.min(selected.saturating_add(1).max(limit));
    end.saturating_sub(limit)..end
}

/// Disposable worker-only cache. Exact vault path, note path and the complete
/// source revision form the key; neither filenames nor mtime alone identify it.
pub(super) struct MetadataCache<R> {
    entries: std::collections::VecDeque<(std::path::PathBuf, String, R, Option<String>)>,
}

impl<R> Default for MetadataCache<R> {
    fn default() -> Self {
        Self {
            entries: Default::default(),
        }
    }
}

impl<R: PartialEq> MetadataCache<R> {
    pub(super) const LIMIT: usize = 128;

    pub(super) fn get(
        &self,
        root: &std::path::Path,
        path: &str,
        revision: &R,
    ) -> Option<&Option<String>> {
        self.entries
            .iter()
            .rev()
            .find(|(r, p, v, _)| r == root && p == path && v == revision)
            .map(|(_, _, _, value)| value)
    }

    pub(super) fn insert(
        &mut self,
        root: &std::path::Path,
        path: &str,
        revision: R,
        badge: Option<String>,
    ) {
        // A new revision replaces this identity's older cached value.
        self.entries.retain(|(r, p, _, _)| r != root || p != path);
        self.entries
            .push_back((root.into(), path.into(), revision, badge));
        while self.entries.len() > Self::LIMIT {
            self.entries.pop_front();
        }
    }
}

pub(super) const LANGUAGES: &[&str] = &[
    "bash",
    "c",
    "cpp",
    "csharp",
    "css",
    "diff",
    "dockerfile",
    "go",
    "graphql",
    "html",
    "java",
    "javascript",
    "json",
    "kotlin",
    "lua",
    "markdown",
    "mermaid",
    "python",
    "ruby",
    "rust",
    "shell",
    "sql",
    "swift",
    "toml",
    "typescript",
    "xml",
    "yaml",
];
pub(super) const EMOJI: &[(&str, &str)] = &[
    ("smile", "😄"),
    ("smiley", "😃"),
    ("grinning", "😀"),
    ("laughing", "😆"),
    ("wink", "😉"),
    ("heart", "❤️"),
    ("heart_eyes", "😍"),
    ("thinking", "🤔"),
    ("thumbsup", "👍"),
    ("+1", "👍"),
    ("thumbsdown", "👎"),
    ("-1", "👎"),
    ("tada", "🎉"),
    ("rocket", "🚀"),
    ("fire", "🔥"),
    ("warning", "⚠️"),
    ("check", "✅"),
    ("white_check_mark", "✅"),
    ("x", "❌"),
    ("eyes", "👀"),
    ("brain", "🧠"),
    ("bulb", "💡"),
    ("memo", "📝"),
    ("calendar", "📅"),
    ("star", "⭐"),
    ("sparkles", "✨"),
    ("bug", "🐛"),
    ("wave", "👋"),
    ("pray", "🙏"),
    ("clap", "👏"),
    ("muscle", "💪"),
    ("cry", "😢"),
    ("sob", "😭"),
    ("joy", "😂"),
    ("sunglasses", "😎"),
    ("ok_hand", "👌"),
];
pub(super) const BLOCKS: &[(&str, &str)] = &[
    ("heading", "## "),
    ("list", "- "),
    ("task", "- [ ] "),
    ("table", "| Column | Column |\n| --- | --- |\n|  |  |"),
    ("code", "```\n\n```"),
    ("callout", "> [!note]\n> "),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_cache_uses_complete_identity_and_revision_and_bounds_entries() {
        use std::path::Path;
        let root = Path::new("/vault-a");
        let other = Path::new("/vault-b");
        let mut cache = MetadataCache::default();
        // Revision includes a change witness, not just equal mtime/size.
        let revision = (64, 100, 7);
        cache.insert(root, "Work/Plan.md", revision, Some("project".into()));
        assert_eq!(
            cache.get(root, "Work/Plan.md", &revision),
            Some(&Some("project".into()))
        );
        assert_eq!(cache.get(root, "Home/Plan.md", &revision), None);
        assert_eq!(cache.get(other, "Work/Plan.md", &revision), None);
        assert_eq!(cache.get(root, "Work/Plan.md", &(64, 100, 8)), None);
        cache.insert(root, "Work/Plan.md", (64, 100, 8), None);
        assert_eq!(cache.get(root, "Work/Plan.md", &revision), None);
        assert_eq!(cache.get(root, "Work/Plan.md", &(64, 100, 8)), Some(&None));
        for n in 0..MetadataCache::<(i32, i32, i32)>::LIMIT {
            cache.insert(root, &format!("{n}.md"), revision, None);
        }
        assert_eq!(cache.entries.len(), MetadataCache::<(i32, i32, i32)>::LIMIT);
        assert_eq!(cache.get(root, "Work/Plan.md", &(64, 100, 8)), None);
    }

    #[test]
    fn keyboard_window_reaches_every_candidate_and_wraps() {
        for count in [1, 8, 9, 36, 5576] {
            for selected in 0..count {
                let visible = visible_rows(count, selected, 8);
                assert!(visible.contains(&selected));
                assert!(visible.len() <= 8);
                assert!(visible.end <= count);
            }
        }
        assert_eq!(visible_rows(36, 35, 8), 28..36);
        assert_eq!(visible_rows(36, 0, 8), 0..8);
    }

    #[test]
    fn triggers_keep_providers_disjoint_and_colons_in_prose_safe() {
        assert_eq!(
            trigger("See [[/proj/mac", true),
            Some((Kind::Note, 6, "/proj/mac".into()))
        );
        assert_eq!(
            trigger("[[Plan#Go", true),
            Some((Kind::Heading("Plan".into()), 2, "Go".into()))
        );
        assert_eq!(
            trigger("[[#", true),
            Some((Kind::Heading("".into()), 2, "".into()))
        );
        assert_eq!(
            trigger("```ru", true),
            Some((Kind::Language, 3, "ru".into()))
        );
        assert_eq!(trigger("/", true), Some((Kind::Block, 0, "".into())));
        assert_eq!(
            trigger("Hi :smi", true),
            Some((Kind::Emoji, 3, "smi".into()))
        );
        for line in [
            "[[Done]]",
            "[[Plan|alias",
            "[[a#b#c",
            "https:",
            "12:30",
            "word:smile",
            "x /task",
            "/tmp/file",
            "```",
        ] {
            assert_eq!(trigger(line, true), None, "{line}");
        }
        assert_eq!(trigger(":smile", false), None);
    }
    fn legacy_score(query: &str, title: &str, path: &str) -> Option<u8> {
        if query.is_empty() || title == query {
            return Some(0);
        }
        if title.starts_with(query) {
            return Some(1);
        }
        if title
            .match_indices(query)
            .any(|(at, _)| title[..at].ends_with([' ', '-', '_', '.', '(']))
        {
            return Some(2);
        }
        if title.contains(query) {
            return Some(3);
        }
        if path.contains(query) {
            return Some(4);
        }
        let fragments: Vec<_> = query.trim_start_matches('/').split('/').collect();
        if fragments.len() > 1 {
            let segments: Vec<_> = path.split('/').collect();
            if segments.windows(fragments.len()).any(|parts| {
                parts
                    .iter()
                    .zip(&fragments)
                    .all(|(part, fragment)| part.starts_with(fragment))
            }) {
                return Some(5);
            }
        }
        let mut chars = query.chars().filter(|c| *c != '/');
        let mut next = chars.next();
        for ch in path.chars() {
            if next == Some(ch) {
                next = chars.next();
            }
            if next.is_none() {
                return Some(6);
            }
        }
        None
    }

    #[test]
    fn prepared_ranker_preserves_scores_and_measures_same_host_work() {
        use std::hint::black_box;
        use std::time::Instant;
        let paths: Vec<_> = (0..5576)
            .map(|n| format!("library/group {}/observation {n:04} about biology", n % 73))
            .collect();
        let queries = [
            "", "obs", "bio", "zzz", "l/gr", "/lib/gro", "gr/obs", "///", "a//b", "éla",
        ];
        for query in queries {
            let matcher = Matcher::new(query);
            for path in &paths {
                let title = path.rsplit('/').next().unwrap();
                assert_eq!(matcher.score(title, path), legacy_score(query, title, path));
            }
            for path in [
                "",
                "a",
                "/a/b",
                "a//b",
                "dev/projects/macbook",
                "écriture/élan",
                "a/b/",
            ] {
                assert_eq!(matcher.score(path, path), legacy_score(query, path, path));
            }
        }
        let mut samples = [Vec::new(), Vec::new()];
        for rep in 0..8 {
            // Alternate ordering on the same corpus and CPU in this invocation.
            for variant in [rep % 2, 1 - rep % 2] {
                let start = Instant::now();
                let mut checksum = 0usize;
                for query in queries {
                    let matcher = Matcher::new(query);
                    for path in &paths {
                        let title = path.rsplit('/').next().unwrap();
                        checksum += if variant == 0 {
                            legacy_score(black_box(query), title, black_box(path))
                        } else {
                            matcher.score(black_box(title), black_box(path))
                        }
                        .map_or(7, usize::from);
                    }
                }
                black_box(checksum);
                samples[variant].push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        println!(
            "COMPLETION_RANK_AB {}",
            serde_json::json!({
                "notes": paths.len(), "queries": queries, "boundary": "rank-score-only",
                "baseline_ms": samples[0], "prepared_ms": samples[1],
            })
        );
    }

    #[test]
    fn shared_ranker_matches_path_fragments_and_fuzzy_short_names() {
        assert_eq!(
            score("/proj/mac", "macbook", "dev/projects/macbook"),
            Some(5)
        );
        assert_eq!(score("rdmp", "roadmap", "work/roadmap"), Some(6));
        assert_eq!(score("rust", "rust", "rust"), Some(0));
        assert_eq!(score("smi", "smile", "smile"), Some(1));
        assert_eq!(score("zzq", "rust", "rust"), None);
        let mut notes: Vec<_> = (0..5575).map(|n| format!("archive/note{n}")).collect();
        notes.push("projects/macbook".into());
        let matches: Vec<_> = notes
            .iter()
            .filter(|p| score("mac", p, p).is_some())
            .collect();
        assert_eq!(matches, [&"projects/macbook".to_owned()]);
    }
    fn accept(marked: &str, target: &str) -> String {
        let caret = marked.find('¦').unwrap();
        let mut text = marked.replace('¦', "");
        let line_start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
        let (kind, start, _) = trigger(&text[line_start..caret], true).unwrap();
        let (range, replacement, caret) =
            replacement(&text, line_start + start, caret, &kind, target);
        text.replace_range(range, &replacement);
        text.insert(caret, '¦');
        text
    }
    #[test]
    fn all_providers_make_one_source_edit_and_preserve_surrounding_text() {
        assert_eq!(accept("See [[Pl¦]] suffix", "Plan"), "See [[Plan]]¦ suffix");
        assert_eq!(accept("[[Pl¦an|alias]]", "Plan"), "[[Plan¦|alias]]");
        assert_eq!(accept("[[Plan#Go¦als]]", "Plan#Goals"), "[[Plan#Goals]]¦");
        assert_eq!(accept("[[#Go¦", "#Goals"), "[[#Goals]]¦");
        assert_eq!(accept("[[Pl¦an] suffix", "Plan"), "[[Plan]]¦ suffix");
        assert_eq!(accept("Hi :smi¦le:!", "😄"), "Hi 😄¦!");
        assert_eq!(accept("```ru¦st\ncode\n```", "rust"), "```rust¦\ncode\n```");
        assert_eq!(accept("/ta¦sk", "- [ ] "), "- [ ] ¦");
        assert_eq!(accept("before\n/co¦", "```\n\n```"), "before\n```\n¦\n```");
        assert_eq!(
            accept("before\r\n/co¦", "```\n\n```"),
            "before\r\n```\r\n¦\r\n```"
        );
        for (name, source) in BLOCKS {
            let marked = format!("/{name}¦");
            let result = accept(&marked, source);
            assert_eq!(result.replace('¦', ""), *source);
        }
        assert!(LANGUAGES.contains(&"rust"));
        assert!(EMOJI.contains(&("smile", "😄")));
    }
}
