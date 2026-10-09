//! Bounded local Markdown draft recovery. No RPC or automatic replay occurs here.
//! Every update compares the full retained generation under an OS file lock.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};
use uuid::Uuid;

mod auto_resolution;
const SCHEMA: &str = "tessera-editor-recovery/v1";
const AUTO_SCHEMA: &str = "tessera-editor-recovery/v2";

#[derive(Debug)]
pub(super) enum AutoResolution {
    Ready {
        draft: Box<Draft>,
        request: Value,
    },
    Manual {
        reason: super::merge_preview::ManualReason,
    },
}
pub(super) const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: u64 = 80 * 1024 * 1024;
const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DRAFTS: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Draft {
    pub automatic_format: bool,
    pub id: String,
    pub generation: u64,
    pub base: Value,
    pub text: String,
    pub pending_save: Option<Value>,
    pub conflict: Option<Value>,
}

impl Draft {
    pub fn has_terminal_criteria_conflict(&self) -> bool {
        self.pending_save
            .as_ref()
            .is_some_and(|r| r["op"] == "goal_criteria_write")
            && self.has_terminal_guarded_conflict()
    }
    pub fn has_terminal_guarded_conflict(&self) -> bool {
        self.pending_save.as_ref().is_some_and(|request| {
            is_guarded(request)
                && self.conflict.as_ref().is_some_and(|c| {
                    c["conflict"]["conflict_id"] == request["request"]["operation_id"]
                        && c["conflict"]["path"] == request["request"]["path"]
                        && c["conflict"]["expected_revision"]
                            == request["request"]["expected_revision"]
                        && c["base"] == request["base"]
                        && c["proposed"]["content_base64"] == request["request"]["content_base64"]
                })
        })
    }
}

#[derive(Default, Debug)]
pub(super) struct RecoveryList {
    pub drafts: Vec<Draft>,
    pub problems: Vec<String>,
}

#[derive(Clone)]
pub(super) struct EditorRecovery {
    root: PathBuf,
    workspace: Value,
    #[cfg(test)]
    pub(super) fail_after_publish: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    fail_confirmation: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub(super) fn is_guarded(request: &Value) -> bool {
    matches!(
        request["op"].as_str(),
        Some("goal_criteria_write" | "discussion_decision_reuse_write")
    )
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("Missing editor recovery {key}."))
}
fn revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}
fn snapshot_text(v: &Value, workspace: &Value) -> Result<String, String> {
    if v["schema"] != "ai-brain/v1"
        || v["brain_id"] != workspace["brain_id"]
        || !valid_path(field(v, "path")?)
    {
        return Err("Editor source belongs to another workspace or path.".into());
    }
    let encoded = field(v, "content_base64")?;
    if encoded.len() > MAX_TEXT_BYTES.div_ceil(3) * 4 {
        return Err("Editor source exceeds the recovery budget.".into());
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid source bytes.")?;
    if bytes.len() > MAX_TEXT_BYTES || field(v, "revision")? != revision(&bytes) {
        return Err("Editor source revision does not match its bytes.".into());
    }
    String::from_utf8(bytes).map_err(|_| "Only exact UTF-8 editor drafts are recoverable.".into())
}

impl EditorRecovery {
    pub fn open(workspace: &Value) -> Result<Self, String> {
        let home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .ok_or("No local editor recovery directory is available.")?;
        Self::at(home.join("okilum/editor-recovery"), workspace)
    }
    pub fn at(base: PathBuf, workspace: &Value) -> Result<Self, String> {
        let brain = Uuid::parse_str(field(workspace, "brain_id")?)
            .map_err(|_| "Invalid editor workspace identity.")?;
        if field(workspace, "root")?.is_empty()
            || field(workspace, "records_dir")?.is_empty()
            || workspace["managed"] != true
        {
            return Err("Editor recovery requires a complete writable workspace identity.".into());
        }
        Ok(Self {
            root: base.join(brain.to_string()),
            workspace: workspace.clone(),
            #[cfg(test)]
            fail_after_publish: Default::default(),
            #[cfg(test)]
            fail_confirmation: Default::default(),
        })
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        let parsed = Uuid::parse_str(id).map_err(|_| "Invalid editor draft identity.")?;
        if parsed.to_string() != id {
            return Err("Noncanonical editor draft identity.".into());
        }
        Ok(self.root.join(format!("{id}.json")))
    }
    fn lock(&self) -> Result<File, String> {
        let mut missing = vec![];
        let mut ancestor = self.root.as_path();
        while !ancestor.exists() {
            missing.push(ancestor.to_path_buf());
            ancestor = ancestor
                .parent()
                .ok_or("Invalid editor recovery directory.")?;
        }
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        for directory in missing.iter().rev() {
            File::open(directory.parent().unwrap())
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        if fs::symlink_metadata(&self.root)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Editor recovery directory must not be a symlink.".into());
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(self.root.join("recovery.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|_| {
            "Another editor is updating recovery. Draft remains unprotected; retry.".to_string()
        })?;
        Ok(lock)
    }
    fn validate(&self, d: &Draft) -> Result<(), String> {
        self.path(&d.id)?;
        snapshot_text(&d.base, &self.workspace)?;
        if d.generation == 0 || d.text.len() > MAX_TEXT_BYTES {
            return Err("Editor draft exceeds its generation or text budget.".into());
        }
        if let Some(request) = &d.pending_save {
            self.validate_request(d, request, false)?;
        }
        if let Some(conflict) = &d.conflict {
            snapshot_text(&conflict["proposed"], &self.workspace)?;
            if conflict["conflict"]["path"] != d.base["path"]
                || conflict["proposed"]["brain_id"] != self.workspace["brain_id"]
                || conflict["proposed"]["path"] != d.base["path"]
            {
                return Err("Recovered conflict belongs to another note.".into());
            }
            Uuid::parse_str(field(&conflict["conflict"], "conflict_id")?)
                .map_err(|_| "Invalid recovered conflict identity.")?;
        }
        Ok(())
    }
    fn validate_request(
        &self,
        d: &Draft,
        request: &Value,
        current_text: bool,
    ) -> Result<(), String> {
        let r = &request["request"];
        let base = &request["base"];
        snapshot_text(base, &self.workspace)?;
        Uuid::parse_str(field(r, "operation_id")?).map_err(|_| "Invalid Save identity.")?;
        let bytes = STANDARD
            .decode(field(r, "content_base64")?)
            .map_err(|_| "Invalid Save bytes.")?;
        if !matches!(
            request["op"].as_str(),
            Some("source_write" | "goal_criteria_write" | "discussion_decision_reuse_write")
        ) || r["schema"] != "ai-brain/v1"
            || r["brain_id"] != self.workspace["brain_id"]
            || r["path"] != d.base["path"]
            || base["path"] != d.base["path"]
            || r["expected_revision"] != base["revision"]
            || bytes.len() > MAX_TEXT_BYTES
            || std::str::from_utf8(&bytes).is_err()
            || (current_text && bytes != d.text.as_bytes())
            || (base != &d.base && d.conflict.as_ref().is_none_or(|c| &c["current"] != base))
        {
            return Err("Save does not match the exact retained draft and displayed base.".into());
        }
        let guarded = is_guarded(request);
        let allowed = if request["op"] == "discussion_decision_reuse_write" {
            &["op", "goal_id", "decision_id", "request", "base"][..]
        } else if guarded {
            &["op", "goal_id", "request", "base"][..]
        } else {
            &["op", "request", "base"][..]
        };
        if request["op"] == "discussion_decision_reuse_write" {
            let source = serde_json::from_value(base.clone()).map_err(|_| "Invalid reuse base")?;
            let write = serde_json::from_value(r.clone()).map_err(|_| "Invalid reuse write")?;
            okilum_core::decision_reuse::validate_write(
                &source,
                &write,
                field(request, "goal_id")?,
                field(request, "decision_id")?,
            )?;
        } else if guarded {
            let source =
                serde_json::from_value(base.clone()).map_err(|_| "Invalid criteria base")?;
            let write = serde_json::from_value(r.clone()).map_err(|_| "Invalid criteria write")?;
            okilum_core::goal_criteria::validate_write(
                &source,
                &write,
                field(request, "goal_id")?,
            )?;
        }
        if request
            .as_object()
            .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
        {
            return Err("Unexpected fields in retained Save envelope.".into());
        }
        Ok(())
    }
    fn encode(&self, d: &Draft) -> Result<Vec<u8>, String> {
        self.validate(d)?;
        serde_json::to_vec(
            &json!({"schema":if d.automatic_format { AUTO_SCHEMA } else { SCHEMA },"workspace":self.workspace,"id":d.id,
            "generation":d.generation,"base":d.base,"text":d.text,
            "pending_save":d.pending_save,"conflict":d.conflict}),
        )
        .map_err(|e| e.to_string())
    }
    fn read(&self, id: &str) -> Result<Draft, String> {
        let path = self.path(id)?;
        if fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Editor recovery record must not be a symlink.".into());
        }
        let mut bytes = vec![];
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(MAX_RECORD_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err("Editor recovery record exceeds its budget.".into());
        }
        let v: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Unreadable editor recovery record.")?;
        if !matches!(v["schema"].as_str(), Some(SCHEMA | AUTO_SCHEMA))
            || v["workspace"] != self.workspace
            || v["id"] != id
        {
            return Err("Editor recovery identity does not match this workspace.".into());
        }
        let d = Draft {
            automatic_format: v["schema"] == AUTO_SCHEMA,
            id: id.into(),
            generation: v["generation"]
                .as_u64()
                .ok_or("Invalid draft generation.")?,
            base: v["base"].clone(),
            text: field(&v, "text")?.into(),
            pending_save: (!v["pending_save"].is_null()).then(|| v["pending_save"].clone()),
            conflict: (!v["conflict"].is_null()).then(|| v["conflict"].clone()),
        };
        self.validate(&d)?;
        Ok(d)
    }
    fn compare(&self, expected: &Draft) -> Result<Draft, String> {
        let current = self.read(&expected.id)?;
        if &current != expected {
            return Err(
                "Another editor changed this recovered draft. Reload recovery before changing it."
                    .into(),
            );
        }
        Ok(current)
    }
    fn publish(&self, d: &Draft) -> Result<(), String> {
        let bytes = self.encode(d)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err("Editor recovery record exceeds its budget.".into());
        }
        let mut total = bytes.len() as u64;
        let target = self.path(&d.id)?;
        for e in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            if e.path() != target && e.file_type().map_err(|e| e.to_string())?.is_file() {
                total = total.saturating_add(e.metadata().map_err(|e| e.to_string())?.len());
            }
        }
        if total > MAX_STORE_BYTES {
            return Err("Editor recovery storage is full. Existing drafts are retained.".into());
        }
        let temporary = self.root.join(format!(".{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<(), String> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temporary, &target).map_err(|e| e.to_string())?;
            #[cfg(test)]
            if self
                .fail_after_publish
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(
                    "Injected failure after publication; protection is not acknowledged.".into(),
                );
            }
            File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())
        })();
        let _ = fs::remove_file(temporary);
        result
    }
    fn bump(d: &mut Draft) -> Result<(), String> {
        d.generation = d
            .generation
            .checked_add(1)
            .ok_or("Editor draft generation exhausted.")?;
        Ok(())
    }
    // A prior publication may have become visible before its directory sync
    // failed. Equality is not a durability receipt; idempotent success must
    // establish the same barrier as a newly published record.
    fn confirm(&self, d: &Draft) -> Result<(), String> {
        #[cfg(test)]
        if self
            .fail_confirmation
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err("Injected recovery durability confirmation failure.".into());
        }
        File::open(self.path(&d.id)?)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
    pub fn list(&self) -> Result<RecoveryList, String> {
        if !self.root.exists() {
            return Ok(RecoveryList::default());
        }
        let _lock = self.lock()?;
        let mut result = RecoveryList::default();
        for e in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let p = e.map_err(|e| e.to_string())?.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let id = p.file_stem().and_then(|x| x.to_str()).unwrap_or("");
            match self.read(id) {
                Ok(d) => result.drafts.push(d),
                Err(error) => result
                    .problems
                    .push(format!("Recovery record {id}: {error}")),
            }
            if result.drafts.len() + result.problems.len() > MAX_DRAFTS {
                return Err(
                    "Editor recovery inventory exceeds its budget. Existing records are retained."
                        .into(),
                );
            }
        }
        result.drafts.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }
    pub fn create(&self, base: &Value, text: &str) -> Result<Draft, String> {
        let _lock = self.lock()?;
        let entries = fs::read_dir(&self.root)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let count = entries
            .iter()
            .filter(|e| e.path().extension().is_some_and(|s| s == "json"))
            .count();
        if count >= MAX_DRAFTS {
            return Err("Too many retained editor drafts. Recover or discard one first.".into());
        }
        let d = Draft {
            automatic_format: false,
            id: Uuid::new_v4().to_string(),
            generation: 1,
            base: base.clone(),
            text: text.into(),
            pending_save: None,
            conflict: None,
        };
        self.publish(&d)?;
        Ok(d)
    }
    pub fn update(&self, expected: &Draft, text: &str) -> Result<Draft, String> {
        let _lock = self.lock()?;
        let mut d = self.compare(expected)?;
        if d.text == text {
            self.confirm(&d)?;
            return Ok(d);
        }
        d.text = text.into();
        Self::bump(&mut d)?;
        self.publish(&d)?;
        Ok(d)
    }
    pub fn retain_save(&self, expected: &Draft, request: &Value) -> Result<Draft, String> {
        let _lock = self.lock()?;
        let mut d = self.compare(expected)?;
        if let Some(old) = &d.pending_save {
            if old == request {
                self.confirm(&d)?;
                return Ok(d);
            }
            return Err(
                "Recover the original uncertain Save before requesting another write.".into(),
            );
        }
        self.validate_request(&d, request, true)?;
        d.pending_save = Some(request.clone());
        Self::bump(&mut d)?;
        self.publish(&d)?;
        Ok(d)
    }
    pub fn acknowledge(
        &self,
        id: &str,
        request: &Value,
        receipt: &Value,
    ) -> Result<Option<Draft>, String> {
        let _lock = self.lock()?;
        let mut d = self.read(id)?;
        if d.is_automatic_request(request) {
            return Err(
                "Automatic Save needs receipt handling that preserves the original draft base."
                    .into(),
            );
        }
        let r = &request["request"];
        let bytes = STANDARD
            .decode(field(r, "content_base64")?)
            .map_err(|_| "Invalid Save bytes.")?;
        if d.pending_save.as_ref() != Some(request)
            || receipt["operation_id"] != r["operation_id"]
            || receipt["path"] != r["path"]
            || receipt["previous_revision"] != r["expected_revision"]
            || field(receipt, "revision")? != revision(&bytes)
            || !matches!(receipt["outcome"].as_str(), Some("written" | "unchanged"))
        {
            return Err("Save acknowledgement does not match the retained operation.".into());
        }
        if d.text.as_bytes() == bytes {
            self.remove(&d.id)?;
            return Ok(None);
        }
        d.base = request["base"].clone();
        d.base["revision"] = receipt["revision"].clone();
        d.base["content_base64"] = r["content_base64"].clone();
        d.pending_save = None;
        d.conflict = None;
        Self::bump(&mut d)?;
        self.publish(&d)?;
        Ok(Some(d))
    }
    pub fn record_conflict(
        &self,
        id: &str,
        request: &Value,
        conflict: &Value,
    ) -> Result<Draft, String> {
        let _lock = self.lock()?;
        let mut d = self.read(id)?;
        let r = &request["request"];
        if d.pending_save.as_ref() != Some(request)
            || conflict["conflict"]["conflict_id"] != r["operation_id"]
            || conflict["conflict"]["expected_revision"] != r["expected_revision"]
            || conflict["proposed"]["content_base64"] != r["content_base64"]
            || conflict["base"] != request["base"]
        {
            return Err("Conflict does not match the retained Save.".into());
        }
        d.conflict = Some(conflict.clone());
        // A guarded conflict keeps its command identity through restart. The
        // user must discard and review a fresh form to choose different bytes.
        if !is_guarded(request) {
            d.pending_save = None;
        }
        Self::bump(&mut d)?;
        self.publish(&d)?;
        Ok(d)
    }
    /// Refresh only the current side of the same retained conflict after an
    /// explicit source_conflict read. Never rewrite its original base/proposal.
    pub fn refresh_conflict(&self, expected: &Draft, conflict: &Value) -> Result<Draft, String> {
        let _lock = self.lock()?;
        let mut d = self.compare(expected)?;
        let old = d
            .conflict
            .as_ref()
            .ok_or("No retained conflict to refresh.")?;
        if d.pending_save.is_some()
            || old["conflict"] != conflict["conflict"]
            || old["base"] != conflict["base"]
            || old["proposed"] != conflict["proposed"]
        {
            return Err("Conflict refresh changed its retained identity, base or proposal.".into());
        }
        d.conflict = Some(conflict.clone());
        Self::bump(&mut d)?;
        self.publish(&d)?;
        Ok(d)
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        fs::remove_file(self.path(id)?).map_err(|e| e.to_string())?;
        File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
    pub fn discard(&self, expected: &Draft) -> Result<(), String> {
        let _lock = self.lock()?;
        let d = self.compare(expected)?;
        // Only a positively correlated terminal conflict can retire a
        // guarded pending command. Unknown sends remain recoverable.
        if d.pending_save.is_some() && !d.has_terminal_guarded_conflict() {
            return Err(
                "This draft has an uncertain Save. Recover its outcome before discarding.".into(),
            );
        }
        self.remove(&d.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        directory: PathBuf,
        store: EditorRecovery,
        workspace: Value,
    }
    impl Fixture {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("okilum-editor-test-{}", Uuid::new_v4()));
            let workspace = json!({"brain_id":Uuid::new_v4().to_string(),"root":"/fixture/brain","records_dir":"records","managed":true});
            let store = EditorRecovery::at(directory.clone(), &workspace).unwrap();
            Self {
                directory,
                store,
                workspace,
            }
        }
        fn base(&self, text: &str) -> Value {
            json!({"schema":"ai-brain/v1","brain_id":self.workspace["brain_id"],"path":"notes/original.md","revision":revision(text.as_bytes()),"content_base64":STANDARD.encode(text),"media_type":"text/markdown"})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    fn request(base: &Value, text: &str) -> Value {
        json!({"op":"source_write","base":base,"request":{"schema":"ai-brain/v1","operation_id":Uuid::new_v4().to_string(),"brain_id":base["brain_id"],"path":base["path"],"expected_revision":base["revision"],"content_base64":STANDARD.encode(text)}})
    }
    fn receipt(r: &Value) -> Value {
        let r = &r["request"];
        json!({"operation_id":r["operation_id"],"path":r["path"],"previous_revision":r["expected_revision"],"revision":revision(&STANDARD.decode(r["content_base64"].as_str().unwrap()).unwrap()),"outcome":"written"})
    }
    #[test]
    fn restart_recovers_exact_bytes_and_listing_never_retires_or_replays() {
        let f = Fixture::new();
        let text = "\u{feff}---\r\ntype: Note\r\n---\r\n[[link]]\r\nשלום Мысль 🧠\r\n";
        let base = f.base("original\r\n");
        let d = f.store.create(&base, text).unwrap();
        let r = request(&base, text);
        let d = f.store.retain_save(&d, &r).unwrap();
        let reopen = EditorRecovery::at(f.directory.clone(), &f.workspace).unwrap();
        for _ in 0..2 {
            let list = reopen.list().unwrap();
            assert!(list.problems.is_empty());
            assert_eq!(list.drafts, vec![d.clone()]);
            assert_eq!(list.drafts[0].pending_save, Some(r.clone()));
        }
        assert!(!f.directory.join("notes/original.md").exists());
    }
    #[test]
    fn independent_windows_and_restored_generation_never_clobber() {
        let f = Fixture::new();
        let base = f.base("base");
        let a = f.store.create(&base, "window A").unwrap();
        let b = f.store.create(&base, "window B").unwrap();
        assert_ne!(a.id, b.id);
        let newer = f.store.update(&a, "later A").unwrap();
        assert!(f.store.update(&a, "stale A").is_err());
        assert!(f.store.discard(&a).is_err());
        assert_eq!(f.store.list().unwrap().drafts.len(), 2);
        f.store.discard(&newer).unwrap();
        assert_eq!(f.store.list().unwrap().drafts, vec![b]);
        assert!(f.store.update(&newer, "resurrect").is_err());
    }
    #[test]
    fn wrong_workspace_corruption_and_tampered_revision_are_visible() {
        let f = Fixture::new();
        let d = f.store.create(&f.base("base"), "text").unwrap();
        let mut wrong = f.workspace.clone();
        wrong["root"] = json!("/another/brain");
        let other = EditorRecovery::at(f.directory.clone(), &wrong).unwrap();
        let list = other.list().unwrap();
        assert!(list.drafts.is_empty());
        assert_eq!(list.problems.len(), 1);
        assert!(other.update(&d, "wrong").is_err());
        let mut base = f.base("base");
        base["content_base64"] = json!(STANDARD.encode("different"));
        assert!(f.store.create(&base, "draft").is_err());
        fs::write(f.store.path(&d.id).unwrap(), b"truncated{").unwrap();
        let list = f.store.list().unwrap();
        assert!(list.drafts.is_empty());
        assert_eq!(list.problems.len(), 1);
        assert!(f.store.discard(&d).is_err());
    }
    #[test]
    fn uncertain_save_keeps_exact_request_and_old_ack_preserves_later_draft() {
        let f = Fixture::new();
        let base = f.base("base");
        let d = f.store.create(&base, "save this").unwrap();
        let r = request(&base, "save this");
        let d = f.store.retain_save(&d, &r).unwrap();
        assert_eq!(f.store.retain_save(&d, &r).unwrap(), d);
        assert!(f
            .store
            .retain_save(&d, &request(&base, "save this"))
            .is_err());
        assert!(f.store.discard(&d).is_err());
        let later = f.store.update(&d, "newer unsaved text").unwrap();
        assert_eq!(later.pending_save, Some(r.clone()));
        let mut forged = receipt(&r);
        forged["revision"] = json!(revision(b"different"));
        assert!(f.store.acknowledge(&d.id, &r, &forged).is_err());
        let retained = f
            .store
            .acknowledge(&d.id, &r, &receipt(&r))
            .unwrap()
            .unwrap();
        assert_eq!(retained.text, "newer unsaved text");
        assert!(retained.pending_save.is_none());
        assert_eq!(
            snapshot_text(&retained.base, &f.workspace).unwrap(),
            "save this"
        );
        assert!(f.store.update(&later, "late old state").is_err());
        let next = request(&retained.base, &retained.text);
        let pending = f.store.retain_save(&retained, &next).unwrap();
        assert!(f
            .store
            .acknowledge(&pending.id, &next, &receipt(&next))
            .unwrap()
            .is_none());
        assert!(f.store.list().unwrap().drafts.is_empty());
    }
    #[test]
    fn conflict_survives_restart_and_resolution_uses_displayed_current_only() {
        let f = Fixture::new();
        let base = f.base("base");
        let d = f.store.create(&base, "proposal").unwrap();
        let r = request(&base, "proposal");
        let d = f.store.retain_save(&d, &r).unwrap();
        let current = f.base("other writer");
        let conflict = json!({"conflict":{"conflict_id":r["request"]["operation_id"],"path":base["path"],"expected_revision":base["revision"],"current_revision":current["revision"],"reason":"changed"},"base":base,"proposed":f.base("proposal"),"current":current});
        let d = f.store.record_conflict(&d.id, &r, &conflict).unwrap();
        assert_eq!(f.store.list().unwrap().drafts, vec![d.clone()]);
        let mut latest = conflict.clone();
        latest["current"] = f.base("third writer");
        let d = f.store.refresh_conflict(&d, &latest).unwrap();
        assert_eq!(d.conflict, Some(latest.clone()));
        let mut forged = latest.clone();
        forged["base"] = f.base("invented base");
        assert!(f.store.refresh_conflict(&d, &forged).is_err());
        let d = f.store.update(&d, "resolved draft").unwrap();
        assert!(f
            .store
            .retain_save(&d, &request(&f.base("invented current"), &d.text))
            .is_err());
        assert!(f
            .store
            .retain_save(&d, &request(&current, &d.text))
            .is_err());
        let next = request(&latest["current"], &d.text);
        let d = f.store.retain_save(&d, &next).unwrap();
        assert_eq!(d.base, base);
        assert_eq!(d.conflict, Some(latest));
        assert!(f
            .store
            .acknowledge(&d.id, &next, &receipt(&next))
            .unwrap()
            .is_none());
    }
    #[test]
    fn failed_publish_is_not_acknowledged_and_reopen_preserves_published_data() {
        let f = Fixture::new();
        let d = f.store.create(&f.base("base"), "first").unwrap();
        f.store
            .fail_after_publish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(f
            .store
            .update(&d, "retained despite uncertain fsync")
            .is_err());
        assert!(f
            .store
            .update(&d, "must not replace newer publication")
            .is_err());
        let list = f.store.list().unwrap();
        assert_eq!(list.drafts.len(), 1);
        assert_eq!(list.drafts[0].text, "retained despite uncertain fsync");
        let lock = f.store.lock().unwrap();
        assert!(f.store.update(&list.drafts[0], "busy").is_err());
        drop(lock);
        assert_eq!(f.store.list().unwrap().drafts, list.drafts);
    }
    #[test]
    fn idempotent_success_requires_durability_after_uncertain_publication() {
        let f = Fixture::new();
        let base = f.base("base");
        let d = f.store.create(&base, "draft").unwrap();
        let r = request(&base, "draft");
        f.store
            .fail_after_publish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(f.store.retain_save(&d, &r).is_err());
        let recovered = f.store.list().unwrap().drafts.remove(0);
        assert_eq!(recovered.pending_save, Some(r.clone()));
        f.store
            .fail_confirmation
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(f.store.update(&recovered, &recovered.text).is_err());
        f.store
            .fail_confirmation
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(f.store.retain_save(&recovered, &r).is_err());
        assert_eq!(f.store.list().unwrap().drafts, vec![recovered.clone()]);
        assert_eq!(
            f.store.update(&recovered, &recovered.text).unwrap(),
            recovered
        );
        assert_eq!(f.store.retain_save(&recovered, &r).unwrap(), recovered);
    }
    #[test]
    fn source_and_inventory_budgets_refuse_without_deleting_existing_drafts() {
        let f = Fixture::new();
        let base = f.base("base");
        assert!(f
            .store
            .create(&base, &"x".repeat(MAX_TEXT_BYTES + 1))
            .is_err());
        for _ in 0..MAX_DRAFTS {
            f.store.create(&base, "retained").unwrap();
        }
        assert!(f.store.create(&base, "overflow").is_err());
        assert_eq!(f.store.list().unwrap().drafts.len(), MAX_DRAFTS);
        let mut invalid = base.clone();
        invalid["path"] = json!("../escape.md");
        assert!(f.store.create(&invalid, "draft").is_err());
    }
}
