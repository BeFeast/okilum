//! Lossless field extraction for one record, and the role aliases (time,
//! level, message, logger) shared with the indexer.

use std::borrow::Cow;

use super::{json, logfmt, timestamp, Format, Level};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueKind {
    String,
    Number,
    Bool,
    Null,
    Array,
    Object,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Escape {
    None,
    Json,
    Logfmt,
}

/// A value as it appears in the source line. For strings `raw` is the
/// content between the quotes, still escaped.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawValue<'a> {
    pub kind: ValueKind,
    pub raw: &'a [u8],
    pub escape: Escape,
}

impl<'a> RawValue<'a> {
    /// Display text: strings decoded, everything else exactly as written.
    pub fn text(&self) -> Cow<'a, str> {
        match self.escape {
            Escape::None => String::from_utf8_lossy(self.raw),
            Escape::Json => Cow::Owned(json::unescape(self.raw)),
            Escape::Logfmt => Cow::Owned(logfmt::unescape(self.raw)),
        }
    }

    fn level(&self) -> Level {
        match self.kind {
            ValueKind::String => Level::from_text(&self.text()),
            ValueKind::Number => Level::from_number(self.raw),
            _ => Level::Unknown,
        }
    }

    fn timestamp(&self) -> Option<i64> {
        match self.kind {
            ValueKind::String if self.escape == Escape::None => timestamp::parse_text(self.raw),
            ValueKind::String => timestamp::parse_text(self.text().as_bytes()),
            ValueKind::Number => timestamp::parse_number(self.raw),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Time,
    Level,
    Message,
    Logger,
}

/// Field-name aliases per role, in priority order. Matching is ASCII
/// case-insensitive on the dotted key path. The idea of an alias table comes
/// from hl; the spellings are the ones common loggers emit.
const ALIASES: [(Role, &[&str]); 4] = [
    (
        Role::Time,
        &[
            "ts",
            "time",
            "timestamp",
            "@timestamp",
            "@t",
            "t",
            "datetime",
        ],
    ),
    (
        Role::Level,
        &[
            "level",
            "lvl",
            "severity",
            "loglevel",
            "log_level",
            "levelname",
            "@l",
            "@level",
            "log.level",
        ],
    ),
    (
        Role::Message,
        &["msg", "message", "@message", "@m", "fields.message", "@mt"],
    ),
    (
        Role::Logger,
        &[
            "logger",
            "logger_name",
            "log.logger",
            "target",
            "module",
            "component",
        ],
    ),
];

/// The role and alias rank (lower wins) of a key path, if any.
fn role_of(path: &[Cow<'_, str>]) -> Option<(Role, usize)> {
    ALIASES.iter().find_map(|(role, names)| {
        names
            .iter()
            .position(|name| path_eq(path, name))
            .map(|rank| (*role, rank))
    })
}

fn path_eq(path: &[Cow<'_, str>], dotted: &str) -> bool {
    let mut rest = dotted.as_bytes();
    for (i, segment) in path.iter().enumerate() {
        if i > 0 {
            match rest.split_first() {
                Some((b'.', tail)) => rest = tail,
                _ => return false,
            }
        }
        let segment = segment.as_bytes();
        if rest.len() < segment.len() || !rest[..segment.len()].eq_ignore_ascii_case(segment) {
            return false;
        }
        rest = &rest[segment.len()..];
    }
    rest.is_empty()
}

/// Calls the scanner the format implies. `Mixed` tries JSON for lines that
/// open with `{` and logfmt otherwise; `Plain` parses nothing.
pub(crate) fn scan<'a>(
    line: &'a [u8],
    format: Format,
    visit: &mut dyn FnMut(&[Cow<'a, str>], RawValue<'a>),
) -> Result<(), ()> {
    match format {
        Format::JsonLines => json::scan(line, visit),
        Format::Logfmt => logfmt::scan(line, visit),
        Format::Mixed if line.trim_ascii_start().first() == Some(&b'{') => json::scan(line, visit),
        Format::Mixed => logfmt::scan(line, visit),
        Format::Plain => Err(()),
    }
}

/// What the index keeps per line: level and timestamp, without allocating
/// for fields that play no role.
pub(crate) fn classify(line: &[u8], format: Format) -> (Level, Option<i64>) {
    let mut level: Option<(usize, Level)> = None;
    let mut time: Option<(usize, Option<i64>)> = None;
    let parsed = scan(line, format, &mut |path, value| match role_of(path) {
        Some((Role::Level, rank)) if level.is_none_or(|(best, _)| rank < best) => {
            level = Some((rank, value.level()));
        }
        Some((Role::Time, rank)) if time.is_none_or(|(best, _)| rank < best) => {
            time = Some((rank, value.timestamp()));
        }
        _ => {}
    });
    match parsed {
        Ok(()) => (
            level.map_or(Level::Missing, |(_, level)| level),
            time.and_then(|(_, ts)| ts),
        ),
        Err(()) => (Level::Unparsed, None),
    }
}

/// One field, exactly as the line spells it. Nested JSON objects are
/// flattened to dotted keys; arrays stay raw JSON text. Duplicate keys are
/// kept, in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub value: String,
    pub kind: ValueKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub fields: Vec<Field>,
    /// Indices into `fields` of the field chosen for each role.
    pub time: Option<usize>,
    pub level: Option<usize>,
    pub message: Option<usize>,
    pub logger: Option<usize>,
}

impl Record {
    /// `None` when the line is not a record in `format`.
    pub fn parse(line: &[u8], format: Format) -> Option<Record> {
        let mut record = Record::default();
        let mut ranks = [usize::MAX; 4];
        scan(line, format, &mut |path, value| {
            let index = record.fields.len();
            if let Some((role, rank)) = role_of(path) {
                let slot = role as usize;
                if rank < ranks[slot] {
                    ranks[slot] = rank;
                    *record.role_mut(role) = Some(index);
                }
            }
            record.fields.push(Field {
                key: path.join("."),
                value: value.text().into_owned(),
                kind: value.kind,
            });
        })
        .ok()?;
        Some(record)
    }

    fn role_mut(&mut self, role: Role) -> &mut Option<usize> {
        match role {
            Role::Time => &mut self.time,
            Role::Level => &mut self.level,
            Role::Message => &mut self.message,
            Role::Logger => &mut self.logger,
        }
    }

    pub fn role(&self, role: Role) -> Option<&Field> {
        let index = match role {
            Role::Time => self.time,
            Role::Level => self.level,
            Role::Message => self.message,
            Role::Logger => self.logger,
        };
        index.and_then(|index| self.fields.get(index))
    }

    /// Fields not chosen for a role, in source order.
    pub fn rest(&self) -> impl Iterator<Item = &Field> {
        let roles = [self.time, self.level, self.message, self.logger];
        self.fields
            .iter()
            .enumerate()
            .filter(move |(index, _)| !roles.contains(&Some(*index)))
            .map(|(_, field)| field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_follow_alias_priority_and_case() {
        let record = Record::parse(
            br#"{"Message":"second","time":"2025-10-01T00:00:00Z","ts":"x","msg":"first","Level":"WARN","log":{"logger":"vault.scan"}}"#,
            Format::JsonLines,
        )
        .unwrap();
        assert_eq!(record.role(Role::Message).unwrap().value, "first");
        assert_eq!(
            record.role(Role::Time).unwrap().value,
            "x",
            "ts outranks time"
        );
        assert_eq!(record.role(Role::Level).unwrap().key, "Level");
        assert_eq!(record.role(Role::Logger).unwrap().key, "log.logger");
        let rest: Vec<_> = record.rest().map(|f| f.key.as_str()).collect();
        assert_eq!(rest, ["Message", "time"]);
        // The index takes the same choices: an unparseable `ts` wins over a
        // valid `time`, and is reported as no timestamp rather than guessed.
        let (level, ts) = classify(
            br#"{"time":"2025-10-01T00:00:00Z","ts":"x","Level":"WARN"}"#,
            Format::JsonLines,
        );
        assert_eq!((level, ts), (Level::Warn, None));
    }

    #[test]
    fn tracing_and_pino_shapes() {
        let (level, ts) = classify(
            br#"{"timestamp":"2025-10-01T00:00:00.5Z","level":"INFO","fields":{"message":"hi"},"target":"okilum"}"#,
            Format::JsonLines,
        );
        assert_eq!(level, Level::Info);
        assert_eq!(ts, Some(1_759_276_800_500_000_000));
        let record = Record::parse(
            br#"{"level":50,"time":1759276800000,"msg":"boom"}"#,
            Format::JsonLines,
        )
        .unwrap();
        assert_eq!(record.role(Role::Level).unwrap().value, "50");
        assert_eq!(
            classify(br#"{"level":50,"time":1759276800000}"#, Format::JsonLines),
            (Level::Error, Some(1_759_276_800_000_000_000))
        );
    }

    #[test]
    fn missing_unknown_and_unparsed_are_distinct() {
        assert_eq!(classify(br#"{"a":1}"#, Format::JsonLines).0, Level::Missing);
        assert_eq!(
            classify(br#"{"level":"loud"}"#, Format::JsonLines).0,
            Level::Unknown
        );
        assert_eq!(
            classify(b"plain text", Format::JsonLines).0,
            Level::Unparsed
        );
        assert_eq!(classify(b"level=warn", Format::Plain).0, Level::Unparsed);
        assert_eq!(classify(b"level=warn", Format::Mixed).0, Level::Warn);
        assert_eq!(
            classify(br#"{"level":"warn"}"#, Format::Mixed).0,
            Level::Warn
        );
    }
}
