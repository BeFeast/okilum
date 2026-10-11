//! Lossless field extraction for one record, and the role aliases (time,
//! level, message, logger) shared with the indexer.

use std::borrow::Cow;

use super::{block, json, logfmt, text, timestamp, Format, Level};

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

/// A logger with its own vocabulary, recognised by keys only it writes
/// (#1163). Its aliases win over the common ones, and its numeric levels are
/// read on its own scale: journald's `PRIORITY` 3 is syslog's `err`, not a
/// bunyan level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    Common,
    /// `journalctl -o json`
    Journald,
    /// macOS `log show --style ndjson`
    MacOs,
    /// PowerShell `Get-WinEvent | ConvertTo-Json`
    Windows,
}

const DIALECTS: usize = 4;

/// Keys whose presence names the dialect. Matching is exact: these are
/// spellings one tool writes, not conventions.
const SIGNATURES: [(Dialect, &[&str]); 3] = [
    (Dialect::Journald, &["__REALTIME_TIMESTAMP", "__CURSOR"]),
    (Dialect::MacOs, &["eventMessage", "machTimestamp"]),
    (
        Dialect::Windows,
        &["TimeCreated", "LevelDisplayName", "ProviderName"],
    ),
];

/// How a level value reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scale {
    /// Names, and bunyan/pino numbers (`Level::from_text`, `from_number`).
    Common,
    /// syslog severities 0–7, or their names.
    Syslog,
    /// Windows event levels 1–5.
    Windows,
    /// `messageType`: Default, Info, Debug, Error, Fault.
    MacOs,
}

/// Dialect aliases, in priority order per role. Exact spelling.
const DIALECT_ALIASES: [(Dialect, Role, &str, Scale); 15] = [
    (
        Dialect::Journald,
        Role::Time,
        "__REALTIME_TIMESTAMP",
        Scale::Common,
    ),
    (Dialect::Journald, Role::Level, "PRIORITY", Scale::Syslog),
    (Dialect::Journald, Role::Message, "MESSAGE", Scale::Common),
    (
        Dialect::Journald,
        Role::Logger,
        "SYSLOG_IDENTIFIER",
        Scale::Common,
    ),
    (
        Dialect::Journald,
        Role::Logger,
        "_SYSTEMD_UNIT",
        Scale::Common,
    ),
    (Dialect::MacOs, Role::Level, "messageType", Scale::MacOs),
    (Dialect::MacOs, Role::Message, "eventMessage", Scale::Common),
    (Dialect::MacOs, Role::Logger, "subsystem", Scale::Common),
    (Dialect::MacOs, Role::Logger, "category", Scale::Common),
    (
        Dialect::MacOs,
        Role::Logger,
        "processImagePath",
        Scale::Common,
    ),
    (Dialect::Windows, Role::Time, "TimeCreated", Scale::Common),
    (
        Dialect::Windows,
        Role::Level,
        "LevelDisplayName",
        Scale::Common,
    ),
    (Dialect::Windows, Role::Level, "Level", Scale::Windows),
    (Dialect::Windows, Role::Message, "Message", Scale::Common),
    (
        Dialect::Windows,
        Role::Logger,
        "ProviderName",
        Scale::Common,
    ),
];

/// The best candidate per dialect and role, and which dialects the record's
/// keys name. One pass over the fields; the choice is made at the end, when
/// every signature has been seen.
struct Roles<T> {
    best: [[Option<(usize, T)>; 4]; DIALECTS],
    seen: [bool; DIALECTS],
}

impl<T: Copy> Roles<T> {
    fn new() -> Self {
        Roles {
            best: [[None; 4]; DIALECTS],
            seen: [false; DIALECTS],
        }
    }

    /// Offers one field. `keep` gives what to remember for a role, read on
    /// the alias's level scale.
    fn offer(
        &mut self,
        path: &[Cow<'_, str>],
        value: &RawValue<'_>,
        mut keep: impl FnMut(Role, Scale) -> T,
    ) {
        let mut take = |dialect: Dialect, role: Role, rank: usize, scale: Scale| {
            // An empty logger names nothing (macOS writes `"subsystem":""`).
            if role == Role::Logger && value.raw.is_empty() {
                return;
            }
            let slot = &mut self.best[dialect as usize][role as usize];
            if slot.is_none_or(|(best, _)| rank < best) {
                *slot = Some((rank, keep(role, scale)));
            }
        };
        if let Some((role, rank)) = role_of(path) {
            take(Dialect::Common, role, rank, Scale::Common);
        }
        let [key] = path else {
            return;
        };
        for (dialect, keys) in SIGNATURES {
            if keys.contains(&key.as_ref()) {
                self.seen[dialect as usize] = true;
            }
        }
        for (rank, (dialect, role, name, scale)) in DIALECT_ALIASES.iter().enumerate() {
            if *name == key.as_ref() {
                take(*dialect, *role, rank, *scale);
            }
        }
    }

    /// The named dialect's choice, else the common one.
    fn pick(&self, role: Role) -> Option<T> {
        let dialect = (1..DIALECTS).find(|&d| self.seen[d]);
        dialect
            .and_then(|d| self.best[d][role as usize])
            .or(self.best[Dialect::Common as usize][role as usize])
            .map(|(_, kept)| kept)
    }
}

/// The level a value names on `scale`.
fn level_on(value: &RawValue<'_>, scale: Scale) -> Level {
    let text = value.text();
    let text = text.trim();
    match scale {
        Scale::Common => value.level(),
        Scale::Syslog => match text {
            "0" | "1" | "2" => Level::Fatal,
            "3" => Level::Error,
            "4" => Level::Warn,
            "5" | "6" => Level::Info,
            "7" => Level::Debug,
            _ => Level::from_text(text),
        },
        Scale::Windows => match text {
            "1" => Level::Fatal,
            "2" => Level::Error,
            "3" => Level::Warn,
            "4" => Level::Info,
            "5" => Level::Trace,
            _ => Level::Unknown,
        },
        Scale::MacOs => match text {
            "Default" | "Info" => Level::Info,
            "Debug" => Level::Debug,
            "Error" => Level::Error,
            "Fault" => Level::Fatal,
            _ => Level::from_text(text),
        },
    }
}

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
/// open with `{` and logfmt otherwise; `Plain` parses nothing. A text format
/// reads only its own lines: a file is one format, decided up front.
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
        Format::Spdlog => text::spdlog(line, visit),
        Format::Glog => text::glog(line, visit),
        Format::AccessLog => text::access(line, visit),
        Format::Syslog => text::syslog(line, visit),
        Format::Plain => Err(()),
    }
}

/// What the index keeps per line: level and timestamp, without allocating
/// for fields that play no role.
pub(crate) fn classify(line: &[u8], format: Format) -> (Level, Option<i64>) {
    let mut roles = Roles::<(Level, Option<i64>)>::new();
    let parsed = scan(line, format, &mut |path, value| {
        roles.offer(path, &value, |role, scale| match role {
            Role::Level => (level_on(&value, scale), None),
            Role::Time => (Level::Missing, value.timestamp()),
            Role::Message | Role::Logger => (Level::Missing, None),
        })
    });
    match parsed {
        Ok(()) => (
            roles
                .pick(Role::Level)
                .map_or(Level::Missing, |(level, _)| level),
            roles.pick(Role::Time).and_then(|(_, ts)| ts),
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
    /// `None` when the entry is not a record in `format`. Fields come from
    /// the entry's first line; its continuation lines (block.rs) are not
    /// fields.
    pub fn parse(line: &[u8], format: Format) -> Option<Record> {
        let line = block::head(line);
        let mut record = Record::default();
        let mut roles = Roles::<usize>::new();
        scan(line, format, &mut |path, value| {
            let index = record.fields.len();
            roles.offer(path, &value, |_, _| index);
            record.fields.push(Field {
                key: path.join("."),
                value: value.text().into_owned(),
                kind: value.kind,
            });
        })
        .ok()?;
        for role in [Role::Time, Role::Level, Role::Message, Role::Logger] {
            *record.role_mut(role) = roles.pick(role);
        }
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
    fn quill_json_sink_shape() {
        // include/quill/sinks/JsonSink.h writes the time as a string (#1157).
        let (_, ts) = classify(
            br#"{"timestamp":"1791612242497259677","file_name":"main.cpp","line":"42","thread_id":"7","logger":"root","log_level":"INFO","message":"order accepted"}"#,
            Format::JsonLines,
        );
        assert_eq!(ts, Some(1_791_612_242_497_259_677));
    }

    #[test]
    fn dialects_are_named_by_their_own_keys() {
        let at = |line: &str| classify(line.as_bytes(), Format::JsonLines);
        // journald: PRIORITY is syslog, wherever the signature key sits.
        assert_eq!(
            at(r#"{"PRIORITY":"3","MESSAGE":"x","__CURSOR":"s=1"}"#).0,
            Level::Error
        );
        assert_eq!(
            at(r#"{"__REALTIME_TIMESTAMP":"1759276800000000","PRIORITY":"2"}"#),
            (Level::Fatal, Some(1_759_276_800_000_000_000))
        );
        // Without the signature PRIORITY is just a field: no level, and
        // never a bunyan number.
        assert_eq!(at(r#"{"PRIORITY":"3","MESSAGE":"x"}"#).0, Level::Missing);
        // The dialect's alias wins over a common one in the same record.
        assert_eq!(
            at(r#"{"level":50,"PRIORITY":"6","__CURSOR":"s=1"}"#).0,
            Level::Info
        );
        // Windows: the display name first, then the numeric level on the
        // Windows scale; .NET JSON dates.
        assert_eq!(
            at(r#"{"TimeCreated":"/Date(1759276800000)/","Level":2,"ProviderName":"p"}"#),
            (Level::Error, Some(1_759_276_800_000_000_000))
        );
        assert_eq!(
            at(r#"{"Level":2,"LevelDisplayName":"Warning","ProviderName":"p"}"#).0,
            Level::Warn
        );
        assert_eq!(
            at(r#"{"Level":2}"#).0,
            Level::Unknown,
            "a bare 2 is ambiguous"
        );
        // macOS
        assert_eq!(
            at(r#"{"eventMessage":"m","messageType":"Fault"}"#).0,
            Level::Fatal
        );
        assert_eq!(
            at(r#"{"eventMessage":"m","messageType":"Default"}"#).0,
            Level::Info
        );
        let mac = Record::parse(
            br#"{"eventMessage":"m","subsystem":"","category":"","processImagePath":"/usr/libexec/kernel"}"#,
            Format::JsonLines,
        )
        .unwrap();
        assert_eq!(mac.role(Role::Message).unwrap().value, "m");
        assert_eq!(
            mac.role(Role::Logger).unwrap().value,
            "/usr/libexec/kernel",
            "empty subsystem and category name nothing"
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
