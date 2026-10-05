//! Obsidian's existing-file fallback keeps CommonMark syntax authoritative.
use super::*;
use std::{borrow::Cow, path::Path};

/// Translate an OS path inside this vault into its portable root-relative form.
/// Keep authored vault-root paths working when no OS file occupies that path.
pub fn markdown_path<'a>(vault: &Vault, path: &'a str) -> Option<Cow<'a, str>> {
    let input = Path::new(path);
    if !input.is_absolute() {
        return Some(Cow::Borrowed(path));
    }
    if let Some(root) = &vault.graph_root {
        for root in [root, &vault.root] {
            if let Ok(relative) = input.strip_prefix(root) {
                if !vault.graph_alias_path(relative) {
                    return Some(Cow::Owned(format!(
                        "/{}",
                        crate::vault::note_path(relative)
                    )));
                }
            }
        }
        // A path outside the known roots, or crossing an observed alias, can be an OS file, an
        // outside alias into the vault, or authored vault-root syntax. Verify
        // that ambiguity once per distinct destination, never per occurrence.
        let mut paths = vault.graph_os_paths.lock().unwrap();
        return paths
            .entry(path.to_owned())
            .or_insert_with(|| match input.canonicalize() {
                Ok(real) => real
                    .strip_prefix(root)
                    .ok()
                    .map(|relative| format!("/{}", crate::vault::note_path(relative))),
                Err(_) => Some(path.to_owned()),
            })
            .clone()
            .map(Cow::Owned);
    }
    if !vault.inventory_complete {
        // Cached identities already carry a canonical root. OS paths outside
        // it need background verification before choosing outside/root meaning.
        return input
            .strip_prefix(&vault.root)
            .ok()
            .map(|relative| Cow::Owned(format!("/{}", relative.to_string_lossy())));
    }
    let root = vault
        .root
        .canonicalize()
        .unwrap_or_else(|_| vault.root.clone());
    if let Ok(real) = input.canonicalize() {
        if let Ok(relative) = real.strip_prefix(&root) {
            return Some(Cow::Owned(format!("/{}", relative.to_string_lossy())));
        }
        return None;
    }
    for root in [&root, &vault.root] {
        if let Ok(relative) = input.strip_prefix(root) {
            return Some(Cow::Owned(format!("/{}", relative.to_string_lossy())));
        }
    }
    Some(Cow::Borrowed(path))
}

/// Parsed fallback and exact destination range in the untouched source.
pub(crate) fn fallback_links(
    source: &str,
    vault: &Vault,
    from: &str,
) -> Vec<(ParsedLink, Range<usize>)> {
    fallback_after_strict(source, vault, from, &parse(source))
}

fn fallback_after_strict(
    source: &str,
    vault: &Vault,
    from: &str,
    strict: &[ParsedLink],
) -> Vec<(ParsedLink, Range<usize>)> {
    if !source.contains("](") || !source.contains(' ') {
        return Vec::new();
    }
    let prose = crate::prose::prose_spans(source);
    let bytes = source.as_bytes();
    let mut candidates = Vec::new();
    let mut masked = bytes.to_vec();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'[' || (at > 0 && matches!(bytes[at - 1], b'!' | b'[')) {
            at += 1;
            continue;
        }
        let start = at;
        let mut depth = 1;
        at += 1;
        while at < bytes.len() && depth > 0 && !matches!(bytes[at], b'\n' | b'\r') {
            match bytes[at] {
                b'\\' => {
                    at += 2;
                    continue;
                }
                b'[' => depth += 1,
                b']' => depth -= 1,
                _ => {}
            }
            at += 1;
        }
        if bytes.get(at) != Some(&b'(') || depth != 0 {
            continue;
        }
        at += 1;
        let target_start = at;
        depth = 0;
        while at < bytes.len() && !matches!(bytes[at], b'\n' | b'\r') {
            match bytes[at] {
                b'\\' => {
                    at += 2;
                    continue;
                }
                b'(' => depth += 1,
                b')' if depth == 0 => break,
                b')' => depth -= 1,
                _ => {}
            }
            at += 1;
        }
        if bytes.get(at) != Some(&b')') {
            continue;
        }
        let end = at + 1;
        let range = target_start..at;
        at = end;
        if !prose
            .iter()
            .any(|span| span.start <= start && end <= span.end)
        {
            continue;
        }
        if strict
            .iter()
            .any(|link| link.range.start < end && link.range.end > start)
        {
            continue;
        }
        let target = &source[range.clone()];
        if !target.contains(' ') || target.trim() != target || target.starts_with('<') {
            continue;
        }
        let resolved = resolve(target, false, vault, from);
        if !matches!(resolved.status, "resolved" | "attachment" | "outside_file") {
            continue;
        }
        // A second strict parse proves the candidate is a link in this context:
        // fenced/inline code, HTML, escapes and image labels remain excluded.
        masked[range.clone()].fill(b'x');
        candidates.push((start..end, range));
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    let masked = String::from_utf8(masked).expect("whole destinations replaced with ASCII");
    parse(&masked)
        .into_iter()
        .filter_map(|mut link| {
            let (_, range) = candidates.iter().find(|(whole, _)| *whole == link.range)?;
            link.target = source[range.clone()].into();
            Some((link, range.clone()))
        })
        .collect()
}

pub fn parse_in_vault(source: &str, vault: &Vault, from: &str) -> Vec<ParsedLink> {
    let mut links = parse(source);
    let fallback = fallback_after_strict(source, vault, from, &links);
    links.extend(fallback.into_iter().map(|(link, _)| link));
    links.sort_by_key(|link| link.range.start);
    links
}
