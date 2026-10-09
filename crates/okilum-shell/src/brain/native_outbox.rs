//! Immutable native delivery journal shared by capture and bound attention.
use super::*;
use std::path::{Path, PathBuf};

const OUTBOX_SCHEMA: &str = "tessera-inbox-outbox/v1";

pub(super) fn write_once(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let root = path.parent().ok_or("Invalid inbox recovery path")?;
    let mut missing = Vec::new();
    let mut parent = root;
    while !parent.exists() {
        missing.push(parent.to_path_buf());
        parent = parent.parent().ok_or("Invalid inbox recovery directory")?;
    }
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    for directory in missing.iter().rev() {
        std::fs::File::open(directory.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    let temp = root.join(format!(".inbox-{}.tmp", uuid()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match std::fs::hard_link(&temp, path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(path)? != bytes {
                    return Err(std::io::Error::other("Inbox recovery identity mismatch"));
                }
            }
            Err(e) => return Err(e),
        }
        std::fs::File::open(root)?.sync_all()
    })();
    let _ = std::fs::remove_file(temp);
    result.map_err(|e| format!("Cannot save inbox recovery state: {e}"))
}

pub(super) struct InboxJournal {
    pub(super) root: PathBuf,
    pub(super) workspace: Value,
    pub(super) instance: String,
    attention: bool,
    planning: bool,
    maestro: bool,
    #[cfg(test)]
    pub(super) fail_retain_after_publish: std::cell::Cell<bool>,
}
impl InboxJournal {
    fn schema(&self) -> &str {
        if self.maestro {
            "tessera-maestro-outbox/v1"
        } else if self.planning {
            "tessera-inbox-plan-outbox/v1"
        } else if self.attention {
            "tessera-attention-outbox/v1"
        } else {
            OUTBOX_SCHEMA
        }
    }
    fn valid_action(&self, request: &Value) -> bool {
        if self.maestro {
            matches!(
                request["op"].as_str(),
                Some("maestro_link" | "maestro_unlink" | "maestro_approval_decision")
            )
        } else if self.planning {
            request["op"] == "inbox_plan"
        } else if self.attention {
            matches!(
                request["op"].as_str(),
                Some("attention_reply" | "attention_ack")
            )
        } else {
            request["op"] == "inbox_capture"
        }
    }
    fn valid_receipt(&self, request: &Value, receipt: &Value) -> bool {
        match request["op"].as_str() {
            Some("inbox_plan") => {
                Uuid::parse_str(&text(&receipt["goal_id"])).is_ok()
                    && receipt["origin"]["schema"] == "ai-brain/inbox-origin-v1"
                    && receipt["origin"]["brain_id"] == self.workspace["brain_id"]
                    && receipt["origin"]["capture_id"] == request["capture_id"]
                    && receipt["origin"]["path"] == request["_inbox_plan_origin"]["item"]["path"]
                    && receipt["origin"]["revision"] == request["expected_capture_revision"]
                    && receipt["origin"]["operation_id"] == request["operation_id"]
                    && receipt["origin"]["planned_by"] == request["source"]
                    && receipt["origin"]["text"] == request["_inbox_plan_origin"]["text"]
                    && receipt["origin"]["source_snapshot"]
                        == request["_inbox_plan_origin"]["source"]
            }
            Some("inbox_capture") => {
                Uuid::parse_str(&text(&receipt["capture_id"])).is_ok()
                    && text(&receipt["revision"]).starts_with("sha256:")
            }
            Some("attention_reply") => {
                Uuid::parse_str(&text(&receipt["decision_id"])).is_ok()
                    && text(&receipt["revision"]).starts_with("sha256:")
            }
            Some("attention_ack") => receipt["acknowledged_at"].is_string(),
            _ => false,
        }
    }
    pub(super) fn open_maestro(workspace: &Value) -> Result<Self, String> {
        Ok(Self::open(workspace)?.for_maestro())
    }
    pub(super) fn for_maestro(mut self) -> Self {
        self.root = self.root.join("maestro");
        self.maestro = true;
        self
    }
    pub(super) fn open_planning(workspace: &Value) -> Result<Self, String> {
        Ok(Self::open(workspace)?.for_planning())
    }
    pub(super) fn for_planning(mut self) -> Self {
        self.root = self.root.join("planning");
        self.planning = true;
        self
    }
    pub(super) fn open_attention(workspace: &Value) -> Result<Self, String> {
        Ok(Self::open(workspace)?.for_attention())
    }
    pub(super) fn for_attention(mut self) -> Self {
        self.root = self.root.join("attention");
        self.attention = true;
        self
    }
    /// Only an explicitly chosen replacement may retire an old stale request.
    /// Publish the new immutable request first; interruption preserves recovery.
    pub(super) fn supersede(&self, old: &Value, next: &Value) -> Result<(), String> {
        if !self.attention || old["operation_id"] == next["operation_id"] {
            return Err("Invalid attention replacement identity.".into());
        }
        self.retain(next)?;
        write_once(
            &self.path(old, "superseded")?,
            &serde_json::to_vec(next).map_err(|e| e.to_string())?,
        )
    }
    pub(super) fn open(workspace: &Value) -> Result<Self, String> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            })
            .join("okilum/inbox-outbox");
        Self::at(base, workspace)
    }
    pub(super) fn at(base: PathBuf, workspace: &Value) -> Result<Self, String> {
        let brain = Uuid::parse_str(&text(&workspace["brain_id"]))
            .map_err(|_| "Invalid inbox workspace identity")?;
        let identity_path = base.join("client.json");
        if !identity_path.exists() {
            // Two windows may race to install the client identity. The winner's
            // existing file remains authoritative; no second identity is used.
            let _ = write_once(
                &identity_path,
                &serde_json::to_vec(&json!({"instance_id":uuid()})).unwrap(),
            );
        }
        let identity: Value =
            serde_json::from_slice(&std::fs::read(identity_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let instance = Uuid::parse_str(&text(&identity["instance_id"]))
            .map_err(|_| "Invalid inbox client identity")?
            .to_string();
        Ok(Self {
            root: base.join(brain.to_string()),
            workspace: workspace.clone(),
            instance,
            attention: false,
            planning: false,
            maestro: false,
            #[cfg(test)]
            fail_retain_after_publish: std::cell::Cell::new(false),
        })
    }
    fn path(&self, request: &Value, extension: &str) -> Result<PathBuf, String> {
        let id = Uuid::parse_str(&text(&request["operation_id"]))
            .map_err(|_| "Invalid inbox operation identity")?;
        Ok(self.root.join(format!("{id}.{extension}")))
    }
    fn bytes(&self, request: &Value) -> Result<Vec<u8>, String> {
        serde_json::to_vec(
            &json!({"schema":if self.maestro && request["op"]=="maestro_approval_decision" {"tessera-maestro-outbox/v2"} else {self.schema()},"workspace":self.workspace,"request":request}),
        )
        .map_err(|e| e.to_string())
    }
    pub(super) fn retain(&self, request: &Value) -> Result<(), String> {
        write_once(&self.path(request, "json")?, &self.bytes(request)?)?;
        #[cfg(test)]
        if self.fail_retain_after_publish.replace(false) {
            return Err("Injected failure after outbox publication".into());
        }
        Ok(())
    }
    pub(super) fn acknowledge(&self, request: &Value, receipt: &Value) -> Result<(), String> {
        let valid = if self.maestro {
            receipt["operation_id"] == request["operation_id"]
                && receipt["goal_id"] == request["goal_id"]
                && Uuid::parse_str(&text(&receipt["link_id"])).is_ok()
                && match request["op"].as_str() {
                    Some("maestro_link") => receipt["linked"] == true,
                    Some("maestro_unlink") => {
                        receipt["unlinked"] == true
                            && receipt["link_id"] == request["expected_link_id"]
                    }
                    Some("maestro_approval_decision") => {
                        super::maestro_ui::matching_decision_receipt(request, receipt)
                    }
                    _ => false,
                }
        } else {
            receipt["receipt"]["status"] == "committed"
                && receipt["receipt"]["operation_id"] == request["operation_id"]
                && self.valid_receipt(request, receipt)
        };
        if !valid {
            return Err("Inbox acknowledgement does not match the retained request.".into());
        }
        let bytes = self.bytes(request)?;
        if std::fs::read(self.path(request, "json")?).map_err(|e| e.to_string())? != bytes {
            return Err("Inbox recovery record changed; the request is retained.".into());
        }
        write_once(&self.path(request, "done")?, &bytes)
    }
    pub(super) fn archive_never_sent(
        &self,
        request: &Value,
        disposition: &Value,
    ) -> Result<(), String> {
        if request["op"] != "maestro_approval_decision"
            || !super::maestro_ui::matching_disposition(request, disposition)
            || disposition["status"] != "rejected"
            || disposition["rejection"]["never_sent"] != true
        {
            return Err("No exact durable never-sent proof is available.".into());
        }
        self.retain(request)?;
        write_once(&self.path(request, "rejected")?, &self.bytes(request)?)
    }
    pub(super) fn archive_rejected(&self, request: &Value) -> Result<(), String> {
        if request["op"] == "maestro_approval_decision" {
            return Err("A possibly sent remote decision cannot be archived as rejected.".into());
        }
        if !self.attention && !self.planning && !self.maestro {
            return Err("Only a definitively rejected action can be archived.".into());
        }
        self.retain(request)?;
        write_once(&self.path(request, "rejected")?, &self.bytes(request)?)
    }
    pub(super) fn rejected(&self) -> Result<Vec<Value>, String> {
        self.read_pending(true)
    }
    pub(super) fn pending(&self) -> Result<Vec<Value>, String> {
        self.read_pending(false)
    }
    fn read_pending(&self, rejected_only: bool) -> Result<Vec<Value>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let mut paths = entries
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        paths.sort();
        let mut pending = Vec::new();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let entry: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let request = &entry["request"];
            let schema = if self.maestro && request["op"] == "maestro_approval_decision" {
                "tessera-maestro-outbox/v2"
            } else {
                self.schema()
            };
            if entry["schema"] != schema
                || entry["workspace"] != self.workspace
                || !self.valid_action(request)
                || self.path(request, "json")? != path
                || request["source"]["instance_id"] != self.instance
            {
                return Err("Inbox recovery belongs to another client or workspace.".into());
            }
            let rejected = match std::fs::read(self.path(request, "rejected")?) {
                Ok(rejected) if rejected == bytes => true,
                Ok(_) => return Err("Rejected delivery identity mismatch.".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.to_string()),
            };
            if rejected_only {
                if rejected {
                    pending.push(request.clone());
                }
                continue;
            }
            if rejected {
                continue;
            }
            if let Ok(next) = std::fs::read(self.path(request, "superseded")?) {
                let next: Value = serde_json::from_slice(&next).map_err(|e| e.to_string())?;
                if std::fs::read(self.path(&next, "json")?).map_err(|e| e.to_string())?
                    != self.bytes(&next)?
                {
                    return Err("Replacement recovery record is missing or changed.".into());
                }
                continue;
            }
            match std::fs::read(self.path(request, "done")?) {
                Ok(done) if done == bytes => continue,
                Ok(_) => return Err("Inbox acknowledgement identity mismatch.".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            pending.push(request.clone());
        }
        Ok(pending)
    }
    pub(super) fn request(&self, text: &str, actor: &str) -> Result<Value, String> {
        if text.trim().is_empty() || text.len() > 65_536 {
            return Err("Enter a thought of at most 65,536 UTF-8 bytes.".into());
        }
        if actor.is_empty() {
            return Err("The backend has not supplied a local capture identity.".into());
        }
        let id = uuid();
        Ok(
            json!({"op":"inbox_capture","operation_id":id,"text":text,"source":{
            "channel":"native","instance_id":self.instance,"account_id":"local","actor_id":actor,
            "chat_id":null,"topic_id":null,"message_id":id,"update_id":id,"uri":null}}),
        )
    }
}
