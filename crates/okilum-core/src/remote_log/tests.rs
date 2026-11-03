use super::*;
use discover::{Discover, TimeRange};
use serde_json::json;

pub(super) fn url(app: &str, global: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("_a", app)
        .append_pair("_g", global)
        .finish();
    format!("https://logs.example/s/work/app/discover#/?{query}")
}
fn sample() -> String {
    url("(dataSource:(dataViewId:'remote-*:events',type:dataView),query:(language:kuery,query:'service:שלום'),columns:!(message,request_id),sort:!(!('@timestamp',desc)),filters:!((meta:(disabled:!t,negate:!f,key:message),query:(match_phrase:(message:old))),(meta:(disabled:!f,negate:!f,index:'remote-*:events',key:request_id),query:(match_phrase:(request_id:'a%2Fb')))))", "(time:(from:'2026-10-20T12:00:00.123456789Z',to:'2026-10-20T12:15:00Z'),refreshInterval:(pause:!t,value:0),filters:!(('$state':(store:globalState),meta:(disabled:!f,negate:!t),query:(term:(service:noise)))))")
}
#[test]
fn full_real_shape_import_preserves_semantics_and_decodes_once() {
    let (endpoint, state) = discover::import(&sample()).unwrap();
    assert_eq!(endpoint, "https://logs.example/s/work");
    assert_eq!(state.query.query, "service:שלום");
    assert_eq!(state.filters.len(), 3);
    assert_eq!(state.filters[0]["$state"]["store"], "globalState");
    assert_eq!(state.filters[1]["meta"]["disabled"], true);
    assert_eq!(
        state.filters[2]["query"]["match_phrase"]["request_id"],
        "a%2Fb"
    );
    let body = state
        .search_body(
            &state.time,
            Some(&json!([
                "2026-10-20T12:00:00.123456789Z",
                9223372036854775807u64
            ])),
            200,
        )
        .unwrap();
    assert_eq!(body["sort"][0]["@timestamp"]["order"], "desc");
    assert_eq!(body["search_after"][1], 9223372036854775807u64);
    assert_eq!(
        body["query"]["bool"]["must_not"][0],
        json!({"term":{"service":"noise"}})
    );
    let filter = body["query"]["bool"]["filter"].as_array().unwrap();
    assert!(!filter.iter().any(|v| v["match_phrase"]["message"] == "old"));
    assert!(filter.iter().any(|v| v["kql"]["query"] == "service:שלום"));
    assert!(!serde_json::to_string(&body)
        .unwrap()
        .contains("query_string"));
    assert!(state.refresh_paused);
}
#[test]
fn legacy_index_fallback_and_unknown_semantics_fail_closed() {
    let app = "(index:'remote-*:events',query:(language:kuery,query:''))";
    let (_, state) = discover::import(&url(app, "()")).unwrap();
    assert_eq!(
        state.time,
        TimeRange {
            from: "now-15m".into(),
            to: "now".into()
        }
    );
    assert_eq!(state.sort[0].order, "desc");
    for bad in [
        "(index:x,query:(language:lucene,query:a))",
        "(index:x,unknownSemantic:!t)",
        "(index:x,filters:!((meta:(disabled:no),query:(term:(a:b)))))",
        "(index:x,dataSource:(dataViewId:y,type:dataView))",
    ] {
        assert!(discover::import(&url(bad, "()")).is_err(), "{bad}");
    }
    assert!(
        discover::import(&sample().replace("https://logs", "https://key:secret@logs")).is_err()
    );
    assert!(discover::import(&sample().replace("https:", "http:")).is_err());
}
#[test]
fn date_math_freezes_a_single_window_and_rejects_unsupported_forms() {
    let now = time::OffsetDateTime::parse(
        "2026-10-20T12:15:00.123456789Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let range = TimeRange {
        from: "now-15m".into(),
        to: "now".into(),
    }
    .resolve(now)
    .unwrap();
    assert_eq!(range.from, "2026-10-20T12:00:00.123456789Z");
    assert_eq!(range.to, "2026-10-20T12:15:00.123456789Z");
    assert!(TimeRange {
        from: "now-1M".into(),
        to: "now".into()
    }
    .resolve(now)
    .is_err());
}
#[test]
fn sources_are_vault_scoped_durable_and_contain_no_key_or_original_url() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let personal = dir.path().join("personal");
    let source = Source::import("Work", &sample()).unwrap();
    save_source(dir.path(), &work, &source).unwrap();
    assert_eq!(load_sources(dir.path(), &work).unwrap()[0].id, source.id);
    assert!(load_sources(dir.path(), &personal).unwrap().is_empty());
    assert_ne!(
        source.credential_url(&work),
        source.credential_url(&personal)
    );
    let mut changed = source.clone();
    changed.endpoint = "https://attacker.example".into();
    assert_ne!(source.credential_url(&work), changed.credential_url(&work));
    let persisted = serde_json::to_value(&source).unwrap();
    assert!(persisted.get("key").is_none());
    assert!(!persisted["endpoint"].as_str().unwrap().contains("_a="));
    assert_eq!(
        format!(
            "{:?}",
            transport::ApiKey::new(b"test-secret".to_vec()).unwrap()
        ),
        "ApiKey([redacted])"
    );
}

// Used by transport's real loopback-HTTP controls; never a live compatibility claim.
#[test]
fn ambiguous_enabled_filters_and_unproven_time_rounding_refuse_execution() {
    let mut state = state();
    state.filters = vec![
        json!({"meta":{"disabled":false},"query":{"term":{"a":"b"}},"range":{"@timestamp":{"gte":"now-1m"}}}),
    ];
    assert!(state
        .validate()
        .unwrap_err()
        .to_string()
        .contains("exactly one"));
    // Keeping a disabled filter is intentional; enabling it must not select
    // one of its clauses invisibly by precedence.
    state.filters[0]["meta"]["disabled"] = json!(true);
    assert!(state.validate().is_ok());
    state.filters.clear();
    state.time.to = "now-1d/d".into();
    assert!(state
        .validate()
        .unwrap_err()
        .to_string()
        .contains("Rounded date math"));
    state.time.to = "now".into();
    state.time.from = "now-2d/d".into();
    assert!(state
        .validate()
        .unwrap_err()
        .to_string()
        .contains("Rounded date math"));
}

pub(super) fn state() -> Discover {
    discover::import(&sample()).unwrap().1
}

#[test]
fn pinned_scope_fields_roundtrip_without_changing_legacy_sources() {
    let root = tempfile::tempdir().unwrap();
    let mut source = Source::direct("Synthetic", "https://logs.example", "events", "").unwrap();
    let mut legacy = serde_json::to_value(&source).unwrap();
    legacy.as_object_mut().unwrap().remove("scope_fields");
    assert!(serde_json::from_value::<Source>(legacy)
        .unwrap()
        .scope_fields
        .is_empty());
    source.scope_fields = vec!["service".into(), "context.operation".into()];
    save_source(root.path(), root.path(), &source).unwrap();
    assert_eq!(
        load_sources(root.path(), root.path()).unwrap()[0].scope_fields,
        source.scope_fields
    );
    source.scope_fields.push("service".into());
    assert!(source.validate().is_err(), "duplicate pins are rejected");
}

#[test]
fn role_field_identity_is_exhaustive_and_never_chooses_a_winner() {
    let source = Source::direct("Synthetic", "https://logs.example", "events", "").unwrap();
    let first = Arc::new(json!({"_index":"events","_id":"1","_source":{"level":"INFO"}}));
    let one = EventWindow::new(vec![first.clone()], &source).unwrap();
    assert_eq!(one.role_field(crate::log::Role::Level), Some("level"));
    let mixed = EventWindow::new(
        vec![
            first,
            Arc::new(json!({"_index":"events","_id":"2","_source":{"severity":"WARN"}})),
        ],
        &source,
    )
    .unwrap();
    assert_eq!(
        mixed.role_field(crate::log::Role::Level),
        None,
        "different underlying keys cannot be silently treated as one server field"
    );
}
