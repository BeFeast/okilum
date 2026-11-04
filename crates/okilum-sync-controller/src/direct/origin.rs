//! Read-only provenance for an incoming editor conflict. Never guess a peer.
use super::*;

impl Controller {
    pub fn file_change_origin(
        &self,
        root: &Path,
        rel: &str,
        reviewed: &str,
    ) -> Result<Option<String>> {
        self.file_version_origin_inner(root, rel, reviewed, false)
    }

    /// Source-bound attribution for a conflict comparison. Unlike the incoming
    /// change notice, the local computer is also a meaningful origin here.
    pub fn file_version_origin(
        &self,
        root: &Path,
        rel: &str,
        reviewed: &str,
    ) -> Result<Option<String>> {
        self.file_version_origin_inner(root, rel, reviewed, true)
    }

    fn file_version_origin_inner(
        &self,
        root: &Path,
        rel: &str,
        reviewed: &str,
        include_local: bool,
    ) -> Result<Option<String>> {
        self.check()?;
        let _access = self.shared.access.enter();
        let path = Path::new(rel);
        ensure!(
            path.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid note path"
        );
        ensure!(root.canonicalize()? == root, "vault root redirected");
        let file = root.join(path);
        ensure!(
            file.parent().unwrap().canonicalize()? == file.parent().unwrap()
                && std::fs::symlink_metadata(&file)?.is_file(),
            "note path redirected"
        );
        ensure!(
            std::fs::read_to_string(&file)? == reviewed,
            "conflict changed"
        );
        let inventory = crate::daemon::discover(&self.configuration_paths()?);
        inventory.folders()?;
        let bindings: Vec<_> = inventory
            .candidates
            .iter()
            .flat_map(|c| {
                c.folders
                    .iter()
                    .filter(|(_, p)| p == root)
                    .map(move |(id, _)| (&c.identity, id))
            })
            .collect();
        if bindings.len() != 1 {
            return Ok(None);
        }
        let (identity, folder) = bindings[0];
        let api = identity.connect()?;
        let info = api.file_info(folder, rel)?;
        // Do not attribute an unscanned local write or an unreceived global
        // revision to the last peer in the database.
        let local = &info["local"];
        let global = &info["global"];
        if local["version"].is_null()
            || local["version"] != global["version"]
            || global["deleted"] == true
            || !matches_reviewed_bytes(local, global, reviewed.as_bytes())
        {
            return Ok(None);
        }
        let config = api.config()?;
        let name = if include_local {
            origin_name_any(&config, global["modifiedBy"].as_str())
        } else {
            origin_name(&config, global["modifiedBy"].as_str(), &identity.device_id)
        };
        ensure!(
            std::fs::read_to_string(file)? == reviewed,
            "conflict changed during lookup"
        );
        self.check()?;
        Ok(name)
    }
}

/// Syncthing v2.1.x exposes SHA-256(concatenated block SHA-256 hashes).
/// Its REST response omits rawBlockSize, so check the supported block sizes
/// against the reported count. Size/mtime alone cannot identify a same-size
/// external write that Syncthing has not scanned yet.
fn matches_reviewed_bytes(
    local: &serde_json::Value,
    global: &serde_json::Value,
    bytes: &[u8],
) -> bool {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use sha2::{Digest, Sha256};
    if local["size"].as_u64() != Some(bytes.len() as u64)
        || local["blocksHash"] != global["blocksHash"]
    {
        return false;
    }
    let Some(hash) = local["blocksHash"]
        .as_str()
        .and_then(|h| STANDARD.decode(h).ok())
    else {
        return false;
    };
    // Some metadata-only responses omit the block list/count. The hash is
    // still exact; no count or timestamp can substitute for matching it.
    let count = local["numBlocks"].as_u64().filter(|n| *n > 0);
    (17..=24).any(|power| {
        let block_size = 1usize << power;
        if count.is_some_and(|count| bytes.len().div_ceil(block_size).max(1) as u64 != count) {
            return false;
        }
        let mut digest = Sha256::new();
        if bytes.is_empty() {
            digest.update(Sha256::digest([]));
        } else {
            for block in bytes.chunks(block_size) {
                digest.update(Sha256::digest(block));
            }
        }
        let actual: [u8; 32] = digest.finalize().into();
        actual.as_slice() == hash.as_slice()
    })
}

fn origin_name(config: &serde_json::Value, short: Option<&str>, local: &str) -> Option<String> {
    if short.is_some_and(|short| local.starts_with(short)) {
        return None;
    }
    origin_name_any(config, short)
}
fn origin_name_any(config: &serde_json::Value, short: Option<&str>) -> Option<String> {
    let short = short.filter(|s| {
        s.len() == 7
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))
    })?;
    let mut matching = config["devices"].as_array()?.iter().filter(|device| {
        device["deviceID"]
            .as_str()
            .is_some_and(|id| id.starts_with(short))
    });
    let device = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    device["name"]
        .as_str()
        .filter(|n| !n.trim().is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn issue2486_origin_rejects_unscanned_same_size_content() {
        let one = json!({"size":12, "numBlocks":1, "blocksHash":"TySb+a4YDBNk45oMorddvkviK/qHGlJanSTJWfALCGc="});
        assert!(matches_reviewed_bytes(&one, &one, b"remote line\n"));
        assert!(
            !matches_reviewed_bytes(&one, &one, b"edited line\n"),
            "equal size is not a content witness"
        );
        let two = json!({"size":131073, "numBlocks":2, "blocksHash":"Fj8vzIwO2Pex2hFd7Nm+U/aHGlR9X5DbHjJGkZVd2NY="});
        assert!(matches_reviewed_bytes(&two, &two, &vec![b'a'; 131073]));
        let mut metadata_only = one.clone();
        metadata_only["numBlocks"] = json!(0);
        assert!(matches_reviewed_bytes(
            &metadata_only,
            &metadata_only,
            b"remote line\n"
        ));
        let mut unknown = one.clone();
        unknown["blocksHash"] = serde_json::Value::Null;
        assert!(!matches_reviewed_bytes(
            &unknown,
            &unknown,
            b"remote line\n"
        ));
        assert!(!matches_reviewed_bytes(&one, &unknown, b"remote line\n"));
    }

    #[test]
    fn issue2486_origin_requires_one_named_remote_device() {
        let config = json!({"devices":[{"deviceID":"AAAAAAA-LOCAL", "name":"Desktop"}, {"deviceID":"BBBBBBB-REMOTE", "name":"Laptop"}]});
        assert_eq!(
            origin_name(&config, Some("BBBBBBB"), "AAAAAAA-LOCAL"),
            Some("Laptop".into())
        );
        assert_eq!(origin_name(&config, Some("AAAAAAA"), "AAAAAAA-LOCAL"), None);
        assert_eq!(
            origin_name_any(&config, Some("AAAAAAA")),
            Some("Desktop".into()),
            "a conflict pane can identify a byte-matched local origin"
        );
        assert_eq!(origin_name(&config, Some("CCCCCCC"), "AAAAAAA-LOCAL"), None);
        assert_eq!(origin_name(&config, None, "AAAAAAA-LOCAL"), None);
        let ambiguous = json!({"devices":[{"deviceID":"BBBBBBB-ONE", "name":"One"}, {"deviceID":"BBBBBBB-TWO", "name":"Two"}]});
        assert_eq!(
            origin_name(&ambiguous, Some("BBBBBBB"), "AAAAAAA-LOCAL"),
            None
        );
    }
}
