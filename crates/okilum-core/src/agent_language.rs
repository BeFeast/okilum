//! Bounded, read-only language diagnostics for the vault's language contract.
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::ops::Range;

pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_DIAGNOSTICS: usize = 2048;
const MAX_RULES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Checked,
    NotApplicable,
    Unknown(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bucket {
    Gate,
    Backlog,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// UTF-8 byte offsets into the unchanged source, suitable for selection/hover.
    pub range: Range<usize>,
    pub line: usize,
    pub bucket: Bucket,
    pub replacement: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub status: Status,
    pub diagnostics: Vec<Diagnostic>,
}
impl Report {
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: Status::Unknown(reason.into()),
            diagnostics: Vec::new(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schema {
    #[serde(default)]
    gate: Option<Vec<Entry>>,
    #[serde(default)]
    backlog: Option<Vec<Entry>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    pattern: String,
    good: String,
    #[serde(default, rename = "note")]
    _note: Option<String>,
}

/// No filesystem, processes, Python, providers or network are involved. Unknown
/// contracts fail visibly, never as a clean bill of health.
pub fn diagnose(path: &str, source: &str, schema: Option<&str>) -> Report {
    if !crate::agent_imports::is_agent_document(path) {
        return Report {
            status: Status::NotApplicable,
            diagnostics: Vec::new(),
        };
    }
    let Some(schema) = schema else {
        return Report::unknown(
            "Missing language policy (@Schema/language.yaml or _Schema/language.yaml)",
        );
    };
    if schema.len() > MAX_SCHEMA_BYTES || source.len() > MAX_SOURCE_BYTES {
        return Report::unknown("Language check input exceeds local size limit");
    }
    let contract: Schema = match serde_yaml::from_str(schema) {
        Ok(value) => value,
        Err(error) => return Report::unknown(format!("Unsupported language contract: {error}")),
    };
    if contract.gate.is_none() && contract.backlog.is_none() {
        return Report::unknown("Language contract has no recognized rule buckets");
    }
    let mut rules = Vec::new();
    for (bucket, entries) in [
        (Bucket::Gate, contract.gate),
        (Bucket::Backlog, contract.backlog),
    ] {
        for entry in entries.unwrap_or_default() {
            if rules.len() == MAX_RULES
                || entry.pattern.len() > 1024
                || entry.good.trim().is_empty()
            {
                return Report::unknown("Unsupported language rule or rule limit exceeded");
            }
            // Explicitly bounded regex compilation; unsupported Python constructs
            // are surfaced rather than silently changing their meaning.
            let regex = match RegexBuilder::new(&entry.pattern)
                .case_insensitive(true)
                .size_limit(256 * 1024)
                .dfa_size_limit(256 * 1024)
                .build()
            {
                Ok(regex) if !regex.is_match("") => regex,
                _ => return Report::unknown("Unsupported language rule regex"),
            };
            rules.push((bucket.clone(), regex, entry.good));
        }
    }
    let prose = prose(source);
    let mut diagnostics = Vec::new();
    let mut offset = 0;
    // Python str.splitlines() recognizes these separators, with CRLF as one.
    let line_breaks = Regex::new(r"\r\n|[\n\r\x0b\x0c\x1c-\x1e\x{85}\x{2028}\x{2029}]")
        .expect("constant line separator regex");
    let ends = line_breaks
        .find_iter(&prose)
        .map(|m| (m.start(), m.end()))
        .chain(std::iter::once((prose.len(), prose.len())));
    for (line_index, (end, next)) in ends.enumerate() {
        let line = &prose[offset..end];
        for (bucket, regex, replacement) in &rules {
            for hit in regex.find_iter(line) {
                if diagnostics.len() == MAX_DIAGNOSTICS {
                    return Report {
                        status: Status::Unknown(
                            "Language diagnostic limit exceeded; partial results".into(),
                        ),
                        diagnostics,
                    };
                }
                diagnostics.push(Diagnostic {
                    range: offset + hit.start()..offset + hit.end(),
                    line: line_index + 1,
                    bucket: bucket.clone(),
                    replacement: replacement.clone(),
                });
            }
        }
        offset = next;
    }
    diagnostics.sort_by_key(|hit| (hit.range.start, hit.range.end));
    Report {
        status: Status::Checked,
        diagnostics,
    }
}

/// Same ordered exclusions as validate-language.py, retaining original byte
/// offsets as well as newlines. In particular this deliberately does not broaden
/// the reference validator's backtick fence syntax to other Markdown dialects.
fn prose(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for pattern in [
        r"(?ms)^```.*?^```",
        r"`[^`\n]*`",
        r"https?://\S+",
        r"\]\([^)]*\)",
        r"\[\[([^\]|]*)",
    ] {
        let text = std::str::from_utf8(&bytes).expect("mask preserves UTF-8");
        let regex = Regex::new(pattern).expect("constant exclusion regex");
        let ranges: Vec<_> = regex
            .captures_iter(text)
            .map(|hit| {
                let matched = hit
                    .get(if pattern.starts_with(r"\[\[") { 1 } else { 0 })
                    .unwrap();
                matched.start()..matched.end()
            })
            .collect();
        for range in ranges {
            for byte in &mut bytes[range] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        }
    }
    String::from_utf8(bytes).expect("mask preserves UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    const POLICY: &str = "gate:\n  - pattern: '\\bгоч'\n    good: gotcha\nbacklog:\n  - pattern: 'воркер'\n    good: worker\n";
    #[test]
    fn reference_exclusions_ranges_and_visible_unknown() {
        let source = "многочисленный ГОЧа воркер\r\n`воркер` https://x/воркер [воркер](воркер) [[воркер|воркер]]\n```rust\nворкер\n```\nворкер";
        let report = diagnose("nested/AGENTS.md", source, Some(POLICY));
        assert_eq!(report.status, Status::Checked);
        let hits: Vec<_> = report
            .diagnostics
            .iter()
            .map(|d| (&source[d.range.clone()], d.line))
            .collect();
        assert_eq!(
            hits,
            [
                ("ГОЧ", 1),
                ("воркер", 1),
                ("воркер", 2),
                ("воркер", 2),
                ("воркер", 6)
            ]
        );
        assert_eq!(report.diagnostics[0].bucket, Bucket::Gate);
        assert_eq!(report.diagnostics[1].replacement, "worker");
        assert!(matches!(
            diagnose("AGENTS.md", "воркер", None).status,
            Status::Unknown(_)
        ));
        for policy in [
            "language: ru",
            "gate: [{pattern: '(?=x)', good: x}]",
            "gate: [{pattern: '', good: x}]",
        ] {
            assert!(matches!(
                diagnose("SKILL.md", "x", Some(policy)).status,
                Status::Unknown(_)
            ));
        }
        assert_eq!(
            diagnose("note.md", source, None).status,
            Status::NotApplicable
        );
    }
    #[test]
    fn diagnostics_are_bounded_and_refresh_uses_current_source() {
        let source = "воркер ".repeat(MAX_DIAGNOSTICS + 1);
        let report = diagnose("AGENTS.md", &source, Some(POLICY));
        assert_eq!(report.diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(matches!(report.status, Status::Unknown(_)));
        assert!(diagnose("AGENTS.md", "worker", Some(POLICY))
            .diagnostics
            .is_empty());
    }
}
