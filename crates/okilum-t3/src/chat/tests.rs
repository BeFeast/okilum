use serde_json::json;
use std::sync::{Arc, Mutex};

use super::*;
use crate::gateway::GatewayResult;
use crate::*;

type Read = dyn Fn(&ThreadQuery) -> GatewayResult<ThreadPage> + Send + Sync;
type SendHandler = dyn Fn(&OutgoingMessage) -> GatewayResult<SendReceipt> + Send + Sync;
struct Fake {
    read: Box<Read>,
    send: Box<SendHandler>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            read: Box::new(|_| panic!("unexpected read")),
            send: Box::new(|_| panic!("unexpected send")),
        }
    }
}
impl ManagerGateway for Fake {
    fn server_identity(&self) -> GatewayResult<ServerIdentity> {
        unimplemented!()
    }
    fn status(&self) -> GatewayStatus {
        GatewayStatus::Connected {
            expires_at: u64::MAX,
        }
    }
    fn list_projects(&self) -> GatewayResult<Vec<Project>> {
        unimplemented!()
    }
    fn list_threads(&self, _: &str) -> GatewayResult<Vec<ThreadSummary>> {
        unimplemented!()
    }
    fn read_thread(&self, q: &ThreadQuery) -> GatewayResult<ThreadPage> {
        (self.read)(q)
    }
    fn send_message(&self, m: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        (self.send)(m)
    }
    fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
        Ok(vec!["question".into()])
    }
    fn capabilities(&self) -> GatewayResult<Capabilities> {
        unimplemented!()
    }
    fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
    fn configure_model(&self, _: &str, _: &ModelSelection) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
}
fn item(text: &str, status: &str, offset: Option<u64>) -> TimelineItem {
    serde_json::from_value(json!({"position":7,"itemId":"i","runId":"r","messageId":null,"createdBy":"agent","type":"assistant_message","status":status,"title":null,"text":text,"textTruncated":offset.is_some(),"nextTextOffset":offset,"updatedAt":"now"})).unwrap()
}
fn page(items: Vec<TimelineItem>) -> ThreadPage {
    serde_json::from_value(json!({"thread":{"threadId":"manager","projectId":"p","title":"Manager","status":"running","activeRunId":"r","providerInstanceId":"codex","model":"gpt-6.1-sol","runtimeMode":"full-access","linkedPullRequest":null,"itemCount":8,"pendingRequestCount":1,"updatedAt":"now"},"items":[],"nextPosition":7,"hasMore":false})).map(|mut page:ThreadPage| {page.items=items;page}).unwrap()
}

#[test]
fn polling_hydrates_chunks_and_refreshes_an_item_behind_the_position_cursor() {
    let phase = Arc::new(Mutex::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (flag, queries) = (phase.clone(), seen.clone());
    let fake = Fake {
        read: Box::new(move |q| {
            queries.lock().unwrap().push(q.clone());
            Ok(
                match (*flag.lock().unwrap(), q.item_id.is_some(), q.text_offset) {
                    (0, false, _) => page(vec![item("Hel😀", "running", Some(5))]),
                    (0, true, Some(5)) => page(vec![item("lo", "running", None)]),
                    (1, false, _) => page(vec![]),
                    (1, true, None) => page(vec![item("Hel😀lo again", "completed", None)]),
                    other => panic!("unexpected query {other:?}"),
                },
            )
        }),
        ..Default::default()
    };
    let mut chat = Conversation::new("manager");
    chat.poll(&fake).unwrap();
    assert_eq!(chat.items[&7].text.as_deref(), Some("Hel😀lo"));
    assert_eq!(chat.pending_questions, 1);
    *phase.lock().unwrap() = 1;
    chat.poll(&fake).unwrap();
    assert_eq!(chat.items.len(), 1);
    assert_eq!(chat.items[&7].text.as_deref(), Some("Hel😀lo again"));
    let queries = seen.lock().unwrap();
    assert_eq!(queries[2].after_position, Some(7));
    assert_eq!(queries[3].item_id.as_deref(), Some("i"));
    assert_eq!(queries[3].text_offset, None);
}

#[test]
fn failed_poll_does_not_advance_or_erase_visible_history() {
    let mut chat = Conversation::new("manager");
    chat.cursor = Some(7);
    chat.items.insert(7, item("retained", "completed", None));
    let fake = Fake {
        read: Box::new(|_| Err(GatewayError::Revoked)),
        ..Default::default()
    };
    assert_eq!(chat.poll(&fake).unwrap_err(), GatewayError::Revoked);
    assert_eq!(chat.cursor, Some(7));
    assert_eq!(chat.items[&7].text.as_deref(), Some("retained"));
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("okilum-chat-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_lost_send_response_and_process_restart_keep_the_same_request_identity() {
    let state = Directory::new();
    let env = Environment::new("test", "https://t3.oklabs.uk").unwrap();
    let journal = MessageJournal::new(&state.0, &env, "manager");
    let record = journal.prepare("hello").unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let fake = Fake {
        send: Box::new(move |m| {
            let mut calls = seen.lock().unwrap();
            calls.push(m.clone());
            if calls.len() == 1 {
                return Err(GatewayError::Unreachable(
                    "response lost after acceptance".into(),
                ));
            }
            Ok(SendReceipt {
                thread_id: m.thread_id.clone(),
                message_id: "m1".into(),
                run_id: "r1".into(),
                status: "queued".into(),
                delivery: "queued".into(),
            })
        }),
        ..Default::default()
    };
    assert!(journal.send(&fake, &record).is_err());
    drop(journal);
    let journal = MessageJournal::new(&state.0, &env, "manager");
    let retained = journal.records().unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0], record);
    let acknowledged = journal.send(&fake, &retained[0]).unwrap();
    assert_eq!(acknowledged.receipt.as_ref().unwrap().message_id, "m1");
    assert_eq!(journal.send(&fake, &record).unwrap(), acknowledged);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    assert!(MessageJournal::new(&state.0, &env, "different-manager")
        .records()
        .unwrap()
        .is_empty());
}

#[test]
fn a_changed_intent_cannot_reuse_an_existing_request_identity() {
    let state = Directory::new();
    let env = Environment::new("test", "https://t3.oklabs.uk").unwrap();
    let journal = MessageJournal::new(&state.0, &env, "manager");
    let mut record = journal.prepare("original").unwrap();
    record.message.text = "replacement".into();
    assert!(journal
        .send(&Fake::default(), &record)
        .unwrap_err()
        .to_string()
        .contains("identity changed"));
}

#[test]
fn unchanged_poll_reports_no_change_and_keeps_the_displayed_text_allocation() {
    let grown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = grown.clone();
    let fake = Fake {
        read: Box::new(move |_| {
            Ok(page(vec![item(
                if flag.load(std::sync::atomic::Ordering::SeqCst) {
                    "expanded reply"
                } else {
                    "reply"
                },
                "running",
                None,
            )]))
        }),
        ..Default::default()
    };
    let mut chat = Conversation::new("manager");
    let first = chat.poll_request().fetch(&fake).unwrap();
    assert!(chat.apply_poll(first).unwrap().changed());
    let pointer = chat.items[&7].text.as_ref().unwrap().as_ptr();
    let unchanged = chat.poll_request().fetch(&fake).unwrap();
    assert!(!chat.apply_poll(unchanged).unwrap().changed());
    assert_eq!(chat.items[&7].text.as_ref().unwrap().as_ptr(), pointer);
    grown.store(true, std::sync::atomic::Ordering::SeqCst);
    let update = chat.poll_request().fetch(&fake).unwrap();
    assert_eq!(chat.apply_poll(update).unwrap().positions, [7]);
    assert_eq!(chat.items[&7].text.as_deref(), Some("expanded reply"));
}

#[test]
fn poll_request_carries_growing_identities_without_copying_completed_history() {
    let mut chat = Conversation::new("manager");
    for position in 0..10_000 {
        let mut message = item("completed history", "completed", None);
        message.position = position;
        chat.items.insert(position, message);
    }
    let mut live = item("streaming", "running", None);
    live.position = 10_000;
    live.item_id = "live".into();
    chat.items.insert(live.position, live);
    chat.cursor = Some(10_000);
    let request = chat.poll_request();
    assert_eq!(request.growing, vec![String::from("live")]);
    assert_eq!(request.cursor, Some(10_000));
    assert_eq!(request.thread_id, "manager");
}

#[test]
fn poll_update_for_another_binding_cannot_mutate_history() {
    let fake = Fake {
        read: Box::new(|_| Ok(page(vec![item("other", "completed", None)]))),
        ..Default::default()
    };
    let update = Conversation::new("manager")
        .poll_request()
        .fetch(&fake)
        .unwrap();
    let mut current = Conversation::new("new-manager");
    assert!(current.apply_poll(update).is_err());
    assert!(current.items.is_empty());
    assert!(current.project_id.is_none());
    assert!(current.cursor.is_none());
}
