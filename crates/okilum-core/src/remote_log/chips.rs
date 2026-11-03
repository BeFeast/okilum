//! Typed chip compilation. Elasticsearch owns keyword normalization; UI matching
//! must never reapply a server chip to the original source text.
use super::result::{AggregatableField, FieldType, FieldValue};
use crate::log::{
    chips::{FilterChip, FilterExecution, FilterMode, FilterValue},
    query::FieldRef,
    ValueKind,
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};

/// Exact mappings win. A text field can use its only advertised aggregatable
/// multifield; several candidates require an explicit named-column choice.
pub fn mapped_field<'a>(
    key: &str,
    fields: &'a [AggregatableField],
) -> Result<&'a AggregatableField> {
    if let Some(field) = fields.iter().find(|field| field.name == key) {
        return Ok(field);
    }
    let mut candidates = fields.iter().filter(|field| field.source_path == key);
    let field=candidates.next().context("No scalar aggregatable mapping for this field; choose Loaded rows or an explicit keyword column")?;
    ensure!(
        candidates.next().is_none(),
        "Several keyword multifields match; choose an explicit field instead of guessing"
    );
    Ok(field)
}
pub fn field_value(value: &FilterValue, field_type: FieldType) -> Result<FieldValue> {
    Ok(match value {
        FilterValue::Missing=>FieldValue::Missing,
        FilterValue::Value {kind:ValueKind::String,text} if field_type==FieldType::String=>FieldValue::String(text.clone()),
        FilterValue::Value {kind:ValueKind::Number,text} if field_type==FieldType::Number=>FieldValue::Number(text.parse().context("Invalid exact numeric value")?),
        FilterValue::Value {kind:ValueKind::Bool,text} if field_type==FieldType::Boolean=>FieldValue::Boolean(text.parse().context("Invalid boolean value")?),
        _=>bail!("The value type does not match a scalar server mapping; choose Loaded rows for null, structured or conflicting values"),
    })
}
pub fn picker_value(value: &FieldValue) -> FilterValue {
    match value {
        FieldValue::Missing => FilterValue::Missing,
        FieldValue::String(text) => FilterValue::string(text),
        FieldValue::Number(number) => FilterValue::Value {
            kind: ValueKind::Number,
            text: number.to_string(),
        },
        FieldValue::Boolean(value) => FilterValue::Value {
            kind: ValueKind::Bool,
            text: value.to_string(),
        },
    }
}
/// Resolve role aliases before this seam. Local and disabled chips never reach
/// the network, and negation excludes the whole OR group (including Missing).
pub fn compile(chip: &FilterChip, fields: &[AggregatableField]) -> Result<Option<Value>> {
    if !chip.enabled || chip.execution != FilterExecution::Server {
        return Ok(None);
    }
    let FieldRef::Key(key) = &chip.field else {
        bail!("Choose a named field column for this server filter")
    };
    let field = mapped_field(key, fields)?;
    ensure!(
        !chip.values.is_empty() && chip.values.len() <= 1000,
        "Select 1–1,000 filter values"
    );
    let terms = chip
        .values
        .iter()
        .map(|value| Ok(field_value(value, field.field_type)?.filter(&field.name)))
        .collect::<Result<Vec<_>>>()?;
    let query = if terms.len() == 1 {
        terms.into_iter().next().unwrap()
    } else {
        json!({"bool":{"should":terms,"minimum_should_match":1}})
    };
    Ok(Some(
        json!({"meta":{"controlledBy":"okilum-guided","negate":chip.mode==FilterMode::Exclude},"query":query}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn field(name: &str, kind: FieldType) -> AggregatableField {
        AggregatableField {
            name: name.into(),
            source_path: name.into(),
            field_type: kind,
        }
    }
    fn chip(key: &str, values: Vec<FilterValue>) -> FilterChip {
        let mut chip = FilterChip::new(FieldRef::Key(key.into()), values, FilterMode::Include);
        chip.execution = FilterExecution::Server;
        chip
    }
    #[test]
    fn server_filters_preserve_typed_values_and_escape_structural_characters() {
        let name = "context.\"odd\\field";
        let fields = vec![
            field(name, FieldType::String),
            field("duration", FieldType::Number),
            field("active", FieldType::Boolean),
        ];
        let value = "quoted \"text\" \\ and:() שלום";
        assert_eq!(
            compile(&chip(name, vec![FilterValue::string(value)]), &fields)
                .unwrap()
                .unwrap()["query"],
            json!({"term":{name:value}})
        );
        assert_eq!(
            compile(
                &chip(
                    "duration",
                    vec![FilterValue::Value {
                        kind: ValueKind::Number,
                        text: "9007199254740993".into()
                    }]
                ),
                &fields
            )
            .unwrap()
            .unwrap()["query"],
            json!({"term":{"duration":9007199254740993u64}})
        );
        assert_eq!(
            compile(
                &chip(
                    "active",
                    vec![FilterValue::Value {
                        kind: ValueKind::Bool,
                        text: "false".into()
                    }]
                ),
                &fields
            )
            .unwrap()
            .unwrap()["query"],
            json!({"term":{"active":false}})
        );
        let mut group = chip(
            name,
            vec![FilterValue::Missing, FilterValue::string("checkout")],
        );
        group.mode = FilterMode::Exclude;
        let compiled = compile(&group, &fields).unwrap().unwrap();
        assert_eq!(compiled["meta"]["negate"], true);
        assert_eq!(compiled["query"]["bool"]["minimum_should_match"], 1);
        assert_eq!(
            compiled["query"]["bool"]["should"][0],
            json!({"bool":{"must_not":[{"exists":{"field":name}}]}})
        );
        group.execution = FilterExecution::Local;
        assert!(compile(&group, &fields).unwrap().is_none());
        group.execution = FilterExecution::Server;
        group.enabled = false;
        assert!(compile(&group, &fields).unwrap().is_none());
    }
    #[test]
    fn text_multifield_resolution_rejects_ambiguity_and_type_conflicts() {
        let mut keyword = field("service.keyword", FieldType::String);
        keyword.source_path = "service".into();
        let mut fields = vec![keyword.clone()];
        assert_eq!(
            mapped_field("service", &fields).unwrap().name,
            "service.keyword"
        );
        keyword.name = "service.raw".into();
        fields.push(keyword);
        assert!(mapped_field("service", &fields).is_err());
        assert_eq!(
            mapped_field("service.keyword", &fields).unwrap().name,
            "service.keyword"
        );
        assert!(compile(
            &chip(
                "service.keyword",
                vec![FilterValue::Value {
                    kind: ValueKind::Number,
                    text: "1".into()
                }]
            ),
            &fields
        )
        .is_err());
    }
}
