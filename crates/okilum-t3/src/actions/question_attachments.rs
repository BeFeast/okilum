//! Native runtime-request.respond references, scoped by canonical question ID.
use super::*;
use crate::stream::{
    validate_attachments, ChatAttachment, StreamError, StreamResult, MAX_ATTACHMENTS,
};

pub type QuestionAttachments = BTreeMap<String, Vec<ChatAttachment>>;

/// Validate issued upload references and the flattened response's existing T3
/// limits. Upload readiness and confirmed environment identity belong to the
/// caller's upload state; server intake remains authoritative for asset claims.
pub fn validate_question_attachments(attachments: &QuestionAttachments) -> StreamResult<()> {
    let mut count = 0_usize;
    for (question, references) in attachments {
        if question.trim().is_empty() {
            return Err(StreamError::Protocol(
                "missing attachment question identity",
            ));
        }
        count = count
            .checked_add(references.len())
            .filter(|count| *count <= MAX_ATTACHMENTS)
            .ok_or(StreamError::Protocol(
                "a question response supports at most 8 attachments",
            ))?;
    }
    let references: Vec<_> = attachments.values().flatten().cloned().collect();
    validate_attachments(&references)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{AttachmentKind, AttachmentMetadata, MAX_FILE_BYTES, MAX_IMAGE_BYTES};

    fn attachment(id: &str) -> ChatAttachment {
        ChatAttachment {
            id: id.into(),
            metadata: AttachmentMetadata {
                kind: AttachmentKind::Image,
                name: "question.png".into(),
                mime_type: "image/png".into(),
                size_bytes: 19,
            },
            source: Some(json!({"kind":"issued-fixture-reference"})),
        }
    }

    fn state(mode: &str) -> State {
        State {
            ready: true,
            requests: vec![Request {
                id: "request".into(),
                item: json!({"type":"user_input_request","requestId":"request","questions":[
                    {"id":"choice","question":"Choose","multiSelect":true,"allowCustomAnswer":false,
                     "options":[{"label":"A","value":" exact "},{"label":"B","value":"β"}]},
                    {"id":"file","question":"Show the result","multiSelect":true,"options":[]}
                ]}),
                answerable: true,
                dismissible: mode == "message",
                response_mode: Some(mode.into()),
                reason: None,
            }],
            ..Default::default()
        }
    }

    fn response(mode: &str) -> Intent {
        Intent::Respond {
            request_id: "request".into(),
            decision: None,
            answers: Some(BTreeMap::from([
                (
                    "choice".into(),
                    if mode == "message" {
                        json!("[\" exact \",\"β\"]")
                    } else {
                        json!([" exact ", "β"])
                    },
                ),
                ("file".into(), json!("")),
            ])),
            attachments_by_question_id: BTreeMap::from([(
                "file".into(),
                vec![attachment("uploaded-q2")],
            )]),
        }
    }

    #[test]
    fn native_live_and_message_responses_keep_exact_answers_and_only_the_target_question_assets() {
        for mode in ["live", "message"] {
            let intent = response(mode);
            assert!(state(mode).allows(&intent));
            let wire = Command::new("bound-thread", intent).wire().unwrap();
            assert_eq!(wire["type"], "runtime-request.respond");
            assert_eq!(wire["threadId"], "bound-thread");
            assert_eq!(wire["requestId"], "request");
            assert_eq!(wire["answers"]["file"], "");
            assert_eq!(
                wire["attachmentsByQuestionId"],
                json!({"file":[attachment("uploaded-q2")]})
            );
            assert!(wire.get("attachments").is_none());
            assert!(wire.get("messageId").is_none());
            assert!(wire.get("text").is_none());
            assert!(!state(if mode == "live" { "message" } else { "live" })
                .allows(&serde_json::from_value(wire).unwrap()));
        }
    }

    #[test]
    fn attachments_never_supply_another_questions_answer_or_bypass_provider_choice_rules() {
        let positive = response("live");
        let current = state("live");
        assert!(current.allows(&positive));
        for invalid in [
            "foreign-id",
            "forbidden-custom",
            "missing-answer",
            "missing-file",
            "blank-choice",
        ] {
            let mut intent = positive.clone();
            let Intent::Respond {
                answers: Some(answers),
                attachments_by_question_id,
                ..
            } = &mut intent
            else {
                unreachable!()
            };
            match invalid {
                "foreign-id" => {
                    attachments_by_question_id
                        .insert("other-question".into(), vec![attachment("foreign")]);
                }
                "forbidden-custom" => {
                    attachments_by_question_id
                        .insert("choice".into(), vec![attachment("forbidden")]);
                    answers.insert("choice".into(), json!(""));
                }
                "missing-answer" => {
                    answers.remove("file");
                }
                "missing-file" => {
                    attachments_by_question_id.clear();
                }
                "blank-choice" => {
                    answers.insert("choice".into(), json!(""));
                }
                _ => unreachable!(),
            }
            assert!(!current.allows(&intent), "{invalid}");
        }
        let mut duplicate = current;
        duplicate.requests[0].item["questions"][1]["id"] = json!("choice");
        assert!(
            !duplicate.allows(&positive),
            "ambiguous canonical question IDs cannot accept assets"
        );
    }

    #[test]
    fn selected_provider_choices_may_carry_files_without_authorizing_a_custom_or_empty_answer() {
        for mode in ["live", "message"] {
            let mut intent = response(mode);
            let Intent::Respond {
                attachments_by_question_id,
                ..
            } = &mut intent
            else {
                unreachable!()
            };
            attachments_by_question_id.insert("choice".into(), vec![attachment("choice-evidence")]);
            let current = state(mode);
            assert!(
                current.allows(&intent),
                "valid exact provider choices retain files in {mode} mode"
            );
            let original = Command::new("bound-thread", intent.clone()).wire().unwrap();
            assert_eq!(
                original["attachmentsByQuestionId"]["choice"][0]["id"],
                "choice-evidence"
            );
            assert_eq!(
                original["answers"]["choice"],
                if mode == "message" {
                    json!("[\" exact \",\"β\"]")
                } else {
                    json!([" exact ", "β"])
                }
            );
            for invalid in ["", "unlisted custom answer"] {
                let mut rejected = intent.clone();
                let Intent::Respond {
                    answers: Some(answers),
                    ..
                } = &mut rejected
                else {
                    unreachable!()
                };
                answers.insert("choice".into(), json!(invalid));
                assert!(
                    !current.allows(&rejected),
                    "files cannot bypass allowCustomAnswer=false: {invalid:?}"
                );
            }
        }
    }

    #[test]
    fn attachment_only_still_requires_ready_answerable_exact_request_and_known_response_mode() {
        let positive = response("live");
        let current = state("live");
        assert!(current.allows(&positive));
        for invalid in [
            "not-ready",
            "not-answerable",
            "foreign-request",
            "unknown-mode",
            "no-mode",
            "approval",
        ] {
            let mut changed = current.clone();
            match invalid {
                "not-ready" => changed.ready = false,
                "not-answerable" => changed.requests[0].answerable = false,
                "foreign-request" => changed.requests[0].id = "another-request".into(),
                "unknown-mode" => changed.requests[0].response_mode = Some("not_resumable".into()),
                "no-mode" => changed.requests[0].response_mode = None,
                "approval" => changed.requests[0].item["type"] = json!("approval_request"),
                _ => unreachable!(),
            }
            assert!(!changed.allows(&positive), "{invalid}");
        }
        let approval = Intent::Respond {
            request_id: "request".into(),
            decision: Some("accept".into()),
            answers: None,
            attachments_by_question_id: BTreeMap::from([(
                "file".into(),
                vec![attachment("uploaded")],
            )]),
        };
        assert!(Command::new("bound-thread", approval).wire().is_err());
    }

    #[test]
    fn flattened_limits_and_reference_validation_span_all_questions() {
        let references = |prefix: &str, count| {
            (0..count)
                .map(|index| attachment(&format!("{prefix}-{index}")))
                .collect::<Vec<_>>()
        };
        let mut map = BTreeMap::from([
            ("one".into(), references("one", 5)),
            ("two".into(), references("two", 3)),
        ]);
        assert!(validate_question_attachments(&map).is_ok());
        map.get_mut("two").unwrap().push(attachment("ninth"));
        assert!(
            validate_question_attachments(&map).is_err(),
            "per-question lists cannot bypass total eight"
        );
        map.get_mut("two").unwrap().pop();
        for invalid in [
            "unissued-path",
            "empty",
            "oversized-image",
            "oversized-file",
        ] {
            let mut invalid_map = map.clone();
            let asset = &mut invalid_map.get_mut("two").unwrap()[0];
            match invalid {
                "unissued-path" => asset.id = "/tmp/not-an-upload.png".into(),
                "empty" => asset.metadata.size_bytes = 0,
                "oversized-image" => asset.metadata.size_bytes = MAX_IMAGE_BYTES + 1,
                "oversized-file" => {
                    asset.metadata.kind = AttachmentKind::File;
                    asset.metadata.size_bytes = MAX_FILE_BYTES + 1;
                }
                _ => unreachable!(),
            }
            assert!(
                validate_question_attachments(&invalid_map).is_err(),
                "{invalid}"
            );
        }
        let blank = BTreeMap::from([(" \n".into(), vec![attachment("issued")])]);
        assert!(validate_question_attachments(&blank).is_err());
    }

    #[test]
    fn optional_answers_and_legacy_text_validation_remain_unchanged_without_assets() {
        let mut item = state("live").requests.remove(0).item;
        item["questions"][1]["required"] = json!(false);
        let answers = BTreeMap::from([("choice".into(), json!([" exact "]))]);
        assert!(valid_answers(&item, &answers));
        assert!(valid_answers_with_attachments(
            &item,
            &answers,
            &QuestionAttachments::new()
        ));
        let files = BTreeMap::from([("file".into(), vec![attachment("issued")])]);
        assert!(
            !valid_answers_with_attachments(&item, &answers, &files),
            "file references require the same question's explicit answer entry"
        );
        let mut attached = answers.clone();
        attached.insert("file".into(), json!(""));
        assert!(valid_answers_with_attachments(&item, &attached, &files));
        assert!(!valid_answers(&item, &attached));
        let mut whitespace = attached;
        whitespace.insert("file".into(), json!(" \t"));
        assert!(
            !valid_answers(&item, &whitespace),
            "whitespace alone never completes a question"
        );
    }

    #[test]
    fn old_respond_json_deserializes_and_serializes_without_a_new_empty_field() {
        for legacy in [
            json!({"type":"runtime-request.respond","requestId":"request","decision":"accept"}),
            json!({"type":"runtime-request.respond","requestId":"request","answers":{"q":" exact "}}),
        ] {
            let intent: Intent = serde_json::from_value(legacy.clone()).unwrap();
            assert_eq!(serde_json::to_value(&intent).unwrap(), legacy);
            assert!(Command::new("bound-thread", intent)
                .wire()
                .unwrap()
                .get("attachmentsByQuestionId")
                .is_none());
        }
    }
}
