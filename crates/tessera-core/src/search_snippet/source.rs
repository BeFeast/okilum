//! Display context from the exact stored source, never guessed from a YAML-looking excerpt.
use super::{parse_marked, plain_marked, MatchContext, PlainSnippet};
use crate::properties::{self, PropertyValue};
use std::ops::Range;

pub(crate) fn source_snippet(html: &str, source: &str, start: Option<usize>) -> PlainSnippet {
    let (raw, marks) = parse_marked(html);
    let Some(start) = start.filter(|&start| {
        source
            .get(start..)
            .is_some_and(|tail| tail.starts_with(&raw))
    }) else {
        // Repeated fragments cannot establish which occurrence was selected.
        return plain_marked(&raw, &marks);
    };
    let body = crate::render::without_frontmatter(source);
    let body_start = source.len() - body.len();
    let skip = body_start.saturating_sub(start).min(raw.len());
    let body_marks: Vec<_> = marks
        .iter()
        .filter(|r| r.end > skip)
        .map(|r| r.start.saturating_sub(skip)..r.end - skip)
        .collect();
    let mut snippet = plain_marked(&raw[skip..], &body_marks);
    if skip > 0 {
        let absolute: Vec<_> = marks
            .iter()
            .map(|r| start + r.start..start + r.end)
            .collect();
        snippet.property_match = property_context(source, &absolute);
    }
    if snippet.property_match.is_some() && snippet.highlights.is_empty() {
        // A metadata-only hit does not need an unrelated body preview row.
        snippet.text.clear();
    }
    if !snippet.text.is_empty() {
        if !source[body_start..start + skip].trim().is_empty() {
            snippet.text.insert(0, '…');
            for r in &mut snippet.highlights {
                r.start += '…'.len_utf8();
                r.end += '…'.len_utf8();
            }
        }
        if !source[start + raw.len()..].trim().is_empty() {
            snippet.text.push('…');
        }
    }
    snippet
}

fn property_context(source: &str, marks: &[Range<usize>]) -> Option<MatchContext> {
    let yaml = properties::frontmatter_block(source)?;
    let yaml_start = yaml.as_ptr() as usize - source.as_ptr() as usize;
    let yaml_end = yaml_start + yaml.len();
    if !marks
        .iter()
        .any(|r| r.start < yaml_end && r.end > yaml_start)
    {
        return None;
    }
    let Ok(properties) = properties::parse(yaml) else {
        return Some(MatchContext {
            text: "Properties".into(),
            highlights: std::iter::once(0..10).collect(),
            ..Default::default()
        });
    };
    // Top-level key boundaries associate scalar/list/block values with their
    // parsed property. Indented keys belong to that value, not another property.
    let mut offset = yaml_start;
    let mut keys = Vec::new();
    for line in yaml.split_inclusive('\n') {
        if !line.starts_with([' ', '\t', '#', '-']) {
            if let Some((key, _)) = line.split_once(':') {
                let key = key.trim().trim_matches(['"', '\'']);
                if let Some(property) = properties.iter().find(|p| p.key == key) {
                    keys.push((offset, offset + line.find(':').unwrap(), property));
                }
            }
        }
        offset += line.len();
    }
    let mut result = MatchContext::default();
    for (i, &(from, key_end, property)) in keys.iter().enumerate() {
        let to = keys.get(i + 1).map_or(yaml_end, |(at, _, _)| *at);
        let hits: Vec<_> = marks
            .iter()
            .filter(|r| r.start < to && r.end > from)
            .collect();
        if hits.is_empty() {
            continue;
        }
        if !result.text.is_empty() {
            result.text.push_str(" · ");
        }
        result.text.push_str("Property · ");
        let name_start = result.text.len();
        result.text.push_str(&property_name(&property.key));
        let name_end = result.text.len();
        result.text.push_str(": ");
        let value_start = result.text.len();
        let value = property_value(&property.value);
        result.text.push_str(&value);
        if hits.iter().any(|r| r.start < key_end) && name_start < name_end {
            result.highlights.push(name_start..name_end);
        }
        let value_hits: Vec<_> = hits.iter().filter(|r| r.end > key_end + 1).collect();
        if !value_hits.is_empty() && !value.is_empty() {
            // Keep exact visible terms when possible. A formatted date or a
            // hidden link target stands for its source and marks the whole value.
            let mut literal = Vec::new();
            for r in value_hits {
                let term = source[r.start.max(key_end + 1)..r.end.min(to)].trim();
                if term.is_empty() {
                    continue;
                }
                literal.extend(
                    value
                        .match_indices(term)
                        .map(|(at, _)| value_start + at..value_start + at + term.len()),
                );
            }
            if literal.is_empty() {
                result.highlights.push(value_start..result.text.len());
            } else {
                result.highlights.extend(literal);
            }
        }
    }
    result.highlights.sort_by_key(|r| r.start);
    result.highlights.dedup();
    if result.text.is_empty() {
        // Exotic YAML keys still have an honest explanation, without exposing raw syntax.
        result.text = "Properties".into();
        result.highlights.push(0..result.text.len());
    }
    Some(result)
}

fn property_name(key: &str) -> String {
    let spaced = key.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

fn property_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Text(t) | PropertyValue::Number(t) | PropertyValue::Url(t) => {
            t.split_whitespace().collect::<Vec<_>>().join(" ")
        }
        PropertyValue::Link { label, .. } => label
            .rsplit('/')
            .next()
            .unwrap_or(label)
            .trim_end_matches(".md")
            .to_owned(),
        PropertyValue::Date {
            year,
            month,
            day,
            time,
        } => {
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let mut date = format!(
                "{day} {} {year}",
                MONTHS[usize::from((*month).clamp(1, 12) - 1)]
            );
            if let Some((hour, minute)) = time {
                date.push_str(&format!(", {hour:02}:{minute:02}"));
            }
            date
        }
        PropertyValue::Bool(b) => if *b { "Yes" } else { "No" }.into(),
        PropertyValue::List(items) => items
            .iter()
            .map(property_value)
            .collect::<Vec<_>>()
            .join(", "),
        PropertyValue::Empty => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_properties_do_not_leak_into_prose_and_keep_hebrew_marks() {
        let source =
            "---\ntype: Note\nqa_label: שלום\nupdated: 2026-10-08\n---\n# Title\nBody שלום.";
        let html = source.replace("שלום", "<b>שלום</b>");
        let snippet = source_snippet(&html, source, Some(0));
        assert_eq!(snippet.text, "Title Body שלום.");
        assert_eq!(&snippet.text[snippet.highlights[0].clone()], "שלום");
        let reason = snippet.property_match.unwrap();
        assert_eq!(reason.text, "Property · Qa label: שלום");
        assert_eq!(&reason.text[reason.highlights[0].clone()], "שלום");
        assert!(snippet.hidden_match.is_none());
        let snippet = source_snippet(
            &source.replace("2026-10-08", "<b>2026-10-08</b>"),
            source,
            Some(0),
        );
        let reason = snippet.property_match.unwrap();
        assert_eq!(reason.text, "Property · Updated: 8 Oct 2026");
        assert_eq!(&reason.text[reason.highlights[0].clone()], "8 Oct 2026");
    }

    #[test]
    fn partial_crlf_frontmatter_uses_full_property_and_body_yaml_is_prose() {
        let source =
            "\u{feff}---\r\nqa_label: שלום\r\nrelated:\r\n  - Alpha\r\n  - Beta\r\n---\r\nBody";
        let start = source.find("שלום").unwrap();
        let snippet = source_snippet("<b>שלום</b>", source, Some(start));
        assert!(snippet.text.is_empty());
        assert_eq!(
            snippet.property_match.unwrap().text,
            "Property · Qa label: שלום"
        );
        let start = source.find("Beta").unwrap();
        let reason = source_snippet("<b>Beta</b>", source, Some(start))
            .property_match
            .unwrap();
        assert_eq!(reason.text, "Property · Related: Alpha, Beta");
        assert_eq!(&reason.text[reason.highlights[0].clone()], "Beta");
        let prose = "qa_label: שלום";
        let snippet = source_snippet("qa_label: <b>שלום</b>", prose, Some(0));
        assert_eq!(snippet.text, prose);
        assert!(snippet.property_match.is_none());
    }

    #[test]
    fn cut_markers_and_offsets_preserve_logical_unicode_text() {
        let source = "Start. שלום world. End.";
        let start = source.find("שלום").unwrap();
        let snippet = source_snippet("<b>שלום</b> world", source, Some(start));
        assert_eq!(snippet.text, "…שלום world…");
        assert_eq!(&snippet.text[snippet.highlights[0].clone()], "שלום");
        assert_eq!(
            source_snippet("<b>whole</b>", "whole", Some(0)).text,
            "whole"
        );
        assert!(source_snippet("<b>שלום</b>", source, None)
            .property_match
            .is_none());
    }
}
