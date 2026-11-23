use super::{
    parse::{self, Block, ParamType, SectionDoc, ViewDoc},
    Finding, Severity,
};
use crate::{
    log::{
        chips::FilterValue,
        query::{self, Expr, FieldRef, Query, Test, ViewOp},
        ValueKind,
    },
    records::{
        value::{date, datetime, local_day},
        Field, FieldSchema, FieldType, Fields, Link, Value,
    },
};
use chrono::{DateTime, Datelike, Duration, FixedOffset, Months, NaiveDate, TimeZone};
use chrono_tz::Tz;
use regex::{Regex, RegexBuilder};
use serde_json::{Number, Value as Json};
use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};
use time::Date;

pub(super) type Params = BTreeMap<String, Value>;

#[derive(Clone, Copy)]
pub(super) struct Clock {
    pub at: DateTime<FixedOffset>,
    pub zone: Tz,
}

impl Clock {
    pub fn today(self) -> Option<Date> {
        local_day(self.at, self.zone)
    }

    pub(super) fn relative(self, text: &str) -> Result<Option<Value>, String> {
        let (rest, today) = if let Some(rest) = text.strip_prefix("today") {
            (rest, true)
        } else if let Some(rest) = text.strip_prefix("now") {
            (rest, false)
        } else {
            return Ok(None);
        };
        let local = self.at.with_timezone(&self.zone);
        if rest.is_empty() {
            return Ok(Some(if today {
                Value::Date(self.today().ok_or("as_of is outside the calendar range")?)
            } else {
                Value::DateTime(local.fixed_offset())
            }));
        }
        if !rest.starts_with(['+', '-']) {
            return Ok(None);
        }
        let invalid = || format!("Invalid relative date '{text}'");
        let backwards = rest.starts_with('-');
        let amount_end = rest[1..].bytes().take_while(u8::is_ascii_digit).count() + 1;
        let (amount, unit) = rest[1..].split_at(amount_end - 1);
        if amount.is_empty() || !amount.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let amount: u32 = amount.parse().map_err(|_| invalid())?;
        let signed = i64::from(amount) * if backwards { -1 } else { 1 };
        let result = if today {
            let day = local.date_naive();
            match unit {
                "d" => day.checked_add_signed(Duration::days(signed)),
                "w" => day.checked_add_signed(Duration::weeks(signed)),
                "m" if backwards => day.checked_sub_months(Months::new(amount)),
                "m" => day.checked_add_months(Months::new(amount)),
                _ => None,
            }
            .and_then(record_date)
            .map(Value::Date)
        } else {
            let duration = match unit {
                "h" => Duration::try_hours(signed),
                "d" => Duration::try_days(signed),
                _ => None,
            };
            duration
                .and_then(|delta| local.checked_add_signed(delta))
                .map(|at| Value::DateTime(at.fixed_offset()))
        };
        result.map(Some).ok_or_else(invalid)
    }

    fn midnight(self, day: Date) -> Option<Value> {
        let naive =
            NaiveDate::from_ymd_opt(day.year(), u32::from(day.month() as u8), day.day().into())?;
        let at = self
            .zone
            .from_local_datetime(&naive.and_hms_opt(0, 0, 0)?)
            .earliest()?;
        Some(Value::DateTime(at.fixed_offset()))
    }
}

pub(super) fn record_date(date: NaiveDate) -> Option<Date> {
    Date::from_calendar_date(
        date.year(),
        (date.month() as u8).try_into().ok()?,
        date.day() as u8,
    )
    .ok()
}

fn scalar_arg(raw: &Json) -> Option<Cow<'_, str>> {
    match raw {
        Json::String(s) => Some(Cow::Borrowed(s)),
        Json::Number(n) => Some(Cow::Owned(n.to_string())),
        _ => None,
    }
}

pub(super) fn param_value(
    kind: ParamType,
    raw: &Json,
    schema: &FieldSchema,
) -> Result<Value, String> {
    let text = scalar_arg(raw).ok_or_else(|| "Expected a string or number".to_owned())?;
    let result = if let Some(name) = kind.field() {
        schema
            .note_field(name)
            .and_then(|field| field_literal(field, &text, schema.timezone))
    } else {
        match kind {
            ParamType::Text => Some(Value::String(text.into_owned())),
            ParamType::Date => date(&text).map(Value::Date),
            ParamType::Number => text.parse().ok().map(Value::Number),
            _ => unreachable!("field param"),
        }
    };
    result.ok_or_else(|| format!("Invalid {kind:?} value '{raw}'"))
}

pub(super) fn params(
    doc: &ViewDoc,
    provided: &BTreeMap<String, Json>,
    schema: &FieldSchema,
) -> (Params, Vec<Finding>) {
    let mut values = Params::new();
    let mut findings = Vec::new();
    for name in provided.keys() {
        if !doc.front.params.contains_key(name) {
            findings.push(parse::finding(
                &doc.path,
                1,
                1,
                format!("Unknown parameter '{name}'"),
            ));
        }
    }
    for (name, param) in &doc.front.params {
        if let Some(raw) = provided.get(name).or_else(|| param.default_value()) {
            match param_value(param.kind(), raw, schema) {
                Ok(value) => {
                    values.insert(name.clone(), value);
                }
                Err(message) => findings.push(parse::finding(
                    &doc.path,
                    1,
                    1,
                    format!("Parameter '{name}': {message}"),
                )),
            }
        }
    }
    (values, findings)
}

pub(super) fn field_literal(field: &Field, text: &str, zone: Tz) -> Option<Value> {
    let mut kind = &field.field_type;
    while let FieldType::List(element) = kind {
        kind = element;
    }
    Some(match kind {
        FieldType::String if field.valid_string(text) => Value::String(text.to_owned()),
        FieldType::Enum => Value::String(
            field
                .enum_value(text, None, false)
                .or_else(|| (!field.closed).then(|| text.to_owned()))?,
        ),
        FieldType::Bool => Value::Bool(text.parse().ok()?),
        FieldType::Number => Value::Number(text.parse().ok()?),
        FieldType::Date => {
            Value::Date(date(text).or_else(|| local_day(datetime(text, zone)?, zone))?)
        }
        FieldType::DateTime => Value::DateTime(datetime(text, zone)?),
        FieldType::Link => {
            let (target, label, external) = if let Some(inner) =
                text.strip_prefix("[[").and_then(|s| s.strip_suffix("]]"))
            {
                let (target, label) = inner.split_once('|').unwrap_or((inner, inner));
                (!target.is_empty()).then_some((target, label, false))?
            } else if field.allow_http_urls
                && url::Url::parse(text).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
            {
                (text, text, true)
            } else {
                return None;
            };
            Value::Link(Link {
                target: target.into(),
                label: label.into(),
                external,
            })
        }
        _ => return None,
    })
}

pub(super) fn inferred(text: &str, zone: Tz) -> Value {
    if text == "null" {
        Value::Null
    } else if let Ok(number) = text.parse::<Number>() {
        Value::Number(number)
    } else if let Ok(boolean) = text.parse::<bool>() {
        Value::Bool(boolean)
    } else if let Some(day) = date(text) {
        Value::Date(day)
    } else if let Some(at) = datetime(text, zone) {
        Value::DateTime(at)
    } else {
        Value::String(text.to_owned())
    }
}

pub(super) fn operand(
    text: &str,
    quoted: bool,
    vars: &Params,
    clock: Clock,
    field: Option<&Field>,
) -> Result<Value, String> {
    let normalized = if quoted {
        None
    } else if let Some(name) = text.strip_prefix('$') {
        Some(
            vars.get(name)
                .cloned()
                .ok_or_else(|| format!("Missing parameter '{name}'"))?,
        )
    } else {
        clock.relative(text)?
    };
    if let Some(field) = field {
        if let Some(value) = &normalized {
            let mut kind = &field.field_type;
            while let FieldType::List(item) = kind {
                kind = item;
            }
            match (kind, value) {
                (FieldType::DateTime, Value::Date(day)) => {
                    return clock
                        .midnight(*day)
                        .ok_or_else(|| format!("No local midnight on {day}"));
                }
                (FieldType::Date, Value::DateTime(at)) => {
                    return local_day(*at, clock.zone)
                        .map(Value::Date)
                        .ok_or_else(|| "Datetime is outside the calendar range".into());
                }
                _ => {}
            }
        }
        let literal = normalized.as_ref().map_or(Cow::Borrowed(text), value_text);
        field_literal(field, &literal, clock.zone)
            .ok_or_else(|| format!("'{literal}' is not a valid {:?} value", field.field_type))
    } else {
        Ok(normalized.unwrap_or_else(|| {
            if quoted {
                Value::String(text.to_owned())
            } else {
                inferred(text, clock.zone)
            }
        }))
    }
}

pub(super) fn value_text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        Value::Number(number) => Cow::Owned(number.to_string()),
        Value::Bool(true) => Cow::Borrowed("true"),
        Value::Bool(false) => Cow::Borrowed("false"),
        Value::Date(day) => Cow::Owned(day.to_string()),
        Value::DateTime(at) | Value::LegacyDate { datetime: at, .. } => {
            Cow::Owned(at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
        }
        Value::Link(link) => Cow::Borrowed(&link.target),
        Value::Null => Cow::Borrowed("null"),
        Value::List(_) | Value::Object(_) => Cow::Owned(plain_json(value).to_string()),
    }
}

fn plain_json(value: &Value) -> Json {
    match value {
        Value::List(values) => Json::Array(values.iter().map(plain_json).collect()),
        Value::Object(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), plain_json(v)))
                .collect(),
        ),
        Value::Bool(value) => Json::Bool(*value),
        Value::Number(value) => Json::Number(value.clone()),
        Value::Null => Json::Null,
        value => Json::String(value_text(value).into_owned()),
    }
}

fn number_cmp(left: &Number, right: &Number) -> Ordering {
    fn integer(number: &Number) -> Option<i128> {
        number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from))
    }

    fn against_float(integer: i128, float: f64) -> Ordering {
        let floor = float.floor();
        integer.cmp(&(floor as i128)).then_with(|| {
            if float == floor {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        })
    }

    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(left), None) => against_float(left, right.as_f64().expect("JSON float")),
        (None, Some(right)) => against_float(right, left.as_f64().expect("JSON float")).reverse(),
        (None, None) => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .expect("finite JSON numbers"),
    }
}

/// Prefer a literal dotted key, then descend through a JSONL object.
pub(super) fn get<'a>(fields: &'a Fields, key: &str) -> Option<&'a Value> {
    if let Some(value) = fields.get(key) {
        return Some(value);
    }
    let (first, rest) = key.split_once('.')?;
    let Value::Object(nested) = fields.get(first)? else {
        return None;
    };
    get(nested, rest)
}

pub(super) fn compare(left: &Value, right: &Value) -> Option<Ordering> {
    use Value::*;
    Some(match (left, right) {
        (Number(a), Number(b)) => number_cmp(a, b),
        (String(a), String(b)) => a.cmp(b),
        (Bool(a), Bool(b)) => a.cmp(b),
        (DateTime(a), DateTime(b)) => a.cmp(b),
        (Link(a), Link(b)) => a.target.cmp(&b.target),
        (Null, Null) => Ordering::Equal,
        (a, b) => a.as_date()?.cmp(&b.as_date()?),
    })
}

pub(super) fn order(left: &Value, right: &Value) -> Ordering {
    // Unknown projected fields may have different types in different rows.
    fn rank(value: &Value) -> u8 {
        match value {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Number(_) => 2,
            Value::Date(_) | Value::LegacyDate { .. } => 3,
            Value::DateTime(_) => 4,
            Value::String(_) => 5,
            Value::Link(_) => 6,
            Value::List(_) => 7,
            Value::Object(_) => 8,
        }
    }
    rank(left).cmp(&rank(right)).then_with(|| {
        compare(left, right).unwrap_or_else(|| value_text(left).cmp(&value_text(right)))
    })
}

pub(super) struct BoundQuery {
    ast: Expr,
    predicates: Vec<Predicate>,
}

enum Operand {
    Values(Vec<Value>),
    Text(String),
    Regex(Regex),
    Typed(FilterValue),
}

struct Predicate {
    op: ViewOp,
    operand: Operand,
}

impl Predicate {
    fn matches(&self, value: &Value) -> bool {
        let items = match value {
            Value::List(items) => items.as_slice(),
            value => std::slice::from_ref(value),
        };
        match &self.operand {
            Operand::Text(text) => {
                items.iter().any(|v| value_text(v).contains(text))
                    != matches!(self.op, ViewOp::NotContains)
            }
            Operand::Regex(regex) => {
                items.iter().any(|v| regex.is_match(&value_text(v)))
                    != matches!(self.op, ViewOp::NotRegex)
            }
            Operand::Typed(filter) => match filter {
                FilterValue::Missing => false,
                FilterValue::Value { kind, text } => {
                    let actual = match value {
                        Value::Number(_) => ValueKind::Number,
                        Value::Bool(_) => ValueKind::Bool,
                        Value::List(_) => ValueKind::Array,
                        Value::Null => ValueKind::Null,
                        Value::Object(_) => ValueKind::Object,
                        _ => ValueKind::String,
                    };
                    actual == *kind
                        && if matches!(kind, ValueKind::Array | ValueKind::Object) {
                            serde_json::from_str::<Json>(text)
                                .is_ok_and(|expected| expected == plain_json(value))
                        } else {
                            value_text(value) == text.as_str()
                        }
                }
            },
            Operand::Values(values) => {
                items.iter().any(|value| {
                    values.iter().any(|expected| {
                        compare(value, expected).is_some_and(|ordering| match self.op {
                            ViewOp::Compare(cmp) => cmp.holds(ordering),
                            _ => ordering.is_eq(),
                        })
                    })
                }) != matches!(self.op, ViewOp::Ne | ViewOp::NotIn)
            }
        }
    }
}

struct QueryRow<'a> {
    fields: &'a Fields,
    predicates: &'a [Predicate],
}

impl query::Row for QueryRow<'_> {
    fn exists(&self, field: &FieldRef) -> bool {
        matches!(field, FieldRef::Key(key) if get(self.fields, key).is_some())
    }

    fn test(&self, field: &FieldRef, test: &Test, include_absent: bool) -> bool {
        let (FieldRef::Key(field), Test::View(index)) = (field, test) else {
            unreachable!("view dialect")
        };
        let predicate = &self.predicates[*index];
        get(self.fields, field).map_or(include_absent, |value| predicate.matches(value))
    }
}

impl BoundQuery {
    pub fn matches(&self, fields: &Fields) -> bool {
        self.ast.evaluate(&QueryRow {
            fields,
            predicates: &self.predicates,
        })
    }
}

fn compile_predicate(
    predicate: &query::ViewPredicate,
    block: &Block,
    schema: &FieldSchema,
    vars: &Params,
    clock: Clock,
    validating: bool,
) -> Result<Option<Predicate>, (usize, String)> {
    let field = if block.lets.contains_key(&predicate.field) {
        None
    } else {
        block.field(schema, &predicate.field)
    };
    let fail = |i: usize, error| (predicate.operands[i].span.start, error);
    let mut values = Vec::new();
    let mut unbound = false;
    for (i, authored) in predicate.operands.iter().enumerate() {
        if validating && !authored.quoted && authored.text.starts_with('$') {
            unbound = true;
            continue;
        }
        let exact = matches!(
            predicate.op,
            ViewOp::Eq | ViewOp::Ne | ViewOp::In | ViewOp::NotIn | ViewOp::Compare(_)
        );
        let value = if exact {
            operand(&authored.text, authored.quoted, vars, clock, field)
        } else if authored.quoted {
            Ok(Value::String(authored.text.clone()))
        } else if let Some(name) = authored.text.strip_prefix('$') {
            vars.get(name)
                .cloned()
                .ok_or_else(|| format!("Missing parameter '{name}'"))
        } else {
            clock
                .relative(&authored.text)
                .map(|value| value.unwrap_or_else(|| Value::String(authored.text.clone())))
        }
        .map_err(|error| fail(i, error))?;
        values.push(value);
    }
    if unbound {
        return Ok(None);
    }
    let operand = match predicate.op {
        ViewOp::Typed => {
            let kind = value_text(&values[0]);
            let mut text = value_text(&values[1]);
            if let Some(field) = field {
                if !matches!(kind.as_ref(), "missing" | "null" | "array" | "object") {
                    let actual = field_literal(field, &text, clock.zone).ok_or_else(|| {
                        fail(
                            1,
                            format!("'{text}' is not a valid {:?} value", field.field_type),
                        )
                    })?;
                    text = Cow::Owned(value_text(&actual).into_owned());
                }
            }
            Operand::Typed(
                FilterValue::parse(&kind, text.into_owned())
                    .ok_or_else(|| fail(1, "Invalid typed value".into()))?,
            )
        }
        ViewOp::Contains | ViewOp::NotContains => {
            Operand::Text(value_text(&values[0]).into_owned())
        }
        ViewOp::Regex | ViewOp::NotRegex => Operand::Regex(
            RegexBuilder::new(&value_text(&values[0]))
                .size_limit(1 << 20)
                .build()
                .map_err(|error| fail(0, format!("Invalid regular expression: {error}")))?,
        ),
        _ => Operand::Values(values),
    };
    Ok(Some(Predicate {
        op: predicate.op,
        operand,
    }))
}

fn unknown_field(block: &Block, schema: &FieldSchema, key: &str) -> bool {
    !block.jsonl() && !block.lets.contains_key(key) && block.field(schema, key).is_none()
}

pub(super) fn warn(mut finding: Finding) -> Finding {
    finding.severity = Severity::Warning;
    finding
}

pub(super) fn validate(
    doc: &ViewDoc,
    section: &SectionDoc,
    block: &Block,
    schema: &FieldSchema,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(query) = &block.filter {
        findings.extend(inspect_filter(
            query,
            doc,
            block,
            schema,
            &mut seen,
            |offset, message| section.where_finding(&doc.path, query, offset, message),
        ));
    }
    for (key, fields) in [
        ("cols", block.cols.as_deref().unwrap_or_default()),
        ("group", block.group()),
        ("sort", block.sort()),
        ("levels", block.levels.as_deref().unwrap_or_default()),
        ("at", block.at.as_slice()),
    ] {
        for field in fields {
            let field = parse::unsign(field);
            if unknown_field(block, schema, field) && seen.insert(field.to_owned()) {
                findings.push(warn(section.finding(
                    &doc.path,
                    key,
                    format!("Unknown field '{field}'"),
                )));
            }
        }
    }
    findings
}

pub(super) fn inspect_filter(
    query: &str,
    doc: &ViewDoc,
    block: &Block,
    schema: &FieldSchema,
    seen: &mut BTreeSet<String>,
    finding: impl Fn(usize, String) -> Finding,
) -> Vec<Finding> {
    let (_, syntax) = match Query::parse_view(query) {
        Ok(parsed) => parsed,
        Err(error) => return vec![finding(error.span.start, error.message)],
    };
    let mut findings = Vec::new();
    for (field, span) in syntax.fields {
        if unknown_field(block, schema, &field) && seen.insert(field.clone()) {
            findings.push(warn(finding(
                span.start,
                format!("Unknown field '{field}'"),
            )));
        }
    }
    let clock = Clock {
        at: DateTime::UNIX_EPOCH.fixed_offset(),
        zone: schema.timezone,
    };
    for predicate in syntax.predicates {
        for operand in &predicate.operands {
            if let Some(param) = operand.text.strip_prefix('$').filter(|_| !operand.quoted) {
                if !doc.front.params.contains_key(param) {
                    findings.push(finding(
                        operand.span.start,
                        format!("Undeclared parameter '{param}'"),
                    ));
                }
            }
        }
        if let Err((offset, message)) =
            compile_predicate(&predicate, block, schema, &Params::new(), clock, true)
        {
            findings.push(finding(offset, message));
        }
    }
    findings
}

pub(super) fn compile_query(
    query: &str,
    block: &Block,
    schema: &FieldSchema,
    vars: &Params,
    clock: Clock,
) -> Result<BoundQuery, (usize, String)> {
    let (ast, syntax) =
        Query::parse_view(query).map_err(|error| (error.span.start, error.message))?;
    let predicates = syntax
        .predicates
        .iter()
        .map(|predicate| {
            compile_predicate(predicate, block, schema, vars, clock, false)
                .map(|bound| bound.expect("runtime predicates are fully bound"))
        })
        .collect::<Result<_, _>>()?;
    Ok(BoundQuery { ast, predicates })
}

pub(super) fn bind(
    doc: &ViewDoc,
    section: &SectionDoc,
    block: &Block,
    schema: &FieldSchema,
    vars: &Params,
    clock: Clock,
) -> Result<Option<BoundQuery>, Finding> {
    block
        .filter
        .as_ref()
        .map(|query| {
            compile_query(query, block, schema, vars, clock).map_err(|(offset, message)| {
                section.where_finding(&doc.path, query, offset, message)
            })
        })
        .transpose()
}
