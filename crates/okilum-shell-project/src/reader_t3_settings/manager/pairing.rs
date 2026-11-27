use okilum_t3::Environment;

/// Secrets never appear in Debug, errors, identifiers or persisted UI state.
pub(crate) struct PairLink {
    credential: String,
    copied: String,
}
impl std::fmt::Debug for PairLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairLink(<redacted>)")
    }
}
impl PairLink {
    pub(crate) fn recognize(env: &Environment, text: &str) -> Option<Self> {
        if text.len() > 4096 {
            return None;
        }
        let url = url::Url::parse(text.trim()).ok()?;
        if url.origin().ascii_serialization() != env.origin()
            || url.path() != "/pair"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
        {
            return None;
        }
        let mut params = url::form_urlencoded::parse(url.fragment()?.as_bytes());
        let (key, value) = params.next()?;
        if key != "token"
            || params.next().is_some()
            || value.is_empty()
            || value.chars().any(char::is_whitespace)
        {
            return None;
        }
        Some(Self {
            credential: value.into_owned(),
            copied: text.to_owned(),
        })
    }
    pub(crate) fn manual(env: &Environment, text: &str) -> Option<Self> {
        Self::recognize(env, text).or_else(|| {
            let token = text.trim().to_ascii_uppercase();
            (token.len() == 12
                && token
                    .bytes()
                    .all(|b| b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains(&b)))
            .then(|| Self {
                credential: token,
                copied: text.to_owned(),
            })
        })
    }
    pub(crate) fn credential(&self) -> &str {
        &self.credential
    }
    pub(crate) fn matches_clipboard(&self, text: &str) -> bool {
        self.copied == text
    }
}
