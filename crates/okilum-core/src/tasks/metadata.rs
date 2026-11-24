use super::date;
use comrak::nodes::{AstNode, NodeValue};
use regex::Regex;
use std::{collections::BTreeMap, ops::Range, sync::LazyLock};
use time::Date;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    Invalid,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub kind: IssueKind,
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub text: String,
    pub due: Option<Date>,
    pub scheduled: Option<Date>,
    pub start: Option<Date>,
    pub done: Option<Date>,
    pub created: Option<Date>,
    pub priority: Option<u8>,
    pub owner: Option<String>,
    pub thread: Option<String>,
    pub other: BTreeMap<String, String>,
    pub issues: Vec<Issue>,
}

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\[([\w-]+)::([^\]\r\n]*)\]|\(([\w-]+)::([^()\r\n]*)\)|([📅⏳🛫✅➕])\s*([^\s]*)|[🔺⏫🔼🔽⏬]",
    )
    .unwrap()
});

pub(super) fn parse<'a>(item: &'a AstNode<'a>, text: &str, offset: usize, line: usize) -> Metadata {
    let mut meta = Metadata::default();
    let mut markers = MARKER.captures_iter(text).peekable();
    if markers.peek().is_none() {
        meta.text = text.to_owned();
        return meta;
    }

    // The Markdown AST accounts for escaped delimiters, reference links and
    // variable-length code spans. Inspect only the checkbox's opening line.
    let protected: Vec<Range<usize>> = item
        .children()
        .next()
        .filter(|node| matches!(node.data.borrow().value, NodeValue::Paragraph))
        .into_iter()
        .flat_map(|paragraph| paragraph.descendants())
        .filter_map(|node| {
            let data = node.data.borrow();
            let pos = data.sourcepos;
            if matches!(
                data.value,
                NodeValue::Code(_)
                    | NodeValue::Link(_)
                    | NodeValue::WikiLink(_)
                    | NodeValue::Image(_)
            ) && pos.start.line == line
            {
                // Comrak code positions start at the code contents.
                Some(
                    pos.start.column.saturating_sub(1)..if pos.end.line == line {
                        pos.end.column
                    } else {
                        usize::MAX
                    },
                )
            } else {
                None
            }
        })
        .collect();

    let mut seen = Vec::new();
    let mut remaining = 0;
    for cap in markers {
        let marker = cap.get(0).unwrap();
        let before = &text[..marker.start()];
        if protected
            .iter()
            .any(|span| span.contains(&(offset + marker.start())))
            || before.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
        {
            continue;
        }

        let (key, value) = if let Some(key) = cap.get(1).or_else(|| cap.get(3)) {
            (
                key.as_str().to_lowercase(),
                cap.get(2).or_else(|| cap.get(4)).unwrap().as_str().trim(),
            )
        } else if let Some(emoji) = cap.get(5) {
            let key = match emoji.as_str() {
                "📅" => "due",
                "⏳" => "scheduled",
                "🛫" => "start",
                "✅" => "done",
                _ => "created",
            };
            (key.to_owned(), cap.get(6).unwrap().as_str())
        } else {
            let value = match marker.as_str() {
                "🔺" => "0",
                "⏫" => "1",
                "🔼" => "2",
                "🔽" => "4",
                _ => "5",
            };
            ("priority".to_owned(), value)
        };
        let key = if key == "completion" {
            "done".into()
        } else {
            key
        };
        if matches!(
            key.as_str(),
            "checked"
                | "text"
                | "line"
                | "path"
                | "rev"
                | "mtime"
                | "stale"
                | "tasks_open"
                | "backlinks"
        ) {
            continue;
        }

        let kind = if seen.contains(&key) {
            Some(IssueKind::Duplicate)
        } else {
            seen.push(key.clone());
            (!meta.assign(&key, value)).then_some(IssueKind::Invalid)
        };
        if let Some(kind) = kind {
            meta.issues.push(Issue {
                kind,
                key,
                value: value.to_owned(),
            });
        }
        let part = text[remaining..marker.start()].trim();
        if !part.is_empty() {
            if !meta.text.is_empty() {
                meta.text.push(' ');
            }
            meta.text.push_str(part);
        }
        remaining = marker.end();
    }
    let tail = text[remaining..].trim();
    if !tail.is_empty() {
        if !meta.text.is_empty() {
            meta.text.push(' ');
        }
        meta.text.push_str(tail);
    }
    meta
}

impl Metadata {
    fn assign(&mut self, key: &str, value: &str) -> bool {
        let date_slot = match key {
            "due" => &mut self.due,
            "scheduled" => &mut self.scheduled,
            "start" => &mut self.start,
            "done" => &mut self.done,
            "created" => &mut self.created,
            "priority" => {
                self.priority = value.parse::<u8>().ok().filter(|&n| n <= 5);
                return self.priority.is_some();
            }
            "owner" => {
                self.owner = super::super::records::is_slug(value).then(|| value.to_owned());
                return self.owner.is_some();
            }
            "thread" => {
                static THREAD: LazyLock<Regex> = LazyLock::new(|| {
                    Regex::new(r"^t3code://threads/[^/?#\s]+/[^/?#\s]+$").unwrap()
                });
                self.thread = THREAD.is_match(value).then(|| value.to_owned());
                return self.thread.is_some();
            }
            _ => {
                if !value.is_empty() {
                    self.other.insert(key.to_owned(), value.to_owned());
                }
                return true;
            }
        };
        *date_slot = date(value);
        date_slot.is_some()
    }
}
