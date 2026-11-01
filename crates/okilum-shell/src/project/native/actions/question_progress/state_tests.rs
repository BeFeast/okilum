use super::*;
use ::core::prelude::v1::test;
use okilum_t3::actions::{Intent, State};
use serde_json::json;

fn question(id: &str, required: bool) -> Value {
    json!({
        "id": id, "header": "Choose an approach", "question": "Which approach should be used?",
        "required": required, "allowCustomAnswer": false,
        "options": [{"value": " exact ", "label": "Exact"}, {"value": "other", "label": "Other"}]
    })
}

fn request(questions: Vec<Value>) -> Request {
    Request {
        id: "runtime-request".into(),
        item: json!({
            "id": "request-row", "requestId": "runtime-request", "threadId": "thread",
            "runId": "run", "nodeId": "node", "type": "user_input_request",
            "status": "pending", "questions": questions
        }),
        answerable: true,
        dismissible: false,
        response_mode: Some("live".into()),
        reason: None,
    }
}

fn answers(values: &[(&str, Value)]) -> BTreeMap<String, Value> {
    values
        .iter()
        .map(|(id, value)| ((*id).into(), value.clone()))
        .collect()
}

fn intent(request: &Request, answers: &BTreeMap<String, Value>) -> Intent {
    Intent::Respond {
        attachments_by_question_id: Default::default(),
        request_id: request.id.clone(),
        decision: None,
        answers: Some(answers.clone()),
    }
}

fn state(request: &Request) -> State {
    State {
        ready: true,
        requests: vec![request.clone()],
        ..State::default()
    }
}

#[test]
fn malformed_or_duplicate_question_ids_disable_progress_without_picking_a_winner() {
    let valid = request(vec![question("q1", true)]);
    let progress = QuestionProgress::new(&valid);
    assert!(progress.accepts(&progress.transition(&valid.id).unwrap()));
    let answer = answers(&[("q1", json!(" exact "))]);
    assert!(question_complete(&valid, "q1", &answer));
    assert!(state(&valid).allows(&intent(&valid, &answer)));

    for questions in [
        json!([]),
        Value::Null,
        json!({"q1": question("q1", true)}),
        json!([question("q1", true), question("q1", true)]),
        json!([question("q1", true), question("", false)]),
        json!([question("q1", true), question(" \n", false)]),
        json!([question("q1", true), {"id": 12, "required": false}]),
        json!([question("q1", true), {"required": false}]),
    ] {
        let mut malformed = valid.clone();
        malformed.item["questions"] = questions;
        let mut progress = QuestionProgress::new(&malformed);
        assert!(progress.position().is_none(), "{:?}", malformed.item);
        assert!(progress.transition(&malformed.id).is_none());
        assert!(!progress.next());
        assert!(!question_complete(&malformed, "q1", &answer));
        assert!(!state(&malformed).allows(&intent(&malformed, &answer)));
    }
}

#[test]
fn previous_and_next_preserve_question_identity_and_question_scoped_collapse() {
    let request = request(vec![
        question("q1", true),
        question("q2", true),
        question("q3", false),
    ]);
    let mut progress = QuestionProgress::new(&request);
    let first = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&first));
    progress.toggle_collapsed();
    assert!(progress.collapsed());
    assert!(!progress.accepts(&first));
    assert!(progress.next());
    assert_eq!(progress.active(), Some("q2"));
    assert!(!progress.collapsed(), "a different question opens");
    let second = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&second));
    assert!(progress.previous());
    assert_eq!(progress.active(), Some("q1"));
    assert!(
        progress.collapsed(),
        "returning to the stored collapsed identity matches T3"
    );
    assert!(!progress.accepts(&second));
    progress.toggle_collapsed();
    assert!(!progress.collapsed());
    assert!(progress.next());
    assert!(progress.next());
    assert_eq!(progress.position(), Some((2, 3)));
    assert!(!progress.next(), "the final step never wraps");
}

#[test]
fn canonical_reorder_retains_active_id_and_removal_discards_its_stale_transition() {
    let mut request = request(vec![
        question("q1", true),
        question("q2", true),
        question("q3", true),
    ]);
    let mut progress = QuestionProgress::new(&request);
    assert!(progress.next());
    let old = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&old));
    progress.toggle_collapsed();
    request.item["questions"] = json!([
        question("q3", true),
        question("q1", true),
        question("q2", true)
    ]);
    assert!(progress.reconcile(&request));
    assert_eq!(progress.active(), Some("q2"));
    assert_eq!(progress.position(), Some((2, 3)));
    assert!(progress.collapsed());
    assert!(!progress.accepts(&old));

    request.item["questions"] = json!([question("q3", true), question("q1", true)]);
    assert!(progress.reconcile(&request));
    assert_eq!(progress.active(), Some("q3"));
    assert!(!progress.collapsed());
    let fresh = progress.transition(&request.id).unwrap();
    assert!(
        progress.accepts(&fresh),
        "a valid surviving question remains actionable"
    );
    assert!(!progress.accepts(&old));
}

#[test]
fn replacement_row_resets_progress_and_cannot_receive_old_submission_completion() {
    let mut request = request(vec![question("q1", true), question("q2", true)]);
    let mut progress = QuestionProgress::new(&request);
    progress.next();
    progress.toggle_collapsed();
    progress.begin_submission();
    let old_submission = progress.transition(&request.id).unwrap();
    assert!(progress.same_submission(&old_submission));

    request.item["id"] = json!("replacement-row");
    assert!(progress.reconcile(&request));
    assert_eq!(progress.active(), Some("q1"));
    assert!(!progress.collapsed());
    assert!(!progress.submitting);
    assert!(!progress.same_instance(&old_submission));
    assert!(!progress.accepts(&old_submission));
    progress.begin_submission();
    let fresh_submission = progress.transition(&request.id).unwrap();
    assert!(progress.same_submission(&fresh_submission));
    assert!(!progress.same_submission(&old_submission));
    assert!(
        !progress.same_instance(&old_submission),
        "old completion cannot release the replacement latch"
    );
}

#[test]
fn invalidation_and_navigation_never_revalidate_an_old_timer_token() {
    let request = request(vec![question("q1", true), question("q2", true)]);
    let mut progress = QuestionProgress::new(&request);
    let selected = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&selected));
    progress.invalidate();
    assert!(!progress.accepts(&selected));
    let replacement_click = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&replacement_click));
    progress.next();
    progress.previous();
    assert_eq!(progress.active(), Some("q1"));
    assert!(!progress.accepts(&selected));
    assert!(
        !progress.accepts(&replacement_click),
        "returning to the same question does not revive an old timer"
    );
    assert!(progress.accepts(&progress.transition(&request.id).unwrap()));
    let separate_instance = QuestionProgress::new(&request);
    assert!(!separate_instance.accepts(&progress.transition(&request.id).unwrap()));
}

#[test]
fn rendered_control_survives_local_edits_but_not_schema_instance_or_active_question_change() {
    let mut request = request(vec![question("q1", true), question("q2", true)]);
    let mut progress = QuestionProgress::new(&request);
    let rendered_control = progress.transition(&request.id).unwrap();
    assert!(progress.interaction_current(&rendered_control));
    assert!(
        progress.accepts(&rendered_control),
        "positive initial timer control"
    );

    progress.invalidate(); // A choice click cancels and replaces its timer.
    assert!(
        progress.interaction_current(&rendered_control),
        "the rendered button must accept another click before repaint"
    );
    assert!(
        !progress.accepts(&rendered_control),
        "the first click's old timer must stay cancelled"
    );
    let replaced_timer = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&replaced_timer));
    progress.invalidate(); // Another local selection/custom edit changes no schema.
    assert!(progress.interaction_current(&rendered_control));
    assert!(!progress.accepts(&replaced_timer));
    assert!(progress.accepts(&progress.transition(&request.id).unwrap()));

    assert!(progress.next());
    assert!(
        !progress.interaction_current(&rendered_control),
        "q1's control cannot select an answer for active q2"
    );
    let q2_control = progress.transition(&request.id).unwrap();
    assert!(progress.interaction_current(&q2_control));
    request.item["questions"][1]["options"][0]["label"] = json!("Revised provider label");
    assert!(progress.reconcile(&request));
    assert!(progress.same_instance(&q2_control));
    assert_eq!(progress.active(), Some("q2"));
    assert!(
        !progress.interaction_current(&q2_control),
        "same identities do not authorize a control rendered from the previous schema"
    );
    let fresh_control = progress.transition(&request.id).unwrap();
    assert!(progress.interaction_current(&fresh_control));
    assert!(progress.accepts(&fresh_control));

    let mut remounted = QuestionProgress::new(&request);
    assert!(remounted.next());
    assert_eq!(remounted.active(), Some(fresh_control.question.as_str()));
    assert!(!remounted.same_instance(&fresh_control));
    assert!(
        !remounted.interaction_current(&fresh_control),
        "reused request/question IDs do not authorize the previous instance's control"
    );
    let remounted_control = remounted.transition(&request.id).unwrap();
    assert!(remounted.interaction_current(&remounted_control));
    assert!(remounted.accepts(&remounted_control));
}

#[test]
fn submission_latch_survives_busy_revision_and_same_instance_reconciliation() {
    let mut request = request(vec![question("q1", true)]);
    let mut progress = QuestionProgress::new(&request);
    let timer = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&timer));
    progress.begin_submission();
    let submission = progress.transition(&request.id).unwrap();
    assert!(!progress.accepts(&timer));
    assert!(
        !progress.accepts(&submission),
        "a submission token cannot trigger another advance"
    );
    assert!(progress.same_submission(&submission));

    progress.invalidate(); // The parent starts its busy operation and cancels timers.
    request.item["questions"][0]["header"] = json!("Updated context");
    assert!(progress.reconcile(&request));
    assert!(progress.submitting);
    assert!(!progress.same_submission(&submission));
    assert!(
        progress.same_instance(&submission),
        "completion still owns this in-flight latch"
    );
    progress.finish_submission();
    assert!(!progress.submitting);
    assert!(!progress.same_submission(&submission));
    assert!(!progress.accepts(&timer));
    assert!(
        progress.accepts(&progress.transition(&request.id).unwrap()),
        "pre-retain failure may be retried explicitly"
    );
}

#[test]
fn unchanged_snapshot_preserves_progress_but_response_mode_change_invalidates_timer() {
    let mut request = request(vec![question("q1", true), question("q2", true)]);
    let mut progress = QuestionProgress::new(&request);
    progress.next();
    let selected = progress.transition(&request.id).unwrap();
    assert!(progress.accepts(&selected));
    assert!(!progress.reconcile(&request));
    assert_eq!(progress.active(), Some("q2"));
    request.response_mode = Some("message".into());
    assert!(progress.reconcile(&request));
    assert_eq!(progress.active(), Some("q2"));
    assert!(!progress.accepts(&selected));
    assert!(progress.accepts(&progress.transition(&request.id).unwrap()));
}

#[test]
fn omitted_optional_question_can_advance_but_invalid_supplied_answer_still_blocks() {
    let request = request(vec![
        question("required", true),
        question("optional", false),
    ]);
    let empty = BTreeMap::new();
    assert!(question_complete(&request, "optional", &empty));
    assert!(!question_complete(&request, "required", &empty));
    assert!(!state(&request).allows(&intent(&request, &empty)));
    let required_only = answers(&[("required", json!(" exact "))]);
    assert!(question_complete(&request, "optional", &required_only));
    assert!(state(&request).allows(&intent(&request, &required_only)));
    for invalid in [json!(""), json!("wrong-provider-value"), json!([])] {
        let mut supplied = required_only.clone();
        supplied.insert("optional".into(), invalid);
        assert!(!question_complete(&request, "optional", &supplied));
        assert!(!state(&request).allows(&intent(&request, &supplied)));
    }
    let all_optional = request_for_all_optional();
    assert!(question_complete(&all_optional, "optional", &empty));
    assert!(
        !state(&all_optional).allows(&intent(&all_optional, &empty)),
        "existing Intent::validate rejects an entirely empty response map"
    );
}

fn request_for_all_optional() -> Request {
    request(vec![question("optional", false)])
}

#[test]
fn message_and_live_multi_answers_preserve_exact_values_and_wire_shapes() {
    let values = [" exact ", "line\nvalue"];
    for mode in ["live", "message"] {
        let mut multi = question("multi", true);
        multi["multiSelect"] = json!(true);
        multi["options"] = json!([
            {"value": values[0], "label": "First"},
            {"value": values[1], "label": "Second"}
        ]);
        let mut request = request(vec![multi]);
        request.response_mode = Some(mode.into());
        // The runtime mode, not the card's potentially stale presentation, wins.
        request.item["responseMode"] = json!(if mode == "live" { "message" } else { "live" });
        let array = json!(values);
        let encoded = json!(serde_json::to_string(&values).unwrap());
        let (valid, wrong_shape) = if mode == "live" {
            (array, encoded)
        } else {
            (encoded, array)
        };
        let valid = answers(&[("multi", valid)]);
        assert!(question_complete(&request, "multi", &valid), "{mode}");
        // Use a consistent canonical card for the independent final authority gate.
        request.item["responseMode"] = json!(mode);
        assert!(state(&request).allows(&intent(&request, &valid)), "{mode}");
        assert!(!question_complete(
            &request,
            "multi",
            &answers(&[("multi", wrong_shape)])
        ));
        for invalid_values in [
            vec!["exact", "line\nvalue"],
            vec![" exact ", " exact "],
            vec![],
        ] {
            let invalid = if mode == "message" {
                json!(serde_json::to_string(&invalid_values).unwrap())
            } else {
                json!(invalid_values)
            };
            let invalid = answers(&[("multi", invalid)]);
            assert!(!question_complete(&request, "multi", &invalid), "{mode}");
            assert!(
                !state(&request).allows(&intent(&request, &invalid)),
                "{mode}"
            );
        }
    }
}

#[test]
fn partial_completeness_never_grants_final_response_authority() {
    let request = request(vec![question("q1", true), question("q2", true)]);
    let partial = answers(&[("q1", json!(" exact "))]);
    assert!(question_complete(&request, "q1", &partial));
    assert!(!state(&request).allows(&intent(&request, &partial)));
    let complete = answers(&[("q1", json!(" exact ")), ("q2", json!("other"))]);
    assert!(
        state(&request).allows(&intent(&request, &complete)),
        "positive final-response control"
    );
    let response = intent(&request, &complete);
    let mut unavailable = state(&request);
    unavailable.ready = false;
    assert!(!unavailable.allows(&response));
    unavailable = state(&request);
    unavailable.requests[0].answerable = false;
    assert!(!unavailable.allows(&response));
    unavailable = state(&request);
    unavailable.requests[0].response_mode = None;
    assert!(!unavailable.allows(&response));
    let mut foreign_answer = complete;
    foreign_answer.insert("foreign-question".into(), json!("other"));
    assert!(question_complete(&request, "q1", &foreign_answer));
    assert!(
        !state(&request).allows(&intent(&request, &foreign_answer)),
        "singleton completeness cannot authorize a foreign key"
    );
}
