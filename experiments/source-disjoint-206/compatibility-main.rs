#![allow(dead_code)]
mod brain {
    mod editor_recovery;
    mod merge_preview;
    mod old_recovery;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use uuid::Uuid;
    pub fn run() {
        let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("fixture root"));
        let workspace = json!({"brain_id":Uuid::new_v4().to_string(),"root":"/isolated/compat206","records_dir":"records","managed":true});
        let snapshot = |s: &str| json!({"schema":"ai-brain/v1","brain_id":workspace["brain_id"],"path":"notes/source.md","media_type":"text/markdown","revision":format!("sha256:{:x}",Sha256::digest(s.as_bytes())),"content_base64":STANDARD.encode(s)});
        let store = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let old = old_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let old_draft = old
            .create(&snapshot("ordinary v1\n"), "original ordinary draft\n")
            .unwrap();
        let old_path = dir
            .join(workspace["brain_id"].as_str().unwrap())
            .join(format!("{}.json", old_draft.id));
        let old_bytes = std::fs::read(&old_path).unwrap();
        let new_list = store.list().unwrap();
        assert_eq!(new_list.drafts.len(), 1);
        assert!(new_list.problems.is_empty());
        assert!(!new_list.drafts[0].automatic_format);
        assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes);
        let draft = store.create(&snapshot("A\nB\n"), "X\nB\n").unwrap();
        let original = json!({"op":"source_write","base":draft.base,"request":{"schema":"ai-brain/v1","operation_id":Uuid::new_v4().to_string(),"brain_id":workspace["brain_id"],"path":draft.base["path"],"expected_revision":draft.base["revision"],"content_base64":STANDARD.encode(&draft.text)}});
        let pending = store.retain_save(&draft, &original).unwrap();
        let conflict = json!({"base":draft.base,"current":snapshot("A\nY\n"),"proposed":snapshot("X\nB\n"),"conflict":{"conflict_id":original["request"]["operation_id"],"path":draft.base["path"],"expected_revision":draft.base["revision"],"current_revision":snapshot("A\nY\n")["revision"],"reason":"stale_revision"}});
        let (auto, child) = match store
            .retain_auto_resolution(&pending, &original, &conflict)
            .unwrap()
        {
            editor_recovery::AutoResolution::Ready { draft, request } => (draft, request),
            _ => panic!("expected child"),
        };
        let newer = store.update(&auto, "X+\nB\n").unwrap();
        let path = dir
            .join(workspace["brain_id"].as_str().unwrap())
            .join(format!("{}.json", newer.id));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["schema"],
            "okilum-editor-recovery/v2"
        );
        let predecessor = old.list().unwrap();
        assert_eq!(predecessor.drafts.len(), 1);
        assert_eq!(predecessor.problems.len(), 1);
        assert!(predecessor.problems[0]
            .contains("Editor recovery identity does not match this workspace."));
        // Even a caller with a remembered id cannot bypass the exact old reader.
        let disguised = old_recovery::Draft {
            id: newer.id.clone(),
            generation: newer.generation,
            base: newer.base.clone(),
            text: newer.text.clone(),
            pending_save: newer.pending_save.clone(),
            conflict: newer.conflict.clone(),
        };
        let retain_error = old.retain_save(&disguised, &child).unwrap_err();
        let discard_error = old.discard(&disguised).unwrap_err();
        let r = &child["request"];
        let receipt = json!({"operation_id":r["operation_id"],"path":r["path"],"previous_revision":r["expected_revision"],"revision":snapshot("X\nY\n")["revision"],"outcome":"written"});
        let ack_error = old.acknowledge(&newer.id, &child, &receipt).unwrap_err();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes);
        let reopened = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let list = reopened.list().unwrap();
        assert!(list.problems.is_empty());
        assert!(list.drafts.contains(&newer));
        println!(
            "{}",
            json!({"verdict":"PASS","old_list_problem":predecessor.problems,"old_retain_error":retain_error,"old_discard_error":discard_error,"old_ack_error":ack_error,"v1_unchanged":true,"v2_unchanged":true,"no_rpc_module_or_call":true,"v2_sha256":format!("{:x}",Sha256::digest(&bytes)),"v1_sha256":format!("{:x}",Sha256::digest(&old_bytes)),"fixture_root":dir})
        );
    }
}
fn main() {
    brain::run();
}
