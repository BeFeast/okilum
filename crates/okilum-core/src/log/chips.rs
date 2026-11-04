//! Point-and-click filters compile into the ordinary query engine. Count providers
//! run on a background executor; the UI never scans loaded rows during a draw.
use super::query::{Expr, FieldRef};
use super::{Field, LogIndex, Query, Record, Role, ValueKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Missing is distinct from JSON null, false and the empty string.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FilterValue {
    Missing,
    Value { kind: ValueKind, text: String },
}
impl FilterValue {
    pub fn string(text: &str) -> Self {
        Self::Value {
            kind: ValueKind::String,
            text: text.into(),
        }
    }
    pub fn from_field(field: &Field) -> Self {
        Self::Value {
            kind: field.kind,
            text: field.value.clone(),
        }
    }
    pub fn text(&self) -> &str {
        match self {
            Self::Missing => "",
            Self::Value { text, .. } => text,
        }
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Value { kind, .. } => match kind {
                ValueKind::String => "string",
                ValueKind::Number => "number",
                ValueKind::Bool => "bool",
                ValueKind::Null => "null",
                ValueKind::Array => "array",
                ValueKind::Object => "object",
            },
        }
    }
    pub(crate) fn parse(kind: &str, text: String) -> Option<Self> {
        let value_kind = match kind {
            "missing" if text.is_empty() => return Some(Self::Missing),
            "string" => ValueKind::String,
            "number" if super::logfmt::bare_kind(text.as_bytes()) == ValueKind::Number => {
                ValueKind::Number
            }
            "bool" if text == "true" || text == "false" => ValueKind::Bool,
            "null" if text == "null" => ValueKind::Null,
            "array" if super::json::container_kind(text.as_bytes()) == Some(ValueKind::Array) => {
                ValueKind::Array
            }
            "object" if super::json::container_kind(text.as_bytes()) == Some(ValueKind::Object) => {
                ValueKind::Object
            }
            _ => return None,
        };
        Some(Self::Value {
            kind: value_kind,
            text,
        })
    }
    pub fn label(&self) -> String {
        match self {
            Self::Missing => "Missing".into(),
            Self::Value {
                kind: ValueKind::String,
                text,
            } => serde_json::to_string(text).unwrap(),
            Self::Value { text, .. } => text.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterExecution {
    #[default]
    Local,
    Server,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterMode {
    #[default]
    Include,
    Exclude,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterChip {
    pub field: FieldRef,
    pub values: Vec<FilterValue>,
    pub mode: FilterMode,
    pub enabled: bool,
    pub execution: FilterExecution,
}
impl FilterChip {
    pub fn new(field: FieldRef, values: Vec<FilterValue>, mode: FilterMode) -> Self {
        Self {
            field,
            values,
            mode,
            enabled: true,
            execution: FilterExecution::Local,
        }
    }
    /// The same expression is used for local evaluation and the advanced query box.
    pub fn expr(&self) -> Expr {
        let mut terms: Vec<_> = self
            .values
            .iter()
            .map(|value| Expr::Typed {
                field: self.field.clone(),
                value: value.clone(),
            })
            .collect();
        let expr = if terms.len() == 1 {
            terms.pop().unwrap()
        } else {
            Expr::Or(terms)
        };
        if self.mode == FilterMode::Exclude {
            Expr::Not(Box::new(expr))
        } else {
            expr
        }
    }
    pub fn from_expr(expr: &Expr) -> Option<Self> {
        let (expr, mode) = match expr {
            Expr::Not(e) => (e.as_ref(), FilterMode::Exclude),
            e => (e, FilterMode::Include),
        };
        let terms = match expr {
            Expr::Or(xs) => xs.as_slice(),
            x => std::slice::from_ref(x),
        };
        let mut field = None;
        let mut values = Vec::new();
        for term in terms {
            let Expr::Typed { field: next, value } = term else {
                return None;
            };
            if field.as_ref().is_some_and(|field| field != next) {
                return None;
            }
            field = Some(next.clone());
            values.push(value.clone());
        }
        Some(Self::new(field?, values, mode))
    }
    pub fn label(&self) -> String {
        let relation = match (self.mode, self.values.len()) {
            (FilterMode::Include, 1) => "is",
            (FilterMode::Include, _) => "is one of",
            (FilterMode::Exclude, 1) => "is not",
            (FilterMode::Exclude, _) => "is none of",
        };
        let values = self
            .values
            .iter()
            .map(FilterValue::label)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} {relation} {values}", self.field)
    }
}

/// Separate recognized chips from advanced expressions without rewriting either matcher.
pub fn split_query(query: &Query) -> (Vec<FilterChip>, Vec<Expr>) {
    let terms = match query.expr() {
        Expr::And(xs) => xs.as_slice(),
        x => std::slice::from_ref(x),
    };
    let mut chips = Vec::new();
    let mut advanced = Vec::new();
    for term in terms {
        if let Some(chip) = FilterChip::from_expr(term) {
            chips.push(chip);
        } else {
            advanced.push(term.clone());
        }
    }
    (chips, advanced)
}
pub fn combined_query(chips: &[FilterChip], advanced: &[Expr]) -> Query {
    let mut terms = advanced.to_vec();
    terms.extend(
        chips
            .iter()
            .filter(|c| c.enabled && !c.values.is_empty())
            .map(FilterChip::expr),
    );
    let text = match terms.len() {
        0 => String::new(),
        1 => terms[0].to_string(),
        _ => Expr::And(terms).to_string(),
    };
    Query::parse(&text).expect("canonical query expressions parse")
}

/// One immutable source supplied to a count provider by the Log view.
pub struct CountSource<'a> {
    pub index: &'a LogIndex,
    pub bytes: &'a [u8],
    pub name: &'a str,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueCount {
    pub value: FilterValue,
    pub count: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnCounts {
    pub label: String,
    pub values: Vec<ValueCount>,
}
/// Invoked off the UI thread. A server adapter can ignore loaded sources and
/// return its own counts and scope label. Errors remain visible in the picker.
pub trait ColumnCountProvider: Send + Sync {
    /// Bind a display role to an underlying field before recording a chip.
    /// Providers that cannot resolve it keep the original reference; they must
    /// surface ambiguity rather than inventing a field name.
    fn canonical_field(&self, field: &FieldRef) -> FieldRef {
        field.clone()
    }
    fn count(&self, field: &FieldRef, sources: &[CountSource<'_>]) -> Result<ColumnCounts, String>;
}
#[derive(Default)]
pub struct LoadedRowCountProvider;

/// Value extraction shared by counts and cell actions. Duplicate fields count
/// a row once per distinct typed value, just as an OR query does.
pub fn row_values(
    field: &FieldRef,
    record: Option<&Record>,
    raw: &[u8],
    source: &str,
) -> Vec<FilterValue> {
    if *field == FieldRef::Source {
        return vec![FilterValue::string(source)];
    }
    if record.is_none() && *field == FieldRef::Message {
        return vec![FilterValue::string(&super::ansi::strip_text(
            String::from_utf8_lossy(raw),
        ))];
    }
    let mut values = BTreeSet::new();
    if let Some(record) = record {
        match field {
            FieldRef::Key(key) => values.extend(
                record
                    .fields
                    .iter()
                    .filter(|f| f.key == *key)
                    .map(FilterValue::from_field),
            ),
            field => {
                let role = match field {
                    FieldRef::Time => Role::Time,
                    FieldRef::Level => Role::Level,
                    FieldRef::Message => Role::Message,
                    FieldRef::Logger => Role::Logger,
                    _ => unreachable!(),
                };
                values.extend(record.role(role).map(FilterValue::from_field));
            }
        }
    }
    if values.is_empty() {
        values.insert(FilterValue::Missing);
    }
    values.into_iter().collect()
}
impl ColumnCountProvider for LoadedRowCountProvider {
    fn count(&self, field: &FieldRef, sources: &[CountSource<'_>]) -> Result<ColumnCounts, String> {
        let mut counts = BTreeMap::new();
        for source in sources {
            for entry in source.index.entries() {
                let raw = source.bytes.get(entry.range()).unwrap_or_default();
                let record = source.index.record(raw);
                for value in row_values(field, record.as_ref(), raw, source.name) {
                    *counts.entry(value).or_insert(0) += 1;
                }
            }
        }
        Ok(ColumnCounts {
            label: "in loaded rows".into(),
            values: counts
                .into_iter()
                .map(|(value, count)| ValueCount { value, count })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::filter::{self, LogFilter};

    const ROWS: &str = concat!(
        "{\"sample\":\"200\",\"flag\":false}\n",
        "{\"sample\":200}\n",
        "{\"sample\":false}\n",
        "{\"sample\":null}\n",
        "{\"sample\":\"\"}\n",
        "{\"other\":0}\n",
        "{\"sample\":\"false\"}\n",
        "{\"sample\":\"null\"}\n",
    );
    fn hits(text: &str, chip: &FilterChip) -> Vec<u32> {
        let index = LogIndex::build_with_threads(text.as_bytes(), 1);
        let query = combined_query(std::slice::from_ref(chip), &[]);
        let filter = LogFilter {
            query: Some(query),
            ..Default::default()
        };
        filter::evaluate_range(&filter, &index, text.as_bytes(), 0..index.len())
    }
    #[test]
    fn typed_values_are_distinct_and_round_trip_through_the_query_engine() {
        for (at, value) in [
            FilterValue::string("200"),
            FilterValue::parse("number", "200".into()).unwrap(),
            FilterValue::parse("bool", "false".into()).unwrap(),
            FilterValue::parse("null", "null".into()).unwrap(),
            FilterValue::string(""),
            FilterValue::Missing,
            FilterValue::string("false"),
            FilterValue::string("null"),
        ]
        .into_iter()
        .enumerate()
        {
            let chip = FilterChip::new(
                FieldRef::Key("sample".into()),
                vec![value],
                FilterMode::Include,
            );
            assert_eq!(hits(ROWS, &chip), [at as u32]);
            let query = combined_query(std::slice::from_ref(&chip), &[]);
            assert_eq!(
                split_query(&Query::parse(&query.to_string()).unwrap()).0,
                std::slice::from_ref(&chip)
            );
            let excluded = FilterChip {
                mode: FilterMode::Exclude,
                ..chip
            };
            assert_eq!(
                hits(ROWS, &excluded),
                (0..8).filter(|i| *i != at as u32).collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn escaping_multiselect_disable_clear_and_appended_rows() {
        let key = "synthetic key:\"\\ λ";
        let values = ["quotes \" spaces : \\ שלום", "", "missing", "null", "false"];
        let text = values
            .iter()
            .map(|value| serde_json::to_string(&serde_json::json!({key: value})).unwrap() + "\n")
            .collect::<String>();
        let chip = FilterChip::new(
            FieldRef::Key(key.into()),
            values[..2].iter().map(|v| FilterValue::string(v)).collect(),
            FilterMode::Include,
        );
        assert_eq!(hits(&text, &chip), [0, 1]);
        let query = combined_query(std::slice::from_ref(&chip), &[]);
        assert_eq!(split_query(&query).0, std::slice::from_ref(&chip));
        assert_eq!(hits(&(text.clone() + &text), &chip), [0, 1, 5, 6]);
        assert!(combined_query(
            &[FilterChip {
                enabled: false,
                ..chip
            }],
            &[]
        )
        .is_empty());
        assert!(combined_query(&[], &[]).is_empty());
    }
    #[test]
    fn counts_use_typed_values_and_count_each_row_once() {
        let index = LogIndex::build_with_threads(ROWS.as_bytes(), 1);
        let counts = LoadedRowCountProvider
            .count(
                &FieldRef::Key("sample".into()),
                &[CountSource {
                    index: &index,
                    bytes: ROWS.as_bytes(),
                    name: "synthetic",
                }],
            )
            .unwrap();
        assert_eq!(counts.label, "in loaded rows");
        assert_eq!(counts.values.len(), 8);
        assert!(counts.values.iter().all(|v| v.count == 1));
        let duplicates = b"{\"sample\":1,\"sample\":1}\n{\"sample\":1}\n";
        let index = LogIndex::build_with_threads(duplicates, 1);
        let counts = LoadedRowCountProvider
            .count(
                &FieldRef::Key("sample".into()),
                &[CountSource {
                    index: &index,
                    bytes: duplicates,
                    name: "synthetic",
                }],
            )
            .unwrap();
        assert_eq!(counts.values[0].count, 2);
    }
    #[test]
    fn source_filters_bind_inside_boolean_expressions() {
        let chip = FilterChip::new(
            FieldRef::Source,
            vec![FilterValue::string("source a")],
            FilterMode::Exclude,
        );
        let query = combined_query(&[chip], &[]);
        let index = LogIndex::build_with_threads(ROWS.as_bytes(), 1);
        let entry = index.get(0).unwrap();
        assert!(!query.for_source("source a").matches(entry, None));
        assert!(query.for_source("source b").matches(entry, None));
    }
    #[test]
    fn logfmt_numeric_spellings_round_trip_without_coercing_quoted_strings() {
        for spelling in ["+1", ".5", "1.", "01", "1e400"] {
            let text = format!("sample={spelling} msg=first\nsample=\"{spelling}\" msg=second\n");
            let chip = FilterChip::new(
                FieldRef::Key("sample".into()),
                vec![FilterValue::parse("number", spelling.into()).unwrap()],
                FilterMode::Include,
            );
            assert_eq!(hits(&text, &chip), [0], "{spelling}");
        }
    }
    #[test]
    fn empty_raw_keys_and_unbounded_numeric_text_keep_the_lossless_record_contract() {
        let text = "{\"\":1e400,\"sample\":[1e400]}\n{\"\":\"1e400\",\"sample\":[0]}\n";
        for (field, value) in [
            ("", FilterValue::parse("number", "1e400".into()).unwrap()),
            (
                "sample",
                FilterValue::parse("array", "[1e400]".into()).unwrap(),
            ),
        ] {
            let chip = FilterChip::new(
                FieldRef::Key(field.into()),
                vec![value],
                FilterMode::Include,
            );
            assert_eq!(hits(text, &chip), [0]);
            let query = combined_query(std::slice::from_ref(&chip), &[]);
            assert_eq!(
                split_query(&Query::parse(&query.to_string()).unwrap()).0,
                [chip]
            );
        }
    }
    #[test]
    fn saved_views_keep_disabled_chips_and_columns_with_legacy_defaults() {
        let chip = FilterChip {
            enabled: false,
            ..FilterChip::new(
                FieldRef::Key("sample".into()),
                vec![FilterValue::Missing],
                FilterMode::Exclude,
            )
        };
        let mut saved =
            super::super::saved_query::SavedQuery::capture("synthetic", &LogFilter::default())
                .unwrap();
        saved.filter_chips.push(chip);
        saved.columns = Some(vec!["sample".into()]);
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(
            serde_json::from_str::<super::super::saved_query::SavedQuery>(&json).unwrap(),
            saved
        );
        let old: super::super::saved_query::SavedQuery =
            serde_json::from_str("{\"name\":\"old\"}").unwrap();
        assert_eq!(old.columns, None);
        assert!(old.filter_chips.is_empty());
    }
}
