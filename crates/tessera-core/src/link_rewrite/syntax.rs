//! Source ranges of link destinations, never ranges in rendered Markdown.
use crate::{document_links, render};
use comrak::{nodes::NodeValue, parse_document, Arena};
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Target {
    pub range: Range<usize>,
    pub text: String,
    pub wiki: bool,
    pub embedded: bool,
    pub reason: Option<String>,
}

pub fn targets_in_vault(source: &str, vault: &crate::Vault, from: &str) -> Vec<Target> {
    let mut out = targets(source);
    for (link, range) in document_links::fallback_links(source, vault, from) {
        out.push(Target {
            range,
            text: link.target,
            wiki: false,
            embedded: false,
            reason: None,
        });
    }
    out.sort_by_key(|target| target.range.start);
    out
}

pub fn targets(source: &str) -> Vec<Target> {
    let body = render::without_frontmatter(source);
    let offset = source.len() - body.len();
    let mut out = Vec::new();
    // YAML is never reserialized. A sentinel parse proves each lexical match is
    // inside a scalar value, not a comment, mapping key or YAML collection syntax.
    if let Some(yaml) = crate::properties::frontmatter_block(source) {
        let yaml_offset = yaml.as_ptr() as usize - source.as_ptr() as usize;
        let re = regex::Regex::new(r"\[\[([^\[\]\r\n]+)\]\]").unwrap();
        for found in re.find_iter(yaml) {
            if let Some(range) = wiki_range(&yaml[found.range()], yaml_offset + found.start()) {
                let sentinel = format!("tessera_rewrite_{}", uuid::Uuid::new_v4().simple());
                let mut candidate = yaml.to_owned();
                candidate.replace_range(
                    range.start - yaml_offset..range.end - yaml_offset,
                    &sentinel,
                );
                if serde_yaml::from_str::<serde_yaml::Value>(&candidate)
                    .ok()
                    .is_some_and(|v| scalar_contains(&v, &sentinel))
                {
                    out.push(Target {
                        text: source[range.clone()].into(),
                        range,
                        wiki: true,
                        embedded: false,
                        reason: None,
                    });
                }
            }
        }
    }
    let arena = Arena::new();
    // Comrak treats Obsidian ![[embeds]] as plain text. Mask only the bang
    // with a non-whitespace ASCII byte: offsets and indentation stay identical,
    // while Comrak still excludes code, HTML and escaped opening brackets.
    let mut masked = document_links::wiki_parse_source(body).into_bytes();
    for (at, _) in body.match_indices("![[") {
        if body.as_bytes()[..at]
            .iter()
            .rev()
            .take_while(|b| **b == b'\\')
            .count()
            % 2
            == 0
        {
            masked[at] = b'x';
        }
    }
    let masked = String::from_utf8(masked).expect("only ASCII bang bytes replaced");
    let root = parse_document(&arena, &masked, &render::comrak_options());
    let lines: Vec<_> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    for node in root.descendants() {
        let data = node.data.borrow();
        let (wiki, decoded) = match &data.value {
            NodeValue::WikiLink(link) => (true, link.url.as_str()),
            NodeValue::Link(link) | NodeValue::Image(link) => (false, link.url.as_str()),
            _ => continue,
        };
        if node.ancestors().skip(1).any(|n| {
            matches!(
                n.data.borrow().value,
                NodeValue::Image(_) | NodeValue::Link(_)
            )
        }) {
            continue;
        }
        let p = data.sourcepos;
        let Some(start) = p
            .start
            .line
            .checked_sub(1)
            .and_then(|i| lines.get(i))
            .and_then(|i| p.start.column.checked_sub(1).map(|c| i + c))
        else {
            continue;
        };
        let Some(end) = p
            .end
            .line
            .checked_sub(1)
            .and_then(|i| lines.get(i))
            .map(|i| i + p.end.column)
        else {
            continue;
        };
        let Some(raw) = body.get(start..end) else {
            continue;
        };
        let embedded = matches!(data.value, NodeValue::Image(_))
            || (wiki && start > 0 && body.as_bytes()[start - 1] == b'!');
        let range = if wiki {
            wiki_range(raw, offset + start)
        } else {
            markdown_range(raw).map(|r| r.start + offset + start..r.end + offset + start)
        };
        match range {
            Some(range) => {
                let text = source[range.clone()].to_owned();
                let reason = (!wiki && text != decoded).then(|| {
                    "Escaped or entity-encoded Markdown target requires manual update".into()
                });
                out.push(Target {
                    range,
                    text,
                    wiki,
                    embedded,
                    reason,
                });
            }
            None => out.push(Target {
                range: offset + start..offset + end,
                text: decoded.into(),
                wiki,
                embedded,
                reason: Some(
                    "Reference-style or unsupported destination syntax requires manual update"
                        .into(),
                ),
            }),
        }
    }
    out.sort_by_key(|t| t.range.start);
    out.dedup_by(|a, b| a.range == b.range);
    out
}

/// A target inside YAML must still decode to exactly the intended scalar after
/// replacement. In particular, a quote/backslash in a filename must not escape
/// its enclosing YAML string or alter other properties.
pub fn validate_frontmatter_replacement(
    source: &str,
    range: Range<usize>,
    replacement: &str,
) -> anyhow::Result<()> {
    let Some(yaml) = crate::properties::frontmatter_block(source) else {
        return Ok(());
    };
    let offset = yaml.as_ptr() as usize - source.as_ptr() as usize;
    if range.start < offset || range.end > offset + yaml.len() {
        return Ok(());
    }
    let local = range.start - offset..range.end - offset;
    let sentinel = format!("tessera_yaml_{}", uuid::Uuid::new_v4().simple());
    let mut masked = yaml.to_owned();
    masked.replace_range(local.clone(), &sentinel);
    let marked: serde_yaml::Value = serde_yaml::from_str(&masked)?;
    let mut expected_before = marked.clone();
    replace_scalar(&mut expected_before, &sentinel, &source[range]);
    anyhow::ensure!(
        expected_before == serde_yaml::from_str::<serde_yaml::Value>(yaml)?,
        "YAML target uses unsupported escaping"
    );
    let mut expected_after = marked;
    replace_scalar(&mut expected_after, &sentinel, replacement);
    let mut proposed = yaml.to_owned();
    proposed.replace_range(local, replacement);
    anyhow::ensure!(
        serde_yaml::from_str::<serde_yaml::Value>(&proposed).ok() == Some(expected_after),
        "Destination requires YAML escaping; update this relationship manually"
    );
    Ok(())
}
fn replace_scalar(value: &mut serde_yaml::Value, needle: &str, replacement: &str) {
    match value {
        serde_yaml::Value::String(s) => *s = s.replace(needle, replacement),
        serde_yaml::Value::Sequence(v) => v
            .iter_mut()
            .for_each(|v| replace_scalar(v, needle, replacement)),
        serde_yaml::Value::Mapping(v) => v
            .values_mut()
            .for_each(|v| replace_scalar(v, needle, replacement)),
        serde_yaml::Value::Tagged(t) => replace_scalar(&mut t.value, needle, replacement),
        _ => {}
    }
}

fn scalar_contains(value: &serde_yaml::Value, needle: &str) -> bool {
    match value {
        serde_yaml::Value::String(s) => s
            .find(needle)
            .is_some_and(|at| s[..at].trim_end().ends_with("[[")),
        serde_yaml::Value::Sequence(v) => v.iter().any(|v| scalar_contains(v, needle)),
        serde_yaml::Value::Mapping(v) => v.values().any(|v| scalar_contains(v, needle)),
        serde_yaml::Value::Tagged(t) => scalar_contains(&t.value, needle),
        _ => false,
    }
}
fn wiki_range(raw: &str, base: usize) -> Option<Range<usize>> {
    let inner = raw.strip_prefix("[[")?.strip_suffix("]]")?;
    let target = inner
        .split('|')
        .next()?
        .strip_suffix('\\')
        .unwrap_or(inner.split('|').next()?);
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = base + 2 + (target.len() - target.trim_start().len());
    Some(start..start + trimmed.len())
}
fn markdown_range(raw: &str) -> Option<Range<usize>> {
    let bytes = raw.as_bytes();
    let mut i = usize::from(raw.starts_with('!'));
    if bytes.get(i) != Some(&b'[') {
        return None;
    }
    let mut depth = 1;
    i += 1;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    if bytes.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    if bytes.get(i) == Some(&b'<') {
        i += 1;
        let start = i;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'>' {
                return Some(start..i);
            }
            i += 1;
        }
        return None;
    }
    let start = i;
    depth = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(start..i),
            b')' => depth -= 1,
            c if c.is_ascii_whitespace() && depth == 0 => return Some(start..i),
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_embed_block_target_is_an_exact_source_range() {
        let source = "е́ ![[Заметка 🧠#^block|🧠]]\r\n";
        let targets = targets(source);
        assert_eq!(targets.len(), 1, "{targets:?}");
        assert_eq!(targets[0].text, "Заметка 🧠#^block");
        assert!(targets[0].reason.is_none(), "{targets:?}");
        assert_eq!(&source[targets[0].range.clone()], "Заметка 🧠#^block");
    }
    #[test]
    fn code_html_and_image_labels_are_never_rewritten() {
        for source in [
            "`![[note]]`",
            "```md\n![[note]]\n```",
            "    ![[note]]",
            "<!-- ![[note]] -->",
            "\\[[note]]",
            "![![[note]]](https://example.com)",
        ] {
            assert!(
                targets(source).iter().all(|t| !t.wiki),
                "{source}: {:?}",
                targets(source)
            );
        }
        for source in [
            "\\![[note]]",
            "   ![[note|alias]]",
            "> ![[note|alias]]",
            "- ![[note|alias]]",
            "[[ note \\| alias ]]",
            "| heading |\n| --- |\n| ![[note\\|alias]] |",
        ] {
            let links = targets(source);
            assert_eq!(links.len(), 1, "{source}: {links:?}");
            assert_eq!(links[0].text, "note");
            assert!(links[0].reason.is_none());
        }
    }
    #[test]
    fn bom_and_yaml_scalars_preserve_authored_ranges() {
        let source = "\u{feff}[[note|🧠]]";
        let links = targets(source);
        assert_eq!(links.len(), 1);
        assert_eq!(&source[links[0].range.clone()], "note");
        let source =
            "---\nrelated: \"[[ note | alias ]]\"\narray: [[note]]\n# comment [[note]]\n---\n";
        let links = targets(source);
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(links[0].text, "note");
    }
}
