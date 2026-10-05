//! Whole-vault name/path matching, independent of the full-text index.
use crate::vault::Note;

/// Rank every note before limiting display rows. Recent paths are oldest first.
pub fn search(notes: &[Note], query: &str, recent: &[String], limit: usize) -> Vec<Note> {
    let query = query.trim().to_lowercase();
    let mut matches: Vec<_> = notes
        .iter()
        .filter_map(|note| {
            let title = note.title.to_lowercase();
            let path = note.path.to_lowercase();
            let score = if query.is_empty() {
                0
            } else {
                score(&title, &query)
                    .map(|s| s + 1000)
                    .into_iter()
                    .chain(score(&path, &query))
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
    fn landing_uses_literal_match_not_query_expression() {
        assert_eq!(
            matched_text("a <b>running</b> example"),
            Some("running".into())
        );
        assert_eq!(matched_text("<b>A&amp;B&lt;C</b>"), Some("A&B<C".into()));
        assert_eq!(matched_text("no marks"), None);
    }
}
