use super::{Field, FieldType};
use crate::{properties::PropertyValue, Resolution, Vault};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde_json::Number;
use time::{Date, Month};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    String(String),
    Number(Number),
    Bool(bool),
    Date(Date),
    DateTime(DateTime<FixedOffset>),
    /// A legacy datetime in a date field compares by its local day.
    LegacyDate {
        day: Date,
        datetime: DateTime<FixedOffset>,
    },
    Link(Link),
    List(Vec<Value>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub label: String,
    pub external: bool,
}

impl Value {
    pub fn as_date(&self) -> Option<Date> {
        match *self {
            Self::Date(day) | Self::LegacyDate { day, .. } => Some(day),
            _ => None,
        }
    }

    /// Resolve links against the same published vault used by the view.
    /// An unchanged record can gain or lose a link target after an inventory batch.
    pub fn to_json(&self, vault: &Vault, from: &str) -> serde_json::Value {
        use serde_json::{json, Value as Json};
        match self {
            Self::String(value) => json!(value),
            Self::Number(value) => Json::Number(value.clone()),
            Self::Bool(value) => json!(value),
            Self::Date(value) => json!(value.to_string()),
            Self::DateTime(value)
            | Self::LegacyDate {
                datetime: value, ..
            } => {
                json!(value.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            }
            Self::Link(link) => match link.resolve(vault, from) {
                Resolution::Resolved { path } => json!({"link": link.target, "path": path}),
                Resolution::Unresolved => json!({"link": link.target, "path": null}),
                Resolution::Ambiguous { mut candidates } => {
                    candidates.sort_unstable();
                    json!({"link": link.target, "path": null, "ambiguous": candidates})
                }
            },
            Self::List(values) => {
                Json::Array(values.iter().map(|v| v.to_json(vault, from)).collect())
            }
        }
    }
}

impl Link {
    pub fn resolve(&self, vault: &Vault, from: &str) -> Resolution {
        if self.external {
            Resolution::Unresolved
        } else if self.target.starts_with(['#', '^']) {
            Resolution::Resolved {
                path: from.to_owned(),
            }
        } else {
            vault.resolve_from(&self.target, from)
        }
    }
}

pub(super) fn date(value: &str) -> Option<Date> {
    (value.len() == 10).then_some(())?;
    Date::parse(
        value,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
}

pub(super) fn datetime(value: &str, zone: Tz) -> Option<DateTime<FixedOffset>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed);
    }
    let local = ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())?;
    zone.from_local_datetime(&local)
        .single()
        .map(|t| t.fixed_offset())
}

pub(super) fn local_day(value: DateTime<FixedOffset>, zone: Tz) -> Option<Date> {
    let local = value.with_timezone(&zone);
    Date::from_calendar_date(
        local.year(),
        Month::try_from(local.month() as u8).ok()?,
        local.day() as u8,
    )
    .ok()
}

pub(super) fn absent(raw: &PropertyValue) -> bool {
    match raw {
        PropertyValue::Empty => true,
        PropertyValue::Text(text) => text.is_empty(),
        PropertyValue::List(list) => list.is_empty(),
        _ => false,
    }
}

pub(super) fn string(raw: &PropertyValue) -> Option<&str> {
    match raw {
        PropertyValue::Text(value) | PropertyValue::Url(value) => Some(value),
        _ => None,
    }
}

pub(super) fn inferred(raw: &PropertyValue, zone: Tz) -> Option<Value> {
    Some(match raw {
        PropertyValue::Text(text) if !text.is_empty() => date(text)
            .map(Value::Date)
            .or_else(|| datetime(text, zone).map(Value::DateTime))
            .unwrap_or_else(|| Value::String(text.clone())),
        PropertyValue::Url(text) => Value::String(text.clone()),
        PropertyValue::Bool(value) => Value::Bool(*value),
        PropertyValue::Number(value) => Value::Number(value.parse().ok()?),
        PropertyValue::Link { target, label } => wiki(target, label)?,
        PropertyValue::List(list) if !list.is_empty() => Value::List(
            list.iter()
                .filter_map(|value| inferred(value, zone))
                .collect(),
        ),
        _ => return None,
    })
}

pub(super) fn convert(
    raw: &PropertyValue,
    field: &Field,
    kind: &FieldType,
    layer: Option<&str>,
    zone: Tz,
) -> Option<Value> {
    Some(match kind {
        FieldType::String => {
            let text = string(raw)?;
            if !field.valid_string(text) {
                return None;
            }
            Value::String(text.to_owned())
        }
        FieldType::Number => match raw {
            PropertyValue::Number(value) => Value::Number(value.parse().ok()?),
            _ => return None,
        },
        FieldType::Bool => match raw {
            PropertyValue::Bool(value) => Value::Bool(*value),
            _ => return None,
        },
        FieldType::Date => {
            let text = string(raw)?;
            if let Some(day) = date(text) {
                Value::Date(day)
            } else {
                let datetime = datetime(text, zone)?;
                Value::LegacyDate {
                    day: local_day(datetime, zone)?,
                    datetime,
                }
            }
        }
        FieldType::DateTime => Value::DateTime(datetime(string(raw)?, zone)?),
        FieldType::Enum => {
            let text = string(raw)?;
            Value::String(
                field
                    .enum_value(text, layer, true)
                    .or_else(|| (!field.closed).then(|| text.to_owned()))?,
            )
        }
        FieldType::Link => {
            return match raw {
                PropertyValue::Link { target, label } => wiki(target, label),
                PropertyValue::Url(url) if field.allow_http_urls => Some(Value::Link(Link {
                    target: url.clone(),
                    label: url.clone(),
                    external: true,
                })),
                _ => None,
            }
        }
        FieldType::List(element) => {
            let list = match raw {
                PropertyValue::List(items) => items.as_slice(),
                _ => std::slice::from_ref(raw),
            };
            Value::List(
                list.iter()
                    .map(|raw| convert(raw, field, element, layer, zone))
                    .collect::<Option<_>>()?,
            )
        }
    })
}

fn wiki(target: &str, label: &str) -> Option<Value> {
    (!target.is_empty()).then(|| {
        Value::Link(Link {
            target: target.to_owned(),
            label: label.to_owned(),
            external: false,
        })
    })
}
