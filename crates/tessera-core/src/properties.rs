//! Read-only note properties (#386): the leading YAML frontmatter as an
//! ordered list of typed values for display. Nothing here writes or
//! normalizes the source; values that are not understood stay plain text.
use serde_yaml::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Text(String),
    /// `[[target]]` or `[[target|alias]]`, as written.
    Link {
        target: String,
        label: String,
    },
    Url(String),
    /// ISO date `YYYY-MM-DD`, optionally with `THH:MM` or ` HH:MM`.
    Date {
        year: i32,
        month: u8,
        day: u8,
        time: Option<(u8, u8)>,
    },
    Bool(bool),
    Number(String),
    List(Vec<PropertyValue>),
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub key: String,
    pub value: PropertyValue,
}

impl Property {
    /// `_`-prefixed keys are tool bookkeeping, hidden unless asked for.
    pub fn hidden(&self) -> bool {
        self.key.starts_with('_')
    }
}

/// The YAML text between the leading `---` delimiters, if the note has a
/// terminated frontmatter block (same rule as `render::without_frontmatter`).
pub fn frontmatter_block(source: &str) -> Option<&str> {
    let body = crate::render::without_frontmatter(source);
    if body.len() == source.len() {
        return None;
    }
    let head = &source[..source.len() - body.len()];
    let head = head.strip_prefix('\u{feff}').unwrap_or(head);
    let inner = head
        .strip_prefix("---\r\n")
        .or_else(|| head.strip_prefix("---\n"))?;
    let inner = inner
        .strip_suffix("---\r\n")
        .or_else(|| inner.strip_suffix("---\n"))?;
    Some(inner)
}

/// Parse frontmatter YAML into properties, in written order. Invalid YAML
/// yields `Err` with the parser's message so the caller can say so instead
/// of showing nothing.
pub fn parse(yaml: &str) -> Result<Vec<Property>, String> {
    if yaml.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
    let Value::Mapping(map) = value else {
        return Err("Frontmatter is not a key/value mapping.".into());
    };
    // Numbers are shown as written (`1.10`, `0x1F`), not re-serialized.
    let written: std::collections::HashMap<&str, &str> = yaml
        .lines()
        .filter(|line| !line.starts_with([' ', '\t', '#', '-']))
        .filter_map(|line| line.split_once(':'))
        .map(|(key, raw)| (key.trim().trim_matches(['"', '\'']), raw.trim()))
        .collect();
    Ok(map
        .into_iter()
        .map(|(key, value)| {
            let key = scalar_text(&key);
            let value = match value {
                Value::Number(n) => PropertyValue::Number(
                    written
                        .get(key.as_str())
                        .filter(|raw| !raw.is_empty() && !raw.starts_with('#'))
                        .map(|raw| (*raw).to_owned())
                        .unwrap_or_else(|| n.to_string()),
                ),
                other => convert(other),
            };
            Property { key, value }
        })
        .collect())
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    }
}

fn convert(value: Value) -> PropertyValue {
    match value {
        Value::Null => PropertyValue::Empty,
        Value::Bool(b) => PropertyValue::Bool(b),
        Value::Number(n) => PropertyValue::Number(n.to_string()),
        Value::String(s) => classify(&s),
        Value::Sequence(items) => {
            if let Some(link) = unquoted_link(&items) {
                return link;
            }
            PropertyValue::List(items.into_iter().map(convert).collect())
        }
        Value::Tagged(tagged) => convert(tagged.value),
        mapping @ Value::Mapping(_) => PropertyValue::Text(scalar_text(&mapping)),
    }
}

/// An unquoted `[[x]]` parses as a one-element sequence holding a
/// one-element sequence; read it back as the link the author wrote.
fn unquoted_link(items: &[Value]) -> Option<PropertyValue> {
    let [Value::Sequence(inner)] = items else {
        return None;
    };
    let [Value::String(text)] = inner.as_slice() else {
        return None;
    };
    Some(classify(&format!("[[{text}]]")))
}

fn classify(text: &str) -> PropertyValue {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("[[")
        .and_then(|rest| rest.strip_suffix("]]"))
        .filter(|inner| !inner.contains("[[") && !inner.contains("]]"))
    {
        let (target, label) = match inner.split_once('|') {
            Some((target, alias)) => (target.trim(), alias.trim()),
            None => (inner.trim(), inner.trim()),
        };
        return PropertyValue::Link {
            target: target.to_owned(),
            label: label.to_owned(),
        };
    }
    if (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
        && !trimmed.contains(char::is_whitespace)
    {
        return PropertyValue::Url(trimmed.to_owned());
    }
    if let Some(date) = parse_date(trimmed) {
        return date;
    }
    if trimmed.is_empty() {
        return PropertyValue::Empty;
    }
    PropertyValue::Text(trimmed.to_owned())
}

fn parse_date(text: &str) -> Option<PropertyValue> {
    let bytes = text.as_bytes();
    // Byte slicing below requires an ASCII prefix: `2026-10-0é` must not
    // split a character.
    if bytes.len() < 10 || !bytes[..10].is_ascii() || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i32 = text[0..4].parse().ok()?;
    let month: u8 = text[5..7].parse().ok()?;
    let day: u8 = text[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let rest = &text[10..];
    let time = if rest.is_empty() {
        None
    } else {
        let clock = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
        if clock.len() < 5 || !clock.as_bytes()[..5].is_ascii() || clock.as_bytes()[2] != b':' {
            return None;
        }
        let hour: u8 = clock[0..2].parse().ok()?;
        let minute: u8 = clock[3..5].parse().ok()?;
        (hour < 24 && minute < 60).then_some((hour, minute))
    };
    Some(PropertyValue::Date {
        year,
        month,
        day,
        time,
    })
}

/// Wikilink targets named anywhere in the properties (relations).
pub fn links(properties: &[Property]) -> Vec<(String, String)> {
    fn walk(key: &str, value: &PropertyValue, out: &mut Vec<(String, String)>) {
        match value {
            PropertyValue::Link { target, .. } => out.push((key.to_owned(), target.clone())),
            PropertyValue::List(items) => items.iter().for_each(|v| walk(key, v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for property in properties {
        walk(&property.key, &property.value, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_requires_terminated_leading_frontmatter() {
        assert_eq!(
            frontmatter_block("---\na: 1\n---\n# Body\n"),
            Some("a: 1\n")
        );
        assert_eq!(
            frontmatter_block("\u{feff}---\r\na: 1\r\n---\r\nx"),
            Some("a: 1\r\n")
        );
        assert_eq!(frontmatter_block("---\na: 1\n# never closed\n"), None);
        assert_eq!(frontmatter_block("# No frontmatter\n"), None);
    }

    #[test]
    fn values_are_typed_in_written_order() {
        let yaml = "type: Note\nstatus: active\nupdated: 2026-10-03T18:40\ncreated: 2026-09-28\n\
                    tags: [cafe, launch]\nproject: \"[[Cafe Zerno]]\"\nrelated:\n  - \"[[Suppliers|Beans]]\"\n  - \"[[Menu]]\"\n\
                    source: https://example.com/lease\nowner: [[Anya]]\ndraft: false\npriority: 2\n_organized: true\nempty:\n";
        let props = parse(yaml).unwrap();
        let keys: Vec<&str> = props.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "type",
                "status",
                "updated",
                "created",
                "tags",
                "project",
                "related",
                "source",
                "owner",
                "draft",
                "priority",
                "_organized",
                "empty"
            ]
        );
        assert_eq!(
            props[2].value,
            PropertyValue::Date {
                year: 2026,
                month: 10,
                day: 3,
                time: Some((18, 40))
            }
        );
        assert_eq!(
            props[5].value,
            PropertyValue::Link {
                target: "Cafe Zerno".into(),
                label: "Cafe Zerno".into()
            }
        );
        assert_eq!(
            props[7].value,
            PropertyValue::Url("https://example.com/lease".into())
        );
        assert_eq!(
            props[8].value,
            PropertyValue::Link {
                target: "Anya".into(),
                label: "Anya".into()
            },
            "unquoted wikilink"
        );
        assert!(props[11].hidden() && !props[0].hidden());
        assert_eq!(props[12].value, PropertyValue::Empty);
        assert_eq!(
            links(&props),
            [
                ("project".into(), "Cafe Zerno".into()),
                ("related".into(), "Suppliers".into()),
                ("related".into(), "Menu".into()),
                ("owner".into(), "Anya".into()),
            ]
        );
        // Positive control for the failure path.
        assert!(parse("a: [unclosed").is_err());
        // Non-ASCII near a date shape must not split a character.
        assert_eq!(
            parse("x: 2026-10-0é\ny: 2026-10-03 18:3я\n").unwrap()[0].value,
            PropertyValue::Text("2026-10-0é".into())
        );
        assert_eq!(
            parse("version: 1.10\nid: 0x1F\n").unwrap()[0].value,
            PropertyValue::Number("1.10".into())
        );
    }
}
