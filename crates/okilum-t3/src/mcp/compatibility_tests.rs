use super::*;

fn catalog_without(name: &str) -> Value {
    let mut catalog = current_tools();
    catalog["tools"]
        .as_array_mut()
        .unwrap()
        .retain(|tool| tool["name"] != name);
    catalog
}

fn list_count(server: &FakeServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.json()["method"] == "tools/list")
        .count()
}

#[test]
fn current_pending_and_configuration_tools_use_the_captured_input_schema() {
    let server = fake_t3(|name, args| {
        let catalog = current_tools();
        let tool = catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .expect("T3_SCHEMA1759: retired tool must not be called");
        let schema = &tool["inputSchema"];
        for field in schema["required"].as_array().into_iter().flatten() {
            assert!(args.get(field.as_str().unwrap()).is_some());
        }
        for field in args.as_object().unwrap().keys() {
            assert!(schema["properties"].get(field).is_some(), "{name}.{field}");
        }
        match name {
            "t3_pending_request" => {
                assert_eq!(args["action"], "list");
                structured(json!({"requestIds":["q1"]}))
            }
            "t3_thread_configure" => {
                assert!(
                    args.get("modelSelection").is_none(),
                    "configuration read cannot mutate a model"
                );
                structured(configuration("old"))
            }
            other => panic!("T3_SCHEMA1759: retired/unexpected tool {other}"),
        }
    });
    let gateway = gateway(&server, Some(token("good")));
    assert_eq!(gateway.pending_requests("mcp:manager").unwrap(), ["q1"]);
    assert_eq!(
        gateway
            .thread_configuration("mcp:manager")
            .unwrap()
            .model_selection
            .model,
        "old"
    );
    assert_eq!(list_count(&server), 1);
    gateway.set_token(Some(token("good")));
    gateway.pending_requests("mcp:manager").unwrap();
    assert_eq!(
        list_count(&server),
        2,
        "a reconnect discovers the scoped catalog again"
    );
}

#[test]
fn missing_required_tool_names_the_incompatibility_before_any_tool_call() {
    let server = fake_t3_with_tools(catalog_without("t3_thread_send"), |_, _| {
        panic!("incompatible client cannot call tools")
    });
    let gateway = gateway(&server, Some(token("good")));
    for _ in 0..2 {
        let error = gateway.server_identity().unwrap_err();
        assert_eq!(
            error,
            GatewayError::Compatibility {
                tool: "t3_thread_send".into()
            }
        );
        assert!(error.to_string().contains("required tool 't3_thread_send'"));
    }
    assert_eq!(
        list_count(&server),
        1,
        "failed compatibility is cached for this connection"
    );
    assert!(tool_calls(&server.requests()).is_empty());
}

#[test]
fn unavailable_questions_do_not_block_manager_history_or_send() {
    let server = fake_t3_with_tools(
        catalog_without("t3_pending_request"),
        |name, args| match name {
            "t3_thread_read" => structured(json!({"thread":{
            "threadId":"mcp:manager", "projectId":"managers", "title":"Manager", "status":"idle",
            "providerInstanceId":"codex", "model":"old", "runtimeMode":"full-access", "updatedAt":"now"},
            "items":[{"position":1,"itemId":"message1","type":"assistant_message","status":"completed","text":"History remains readable","updatedAt":"now"}],
            "nextPosition":1,"hasMore":false})),
            "t3_thread_send" => structured(
                json!({"threadId":args["threadId"],"messageId":"sent1","runId":"run1","status":"queued","delivery":"queued"}),
            ),
            other => panic!("optional missing tool must not be called: {other}"),
        },
    );
    let gateway = gateway(&server, Some(token("good")));
    let mut chat = crate::chat::Conversation::new("mcp:manager");
    chat.poll(&gateway)
        .expect("T3_OPTIONAL1759: missing questions cannot discard history");
    assert_eq!(
        chat.items[&1].text.as_deref(),
        Some("History remains readable")
    );
    assert_eq!(chat.project_id.as_deref(), Some("managers"));
    assert_eq!(chat.cursor, Some(1));
    assert!(chat
        .pending_question_error
        .as_ref()
        .unwrap()
        .contains("t3_pending_request"));
    let receipt = gateway
        .send_message(&OutgoingMessage::queue("mcp:manager", "Hello"))
        .unwrap();
    assert_eq!(receipt.delivery, "queued");
    assert_eq!(list_count(&server), 1);
    assert!(tool_calls(&server.requests())
        .iter()
        .all(|call| call["name"] != "t3_pending_request"));
}

#[test]
fn unavailable_model_tool_disables_only_model_controls() {
    let server = fake_t3_with_tools(catalog_without("t3_thread_configure"), |name, _| {
        assert_eq!(name, "t3_environment_read");
        structured(json!({"environmentId":"test", "label":"Test server"}))
    });
    let gateway = gateway(&server, Some(token("good")));
    assert!(matches!(
        gateway.thread_configuration("mcp:manager"),
        Err(GatewayError::Unsupported { .. })
    ));
    assert_eq!(gateway.server_identity().unwrap().label, "Test server");
    assert_eq!(tool_calls(&server.requests()).len(), 1);
}

#[test]
fn pending_list_accepts_ids_without_a_count_or_thread_wrapper() {
    for payload in [
        json!({"requestIds":["q1"]}),
        json!({"ids":["q1"]}),
        json!(["q1"]),
    ] {
        let server = fake_t3(move |name, _| {
            assert_eq!(name, "t3_pending_request");
            structured(payload.clone())
        });
        assert_eq!(
            gateway(&server, Some(token("good")))
                .pending_requests("mcp:manager")
                .unwrap(),
            ["q1"]
        );
    }
}
