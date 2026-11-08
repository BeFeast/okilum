//! One synced policy, included before a byte-preserved device-local block.
//! The policy file is durable canonical data; local includes are rebuildable.
use super::*;
use globset::GlobBuilder;

#[cfg(unix)]
mod files;
#[cfg(windows)]
#[path = "shared_ignores/files_windows.rs"]
mod files;
use files::Files;
const LIMIT: usize = 65536;
pub const POLICY_PATH: &str = ".okilum/sync-ignore";
pub(super) const INCLUDE: &str = "#include .okilum/sync-ignore";
const LOCAL_HEADER: &str =
    "#include .okilum/sync-ignore\n// Local exclusions on this computer; preserved by Okilum.\n";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IgnorePreview {
    vault: ExistingVault,
    policy: Option<Vec<u8>>,
    local: Option<Vec<u8>>,
    pub text: String,
    pub local_text: String,
    pub proposed_local: String,
}
pub fn validate(text: &str) -> Result<()> {
    ensure!(
        text.len() <= LIMIT && text.lines().count() <= 1024,
        "Shared ignore policy exceeds the limit"
    );
    let rules: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("//"))
        .collect();
    ensure!(
        create_ignores::lines()
            .iter()
            .all(|p| rules.contains(&p.as_str())),
        "Shared policy must retain the index, save/create staging and worktree exclusions"
    );
    for line in rules {
        ensure!(
            line.len() <= 4096
                && !line.chars().any(char::is_control)
                && !line.starts_with('#')
                && !line.contains('!'),
            "Shared policy includes and negations need separate review"
        );
        let mut pattern = line;
        let mut insensitive = false;
        loop {
            if let Some(rest) = pattern.strip_prefix("(?i)") {
                insensitive = true;
                pattern = rest;
            } else if let Some(rest) = pattern.strip_prefix("(?d)") {
                pattern = rest;
            } else {
                break;
            }
        }
        let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
        ensure!(
            !pattern.is_empty()
                && !pattern.contains("..")
                && !pattern.contains('\\')
                && !pattern.contains("(?"),
            "Unsupported shared ignore pattern"
        );
        let matcher = GlobBuilder::new(pattern)
            .literal_separator(true)
            .case_insensitive(insensitive)
            .build()?
            .compile_matcher();
        let own = if pattern.contains('/') {
            matcher.is_match(POLICY_PATH)
        } else {
            matcher.is_match(".okilum") || matcher.is_match("sync-ignore")
        };
        ensure!(
            !own,
            "Shared policy must not exclude its own .okilum/sync-ignore file"
        );
    }
    Ok(())
}
fn default_text(template: Option<&[u8]>) -> Result<String> {
    let local = std::str::from_utf8(template.unwrap_or_default())?;
    ensure!(
        !local
            .lines()
            .any(|s| s.trim_start().starts_with("#include")),
        "Review existing includes before creating a shared policy"
    );
    let text = format!(
        "{}\n.git\n/.obsidian\n.obsidian/workspace*.json\n\n{}",
        create_ignores::lines().join("\n"),
        local
    );
    validate(&text)?;
    Ok(text)
}
fn included(local: &[u8]) -> Result<bool> {
    let text = std::str::from_utf8(local)?;
    let includes: Vec<_> = text
        .lines()
        .filter(|s| s.trim_start().starts_with("#include"))
        .collect();
    ensure!(
        includes.is_empty() || includes == [INCLUDE] && text.lines().next() == Some(INCLUDE),
        "Local ignore includes differ; review before changing anything"
    );
    Ok(!includes.is_empty())
}
fn proposed(local: Option<&[u8]>) -> Result<Vec<u8>> {
    let local = local.unwrap_or_default();
    if included(local)? {
        return Ok(local.to_vec());
    }
    let mut after = LOCAL_HEADER.as_bytes().to_vec();
    after.extend_from_slice(local);
    ensure!(
        after.len() <= LIMIT,
        "Local ignore policy exceeds the limit"
    );
    Ok(after)
}
pub(super) fn shared_source(
    path: &Path,
    identity: (u64, u64),
    local: &[String],
) -> Result<Option<Vec<u8>>> {
    let text = local.join("\n");
    if !included(text.as_bytes())? {
        return Ok(None);
    }
    let data = Files::open(path, identity)?
        .policy()?
        .context("Shared ignore include file missing")?;
    validate(std::str::from_utf8(&data)?)?;
    Ok(Some(data))
}
pub(super) fn validate_source_file(path: &Path) -> Result<Vec<u8>> {
    let root = existing::VaultRoot::open(path)?;
    let files = Files::open(path, root.identity()?)?;
    let local = files.local()?.context("Local ignore file missing")?;
    ensure!(included(&local)?, "Shared ignore include missing");
    let policy = files
        .policy()?
        .context("Shared ignore include file missing")?;
    validate(std::str::from_utf8(&policy)?)?;
    Ok(local)
}
pub(super) fn seed(path: &Path, identity: (u64, u64), bytes: &[u8]) -> Result<()> {
    validate(std::str::from_utf8(bytes)?)?;
    let files = Files::open(path, identity)?;
    match files.policy()? {
        Some(existing) => ensure!(
            existing == bytes,
            "Receiving shared policy changed; review required"
        ),
        None => files.publish_policy(None, bytes)?,
    }
    Ok(())
}

impl Controller {
    pub(super) fn record_ignore_policy(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.record_source_ignore_policy(path, bytes)?;
        self.record_transfer_ignore_policy(path, bytes)
    }
    fn ignore_binding(&self, path: &Path) -> Result<ExistingVault> {
        self.check()?;
        let fresh = inspect_existing(&self.configuration_paths()?, path)?;
        ensure!(
            self.shared
                .db
                .lock()
                .unwrap()
                .attached
                .iter()
                .any(|v| existing::same_binding(v, &fresh)),
            "Shared ignore requires a freshly verified attached vault"
        );
        Ok(fresh)
    }
    fn ignore_preview(&self, path: &Path) -> Result<IgnorePreview> {
        let vault = self.ignore_binding(path)?;
        let files = Files::open(
            &vault.path,
            vault
                .root_identity
                .context("Ignore vault identity missing")?,
        )?;
        let policy = files.policy()?;
        let local = files.local()?;
        let text = match &policy {
            Some(bytes) => {
                let text = std::str::from_utf8(bytes)?;
                text.to_owned()
            }
            None => default_text(files.template()?.as_deref())?,
        };
        let after = proposed(local.as_deref())?;
        Ok(IgnorePreview {
            vault,
            text,
            local_text: String::from_utf8(local.clone().unwrap_or_default())?,
            proposed_local: String::from_utf8(after)?,
            policy,
            local,
        })
    }
    pub fn preview_ignore_policy(&self, path: &Path) -> Result<IgnorePreview> {
        let _guard = self.shared.transfer_lock.lock().unwrap();
        self.ignore_preview(path)
    }
    pub fn apply_ignore_policy(&self, preview: &IgnorePreview, text: &str) -> Result<Snapshot> {
        self.apply_ignore_policy_checked(preview, text, || Ok(()))
    }
    pub fn apply_ignore_policy_checked(
        &self,
        preview: &IgnorePreview,
        text: &str,
        check: impl Fn() -> Result<()>,
    ) -> Result<Snapshot> {
        check()?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        validate(text)?;
        let fresh = self.ignore_preview(&preview.vault.path)?;
        ensure!(
            existing::same_binding(&fresh.vault, &preview.vault)
                && fresh.policy == preview.policy
                && fresh.local == preview.local
                && fresh.text == preview.text,
            "Ignore policy or local rules changed since preview; refresh before saving"
        );
        let files = Files::open(
            &fresh.vault.path,
            fresh
                .vault
                .root_identity
                .context("Ignore root identity missing")?,
        )?;
        self.check()?;
        check()?;
        files.publish_policy(fresh.policy.as_deref(), text.as_bytes())?;
        self.check()?;
        ensure!(
            files.local()? == fresh.local,
            "Local rules changed before adding the include; preserved for review"
        );
        check()?;
        files.publish_local(fresh.local.as_deref(), fresh.proposed_local.as_bytes())?;
        let api = fresh.vault.daemon.connect()?;
        check_daemon(&api, &fresh.vault.folder_id)?;
        api.scan(&fresh.vault.folder_id)?;
        ensure!(
            files.policy()?.as_deref() == Some(text.as_bytes())
                && files.local()?.as_deref() == Some(fresh.proposed_local.as_bytes()),
            "Shared ignore readback changed; refresh to review"
        );
        self.record_ignore_policy(&fresh.vault.path, fresh.proposed_local.as_bytes())?;
        self.snapshot()
    }
    fn verify_policy_revision(&self, vault: &ExistingVault, policy: &[u8]) -> Result<()> {
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(policy));
        let api = vault.daemon.connect()?;
        check_daemon(&api, &vault.folder_id)?;
        let changed = self
            .shared
            .policy_revisions
            .lock()
            .unwrap()
            .get(&vault.path)
            != Some(&hash);
        if changed {
            api.scan(&vault.folder_id)?;
            self.shared
                .policy_revisions
                .lock()
                .unwrap()
                .insert(vault.path.clone(), hash);
        }
        Ok(())
    }
    /// Deterministic replay: a synced valid policy can upgrade only exact
    /// generated local bytes, or acknowledge our exact include+old-witness result.
    pub(super) fn reconcile_shared_ignore(
        &self,
        path: &Path,
        identity: (u64, u64),
        expected: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let files = Files::open(path, identity)?;
        let Some(policy) = files.policy()? else {
            ensure!(
                !included(files.local()?.as_deref().unwrap_or_default())?,
                "Shared ignore include file is missing; restore it before Sync can continue"
            );
            return Ok(None);
        };
        validate(std::str::from_utf8(&policy)?)?;
        let local = files
            .local()?
            .context("Local ignores disappeared; review required")?;
        let after = proposed(Some(expected))?;
        if local == after {
            let vault = self.ignore_binding(path)?;
            self.verify_policy_revision(&vault, &policy)?;
            return Ok((local != expected).then_some(local));
        }
        let standard = format!("{}\n", create_ignores::lines().join("\n")).into_bytes();
        ensure!(local == expected && local == standard,
            "Local .stignore differs from the shared policy; review its exact bytes before adding an include");
        let vault = self.ignore_binding(path)?;
        self.check()?;
        files.publish_local(Some(&local), &after)?;
        self.verify_policy_revision(&vault, &policy)?;
        ensure!(
            files.policy()?.as_deref() == Some(policy.as_slice())
                && files.local()?.as_deref() == Some(after.as_slice()),
            "Shared ignore propagation readback changed"
        );
        Ok(Some(after))
    }
}
fn check_daemon(api: &okilum_sync::Syncthing, id: &str) -> Result<()> {
    let observed = api.ignores(id)?;
    ensure!(
        observed["error"].is_null() || observed["error"] == "",
        "Syncthing could not load the shared include: {}",
        observed["error"]
    );
    let lines = observed["ignore"]
        .as_array()
        .context("Ignore readback missing")?;
    ensure!(
        lines.first().is_some_and(|s| s == INCLUDE),
        "Syncthing include readback mismatch"
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn shared_policy_preserves_local_bytes_and_rejects_self_exclusion() -> Result<()> {
        let local = b"// exact local CRLF\r\n/Life/private\r\n";
        let text = default_text(Some(local))?;
        validate(&text)?;
        let after = proposed(Some(local))?;
        assert!(after.ends_with(local));
        assert_eq!(proposed(Some(&after))?, after);
        for rule in [
            "*",
            "**",
            ".okilum",
            "/.okilum/**",
            "**/sync-ignore",
            "!private",
            "#include ../outside",
            "(?i).OKILUM",
        ] {
            assert!(validate(&format!("{text}\n{rule}\n")).is_err(), "{rule}");
        }
        Ok(())
    }
    #[test]
    fn shared_policy_files_refuse_symlinks_and_stale_revisions() -> Result<()> {
        use std::os::unix::fs::{symlink, MetadataExt};
        let root = tempfile::tempdir()?;
        let metadata = std::fs::metadata(root.path())?;
        let files = Files::open(root.path(), (metadata.dev(), metadata.ino()))?;
        let text = default_text(None)?;
        files.publish_policy(None, text.as_bytes())?;
        assert!(files.publish_policy(None, b"stale").is_err());
        files.publish_local(None, b"// current\r\n")?;
        assert!(files.publish_local(Some(b"stale"), b"replacement").is_err());
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"untouched")?;
        std::fs::remove_file(root.path().join(".stignore"))?;
        symlink(&outside, root.path().join(".stignore"))?;
        assert!(files.publish_local(None, b"no").is_err());
        assert_eq!(std::fs::read(outside)?, b"untouched");
        Ok(())
    }
}
