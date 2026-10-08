//! Level normalisation. The alias set follows hl's idea of accepting the
//! spellings common loggers emit; the table itself is ours.

/// A record's normalised severity, plus the two non-severities a viewer must
/// keep visible: a record without a level field and a line that is not a
/// record at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    /// The record has a level field whose value is not a known spelling.
    Unknown,
    /// The record parsed but carries no level field.
    Missing,
    /// The line is not a record in the detected format.
    Unparsed,
}

impl Level {
    pub const COUNT: usize = 9;
    pub const ALL: [Level; Self::COUNT] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
        Level::Unknown,
        Level::Missing,
        Level::Unparsed,
    ];

    /// Three-letter row badge.
    pub fn badge(self) -> &'static str {
        match self {
            Level::Trace => "TRC",
            Level::Debug => "DBG",
            Level::Info => "INF",
            Level::Warn => "WRN",
            Level::Error => "ERR",
            Level::Fatal => "FTL",
            Level::Unknown | Level::Missing => "---",
            Level::Unparsed => "RAW",
        }
    }

    /// True for the six real severities.
    pub fn is_severity(self) -> bool {
        self <= Level::Fatal
    }

    /// Case-insensitive; surrounding whitespace is ignored. An unmapped
    /// spelling is `Unknown`, never guessed into a severity.
    pub fn from_text(text: &str) -> Level {
        const TABLE: &[(Level, &[&str])] = &[
            (
                Level::Trace,
                &["trace", "trc", "trac", "verbose", "vrb", "finest", "finer"],
            ),
            (Level::Debug, &["debug", "dbg", "debu", "fine"]),
            (
                Level::Info,
                &["info", "inf", "information", "informational", "notice"],
            ),
            (Level::Warn, &["warn", "warning", "wrn"]),
            (Level::Error, &["error", "err", "erro", "severe"]),
            (
                Level::Fatal,
                &[
                    "fatal",
                    "ftl",
                    "fata",
                    "critical",
                    "crit",
                    "panic",
                    "pani",
                    "emerg",
                    "emergency",
                    "alert",
                ],
            ),
        ];
        let text = text.trim();
        TABLE
            .iter()
            .find(|(_, names)| names.iter().any(|n| n.eq_ignore_ascii_case(text)))
            .map_or(Level::Unknown, |(level, _)| *level)
    }

    /// Bunyan/pino numeric levels. Other numbers are `Unknown`: syslog and
    /// bunyan disagree on direction, so a bare 3 cannot be resolved.
    pub fn from_number(raw: &[u8]) -> Level {
        match raw {
            b"10" => Level::Trace,
            b"20" => Level::Debug,
            b"30" => Level::Info,
            b"40" => Level::Warn,
            b"50" => Level::Error,
            b"60" => Level::Fatal,
            _ => Level::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_normalise_and_unknown_stays_unknown() {
        assert_eq!(Level::from_text("INFO"), Level::Info);
        assert_eq!(Level::from_text(" Warning "), Level::Warn);
        assert_eq!(Level::from_text("ERRO"), Level::Error);
        assert_eq!(Level::from_text("Information"), Level::Info);
        assert_eq!(Level::from_text("crit"), Level::Fatal);
        assert_eq!(Level::from_text("verbose"), Level::Trace);
        assert_eq!(Level::from_text("loud"), Level::Unknown);
        assert_eq!(Level::from_text(""), Level::Unknown);
        assert_eq!(Level::from_number(b"50"), Level::Error);
        assert_eq!(Level::from_number(b"3"), Level::Unknown);
        assert!(Level::Fatal.is_severity());
        assert!(!Level::Missing.is_severity());
        assert_eq!(Level::Unparsed.badge(), "RAW");
    }
}
