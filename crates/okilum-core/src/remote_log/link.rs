//! Payload of `okilum://logs` links (docs/deep-links.md). The router resolves
//! the vault among roots the user already opened; nothing here guesses a
//! vault by name, writes an identity or touches credentials.
use super::discover::{Discover, TimeRange};
use super::Source;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// The link for what a Kibana window shows now: the applied query and
    /// filters over the loaded absolute range, with live paused.
    pub fn for_view(vault: &Path, source: &Source, range: &TimeRange) -> Result<Self> {
        let mut state = source.discover.clone();
        state.time = range.clone();
        state.refresh_paused = true;
        let link = Self {
            version: 1,
            vault_id: super::vault_id(vault),
            source_id: source.id,
            state,
        };
        link.validate()?;
        Ok(link)
    }
    /// The one known root this link names, holding its saved source. `known`
    /// are vault roots this computer already opened, canonical as stored.
    pub fn resolve_vault(&self, known: &[PathBuf], state: &Path) -> Result<PathBuf> {
        let root = known
            .iter()
            .find(|root| super::vault_id(root) == self.vault_id)
            .context(
                "This log link belongs to a vault that is not open in Okilum on this computer",
            )?;
        let sources = super::load_sources(state, root)?;
        ensure!(
            sources.iter().any(|source| source.id == self.source_id),
            "This Kibana source is not saved in the linked vault; import it explicitly"
        );
        Ok(root.clone())
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
        // Bound work before URL parsing or case conversion. This deliberately
        // only inspects the route prefix, even for an untrusted enormous URL.
        let claims_logs = input
            .get(..13)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("okilum://logs"))
            && input.as_bytes().get(13).is_none_or(|b| {
                matches!(
                    b,
                    b'/' | b'?' | b'#' | b':' | b'\\' | b'%' | b' ' | b'\t' | b'\r' | b'\n'
                )
            });
        if input.len() > MAX_LINK_BYTES {
            return claims_logs.then(|| Err(anyhow::anyhow!("Remote log link exceeds 128 KiB")));
        }
        let url = match url::Url::parse(input) {
            Ok(url) => url,
            Err(_) => {
                return claims_logs.then(|| Err(anyhow::anyhow!("Malformed remote log link")))
            }
        };
        if url.scheme() != "okilum"
            || !url
                .host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case("logs"))
        {
            return None;
        }
        Some((|| {
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
        assert_eq!(
            RemoteLink::parse(&encoded.replacen("//logs", "//LOGS", 1))
                .unwrap()
                .unwrap(),
            link
        );
        assert!(RemoteLink::parse("okilum://LoGs?v=2").unwrap().is_err());
        assert!(RemoteLink::parse("okilum://LoGs?payload=invalid")
            .unwrap()
            .is_err());
        for prefix in ["okilum://LOGS?payload=", "okilum://logs:invalid/"] {
            let oversized = format!("{prefix}{}", "x".repeat(MAX_LINK_BYTES));
            assert!(RemoteLink::parse(&oversized)
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("128 KiB"));
        }
        assert!(RemoteLink::parse(&format!(
            "okilum://file?path={}",
            "x".repeat(MAX_LINK_BYTES)
        ))
        .is_none());
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

    fn source() -> Source {
        let mut source = Source::direct("Synthetic", "https://logs.example", "events", "").unwrap();
        source.discover = super::super::tests::state();
        source
    }

    #[test]
    fn view_link_freezes_the_loaded_range_and_pauses_live() {
        let vault = Path::new("/vaults/work");
        let mut source = source();
        source.discover.time = TimeRange {
            from: "now-15m".into(),
            to: "now".into(),
        };
        source.discover.refresh_paused = false;
        let loaded = TimeRange {
            from: "2026-11-13T10:00:00Z".into(),
            to: "2026-11-13T10:15:00Z".into(),
        };
        let link = RemoteLink::for_view(vault, &source, &loaded).unwrap();
        assert_eq!(link.state.time, loaded);
        assert!(link.state.refresh_paused);
        assert_eq!(link.state.filters, source.discover.filters);
        assert_eq!(link.source_id, source.id);
        assert_eq!(link.vault_id, super::super::vault_id(vault));
        let decoded = RemoteLink::parse(&link.encode().unwrap()).unwrap().unwrap();
        assert_eq!(decoded, link);
        // Control: a relative range is refused, never silently linked.
        assert!(RemoteLink::for_view(vault, &source, &source.discover.time).is_err());
    }

    #[test]
    fn link_resolves_only_the_vault_that_saved_its_source() {
        let state = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let work = roots.path().join("work");
        // Same folder name elsewhere: a name match must never win.
        let other = roots.path().join("elsewhere").join("work");
        let source = source();
        super::super::save_source(state.path(), &work, &source).unwrap();
        let range = TimeRange {
            from: "2026-11-13T10:00:00Z".into(),
            to: "2026-11-13T10:15:00Z".into(),
        };
        let link = RemoteLink::for_view(&work, &source, &range).unwrap();
        assert_ne!(
            super::super::vault_id(&work),
            super::super::vault_id(&other)
        );
        let known = vec![other.clone(), work.clone()];
        assert_eq!(link.resolve_vault(&known, state.path()).unwrap(), work);
        let missing = link
            .resolve_vault(&[other.clone()], state.path())
            .unwrap_err();
        assert!(
            missing.to_string().contains("not open in Okilum"),
            "{missing}"
        );
        // The vault is known but the source was never saved there.
        let mut foreign = link.clone();
        foreign.source_id = uuid::Uuid::new_v4();
        let unsaved = foreign.resolve_vault(&known, state.path()).unwrap_err();
        assert!(unsaved.to_string().contains("not saved"), "{unsaved}");
    }
}
