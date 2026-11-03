use super::*;
use serde_json::json;

fn report(created: &str, comments: Option<u64>) -> Issue {
    serde_json::from_value(json!({"number":42,"title":"A tester report","state":"open",
        "labels":[{"name":"from-tester"}],"created_at":created,"comments":comments,
        "user":{"id":4,"login":"filing-bot"}}))
    .unwrap()
}
#[test]
fn waiting_questions_parse_only_open_owner_bullets_and_first_separator() {
    let source = "---\ntype: Note\n---\n# Questions\n- [Ignore] Before\n## Open\n\n- [Sync owner] Ship the video? — yes — next work\n  - [Nested] Not a separate question\n- [Board] Which view?\n```md\n- [Example] Not a question\n```\n## For the daily note\n- [Ignore] Not open\n";
    let questions = parse_questions(source, Some(100)).unwrap();
    assert_eq!(
        questions,
        vec![
            Question {
                owner: "Sync owner".into(),
                title: "Ship the video?".into(),
                modified_at: Some(100)
            },
            Question {
                owner: "Board".into(),
                title: "Which view?".into(),
                modified_at: Some(100)
            },
        ]
    );
    assert_eq!(
        parse_questions(&source.replace('\n', "\r\n"), Some(100)).unwrap(),
        questions
    );
    assert!(parse_questions("## Open\n\n## Closed\n- [Owner] Old", None)
        .unwrap()
        .is_empty());
    for invalid in [
        "## Closed\n",
        "## Open\n- missing owner",
        "## Open\n- [] Empty",
        "## Open\n- [Owner]  — options",
        "## Open\n## Open\n",
    ] {
        assert!(parse_questions(invalid, None).is_err(), "{invalid}");
    }
}
#[test]
fn waiting_new_report_requires_under_24h_open_origin_and_verified_comments() {
    let mut issue = report("1970-01-02T00:00:00Z", Some(0));
    assert!(new_report(&issue, 86400));
    assert!(new_report(&issue, 172799));
    assert!(!new_report(&issue, 172800));
    assert!(!new_report(&issue, 86399));
    issue.report.comments = Some(2);
    assert!(!new_report(&issue, 86500));
    issue.report.filing_bot_only = true;
    assert!(new_report(&issue, 86500));
    issue.state = "closed".into();
    assert!(!new_report(&issue, 86500));
    issue.state = "open".into();
    issue.labels.clear();
    assert!(!new_report(&issue, 86500));
    issue.labels.push(super::super::IssueLabel {
        name: "from-oleg".into(),
    });
    assert!(new_report(&issue, 86500));
    issue.report.created_at = Some("invalid".into());
    assert!(!new_report(&issue, 86500));
    assert!(!new_report(&report("1970-01-02T00:00:00Z", None), 86500));
}
#[test]
fn waiting_empty_projection_is_hidden_and_report_order_is_stable() {
    let binding =
        Binding::parse(include_str!("../../../tests/fixtures/board-legacy.yaml")).unwrap();
    let context = context::Snapshot::default();
    let delivery = delivery::Snapshot::default();
    let empty = project(
        &context,
        &delivery,
        &binding,
        &BTreeMap::new(),
        86500,
        86500,
    );
    assert!(!empty.visible());
    assert!(empty.problem.is_none());
    let mut newer = report("1970-01-02T00:01:00Z", Some(0));
    newer.number = 43;
    let issues = BTreeMap::from([(42, report("1970-01-02T00:00:00Z", Some(0))), (43, newer)]);
    let result = project(&context, &delivery, &binding, &issues, 86500, 86500);
    assert_eq!(result.items.len(), 2);
    assert!(result.items[0].title.starts_with("#43"));
    assert!(result.items[1].target.ends_with("/issues/42"));
    // A failed Forgejo source freezes expiry at its last successful read time.
    assert_eq!(
        project(&context, &delivery, &binding, &issues, 200000, 86500)
            .items
            .len(),
        2
    );
}
