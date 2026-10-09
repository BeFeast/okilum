//! What a note carries besides prose: frontmatter, tags, headings, dates.
//!
//! Pulled out of the note text so the index can address them as fields rather
//! than hoping the raw frontmatter happens to appear in the body. Everything
//! here is deliberately forgiving — a vault is written by hand, and a malformed
//! frontmatter block must never cost the note its body text.

use std::collections::BTreeMap;

/// Frontmatter values, tags, headings and a date, as far as they can be read
/// off a note without a YAML parser.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facets {
    /// Scalar `key: value` pairs from the frontmatter block, lowercased keys.
    /// Nested structures are skipped rather than flattened: an index that
    /// silently reshapes them would answer questions nobody asked.
    pub frontmatter: BTreeMap<String, String>,
    /// Tags from `tags: [a, b]`, `tags: a, b`, a YAML list, or `#inline` tags.
    /// Lowercased and deduplicated.
    pub tags: Vec<String>,
    /// ATX heading text, `#` markers stripped, in document order.
    pub headings: Vec<String>,
    /// The first frontmatter value that parses as `YYYY-MM-DD`, from any of the
    /// usual key names. Notes date themselves inconsistently; guessing one key
    /// would silently ignore the others.
    pub date: Option<String>,
}

const DATE_KEYS: [&str; 5] = ["date", "created", "updated", "published", "day"];

/// Split a note into its frontmatter block and the body after it.
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    if let Some(end) = rest.find("\n---\n") {
        return (Some(&rest[..end]), &rest[end + 5..]);
    }
    if let Some(end) = rest.find("\n---\r\n") {
        return (Some(&rest[..end]), &rest[end + 6..]);
    }
    (None, text)
}

fn clean_scalar(v: &str) -> String {
    v.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn looks_like_a_date(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// Read the facets of one note.
pub fn extract(text: &str) -> Facets {
    let (fm, body) = split_frontmatter(text);
    let mut f = Facets::default();

    if let Some(block) = fm {
        let mut pending_list_key: Option<String> = None;
        for line in block.lines() {
            // A YAML list item continuing the previous key.
            if let Some(item) = line.trim_start().strip_prefix("- ") {
                if let Some(k) = &pending_list_key {
                    let v = clean_scalar(item);
                    if k == "tags" && !v.is_empty() {
                        f.tags.push(v.to_lowercase());
                    }
                }
                continue;
            }
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            // Indented keys belong to a nested structure; skipped on purpose.
            if k.starts_with(char::is_whitespace) {
                continue;
            }
            let key = k.trim().to_lowercase();
            let val = clean_scalar(v);
            pending_list_key = val.is_empty().then(|| key.clone());

            if key == "tags" {
                for t in val.trim_matches(['[', ']']).split(',') {
                    let t = clean_scalar(t).to_lowercase();
                    if !t.is_empty() {
                        f.tags.push(t);
                    }
                }
                continue;
            }
            if !val.is_empty() {
                if f.date.is_none() && DATE_KEYS.contains(&key.as_str()) && looks_like_a_date(&val)
                {
                    f.date = Some(val[..10].to_string());
                }
                f.frontmatter.insert(key, val);
            }
        }
    }

    // Headings and inline tags live in prose only. What counts as code —
    // fenced (CommonMark fence rules), indented, or inline — is decided once,
    // in `prose`, so the facets cannot disagree with the link resolver (#24:
    // a second, weaker fence parser here let a `~~~` line close a backtick
    // fence and mis-classified every heading after it).
    for span in crate::prose::prose_spans(body) {
        // A span that opens mid-line (right after an inline code span) is the
        // tail of a line, and a heading marker there is not a heading.
        let mut at_line_start = span.start == 0 || body.as_bytes()[span.start - 1] == b'\n';
        for line in body[span].lines() {
            let t = line.trim_start();
            if at_line_start {
                if let Some(h) = t.strip_prefix('#') {
                    let text = h.trim_start_matches('#').trim();
                    // `#tag` is not a heading: a heading needs whitespace after
                    // the markers. Without this every inline tag becomes a
                    // fake heading.
                    if !text.is_empty() && h.starts_with([' ', '#']) {
                        f.headings.push(text.to_string());
                    }
                }
            }
            at_line_start = true;
            for word in t.split_whitespace() {
                if let Some(tag) = word.strip_prefix('#') {
                    let tag = tag.trim_end_matches([',', '.', ';', ':', ')']);
                    if !tag.is_empty()
                        && tag
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '/')
                    {
                        f.tags.push(tag.to_lowercase());
                    }
                }
            }
        }
    }

    f.tags.sort();
    f.tags.dedup();
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_scalars_tags_headings_and_a_date() {
        let f = extract(
            "---\ntitle: Alpha\ntags: [widget, Gadget]\ndate: 2026-03-01\n---\n\n\
             # Heading One\n\nprose #inline-tag here\n\n## Heading Two\n",
        );
        assert_eq!(
            f.frontmatter.get("title").map(String::as_str),
            Some("Alpha")
        );
        assert_eq!(f.tags, vec!["gadget", "inline-tag", "widget"]);
        assert_eq!(f.headings, vec!["Heading One", "Heading Two"]);
        assert_eq!(f.date.as_deref(), Some("2026-03-01"));
    }

    #[test]
    fn reads_a_yaml_list_of_tags() {
        let f = extract("---\ntags:\n  - alpha\n  - Beta\n---\n\nbody\n");
        assert_eq!(f.tags, vec!["alpha", "beta"]);
    }

    #[test]
    fn an_inline_tag_is_not_a_heading() {
        let f = extract("# Real Heading\n\n#justatag\n");
        assert_eq!(f.headings, vec!["Real Heading"]);
        assert_eq!(f.tags, vec!["justatag"]);
    }

    #[test]
    fn headings_inside_a_fence_are_not_headings() {
        let f = extract("# Real\n\n```sh\n# a shell comment\n```\n\n## Also Real\n");
        assert_eq!(f.headings, vec!["Real", "Also Real"]);
    }

    #[test]
    fn a_tilde_line_inside_a_backtick_fence_does_not_close_it() {
        // #24. The old toggle closed the fence on `~~~`, so `# not a heading`
        // became a heading and `## Heading` (after the real close) was dropped.
        let f = extract("# Real\n\n```\n~~~\n# not a heading\n```\n\n## Heading\n");
        assert_eq!(f.headings, vec!["Real", "Heading"]);
    }

    #[test]
    fn a_heading_marker_inside_a_fence_is_not_a_heading() {
        let f = extract("```md\n# not a heading\n```\n\n~~~\n## nor this\n~~~\n");
        assert!(f.headings.is_empty(), "{:?}", f.headings);
        assert!(f.tags.is_empty(), "{:?}", f.tags);
    }

    #[test]
    fn a_callout_marker_is_neither_a_tag_nor_a_heading() {
        // `> [!warning] #real` — the marker carries no `#`, and the quote
        // does not stop a real inline tag from being read (#46).
        let f = extract("> [!warning] Title #real\n> body [!tip]\n");
        assert_eq!(f.tags, vec!["real"]);
        assert!(f.headings.is_empty(), "{:?}", f.headings);
    }

    #[test]
    fn a_tag_inside_inline_code_is_not_a_tag() {
        let f = extract("prose `#notatag` and #realtag here\n");
        assert_eq!(f.tags, vec!["realtag"]);
        assert!(f.headings.is_empty());
    }

    #[test]
    fn a_heading_marker_after_inline_code_is_not_a_heading() {
        let f = extract("`code` # trailing text\n");
        assert!(f.headings.is_empty(), "{:?}", f.headings);
    }

    #[test]
    fn a_note_without_frontmatter_still_yields_headings() {
        let f = extract("# Only A Heading\n\nbody\n");
        assert!(f.frontmatter.is_empty());
        assert_eq!(f.headings, vec!["Only A Heading"]);
        assert_eq!(f.date, None);
    }

    #[test]
    fn malformed_frontmatter_does_not_eat_the_body() {
        // No closing delimiter: the whole thing is body, and the heading in it
        // is still found. A vault is hand-written; this happens.
        let f = extract("---\ntitle: Broken\n\n# Still A Heading\n");
        assert!(f.frontmatter.is_empty());
        assert_eq!(f.headings, vec!["Still A Heading"]);
    }

    #[test]
    fn a_nested_key_is_skipped_rather_than_flattened() {
        let f = extract("---\ntop: value\nnested:\n  inner: x\n---\n\nbody\n");
        assert_eq!(f.frontmatter.get("top").map(String::as_str), Some("value"));
        assert!(!f.frontmatter.contains_key("inner"));
    }

    #[test]
    fn the_first_recognised_date_key_wins_and_must_look_like_a_date() {
        let f = extract("---\ncreated: not-a-date\nupdated: 2026-07-15\n---\n\nbody\n");
        assert_eq!(f.date.as_deref(), Some("2026-07-15"));
    }
}
