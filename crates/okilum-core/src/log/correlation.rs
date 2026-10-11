//! Request correlation (#1166): ingress, app and worker logs share an id
//! under different names. This finds a record's id and builds the query that
//! shows every record carrying it, whatever the file calls the field.

use super::query::{Expr, FieldRef, Literal, Test};
use super::Record;

/// Keys that hold a request or trace id, as loggers and proxies write them.
/// Matching is ASCII case-insensitive on the whole dotted key.
pub const KEYS: &[&str] = &[
    "request_id",
    "requestId",
    "req_id",
    "reqId",
    "x-request-id",
    "x_request_id",
    "http.request.id",
    "trace_id",
    "traceId",
    "trace.id",
    "dd.trace_id",
    "correlation_id",
    "correlationId",
];

/// The W3C header; its second part is the trace id.
const TRACEPARENT: &str = "traceparent";

/// One id of a record: the key it was found under and the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId {
    pub key: String,
    pub value: String,
}

/// The ids in `record`, request ids first, then trace ids, without repeats.
/// A `traceparent` contributes its trace id.
pub fn ids(record: &Record) -> Vec<RequestId> {
    let mut found: Vec<RequestId> = Vec::new();
    let mut push = |key: &str, value: &str| {
        let value = value.trim();
        if value.is_empty() || value == "-" || found.iter().any(|id| id.value == value) {
            return;
        }
        found.push(RequestId {
            key: key.to_owned(),
            value: value.to_owned(),
        });
    };
    for name in KEYS {
        for field in &record.fields {
            if field.key.eq_ignore_ascii_case(name) {
                push(&field.key, &field.value);
            }
        }
    }
    for field in &record.fields {
        if field.key.eq_ignore_ascii_case(TRACEPARENT) {
            if let Some(trace) = trace_of(&field.value) {
                push(&field.key, trace);
            }
        }
    }
    found
}

/// `00-<32 hex trace id>-<16 hex span id>-<flags>` → the trace id.
fn trace_of(traceparent: &str) -> Option<&str> {
    let mut parts = traceparent.trim().split('-');
    let _version = parts.next()?;
    let trace = parts.next()?;
    (trace.len() == 32 && trace.bytes().all(|b| b.is_ascii_hexdigit())).then_some(trace)
}

/// The query for every record carrying `id`: the id equal under any known
/// key, or inside a `traceparent`.
pub fn query(id: &str) -> Expr {
    let equal = |key: &str| Expr::Field {
        field: FieldRef::Key(key.to_owned()),
        test: Test::Eq(Literal {
            text: id.to_owned(),
            number: None,
        }),
        include_absent: false,
    };
    let mut terms: Vec<Expr> = KEYS.iter().map(|key| equal(key)).collect();
    terms.push(Expr::Field {
        field: FieldRef::Key(TRACEPARENT.to_owned()),
        test: Test::Contains(id.to_owned()),
        include_absent: false,
    });
    Expr::Or(terms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Format, LogIndex, Query};

    fn record(line: &str) -> Record {
        Record::parse(line.as_bytes(), Format::JsonLines).unwrap()
    }

    #[test]
    fn ids_come_from_known_keys_and_traceparent() {
        let r = record(
            r#"{"msg":"GET /cart","x-request-id":"7b1c","trace_id":"4bf92f3577b34da6a3ce929d0e0e4736","user":"u1"}"#,
        );
        let found: Vec<_> = ids(&r).into_iter().map(|id| (id.key, id.value)).collect();
        assert_eq!(
            found,
            [
                ("x-request-id".into(), "7b1c".into()),
                ("trace_id".into(), "4bf92f3577b34da6a3ce929d0e0e4736".into())
            ]
        );
        let w3c = record(
            r#"{"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01","request_id":"-"}"#,
        );
        assert_eq!(ids(&w3c)[0].value, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert!(ids(&record(r#"{"msg":"no id","user_id":"42"}"#)).is_empty());
    }

    #[test]
    fn the_query_matches_the_id_under_any_key_and_parses_back() {
        let expr = query("7b1c");
        let text = expr.to_string();
        let parsed = Query::parse(&text).unwrap();
        assert_eq!(parsed.expr(), &expr, "{text}");
        for (line, matches) in [
            (r#"{"x-request-id":"7b1c"}"#, true),
            (r#"{"req_id":"7b1c"}"#, true),
            (
                r#"{"traceparent":"00-0000000000000000000000000000007b1c-0000000000000001-01"}"#,
                true,
            ),
            (r#"{"req_id":"7b1cd"}"#, false),
            (r#"{"user":"7b1c"}"#, false),
        ] {
            let index = LogIndex::build(format!("{line}\n").as_bytes());
            assert_eq!(
                parsed.matches_line(&index.entries()[0], line.as_bytes(), Format::JsonLines),
                matches,
                "{line}"
            );
        }
    }
}
