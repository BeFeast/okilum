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

/// The `t3_project_update` fields T3's `ProjectUpdatePayload` declares
/// `optional(NullOr(..))`; `null` there clears the setting. Every other
/// optional field is `optional(..)` alone and takes no `null`.
const NULLABLE_PROJECT_UPDATE_FIELDS: [&str; 4] = [
    "projectIcon",
    "faviconPath",
    "defaultModelSelection",
    "defaultThreadEnvMode",
];

/// #1765: the captured `t3_project_update` schema advertises `projectIcon`
/// as an object with a required `kind` and no `null` branch. That is T3's
/// advertising rule, not its input contract: `advertisedJsonSchema` drops
/// "the `null` branch of optional properties" and "decoding still uses the
/// full schema" (pingdotgg/t3code `apps/server/src/mcp/advertisedJsonSchema.ts`,
/// same in the running 0.1.10-nightly.20261116). The full schema is
/// `projectIcon: optional(NullOr(ProjectIconOverride))`, its `project.update`
/// command keeps the `null` and clears the override, and T3's own project
/// settings panel resets an icon with `projectIcon: null`. So the advertised
/// schema is checked as advertised, `null` only where T3's payload is
/// nullable, and a reset is never a guessed default icon.
#[test]
fn icon_save_and_reset_fit_the_captured_project_update_schema() {
    use crate::projects::icon::{IconColor, ProjectIcon};
    let catalog = current_tools();
    let schema = catalog["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "t3_project_update")
        .expect("captured catalog has t3_project_update")["inputSchema"]
        .clone();
    let advertised = &schema["properties"]["projectIcon"];
    assert_eq!(advertised["type"], "object");
    assert!(
        advertised.get("anyOf").is_none(),
        "no null branch is advertised"
    );
    let required: Vec<String> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(required, ["projectId"], "projectIcon is optional");

    let stored = Arc::new(std::sync::Mutex::new(
        json!({"kind": "emoji", "emoji": "🧠"}),
    ));
    let server = {
        let stored = stored.clone();
        let schema = schema.clone();
        fake_t3(move |name, args| match name {
            "t3_project_update" => {
                let fields = args.as_object().unwrap();
                for field in &required {
                    assert!(fields.contains_key(field), "missing {field}");
                }
                for (field, value) in fields {
                    let property = &schema["properties"][field];
                    assert!(property.is_object(), "unadvertised {field}");
                    if value.is_null() {
                        assert!(!required.contains(field), "null required {field}");
                        assert!(
                            NULLABLE_PROJECT_UPDATE_FIELDS.contains(&field.as_str()),
                            "T3 does not take null for {field}"
                        );
                        continue;
                    }
                    if field == "projectIcon" {
                        let kinds = property["properties"]["kind"]["enum"].as_array().unwrap();
                        assert!(kinds.contains(&value["kind"]), "{value}");
                        for key in value.as_object().unwrap().keys() {
                            assert!(property["properties"].get(key).is_some(), "icon.{key}");
                        }
                    }
                }
                *stored.lock().unwrap() = args["projectIcon"].clone();
                structured(json!({ "sequence": 4 }))
            }
            "t3_project_read" => structured(t3_project(stored.lock().unwrap().clone())),
            other => panic!("unexpected tool {other}"),
        })
    };
    let gateway = gateway(&server, Some(token("good")));
    let icon = ProjectIcon::Monogram {
        text: "Ok".into(),
        color: IconColor::Teal,
    };
    assert_eq!(
        gateway
            .set_project_icon("okilum", Some(&icon))
            .unwrap()
            .project_icon,
        Some(icon)
    );
    assert_eq!(
        gateway
            .set_project_icon("okilum", None)
            .unwrap()
            .project_icon,
        None
    );
    let updates: Vec<_> = tool_calls(&server.requests())
        .into_iter()
        .filter(|call| call["name"] == "t3_project_update")
        .map(|call| call["arguments"].clone())
        .collect();
    assert_eq!(
        updates.last().unwrap(),
        &json!({"projectId": "okilum", "projectIcon": null}),
        "a reset is an explicit null, not a guessed default"
    );
}

/// #1765: a client below full access is refused a reset as well; the
/// error names what Okilum lacks and nothing is written in its place.
#[test]
fn a_reset_refused_for_a_client_without_full_access_says_so() {
    use crate::projects::icon;
    use crate::projects::Project as VaultProject;
    let server = fake_t3(|name, args| match name {
        "t3_project_list" => structured(json!({
            "projects": [t3_project(json!({"kind": "emoji", "emoji": "🧠"}))], "nextCursor": null
        })),
        "t3_project_update" => {
            assert_eq!(args["projectIcon"], Value::Null);
            json!({"isError": true, "structuredContent": {
                "code": "capability_denied",
                "message": "Changing projects or environment settings needs a live full-access/default calling thread or a full-access client."
            }})
        }
        other => panic!("unexpected tool {other}"),
    });
    let gateway: Arc<dyn ManagerGateway> = Arc::new(gateway(&server, Some(token("good"))));
    let vault = std::env::temp_dir().join(format!("okilum-icon-reset-{}", uuid::Uuid::new_v4()));
    let ops = vault.join("Dev/Areas/okilum/ops");
    std::fs::create_dir_all(&ops).unwrap();
    std::fs::write(
        ops.join("project.yaml"),
        "name: Okilum\nt3:\n  environment: test\n  executor: {project_id: okilum}\n",
    )
    .unwrap();
    let okilum = VaultProject::read(&vault.join("Dev/Areas/okilum")).unwrap();
    let message = format!(
        "{:#}",
        icon::save_icon(&vault, &okilum, Some(gateway.as_ref()), None).unwrap_err()
    );
    assert!(
        message.contains("only a full-access client change project icons"),
        "{message}"
    );
    assert!(message.contains("capability_denied"), "{message}");
    assert!(message.contains("the icon was not changed"), "{message}");
    assert!(!std::fs::read_to_string(ops.join("project.yaml"))
        .unwrap()
        .contains("icon:"));
    std::fs::remove_dir_all(vault).unwrap();
}
