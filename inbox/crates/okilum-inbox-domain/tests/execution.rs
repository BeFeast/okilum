use okilum_inbox_domain::execution::*;
use uuid::Uuid;
#[test]
fn replies_bind_exact_fields_revision_and_capability() {
    let q = Question {
        approval: None,
        thread_title: None,
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        source: QuestionSource {
            worker_id: None,
            record_kind: SourceRecordKind::Question,
            kind: SourceKind::T3,
            instance_id: "instance".into(),
            project_id: "project".into(),
            thread_id: "thread".into(),
            question_id: "question".into(),
            generation: "attempt".into(),
        },
        source_revision: "opaque".into(),
        state: QuestionState::Pending,
        can_reply: true,
        fields: vec![QuestionField {
            id: "colour".into(),
            prompt: "Colour?".into(),
            options: vec![QuestionOption {
                id: "blue".into(),
                label: "Blue".into(),
            }],
            allow_text: false,
            multiple: false,
        }],
    };
    let reply = Reply {
        operation_id: Uuid::new_v4(),
        question_id: q.id,
        expected_revision: q.source_revision.clone(),
        answers: vec![AnswerField {
            id: "colour".into(),
            text: String::new(),
            option_ids: vec!["blue".into()],
        }],
    };
    q.validate_reply(&reply).unwrap();
    let mut bad = reply.clone();
    bad.expected_revision = "old".into();
    assert!(q.validate_reply(&bad).is_err());
    bad = reply.clone();
    bad.answers[0].option_ids = vec!["green".into()];
    assert!(q.validate_reply(&bad).is_err());
    bad = reply.clone();
    bad.answers[0].text = "unadvertised".into();
    assert!(q.validate_reply(&bad).is_err());
    bad = reply.clone();
    bad.answers.push(reply.answers[0].clone());
    assert!(q.validate_reply(&bad).is_err());
    let mut closed = q.clone();
    closed.state = QuestionState::Answered;
    closed.can_reply = false;
    assert!(closed.validate_reply(&reply).is_err());
    closed = q;
    closed.can_reply = false;
    assert!(closed.validate_reply(&reply).is_err());
}

#[test]
fn maestro_approval_is_typed_and_never_accepts_free_text_authority() {
    let mut q:Question=serde_json::from_value(serde_json::json!({
        "id":Uuid::new_v4(),"project_id":Uuid::new_v4(),
        "source":{"kind":"maestro","record_kind":"approval","instance_id":"instance","project_id":"pilot","thread_id":"approval","question_id":"a1","generation":"approval"},
        "source_revision":"opaque-digest","state":"pending","can_reply":true,
        "approval":{"action":"merge_pr","target":{"repository":"fixture","number":1},"summary":"Merge fixture","risk":"high","payload_hash":"hash","target_state_hash":null},
        "fields":[{"id":"decision","prompt":"Merge fixture?","options":[{"id":"approve","label":"Approve"},{"id":"reject","label":"Reject"}],"allow_text":false,"multiple":false}]
    })).unwrap();
    assert!(q.validate().is_ok());
    let mut r = Reply {
        operation_id: Uuid::new_v4(),
        question_id: q.id,
        expected_revision: q.source_revision.clone(),
        answers: vec![AnswerField {
            id: "decision".into(),
            text: String::new(),
            option_ids: vec!["approve".into()],
        }],
    };
    assert!(q.validate_reply(&r).is_ok());
    r.answers[0].text = "please approve".into();
    assert!(q.validate_reply(&r).is_err());
    r.answers[0].text.clear();
    r.expected_revision = "stale".into();
    assert!(q.validate_reply(&r).is_err());
    q.fields[0].allow_text = true;
    assert!(q.validate().is_err());
    q.fields[0].allow_text = false;
    q.approval.as_mut().unwrap().action = "stop_worker".into();
    assert!(q.validate().is_err());
    q.approval.as_mut().unwrap().action = "merge_pr".into();
    q.source.kind = SourceKind::T3;
    assert!(q.validate().is_err());
}

#[test]
fn old_t3_source_serialization_remains_byte_compatible() {
    let source = serde_json::json!({"kind":"t3","instance_id":"instance","project_id":"project","thread_id":"thread","question_id":"q","generation":"attempt"});
    let s: QuestionSource = serde_json::from_value(source.clone()).unwrap();
    assert_eq!(serde_json::to_value(s).unwrap(), source);
}
