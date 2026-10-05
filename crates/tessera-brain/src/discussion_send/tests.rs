use super::*;
use std::{fs, io::Write};
use tessera_core::source::{SourceStore, WriteBoundary};

#[test]
fn shared_client_digest_fixtures_preserve_exact_utf8_crlf_and_null() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../docs/fixtures/discussion-send-request-digests-v1.json"
    ))
    .unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let input = &case["input"];
        let paths: Vec<String> = serde_json::from_value(input["source_paths"].clone()).unwrap();
        let bytes = serde_json::to_vec(&(
            REQUEST_SCHEMA,
            input["brain_id"].as_str().unwrap(),
            input["goal_id"].as_str().unwrap(),
            input["expected_actor_id"].as_str().unwrap(),
            input["conversation_id"].as_str(),
            input["message"].as_str().unwrap(),
            &paths,
        ))
        .unwrap();
        assert_eq!(bytes, case["canonical_json"].as_str().unwrap().as_bytes());
        assert_eq!(bytes.len() as u64, case["utf8_bytes"].as_u64().unwrap());
        assert_eq!(sha(&bytes), case["request_sha256"]);
    }
}

#[test]
fn exact_serialized_normal_maximum_record_and_bounded_reader_overflow() {
    // Exercise the real RecoveryRecord serializer and actual bounded reader at
    // both normal payload ceilings. No provider, canonical mutation or index.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let state = temp.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&state).unwrap();
    let brain = "11111111-1111-4111-8111-111111111111";
    let operation = "22222222-2222-4222-8222-222222222222";
    let store = SourceStore::open(brain, &root, &state, WriteBoundary::Managed).unwrap();
    // U+0001 needs six JSON bytes. A 4096-byte path conservatively bounds the
    // two path copies, including names needing maximal serde_json escaping.
    let path = "\u{0001}".repeat(MAX_PATH_BYTES);
    let proposed = vec![b'p'; discussion_context::MAX_CANDIDATE];
    let proposed_revision = revision(&proposed);
    let content_base64 = STANDARD.encode(&proposed);
    drop(proposed);
    let preimage = vec![b'b'; discussion_context::MAX_CONVERSATION as usize];
    let previous = revision(&preimage);
    let preimage_base64 = STANDARD.encode(&preimage);
    drop(preimage);
    let record = RecoveryRecord {
        base: None,
        request: SourceWrite {
            schema: SCHEMA.into(),
            operation_id: operation.into(),
            brain_id: brain.into(),
            path: path.clone(),
            expected_revision: Some(previous.clone()),
            content_base64,
        },
        preimage_base64: Some(preimage_base64),
        previous_revision: Some(previous.clone()),
        receipt: Some(WriteReceipt {
            operation_id: operation.into(),
            path,
            previous_revision: Some(previous),
            revision: proposed_revision,
            outcome: WriteOutcome::Written,
        }),
        conflict: None,
        divergent_observations_base64: vec![],
    };
    let journal = state.join(format!("{operation}.json"));
    let mut file = fs::File::create(&journal).unwrap();
    serde_json::to_writer(&mut file, &record).unwrap();
    file.flush().unwrap();
    drop(file);
    drop(record);
    let exact = fs::metadata(&journal).unwrap().len();
    let payload = 4 * (discussion_context::MAX_CANDIDATE as u64).div_ceil(3)
        + 4 * discussion_context::MAX_CONVERSATION.div_ceil(3);
    assert_eq!(exact, 268_485_317);
    assert_eq!(exact - payload, 49_857);
    println!("normal_max_serialized_bytes={exact} base64_payload_bytes={payload} metadata_overhead_bytes={} frozen_reader_limit={MAX_RECOVERY_BYTES}",exact-payload);
    assert!(exact <= MAX_RECOVERY_BYTES && exact > payload);
    let found = store.recovery_record_bounded(operation, exact).unwrap();
    assert_eq!(
        found.request.content_base64.len() as u64,
        4 * (discussion_context::MAX_CANDIDATE as u64).div_ceil(3)
    );
    assert_eq!(
        found.preimage_base64.as_ref().unwrap().len() as u64,
        4 * discussion_context::MAX_CONVERSATION.div_ceil(3)
    );
    drop(found);
    assert!(store
        .recovery_record_bounded(operation, exact - 1)
        .unwrap_err()
        .message
        .contains("byte budget"));
    // Valid JSON whitespace extends the normal-max fixture to the exact reader
    // ceiling, and one extra byte proves the limit is serialized bytes.
    let mut file = fs::OpenOptions::new().append(true).open(&journal).unwrap();
    file.write_all(&vec![b' '; (MAX_RECOVERY_BYTES - exact) as usize])
        .unwrap();
    file.flush().unwrap();
    drop(
        store
            .recovery_record_bounded(operation, MAX_RECOVERY_BYTES)
            .unwrap(),
    );
    file.write_all(b" ").unwrap();
    file.flush().unwrap();
    drop(file);
    assert!(store
        .recovery_record_bounded(operation, MAX_RECOVERY_BYTES)
        .unwrap_err()
        .message
        .contains("byte budget"));
    // A generic record with divergent history has no exemption from the bound.
    let mut file = fs::File::create(&journal).unwrap();
    file.write_all(b"{\"divergent_observations_base64\":[\"")
        .unwrap();
    let chunk = vec![b'A'; 1024 * 1024];
    for _ in 0..=(MAX_RECOVERY_BYTES / chunk.len() as u64) {
        file.write_all(&chunk).unwrap();
    }
    file.write_all(b"\"]}").unwrap();
    drop(file);
    assert!(store
        .recovery_record_bounded(operation, MAX_RECOVERY_BYTES)
        .unwrap_err()
        .message
        .contains("byte budget"));
}
