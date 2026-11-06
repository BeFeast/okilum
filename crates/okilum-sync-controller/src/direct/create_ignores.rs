//! Narrow upgrade of the exact generated pre-#2569 policy. Custom policies
//! never acquire write authority here. The durable intent survives lost replies.
use super::*;
use serde_json::{json, Value};
use std::{fs::File, io::Read, os::unix::fs::MetadataExt};

pub(super) const STAGING: &str = ".okilum-create-*";
const OLD: &[&str] = &[
    ".okilum-index",
    ".okilum-save-*",
    "/.claude/worktrees",
    "/worktrees",
];

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub complete: bool,
    pending: Option<Change>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    folder: Value,
    before: Vec<u8>,
    after: Vec<u8>,
}

pub(super) fn lines() -> Vec<String> {
    OLD.iter()
        .copied()
        .chain([STAGING])
        .map(str::to_owned)
        .collect()
}
fn generated(patterns: &[String]) -> Vec<u8> {
    format!("{}\n", patterns.join("\n")).into_bytes()
}
fn read(root: &Path, identity: (u64, u64)) -> Result<Vec<u8>> {
    let fd = rustix::fs::open(
        root,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )?;
    let held = File::from(fd);
    let metadata = held.metadata()?;
    ensure!(
        (metadata.dev(), metadata.ino()) == identity,
        "Ignore policy vault identity changed"
    );
    let fd = rustix::fs::openat(
        &held,
        ".stignore",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let file = File::from(fd);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= 65536,
        "Ignore policy requires a bounded regular file"
    );
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536,
        "Ignore policy exceeds the migration limit"
    );
    let current = std::fs::symlink_metadata(root)?;
    ensure!(
        current.is_dir() && (current.dev(), current.ino()) == identity,
        "Ignore policy vault moved"
    );
    Ok(bytes)
}

impl Policy {
    pub fn current() -> Self {
        Self {
            complete: true,
            pending: None,
        }
    }
    /// Only exact generated LF bytes and their durable ignore witness qualify.
    /// A comment, CRLF, extra rule, include or negation requires explicit review.
    pub fn prepare(&mut self, folder: &Value, bytes: &[u8], expected: &[String]) -> Result<bool> {
        if self.complete {
            return Ok(false);
        }
        if self.pending.is_some() {
            return Ok(true);
        }
        let old: Vec<_> = OLD.iter().map(|s| s.to_string()).collect();
        if expected == lines() && bytes == generated(expected) {
            self.complete = true;
            return Ok(false);
        }
        ensure!(expected == old && bytes == generated(&old),
            "Ignore policy needs review before excluding interrupted .okilum-create files; custom rules were preserved");
        self.pending = Some(Change {
            folder: folder.clone(),
            before: bytes.to_vec(),
            after: generated(&lines()),
        });
        Ok(true)
    }
    pub fn read(root: &Path, identity: (u64, u64)) -> Result<Vec<u8>> {
        read(root, identity)
    }
    pub fn apply(
        &self,
        api: &okilum_sync::Syncthing,
        id: &str,
        root: &Path,
        identity: (u64, u64),
        check: impl Fn() -> Result<()>,
    ) -> Result<()> {
        let change = self
            .pending
            .as_ref()
            .context("Ignore migration intent missing")?;
        let mut paused = change.folder.clone();
        paused["paused"] = json!(true);
        let verify = || -> Result<Value> {
            check()?;
            let bytes = read(root, identity)?;
            ensure!(
                bytes == change.before || bytes == change.after,
                "Ignore policy changed during migration; review required"
            );
            let folder = api.folder(id)?;
            ensure!(
                folder == change.folder || folder == paused,
                "Ignore migration folder witness changed"
            );
            Ok(folder)
        };
        if verify()? == change.folder && change.folder["paused"] != true {
            api.patch_folder(id, &json!({"paused":true}))?;
        }
        ensure!(
            verify()? == paused,
            "Ignore migration pause readback failed"
        );
        let before: Vec<_> = OLD.iter().map(|s| s.to_string()).collect();
        api.replace_managed_ignores(id, &before, &lines())?;
        ensure!(
            read(root, identity)? == change.after,
            "Ignore migration byte readback failed"
        );
        ensure!(
            verify()? == paused,
            "Ignore migration paused witness changed"
        );
        if change.folder["paused"] != true {
            check()?;
            api.patch_folder(id, &json!({"paused":false}))?;
        }
        ensure!(
            verify()? == change.folder,
            "Ignore migration final readback failed"
        );
        Ok(())
    }
    pub fn finish(&mut self) {
        self.complete = true;
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_generated_policy_only_and_durable_pending_intent() -> Result<()> {
        let old: Vec<_> = OLD.iter().map(|s| s.to_string()).collect();
        let bytes = generated(&old);
        let folder = json!({"id":"fixture","paused":false});
        for changed in [
            format!("// custom\n{}", String::from_utf8_lossy(&bytes)),
            String::from_utf8_lossy(&bytes).replace('\n', "\r\n"),
            format!("!important\n{}", String::from_utf8_lossy(&bytes)),
            "#include policy\n".into(),
        ] {
            assert!(Policy::default()
                .prepare(&folder, changed.as_bytes(), &old)
                .is_err());
        }
        let mut policy = Policy::default();
        assert!(policy.prepare(&folder, &bytes, &old)?);
        let restored: Policy = serde_json::from_slice(&serde_json::to_vec(&policy)?)?;
        assert_eq!(restored.pending.unwrap().before, bytes);
        policy.finish();
        assert!(!policy.prepare(&folder, &generated(&lines()), &lines())?);
        Ok(())
    }
}
