use super::{
    bind::{self, Clock, Params},
    parse::{Block, SectionDoc, ViewDoc},
    Finding,
};
use crate::{
    log::Query,
    records::{value::local_day, FieldSchema, Fields, Value},
};
use serde_json::Number;
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, Duration};

enum Expr {
    Field(String),
    Literal { text: String, quoted: bool },
    Call(Function, Vec<Expr>),
    Condition { query: String, fields: Vec<String> },
    BoundCondition(bind::BoundQuery),
    BoundLiteral(Value),
}

#[derive(Clone, Copy)]
enum Function {
    Days,
    Day,
    Week,
    Month,
    Coalesce,
    If,
    Len,
    Lower,
}

#[derive(Clone, Copy)]
pub(super) enum Aggregate {
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

pub(super) struct Computations {
    lets: Vec<(String, Expr)>,
    pub aggregates: BTreeMap<String, (Aggregate, Option<String>)>,
}

fn arguments(input: &str) -> Result<Vec<&str>, &'static str> {
    if input.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    for (index, ch) in input.char_indices() {
        if let Some(inside) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == inside {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1).ok_or("Unexpected ')'")?,
            ',' if depth == 0 => {
                result.push(input[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err("Unclosed quote or parenthesis");
    }
    result.push(input[start..].trim());
    Ok(result)
}

fn call(text: &str) -> Result<(&str, Vec<&str>), &'static str> {
    let (name, rest) = text.split_once('(').ok_or("Expected a function call")?;
    let inside = rest
        .strip_suffix(')')
        .ok_or("Expected a closing parenthesis")?;
    Ok((name, arguments(inside)?))
}

fn expression(text: &str, depth: usize) -> Result<Expr, String> {
    if depth > 16 {
        return Err("Expression is nested too deeply".into());
    }
    if text.starts_with(['"', '\'']) {
        let value = if let Some(single) = text
            .strip_prefix('\'')
            .and_then(|text| text.strip_suffix('\''))
        {
            single.replace("\\'", "'").replace("\\\\", "\\")
        } else {
            serde_json::from_str::<String>(text).map_err(|error| error.to_string())?
        };
        return Ok(Expr::Literal {
            text: value,
            quoted: true,
        });
    }
    if text.contains('(') {
        let (name, args) = call(text)?;
        let (function, min, max) = match name {
            "days" => (Function::Days, 2, 2),
            "day" => (Function::Day, 1, 1),
            "week" => (Function::Week, 1, 1),
            "month" => (Function::Month, 1, 1),
            "coalesce" => (Function::Coalesce, 1, usize::MAX),
            "if" => (Function::If, 3, 3),
            "len" => (Function::Len, 1, 1),
            "lower" => (Function::Lower, 1, 1),
            _ => return Err(format!("Unknown function '{name}'")),
        };
        if !(min..=max).contains(&args.len()) {
            return Err(format!("Wrong number of arguments for '{name}'"));
        }
        let args = args
            .into_iter()
            .enumerate()
            .map(|(index, arg)| {
                if matches!(function, Function::If) && index == 0 {
                    if let Ok((_, syntax)) = Query::parse_view(arg) {
                        if !arg.is_empty() {
                            return Ok(Expr::Condition {
                                query: arg.to_owned(),
                                fields: syntax.fields.into_iter().map(|(field, _)| field).collect(),
                            });
                        }
                    }
                }
                expression(arg, depth + 1)
            })
            .collect::<Result<_, _>>()?;
        return Ok(Expr::Call(function, args));
    }
    if text.starts_with('$')
        || matches!(text, "true" | "false" | "null" | "today" | "now")
        || ["today+", "today-", "now+", "now-"]
            .iter()
            .any(|prefix| text.starts_with(prefix))
        || matches!(text.as_bytes().first(), Some(b'-' | b'0'..=b'9'))
    {
        return Ok(Expr::Literal {
            text: text.to_owned(),
            quoted: false,
        });
    }
    if super::parse::field_name(text) {
        Ok(Expr::Field(text.into()))
    } else {
        Err(format!(
            "Expected a field, literal or function, got '{text}'"
        ))
    }
}

fn aggregate(text: &str) -> Result<(Aggregate, Option<String>), String> {
    let (name, args) = call(text)?;
    if name == "count" && args.is_empty() {
        return Ok((Aggregate::Count, None));
    }
    let op = match name {
        "sum" => Aggregate::Sum,
        "min" => Aggregate::Min,
        "max" => Aggregate::Max,
        "avg" => Aggregate::Avg,
        _ => return Err(format!("Invalid aggregate '{text}'")),
    };
    let [field] = args.as_slice() else {
        return Err(format!("Expected one field for '{name}'"));
    };
    if !super::parse::field_name(field) || field.contains(['-', '@']) {
        return Err(format!("Invalid aggregate field '{field}'"));
    }
    Ok((op, Some((*field).to_owned())))
}

impl Expr {
    fn visit(&self, visit: &mut impl FnMut(&Expr)) {
        visit(self);
        if let Self::Call(_, args) = self {
            for arg in args {
                arg.visit(visit);
            }
        }
    }

    fn eval(&self, fields: &Fields, clock: Clock) -> Option<Value> {
        match self {
            Self::Field(field) => bind::get(fields, field).cloned(),
            Self::Literal { .. } => unreachable!("literal is bound before evaluation"),
            Self::BoundLiteral(value) => Some(value.clone()),
            Self::Condition { .. } => unreachable!("condition is bound before evaluation"),
            Self::BoundCondition(query) => Some(Value::Bool(query.matches(fields))),
            Self::Call(function, args) => {
                let eval = |arg: usize| args[arg].eval(fields, clock);
                let as_day = |value: Value| match value {
                    Value::DateTime(at) => local_day(at, clock.zone),
                    other => other.as_date(),
                };
                Some(match function {
                    Function::Days => {
                        let (start, end) = (as_day(eval(0)?)?, as_day(eval(1)?)?);
                        Value::Number((end - start).whole_days().into())
                    }
                    Function::Day => Value::Date(as_day(eval(0)?)?),
                    Function::Week => {
                        let day = as_day(eval(0)?)?;
                        Value::Date(day.checked_sub(Duration::days(
                            day.weekday().number_days_from_monday().into(),
                        ))?)
                    }
                    Function::Month => {
                        let day = as_day(eval(0)?)?;
                        Value::Date(Date::from_calendar_date(day.year(), day.month(), 1).ok()?)
                    }
                    Function::Coalesce => {
                        return args.iter().find_map(|arg| {
                            arg.eval(fields, clock)
                                .filter(|value| *value != Value::Null)
                        })
                    }
                    Function::If => {
                        let Value::Bool(cond) = eval(0)? else {
                            return None;
                        };
                        return eval(if cond { 1 } else { 2 });
                    }
                    Function::Len => {
                        let Value::List(values) = eval(0)? else {
                            return None;
                        };
                        Value::Number(values.len().into())
                    }
                    Function::Lower => {
                        let Value::String(text) = eval(0)? else {
                            return None;
                        };
                        Value::String(text.to_lowercase())
                    }
                })
            }
        }
    }
}

fn prepare(block: &Block) -> (Computations, Vec<(&'static str, String)>) {
    let mut pending = BTreeMap::<_, (_, BTreeSet<_>)>::new();
    let mut errors = Vec::new();
    for (name, text) in &block.lets {
        match expression(text, 0) {
            Ok(expr @ Expr::Call(..)) => {
                let mut dependencies = BTreeSet::new();
                expr.visit(&mut |expr| {
                    let fields = match expr {
                        Expr::Field(key) => std::slice::from_ref(key),
                        Expr::Condition { fields, .. } => fields.as_slice(),
                        _ => &[],
                    };
                    dependencies.extend(
                        fields
                            .iter()
                            .filter(|key| block.lets.contains_key(*key))
                            .cloned(),
                    );
                });
                pending.insert(name.clone(), (expr, dependencies));
            }
            Ok(_) => errors.push(("let", format!("'{name}' must be a function call"))),
            Err(error) => errors.push(("let", format!("'{name}': {error}"))),
        }
    }
    let mut lets = Vec::new();
    while !pending.is_empty() {
        let Some((name, _)) = pending.iter().find(|(_, (_, dependencies))| {
            dependencies.iter().all(|name| !pending.contains_key(name))
        }) else {
            errors.push((
                "let",
                format!(
                    "Cyclic expressions: {}",
                    pending.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            ));
            break;
        };
        let name = name.clone();
        let (expr, _) = pending.remove(&name).expect("pending expression");
        lets.push((name, expr));
    }
    let mut aggregates = BTreeMap::new();
    for (name, text) in &block.agg {
        match aggregate(text) {
            Ok(agg) => {
                aggregates.insert(name.clone(), agg);
            }
            Err(error) => errors.push(("agg", format!("'{name}': {error}"))),
        }
    }
    (Computations { lets, aggregates }, errors)
}

pub(super) fn validate(
    doc: &ViewDoc,
    section: &SectionDoc,
    block: &Block,
    schema: &FieldSchema,
) -> Vec<Finding> {
    let (compiled, errors) = prepare(block);
    let mut findings: Vec<_> = errors
        .into_iter()
        .map(|(key, error)| section.finding(&doc.path, key, error))
        .collect();
    let unknown = |key: &str| {
        !block.jsonl() && !block.lets.contains_key(key) && block.field(schema, key).is_none()
    };
    let clock = Clock {
        at: chrono::DateTime::UNIX_EPOCH.fixed_offset(),
        zone: schema.timezone,
    };
    let mut seen = BTreeSet::new();
    for (_, expr) in &compiled.lets {
        expr.visit(&mut |expr| match expr {
            Expr::Condition { query, .. } => findings.extend(bind::inspect_filter(
                query,
                doc,
                block,
                schema,
                &mut seen,
                |_, message| section.finding(&doc.path, "let", message),
            )),
            Expr::Field(field) if unknown(field) => findings.push(bind::warn(section.finding(
                &doc.path,
                "let",
                format!("Unknown field '{field}'"),
            ))),
            Expr::Literal {
                text,
                quoted: false,
            } if text.starts_with('$') => {
                let name = &text[1..];
                if !doc.front.params.contains_key(name) {
                    findings.push(section.finding(
                        &doc.path,
                        "let",
                        format!("Undeclared parameter '{name}'"),
                    ));
                }
            }
            Expr::Literal { text, quoted } => {
                if let Err(error) = bind::operand(text, *quoted, &Params::new(), clock, None) {
                    findings.push(section.finding(&doc.path, "let", error));
                }
            }
            _ => {}
        });
    }
    for (_, field) in compiled.aggregates.values() {
        if let Some(field) = field.as_ref().filter(|field| unknown(field)) {
            findings.push(bind::warn(section.finding(
                &doc.path,
                "agg",
                format!("Unknown field '{field}'"),
            )));
        }
    }
    for (key, argument) in [("near", &block.near), ("match", &block.matching)] {
        if let Some(name) = argument
            .as_ref()
            .and_then(|argument| argument.strip_prefix('$'))
        {
            if !doc.front.params.contains_key(name) {
                findings.push(section.finding(
                    &doc.path,
                    key,
                    format!("Undeclared parameter '{name}'"),
                ));
            }
        }
    }
    if let Some(links) = &block.links {
        for target in [&links.to, &links.from].into_iter().flatten() {
            if let Some(name) = target
                .strip_prefix('$')
                .filter(|name| !doc.front.params.contains_key(*name))
            {
                findings.push(section.finding(
                    &doc.path,
                    "links",
                    format!("Undeclared parameter '{name}'"),
                ));
            }
        }
    }
    findings
}

impl Computations {
    pub fn compile(block: &Block) -> Self {
        prepare(block).0
    }

    pub fn bind(
        &mut self,
        block: &Block,
        schema: &FieldSchema,
        vars: &Params,
        clock: Clock,
    ) -> Result<(), String> {
        fn bind_expr(
            expr: &mut Expr,
            block: &Block,
            schema: &FieldSchema,
            vars: &Params,
            clock: Clock,
        ) -> Result<(), String> {
            match expr {
                Expr::Literal { text, quoted } => {
                    *expr = Expr::BoundLiteral(bind::operand(text, *quoted, vars, clock, None)?);
                }
                Expr::Condition { query, .. } => {
                    *expr = Expr::BoundCondition(
                        bind::compile_query(query, block, schema, vars, clock)
                            .map_err(|(_, error)| error)?,
                    );
                }
                Expr::Call(_, args) => {
                    for arg in args {
                        bind_expr(arg, block, schema, vars, clock)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        for (_, expr) in &mut self.lets {
            bind_expr(expr, block, schema, vars, clock)?;
        }
        Ok(())
    }

    pub fn apply(&self, fields: &mut Fields, clock: Clock) {
        for (key, expr) in &self.lets {
            if let Some(value) = expr.eval(fields, clock) {
                fields.insert(key.clone(), value);
            } else {
                fields.remove(key);
            }
        }
    }
}

impl Aggregate {
    pub fn compute<'a>(
        self,
        count: usize,
        values: impl Iterator<Item = &'a Value>,
    ) -> Option<Value> {
        match self {
            Self::Count => Some(Value::Number(count.into())),
            Self::Min => values
                .min_by(|left, right| bind::order(left, right))
                .cloned(),
            Self::Max => values
                .max_by(|left, right| bind::order(left, right))
                .cloned(),
            Self::Sum | Self::Avg => {
                let mut sum = 0.0;
                let mut integers = Some(0i128);
                let mut count = 0;
                for value in values {
                    if let Value::Number(number) = value {
                        sum += number.as_f64()?;
                        count += 1;
                        integers = integers
                            .zip(
                                number
                                    .as_i64()
                                    .map(i128::from)
                                    .or_else(|| number.as_u64().map(i128::from)),
                            )
                            .and_then(|(sum, number)| sum.checked_add(number));
                    }
                }
                match self {
                    Self::Avg if count == 0 => None,
                    Self::Avg => Number::from_f64(sum / f64::from(count)).map(Value::Number),
                    _ => integers
                        .and_then(|sum| {
                            i64::try_from(sum)
                                .ok()
                                .map(Number::from)
                                .or_else(|| u64::try_from(sum).ok().map(Number::from))
                        })
                        .or_else(|| Number::from_f64(sum))
                        .map(Value::Number),
                }
            }
        }
    }
}
