//! Whole-vault name/path matching, independent of the full-text index.
use std::collections::HashMap;

use crate::vault::{EntryKind, Note, VaultEntry};

/// Every file the Reader can open (#686): the Markdown notes plus the vault's
/// other files. A non-Markdown file is named by its file name with the
/// extension, so `tg.log` and `tg.md` stay distinguishable and both match.
pub fn inventory(notes: impl IntoIterator<Item = Note>, entries: &[VaultEntry]) -> Vec<Note> {
    let mut inventory: Vec<Note> = notes
        .into_iter()
        .filter(|note| {
            !crate::vault::service_path(std::path::Path::new(&note.path))
                && !crate::vault_conventions::asset_content(&note.path)
        })
        .collect();
    inventory.extend(
        entries
            .iter()
            .filter(|entry| {
                entry.kind == EntryKind::Attachment
                    && !(crate::vault_conventions::asset_content(&entry.path)
                        && entry.path.to_ascii_lowercase().ends_with(".md"))
                    && !crate::vault::service_path(std::path::Path::new(&entry.path))
            })
            .map(|entry| Note {
                path: entry.path.clone(),
                title: entry
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&entry.path)
                    .to_owned(),
            }),
    );
    inventory
}

/// Conventional agent instruction files (#1589), matched by exact file name.
const AGENT_FILE_NAMES: [&str; 6] = [
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    "SKILL.md",
    "CONTEXT.md",
    "ANTIGRAVITY.md",
];

/// A vault-relative path names an agent instruction file: its last segment is
/// exactly one of the conventional names, in any folder. A note that merely
/// mentions agents, or `my-AGENTS.md`, is an ordinary file.
pub fn is_agent_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    AGENT_FILE_NAMES.contains(&name)
}

/// Shims are tiny provider files containing only a local AGENTS.md import.
/// Keep this content classification separate from the broader visibility rule.
pub const AGENT_SHIM_MAX_BYTES: u64 = 4096;

pub fn is_agent_shim(path: &str, source: &str) -> bool {
    matches!(path.rsplit('/').next(), Some("CLAUDE.md" | "GEMINI.md"))
        && source.len() as u64 <= AGENT_SHIM_MAX_BYTES
        && matches!(
            source.trim().trim_start_matches('\u{feff}').trim(),
            "@AGENTS.md" | "@./AGENTS.md"
        )
}

/// Worker-only bounded read. Even a file growing after metadata inspection
/// consumes at most 4097 bytes, never its entire body.
pub fn read_agent_shim(path: &str, reader: impl std::io::Read) -> bool {
    use std::io::Read as _;
    let mut source = String::new();
    reader
        .take(AGENT_SHIM_MAX_BYTES + 1)
        .read_to_string(&mut source)
        .is_ok()
        && is_agent_shim(path, &source)
}

/// Which files the search palettes list. Transient: every explicit open starts
/// from [`FileFilter::Default`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileFilter {
    /// Everything but agent files.
    #[default]
    Default,
    /// Agent files only.
    AgentFiles,
}

impl FileFilter {
    /// Whether a file at `path` belongs to this view. The vault-root
    /// `@AgentContract.md` is an agent document like `AGENTS.md` (#2940).
    pub fn admits(self, path: &str) -> bool {
        crate::agent_imports::is_agent_document(path) == (self == FileFilter::AgentFiles)
    }
}

fn is_markdown(path: &str) -> bool {
    path.len() > 3
        && path.is_char_boundary(path.len() - 3)
        && path[path.len() - 3..].eq_ignore_ascii_case(".md")
}

/// Rank every note before limiting display rows. Recent paths are oldest first.
/// Exact beats prefix beats substring beats fuzzy; a name or title match beats
/// a path-only match. Among equal matches, recent files come first, then
/// Markdown notes, so the vault's other files never bury a note.
pub fn search(notes: &[Note], query: &str, recent: &[String], limit: usize) -> Vec<Note> {
    search_titled(notes, &HashMap::new(), query, recent, limit)
}

/// As [`search`], but also matches the display title a note resolves to
/// (frontmatter title or first H1), keyed by path. A title match ranks with
/// the file-name match; the path is always matched as well.
pub fn search_titled(
    notes: &[Note],
    titles: &HashMap<String, String>,
    query: &str,
    recent: &[String],
    limit: usize,
) -> Vec<Note> {
    search_named(
        notes,
        titles,
        &std::collections::BTreeMap::new(),
        query,
        recent,
        limit,
    )
}

/// Match declared aliases at the same rank as file names and display titles.
/// Each canonical path remains one result, including ambiguous aliases.
pub fn search_named(
    notes: &[Note],
    titles: &HashMap<String, String>,
    aliases: &std::collections::BTreeMap<String, Vec<String>>,
    query: &str,
    recent: &[String],
    limit: usize,
) -> Vec<Note> {
    search_named_first(notes, titles, aliases, query, recent, "", limit)
}

/// As [`search_named`], but every match inside `first` (a vault-relative
/// folder, such as the active project) ranks above every match outside it
/// (#1518). An empty `first` is the whole vault and changes nothing.
pub fn search_named_first(
    notes: &[Note],
    titles: &HashMap<String, String>,
    aliases: &std::collections::BTreeMap<String, Vec<String>>,
    query: &str,
    recent: &[String],
    first: &str,
    limit: usize,
) -> Vec<Note> {
    let first = first.trim_end_matches('/');
    let inside = |path: &str| {
        first.is_empty()
            || path
                .strip_prefix(first)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    let query = query.trim().to_lowercase();
    let mut matches: Vec<_> = notes
        .iter()
        .filter_map(|note| {
            let score = if query.is_empty() {
                0
            } else {
                let display = titles.get(&note.path).map(|t| t.to_lowercase());
                score(&note.title.to_lowercase(), &query, true)
                    .into_iter()
                    .chain(display.and_then(|title| score(&title, &query, true)))
                    .chain(
                        aliases
                            .get(&note.path)
                            .into_iter()
                            .flatten()
                            .filter_map(|name| score(&name.to_lowercase(), &query, true)),
                    )
                    .map(|s| s + 1000)
                    .chain(score(&note.path.to_lowercase(), &query, false))
                    .max()?
            };
            let recency = recent
                .iter()
                .rposition(|p| p == &note.path)
                .map(|i| i + 1)
                .unwrap_or(0);
            Some((note, score, recency))
        })
        .collect();
    matches.sort_by(|a, b| {
        inside(&b.0.path)
            .cmp(&inside(&a.0.path))
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(is_markdown(&b.0.path).cmp(&is_markdown(&a.0.path)))
            .then(a.0.path.cmp(&b.0.path))
    });
    matches
        .into_iter()
        .take(limit)
        .map(|(note, _, _)| note.clone())
        .collect()
}

/// `name` gives a prefix its own tier above any other substring; a path
/// prefix is only the top-level folder, so it ranks as a plain substring.
fn score(text: &str, query: &str, name: bool) -> Option<i64> {
    if text == query {
        return Some(10000);
    }
    if name && text.starts_with(query) {
        return Some(7000);
    }
    if let Some(offset) = text.find(query) {
        return Some(5000 - offset.min(4000) as i64);
    }
    let mut wanted = query.chars().filter(|c| !c.is_whitespace());
    let mut next = wanted.next()?;
    let mut gaps = 0;
    for c in text.chars() {
        if c == next {
            match wanted.next() {
                Some(c) => next = c,
                None => return Some(1000 - gaps),
            }
        } else {
            gaps += 1;
        }
    }
    None
}

/// Tantivy emits escaped text and `<b>` marks. Use the actual matched token,
/// not query syntax or its stem, when landing in the rendered document.
pub fn matched_text(html: &str) -> Option<String> {
    let (_, rest) = html.split_once("<b>")?;
    let (text, _) = rest.split_once("</b>")?;
    Some(
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&#x27;", "'")
            .replace("&amp;", "&"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn issue1587_shims_require_only_a_local_agents_import() {
        for source in [
            "@AGENTS.md",
            "@./AGENTS.md",
            " \r\n\u{feff}  @./AGENTS.md \r\n ",
            "\u{feff}@AGENTS.md\n",
        ] {
            for path in ["CLAUDE.md", "Project/GEMINI.md"] {
                assert!(is_agent_shim(path, source), "{path}: {source:?}");
            }
        }
        for source in [
            "",
            "# Own rules",
            "@AGENTS.md\nOwn rules",
            "@../AGENTS.md",
            "@agents.md",
            "@AGENTS.md\n@AGENTS.md",
            "@ AGENTS.md",
            "@AGENTS.md # comment",
        ] {
            assert!(!is_agent_shim("CLAUDE.md", source), "{source:?}");
        }
        for path in ["AGENTS.md", "my-CLAUDE.md", "claude.md", "CLAUDE.md.bak"] {
            assert!(!is_agent_shim(path, "@AGENTS.md"));
        }
    }

    #[test]
    fn issue1587_two_megabyte_provider_file_read_is_bounded() {
        let body = vec![b' '; 2 * 1024 * 1024];
        let mut reader = std::io::Cursor::new(body);
        assert!(!read_agent_shim("CLAUDE.md", &mut reader));
        assert_eq!(reader.position(), AGENT_SHIM_MAX_BYTES + 1);
        assert!(read_agent_shim("CLAUDE.md", "@AGENTS.md\r\n".as_bytes()));
        assert!(!read_agent_shim("CLAUDE.md", [0xff].as_slice()));
    }

    #[test]
    fn ranks_whole_vault_names_paths_unicode_and_recent() {
        let notes: Vec<_> = (0..5000)
            .map(|i| Note {
                path: format!("area-{i}/Note {i}.md"),
                title: format!("Note {i}"),
            })
            .collect();
        let recent = vec![notes[4999].path.clone()];
        assert_eq!(search(&notes, "", &recent, 30)[0].path, notes[4999].path);
        assert_eq!(
            search(&notes, "Note 4999", &[], 30)[0].path,
            notes[4999].path
        );
        assert_eq!(
            search(&notes, "a4999/n4999", &[], 30)[0].path,
            notes[4999].path
        );
        assert!(search(&notes, "absent", &[], 30).is_empty());
        let russian = vec![Note {
            path: "Работа/Заметка.md".into(),
            title: "Заметка".into(),
        }];
        assert_eq!(search(&russian, "ЗМТ", &[], 30).len(), 1);
        for length in 1..="Note 4999".len() {
            assert!(!search(&notes, &"Note 4999"[..length], &recent, 30).is_empty());
        }
    }
    #[test]
    fn display_titles_match_alongside_names_and_paths() {
        let notes = vec![
            Note {
                path: "Projects/2026-10-06-kickoff.md".into(),
                title: "2026-10-06-kickoff".into(),
            },
            Note {
                path: "Projects/Roadmap.md".into(),
                title: "Roadmap".into(),
            },
        ];
        let titles = HashMap::from([(
            "Projects/2026-10-06-kickoff.md".to_string(),
            "Quarterly planning".to_string(),
        )]);
        let paths = |query: &str| -> Vec<String> {
            search_titled(&notes, &titles, query, &[], 30)
                .into_iter()
                .map(|note| note.path)
                .collect()
        };
        // Resolved title: substring and fuzzy.
        assert_eq!(paths("quarterly"), ["Projects/2026-10-06-kickoff.md"]);
        assert_eq!(paths("qplan"), ["Projects/2026-10-06-kickoff.md"]);
        // File name and path still match.
        assert_eq!(paths("kickoff"), ["Projects/2026-10-06-kickoff.md"]);
        assert_eq!(paths("proj/road"), ["Projects/Roadmap.md"]);
        // Without titles, the resolved title is not searchable (positive control).
        assert!(search(&notes, "quarterly", &[], 30).is_empty());
    }
    #[test]
    fn inventory_lists_every_openable_file_with_its_extension() {
        let notes = vec![Note {
            path: "Health/medications.md".into(),
            title: "medications".into(),
        }];
        let entry = |path: &str, kind| VaultEntry {
            path: path.into(),
            kind,
        };
        let entries = [
            entry("Health", EntryKind::Directory),
            entry("Health/medications.md", EntryKind::Markdown),
            entry("Health/medications-data.toml", EntryKind::Attachment),
            entry("tg.log", EntryKind::Attachment),
        ];
        let files: Vec<_> = inventory(notes, &entries)
            .into_iter()
            .map(|note| (note.path, note.title))
            .collect();
        assert_eq!(
            files,
            [
                ("Health/medications.md".into(), "medications".into()),
                (
                    "Health/medications-data.toml".into(),
                    "medications-data.toml".into()
                ),
                ("tg.log".into(), "tg.log".into()),
            ] as [(String, String); 3]
        );
    }
    #[test]
    fn non_markdown_files_match_without_burying_notes() {
        let entries: Vec<_> = [
            "tg.log",
            "Health/medications-data.toml",
            "Archive/old-medications.csv",
            "Media/medications-chart.png",
        ]
        .into_iter()
        .map(|path| VaultEntry {
            path: path.into(),
            kind: EntryKind::Attachment,
        })
        .collect();
        let notes = inventory(
            [
                ("Health/medications.md", "medications"),
                ("Health/Telegram.md", "Telegram"),
                ("medical/Plan.md", "Plan"),
            ]
            .map(|(path, title)| Note {
                path: path.into(),
                title: title.into(),
            }),
            &entries,
        );
        let titles = HashMap::from([(
            "Health/medications.md".to_string(),
            "Medications".to_string(),
        )]);
        let paths = |query: &str| -> Vec<String> {
            search_titled(&notes, &titles, query, &[], 30)
                .into_iter()
                .map(|note| note.path)
                .collect()
        };
        // Non-Markdown files are found by name, extension included.
        assert_eq!(paths("tg.log"), ["tg.log"]);
        assert_eq!(paths("tg")[0], "tg.log");
        assert_eq!(paths("toml"), ["Health/medications-data.toml"]);
        // Name prefix beats substring; at equal strength the note comes first.
        assert_eq!(
            paths("medications"),
            [
                "Health/medications.md",
                "Health/medications-data.toml",
                "Media/medications-chart.png",
                "Archive/old-medications.csv",
            ]
        );
        // A folder prefix is not a name prefix.
        assert_eq!(paths("med")[..3], paths("medications")[..3]);
        // The empty query lists notes before other files; recency still wins.
        let all: Vec<_> = search(&notes, "", &[], 30)
            .into_iter()
            .map(|note| note.path)
            .collect();
        assert_eq!(all.len(), 7);
        assert!(all[..3].iter().all(|path| path.ends_with(".md")));
        let recent = vec!["tg.log".to_string()];
        assert_eq!(search(&notes, "", &recent, 30)[0].path, "tg.log");
    }
    #[test]
    fn project_matches_rank_before_the_rest_of_the_vault() {
        // #1518: inside a project, Files lists the project first.
        let note = |path: &str| Note {
            path: path.into(),
            title: path
                .rsplit('/')
                .next()
                .unwrap()
                .trim_end_matches(".md")
                .into(),
        };
        let notes = vec![
            note("plan.md"),
            note("Dev/App/design/plan draft.md"),
            note("Dev/Apps/plan.md"),
            note("Dev/App/notes.md"),
        ];
        let empty = HashMap::new();
        let aliases = std::collections::BTreeMap::new();
        let paths = |first: &str, recent: &[String]| -> Vec<String> {
            search_named_first(&notes, &empty, &aliases, "plan", recent, first, 30)
                .into_iter()
                .map(|n| n.path)
                .collect()
        };
        // Whole vault: the exact name wins, a sibling folder is not the project.
        assert_eq!(
            paths("", &[]),
            [
                "Dev/Apps/plan.md",
                "plan.md",
                "Dev/App/design/plan draft.md"
            ]
        );
        assert_eq!(paths("", &[]), {
            search_named(&notes, &empty, &aliases, "plan", &[], 30)
                .into_iter()
                .map(|n| n.path)
                .collect::<Vec<_>>()
        });
        // In the project its weaker match leads; the rest keeps its order.
        assert_eq!(
            paths("Dev/App", &[]),
            [
                "Dev/App/design/plan draft.md",
                "Dev/Apps/plan.md",
                "plan.md"
            ]
        );
        assert_eq!(paths("Dev/App/", &[]), paths("Dev/App", &[]));
        // Recency still orders equal matches inside each part.
        let recent = vec!["Dev/Apps/plan.md".to_string(), "plan.md".to_string()];
        assert_eq!(
            paths("Dev/App", &recent),
            [
                "Dev/App/design/plan draft.md",
                "plan.md",
                "Dev/Apps/plan.md"
            ]
        );
    }
    #[test]
    fn agent_files_are_exact_conventional_names_in_any_folder() {
        for path in [
            "AGENTS.md",
            "CLAUDE.md",
            "GEMINI.md",
            "SKILL.md",
            "CONTEXT.md",
            "ANTIGRAVITY.md",
            "Dev/Areas/okilum/AGENTS.md",
            "Dev/Areas/okilum/CLAUDE.md",
            "_Assets/Skills/review/SKILL.md",
            "Работа/Проект/GEMINI.md",
        ] {
            assert!(is_agent_file(path), "{path}");
            assert!(FileFilter::AgentFiles.admits(path), "{path}");
            assert!(!FileFilter::Default.admits(path), "{path}");
        }
        for path in [
            "my-AGENTS.md",
            "Dev/my-AGENTS.md",
            "Dev/AGENTS.md.bak",
            "Dev/AGENTS",
            "Dev/agents.md",
            "agent.md",
            "Dev/AGENT.md",
            "Dev/context.md",
            "Dev/Claude.md",
            "Dev/AGENTS.md/notes.md",
            "Agents/Agent notes.md",
            "Dev/agent-handbook.md",
            "Dev/SKILLS.md",
            "Dev/AGENTS .md",
            "Area/@AgentContract.md",
            "_AgentContract.md",
            "AgentContract.md",
        ] {
            assert!(!is_agent_file(path), "{path}");
            assert!(FileFilter::Default.admits(path), "{path}");
            assert!(!FileFilter::AgentFiles.admits(path), "{path}");
        }
        assert_eq!(FileFilter::default(), FileFilter::Default);
        // #2940: the vault-root contract joins the Agent files view, while the
        // naming rule (tree visibility, Rules discovery) stays unchanged.
        assert!(!is_agent_file("@AgentContract.md"));
        assert!(FileFilter::AgentFiles.admits("@AgentContract.md"));
        assert!(!FileFilter::Default.admits("@AgentContract.md"));
    }
    #[test]
    fn filtering_the_inventory_precedes_the_result_limit() {
        // Excluded exact matches outnumber the limit; the only admitted match
        // ranks below all of them, so a post-limit filter would lose it.
        let mut notes: Vec<_> = (0..150)
            .map(|i| Note {
                path: format!("Area {i}/AGENTS.md"),
                title: "AGENTS".into(),
            })
            .collect();
        notes.push(Note {
            path: "Zone/agents-handbook-review.md".into(),
            title: "agents-handbook-review".into(),
        });
        let empty = HashMap::new();
        let aliases = std::collections::BTreeMap::new();
        let ranked = |notes: &[Note]| -> Vec<String> {
            search_named_first(notes, &empty, &aliases, "agents", &[], "", 100)
                .into_iter()
                .map(|n| n.path)
                .collect()
        };
        let unfiltered = ranked(&notes);
        assert_eq!(unfiltered.len(), 100);
        assert!(
            !unfiltered.contains(&"Zone/agents-handbook-review.md".to_string()),
            "positive control: a post-limit filter would find nothing"
        );
        let admitted: Vec<_> = notes
            .iter()
            .filter(|n| FileFilter::Default.admits(&n.path))
            .cloned()
            .collect();
        assert_eq!(ranked(&admitted), ["Zone/agents-handbook-review.md"]);
        let agents: Vec<_> = notes
            .iter()
            .filter(|n| FileFilter::AgentFiles.admits(&n.path))
            .cloned()
            .collect();
        let agents = ranked(&agents);
        assert_eq!(agents.len(), 100);
        assert!(agents.iter().all(|path| path.ends_with("/AGENTS.md")));
    }
    #[test]
    fn landing_uses_literal_match_not_query_expression() {
        assert_eq!(
            matched_text("a <b>running</b> example"),
            Some("running".into())
        );
        assert_eq!(matched_text("<b>A&amp;B&lt;C</b>"), Some("A&B<C".into()));
        assert_eq!(matched_text("no marks"), None);
    }
}
