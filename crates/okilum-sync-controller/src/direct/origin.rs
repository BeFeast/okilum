//! Read-only provenance for an incoming editor conflict. Never guess a peer.
use super::*;

impl Controller {
    pub fn file_change_origin(
        &self,
        root: &Path,
        rel: &str,
        reviewed: &str,
    ) -> Result<Option<String>> {
        self.check()?;
        let _access = self.shared.access.enter();
        let path = Path::new(rel);
        ensure!(
            path.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid note path"
        );
        let file = root.join(path);
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
            || local["size"].as_u64() != Some(reviewed.len() as u64)
        {
            return Ok(None);
        }
        let config = api.config()?;
        let name = origin_name(&config, global["modifiedBy"].as_str(), &identity.device_id);
        ensure!(
            std::fs::read_to_string(file)? == reviewed,
            "conflict changed during lookup"
        );
        self.check()?;
        Ok(name)
    }
}

fn origin_name(config: &serde_json::Value, short: Option<&str>, local: &str) -> Option<String> {
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
    if matching.next().is_some() || device["deviceID"].as_str() == Some(local) {
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
    fn issue2486_origin_requires_one_named_remote_device() {
        let config = json!({"devices":[{"deviceID":"AAAAAAA-LOCAL", "name":"Desktop"}, {"deviceID":"BBBBBBB-REMOTE", "name":"Laptop"}]});
        assert_eq!(
            origin_name(&config, Some("BBBBBBB"), "AAAAAAA-LOCAL"),
            Some("Laptop".into())
        );
        assert_eq!(origin_name(&config, Some("AAAAAAA"), "AAAAAAA-LOCAL"), None);
        assert_eq!(origin_name(&config, Some("CCCCCCC"), "AAAAAAA-LOCAL"), None);
        assert_eq!(origin_name(&config, None, "AAAAAAA-LOCAL"), None);
        let ambiguous = json!({"devices":[{"deviceID":"BBBBBBB-ONE", "name":"One"}, {"deviceID":"BBBBBBB-TWO", "name":"Two"}]});
        assert_eq!(
            origin_name(&ambiguous, Some("BBBBBBB"), "AAAAAAA-LOCAL"),
            None
        );
    }
}
