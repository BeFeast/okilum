use tessera_inbox_domain::execution::*;
use uuid::Uuid;
#[test]
fn replies_bind_exact_fields_revision_and_capability() {
    let q = Question {
        thread_title: None,
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        source: QuestionSource {
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
