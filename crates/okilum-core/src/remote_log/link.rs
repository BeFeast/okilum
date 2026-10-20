//! Payload seam for the existing external-link router. No vault resolution,
//! identity writes or credentials occur here; that router retains ownership.
use super::discover::Discover;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};

pub const MAX_LINK_BYTES: usize = 131_072;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteLink {
    pub version: u8,
    pub vault_id: uuid::Uuid,
    pub source_id: uuid::Uuid,
    pub state: Discover,
}
impl RemoteLink {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "This remote log link needs a different Okilum version"
        );
        self.state.validate()?;
        ensure!(
            self.state.time.absolute() && self.state.refresh_paused,
            "Remote log links must carry a fixed absolute range with live paused"
        );
        Ok(())
    }
    pub fn encode(&self) -> Result<String> {
        self.validate()?;
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?);
        let value = format!("okilum://logs?v=1&payload={payload}");
        ensure!(
            value.len() <= MAX_LINK_BYTES,
            "Remote log link exceeds 128 KiB"
        );
        Ok(value)
    }
    /// `Some(Err)` claims a malformed logs route so the router must refuse it,
    /// never fall through to note/file or name-based vault resolution.
    pub fn parse(input: &str) -> Option<Result<Self>> {
        let url = match url::Url::parse(input) {
            Ok(url) => url,
            Err(_) => {
                return input
                    .to_ascii_lowercase()
                    .starts_with("okilum://logs")
                    .then(|| Err(anyhow::anyhow!("Malformed remote log link")))
            }
        };
        if url.scheme() != "okilum" || url.host_str() != Some("logs") {
            return None;
        }
        Some((|| {
            ensure!(
                input.len() <= MAX_LINK_BYTES,
                "Remote log link exceeds 128 KiB"
            );
            ensure!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.port().is_none()
                    && url.fragment().is_none()
                    && matches!(url.path(), "" | "/"),
                "Malformed remote log link"
            );
            let mut version = None;
            let mut payload = None;
            for (key, value) in url.query_pairs() {
                match key.as_ref() {
                    "v" => {
                        ensure!(version.is_none(), "Duplicate remote link version");
                        version = Some(value.into_owned());
                    }
                    "payload" => {
                        ensure!(payload.is_none(), "Duplicate remote link payload");
                        payload = Some(value.into_owned());
                    }
                    _ => anyhow::bail!("Unsupported remote link parameter"),
                }
            }
            ensure!(
                version.as_deref() == Some("1"),
                "Unsupported remote log link version"
            );
            let bytes = URL_SAFE_NO_PAD
                .decode(payload.context("Remote log payload is missing")?)
                .context("Malformed remote log payload")?;
            let link: Self =
                serde_json::from_slice(&bytes).context("Unsupported remote log payload")?;
            link.validate()?;
            Ok(link)
        })())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_query_filters_unicode_and_absolute_nanos_round_trip_without_endpoint_or_key() {
        let link = RemoteLink {
            version: 1,
            vault_id: uuid::Uuid::new_v4(),
            source_id: uuid::Uuid::new_v4(),
            state: super::super::tests::state(),
        };
        let encoded = link.encode().unwrap();
        assert!(encoded.starts_with("okilum://logs?"));
        assert_eq!(RemoteLink::parse(&encoded).unwrap().unwrap(), link);
        assert!(!encoded.contains("https"));
        assert!(RemoteLink::parse(&(encoded + "&payload=x"))
            .unwrap()
            .is_err());
        assert!(RemoteLink::parse("okilum://logs?v=2").unwrap().is_err());
        assert!(RemoteLink::parse("okilum://file?path=a").is_none());
        let mut relative = link;
        relative.state.time.to = "now".into();
        assert!(relative.encode().is_err());
    }
}
