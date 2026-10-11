//! Saved log queries (#1169): a query with its level and time filter under a
//! name, kept globally so that it applies to any log.
//!
//! What is saved is what the filter bar holds apart from where it was set: the
//! query text, the free text, the lowest level and the choice about lines
//! without a level, the time preset and the field chips. An absolute time
//! range (the histogram brush, #1168) is not saved: it names a stretch of one
//! log and means nothing for another, so the saved filter keeps `All time`.
//!
//! The stored form is plain JSON, readable and editable by hand. A reader
//! never fails on what it does not know: an unknown time preset or level name
//! falls back to the unrestricted choice for that part, and a query that does
//! not parse stays in the list, flagged when applied, so a saved entry is never
//! lost to a build that understands less than the one that wrote it.

use serde::{Deserialize, Serialize};

use super::{FieldChip, Level, LevelFilter, LogFilter, Query, QueryError, TimeWindow};

/// Longest name kept, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// Most saved queries kept.
pub const MAX_QUERIES: usize = 200;

const LEVELS: [(&str, Level); 6] = [
    ("trace", Level::Trace),
    ("debug", Level::Debug),
    ("info", Level::Info),
    ("warn", Level::Warn),
    ("error", Level::Error),
    ("fatal", Level::Fatal),
];

/// The time presets that mean the same for every log.
const PRESETS: [(&str, TimeWindow); 4] = [
    ("all", TimeWindow::All),
    ("5m", TimeWindow::Last5Minutes),
    ("1h", TimeWindow::LastHour),
    ("24h", TimeWindow::Last24Hours),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedQuery {
    pub name: String,
    /// The query language text (`level>=warn and status>=500`), as it parses.
    pub query: String,
    /// The free-text filter.
    pub text: String,
    /// Lowest level shown (`trace` .. `fatal`); empty for every severity.
    pub min_level: String,
    pub without_level: bool,
    /// `all`, `5m`, `1h` or `24h`.
    pub time: String,
    /// `key`, `value` pairs of the field chips.
    pub chips: Vec<(String, String)>,
}

impl Default for SavedQuery {
    fn default() -> Self {
        Self {
            name: String::new(),
            query: String::new(),
            text: String::new(),
            min_level: String::new(),
            without_level: true,
            time: "all".into(),
            chips: Vec::new(),
        }
    }
}

/// Why a name cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NameError::Empty => f.write_str("Give the query a name"),
            NameError::TooLong => write!(f, "A name is at most {MAX_NAME_CHARS} characters"),
        }
    }
}

/// The name as kept: surrounding space and inner runs of whitespace cut to one space.
pub fn clean_name(name: &str) -> Result<String, NameError> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        Err(NameError::Empty)
    } else if name.chars().count() > MAX_NAME_CHARS {
        Err(NameError::TooLong)
    } else {
        Ok(name)
    }
}

impl SavedQuery {
    /// What `filter` holds, under `name`. The query is saved in its canonical
    /// spelling, which parses back to the same query. A time range is saved as `all`.
    pub fn capture(name: &str, filter: &LogFilter) -> Result<Self, NameError> {
        Ok(Self {
            name: clean_name(name)?,
            query: filter
                .query
                .as_ref()
                .map(|query| query.to_string())
                .unwrap_or_default(),
            text: filter.text.trim().to_owned(),
            min_level: filter
                .level
                .min
                .and_then(|min| LEVELS.iter().find(|(_, level)| *level == min))
                .map(|(name, _)| (*name).to_owned())
                .unwrap_or_default(),
            without_level: filter.level.without_level,
            time: PRESETS
                .iter()
                .find(|(_, window)| *window == filter.time)
                .map_or("all", |(key, _)| key)
                .to_owned(),
            chips: filter
                .chips
                .iter()
                .map(|chip| (chip.key.clone(), chip.value.clone()))
                .collect(),
        })
    }

    /// The filter this saved query stands for. `Err` when its query text does
    /// not parse (a hand edit, or a language this build does not know).
    pub fn to_filter(&self) -> Result<LogFilter, QueryError> {
        let query = Query::parse(&self.query)?;
        Ok(LogFilter {
            level: LevelFilter {
                min: LEVELS
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(self.min_level.trim()))
                    .map(|(_, level)| *level),
                without_level: self.without_level,
            },
            text: self.text.clone(),
            time: PRESETS
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(self.time.trim()))
                .map_or(TimeWindow::All, |(_, window)| *window),
            chips: self
                .chips
                .iter()
                .map(|(key, value)| FieldChip {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
            query: (!query.is_empty()).then_some(query),
            window_end: None,
        })
    }
}

/// The list of saved queries: names are unique ignoring case, saving under an
/// existing name replaces that entry in place.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedQueries {
    queries: Vec<SavedQuery>,
}

impl SavedQueries {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.queries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }

    /// In name order, ignoring case: the order the filter bar lists them in.
    pub fn sorted(&self) -> Vec<&SavedQuery> {
        let mut all: Vec<_> = self.queries.iter().collect();
        all.sort_by_cached_key(|query| query.name.to_lowercase());
        all
    }

    pub fn get(&self, name: &str) -> Option<&SavedQuery> {
        self.queries
            .iter()
            .find(|query| same_name(&query.name, name))
    }

    /// Saves `query` under its cleaned name, replacing the one of the same name
    /// (ignoring case and spacing). `false` when the name cannot be used, or the
    /// list is full and the name is a new one.
    pub fn upsert(&mut self, mut query: SavedQuery) -> bool {
        let Ok(name) = clean_name(&query.name) else {
            return false;
        };
        query.name = name;
        let full = self.queries.len() >= MAX_QUERIES;
        match self
            .queries
            .iter_mut()
            .find(|known| same_name(&known.name, &query.name))
        {
            Some(known) => *known = query,
            None if full => return false,
            None => self.queries.push(query),
        }
        true
    }

    pub fn remove(&mut self, name: &str) -> Option<SavedQuery> {
        let at = self
            .queries
            .iter()
            .position(|query| same_name(&query.name, name))?;
        Some(self.queries.remove(at))
    }

    /// The JSON text of the list.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("saved queries are plain data")
    }

    /// Reads a list; an entry the reader cannot use is skipped, the rest kept.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let raw: serde_json::Value = serde_json::from_str(text)?;
        let mut queries = SavedQueries::new();
        let entries = raw
            .get("queries")
            .and_then(|queries| queries.as_array())
            .cloned()
            .unwrap_or_default();
        for entry in entries {
            if let Ok(query) = serde_json::from_value::<SavedQuery>(entry) {
                queries.upsert(query);
            }
        }
        Ok(queries)
    }
}

fn same_name(a: &str, b: &str) -> bool {
    let fold = |name: &str| {
        name.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    fold(a) == fold(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> LogFilter {
        LogFilter {
            level: LevelFilter {
                min: Some(Level::Warn),
                without_level: false,
            },
            text: "  timeout ".into(),
            time: TimeWindow::LastHour,
            chips: vec![FieldChip {
                key: "user.id".into(),
                value: "7".into(),
            }],
            query: Some(Query::parse("level>=warn and status>=500").unwrap()),
            window_end: Some(5),
        }
    }

    #[test]
    fn a_time_range_is_not_saved() {
        // A brushed range (#1168) names a stretch of one log, not something
        // to apply to another: the query is saved, the range is not.
        let ranged = LogFilter {
            time: TimeWindow::range(10, 20),
            ..filter()
        };
        let saved = SavedQuery::capture("slow", &ranged).unwrap();
        assert_eq!(saved.time, "all");
        assert!(!saved.query.is_empty(), "the query is kept");
        assert_eq!(saved.to_filter().unwrap().time, TimeWindow::All);
    }

    #[test]
    fn a_filter_is_saved_and_restored_part_by_part() {
        let saved = SavedQuery::capture("  Slow   5xx ", &filter()).unwrap();
        assert_eq!(saved.name, "Slow 5xx");
        assert_eq!(saved.text, "timeout");
        assert_eq!(saved.min_level, "warn");
        assert_eq!(saved.time, "1h");
        assert_eq!(saved.chips, [("user.id".to_owned(), "7".to_owned())]);
        let restored = saved.to_filter().unwrap();
        // Everything the user chose; `window_end` is set per run, never saved.
        assert_eq!(
            restored,
            LogFilter {
                window_end: None,
                text: "timeout".into(),
                ..filter()
            }
        );
    }

    #[test]
    fn the_default_filter_round_trips() {
        let saved = SavedQuery::capture("nothing", &LogFilter::default()).unwrap();
        assert_eq!(
            saved,
            SavedQuery {
                name: "nothing".into(),
                ..SavedQuery::default()
            }
        );
        assert_eq!(saved.to_filter().unwrap(), LogFilter::default());
    }

    #[test]
    fn names_are_cleaned_and_checked() {
        assert_eq!(clean_name(" a \t b\n"), Ok("a b".into()));
        assert_eq!(clean_name("   "), Err(NameError::Empty));
        assert_eq!(
            clean_name(&"x".repeat(MAX_NAME_CHARS)).unwrap().len(),
            MAX_NAME_CHARS
        );
        assert_eq!(
            clean_name(&"x".repeat(MAX_NAME_CHARS + 1)),
            Err(NameError::TooLong)
        );
        assert_eq!(
            clean_name(&"é".repeat(MAX_NAME_CHARS))
                .unwrap()
                .chars()
                .count(),
            MAX_NAME_CHARS
        );
        assert!(SavedQuery::capture("", &filter()).is_err());
    }

    #[test]
    fn saving_under_a_name_in_use_replaces_it_ignoring_case_and_spacing() {
        let mut list = SavedQueries::new();
        let mut first = SavedQuery::capture("Errors", &filter()).unwrap();
        assert!(list.upsert(first.clone()));
        assert!(list.upsert(SavedQuery::capture("alpha", &LogFilter::default()).unwrap()));
        first.query = "level>=error".into();
        first.name = "  ERRORS ".into();
        assert!(list.upsert(first));
        assert_eq!(list.len(), 2, "replaced, not added");
        assert_eq!(list.get("errors").unwrap().query, "level>=error");
        assert_eq!(
            list.sorted()
                .iter()
                .map(|q| q.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "ERRORS"]
        );
        assert_eq!(list.remove(" errors").unwrap().query, "level>=error");
        assert!(list.remove("errors").is_none());
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn the_list_is_capped_but_a_known_name_can_still_be_replaced() {
        let mut list = SavedQueries::new();
        for ix in 0..MAX_QUERIES {
            let name = format!("q{ix}");
            assert!(list.upsert(SavedQuery::capture(&name, &LogFilter::default()).unwrap()));
        }
        assert!(!list.upsert(SavedQuery::capture("one too many", &LogFilter::default()).unwrap()));
        assert_eq!(list.len(), MAX_QUERIES);
        let mut again = SavedQuery::capture("q7", &filter()).unwrap();
        again.text = "changed".into();
        assert!(list.upsert(again));
        assert_eq!(list.get("q7").unwrap().text, "changed");
    }

    #[test]
    fn json_round_trips_and_a_reader_keeps_what_it_can_use() {
        let mut list = SavedQueries::new();
        list.upsert(SavedQuery::capture("A", &filter()).unwrap());
        list.upsert(SavedQuery::capture("B", &LogFilter::default()).unwrap());
        assert_eq!(SavedQueries::from_json(&list.to_json()).unwrap(), list);
        // Unknown keys, a bad entry, a nameless one, a future preset and a typo'd level.
        let text = r#"{"queries": [
            {"name": "ok", "query": "status>=500", "future_key": 1},
            {"name": 7},
            {"name": "   "},
            {"name": "odd", "time": "3 weeks", "min_level": "LOUD", "query": "level>=warn"},
            {"name": "broken", "query": "status >="}
        ], "version": 9}"#;
        let list = SavedQueries::from_json(text).unwrap();
        assert_eq!(
            list.sorted()
                .iter()
                .map(|q| q.name.as_str())
                .collect::<Vec<_>>(),
            ["broken", "odd", "ok"]
        );
        let odd = list.get("odd").unwrap().to_filter().unwrap();
        assert_eq!(
            odd.time,
            TimeWindow::All,
            "an unknown preset is not applied"
        );
        assert_eq!(odd.level.min, None, "an unknown level is not applied");
        assert!(odd.query.is_some());
        // A query that does not parse is kept and says so when applied.
        assert!(list.get("broken").unwrap().to_filter().is_err());
        assert!(SavedQueries::from_json("not json").is_err());
        assert_eq!(SavedQueries::from_json("{}").unwrap(), SavedQueries::new());
    }
}
