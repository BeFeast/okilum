//! Whole-vault name/path matching, independent of the full-text index.
use std::collections::HashMap;

use crate::vault::Note;

/// Rank every note before limiting display rows. Recent paths are oldest first.
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
    let query = query.trim().to_lowercase();
    let mut matches: Vec<_> = notes
        .iter()
        .filter_map(|note| {
            let score = if query.is_empty() {
                0
            } else {
                let display = titles.get(&note.path).map(|t| t.to_lowercase());
                score(&note.title.to_lowercase(), &query)
                    .into_iter()
                    .chain(display.and_then(|title| score(&title, &query)))
                    .map(|s| s + 1000)
                    .chain(score(&note.path.to_lowercase(), &query))
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
        b.1.cmp(&a.1)
            .then(b.2.cmp(&a.2))
            .then(a.0.path.cmp(&b.0.path))
    });
    matches
        .into_iter()
        .take(limit)
        .map(|(note, _, _)| note.clone())
        .collect()
}

fn score(text: &str, query: &str) -> Option<i64> {
    if text == query {
        return Some(10000);
    }
    if let Some(offset) = text.find(query) {
        return Some(5000 - offset as i64);
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
    fn landing_uses_literal_match_not_query_expression() {
        assert_eq!(
            matched_text("a <b>running</b> example"),
            Some("running".into())
        );
        assert_eq!(matched_text("<b>A&amp;B&lt;C</b>"), Some("A&B<C".into()));
        assert_eq!(matched_text("no marks"), None);
    }
}
