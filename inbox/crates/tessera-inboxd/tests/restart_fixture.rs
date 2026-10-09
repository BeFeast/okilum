//! Disposable hosted deployment fixture. Never loads a real owner or credential.
use tessera_inbox_domain::{OwnerId, execution::*};
use tessera_inboxd::{auth::Auth, store::Store};
use url::Url;
use uuid::Uuid;
use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
fn fixture(store: &mut Store, who: OwnerId) -> (OwnerId, Question, Reply) {
    let project = Uuid::new_v4();
    store
        .save_execution_project(
            who,
            &SaveProject {
                operation_id: Uuid::new_v4(),
                project_id: project,
                expected_revision: 0,
                draft: ProjectDraft {
                    title: "Pilot".into(),
                    status: String::new(),
                    next_step: String::new(),
                },
            },
        )
        .unwrap();
    let q = Question {
        approval: None,
        thread_title: None,
        id: Uuid::new_v4(),
        project_id: project,
        source: QuestionSource {
            worker_id: None,
            record_kind: SourceRecordKind::Question,
            kind: SourceKind::T3,
            instance_id: "instance".into(),
            project_id: "project".into(),
            thread_id: "thread".into(),
            question_id: "native-request".into(),
            generation: "attempt".into(),
        },
        source_revision: "opaque-1".into(),
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
    store.observe_execution_question(who, &q, 1).unwrap();
    let r = Reply {
        operation_id: Uuid::new_v4(),
        question_id: q.id,
        expected_revision: q.source_revision.clone(),
        answers: vec![AnswerField {
            id: "colour".into(),
            text: String::new(),
            option_ids: vec!["blue".into()],
        }],
    };
    (who, q, r)
}

#[test]
#[ignore = "writes only the explicitly requested disposable deployment fixture"]
fn create_restart_fixture() {
    let path = std::path::PathBuf::from(std::env::var("RESTART_FIXTURE_DB").unwrap());
    assert!(!path.exists(), "never replace an existing database");
    let origin = "https://localhost:8443";
    let mut auth = Auth::new(Store::open(&path).unwrap(), origin).unwrap();
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let token = auth.bootstrap(1000).unwrap();
    let (flow, options) = auth.register_start(&token, 1000).unwrap();
    let response = key
        .do_registration(Url::parse(origin).unwrap(), options)
        .unwrap();
    auth.register_finish(&flow, &response, 1000).unwrap();
    drop(auth);
    let mut store = Store::open(&path).unwrap();
    let (who, _, reply) = fixture(&mut store, OwnerId(Uuid::new_v4()));
    store.prepare_execution_reply(who, &reply).unwrap();
    store
        .advance_execution_reply(
            who,
            reply.operation_id,
            DeliveryState::Queued,
            DeliveryState::Uncertain,
            None,
            None,
        )
        .unwrap();
    store
        .advance_execution_reply(
            who,
            reply.operation_id,
            DeliveryState::Uncertain,
            DeliveryState::Accepted,
            Some("fixture-delivery"),
            None,
        )
        .unwrap();
    store
        .advance_execution_reply(
            who,
            reply.operation_id,
            DeliveryState::Accepted,
            DeliveryState::Delivered,
            Some("fixture-delivery"),
            None,
        )
        .unwrap();
}
