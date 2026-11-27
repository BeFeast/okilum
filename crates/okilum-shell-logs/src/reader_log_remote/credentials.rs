//! Explicit memory-only credentials after an OS-store failure. Never serialized.
use super::*;

/// Credential kind label and secret bytes read from the platform store.
type KeyEntry = (String, Vec<u8>);

#[derive(Default)]
pub(super) struct Credentials {
    identity: String,
    pub(super) key: Option<ApiKey>,
    pub(super) session_only: bool,
    pub(super) retry_save: bool,
    pub(super) replacement: bool,
    pub(super) failure: Option<String>,
    details: bool,
    #[cfg(test)]
    pub(super) store: Option<Arc<std::sync::Mutex<TestStore>>>,
}
impl Credentials {
    pub(super) fn select(&mut self, identity: String) {
        if self.identity != identity {
            self.key = None;
            self.session_only = false;
            self.retry_save = false;
            self.failure = None;
            self.details = false;
            self.identity = identity;
        }
    }
}
#[derive(Debug)]
pub(super) struct StorageError(pub(super) String);
impl From<anyhow::Error> for StorageError {
    fn from(error: anyhow::Error) -> Self {
        Self(format!("{error:#}"))
    }
}
impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(recovery_message())
    }
}
impl std::error::Error for StorageError {}

pub(super) fn recovery_message() -> &'static str {
    if cfg!(target_os = "macos") {
        "The login keychain is locked or not available to this app. Unlock it in Keychain Access, then try again. You can also use this key for this session only."
    } else if cfg!(target_os = "windows") {
        "Windows Credential Manager could not save or read the key. Check your Windows sign-in and Credential Manager, then try again. You can also use this key for this session only."
    } else {
        "The desktop Secret Service is locked or unavailable. Unlock your keyring or start a Secret Service provider, then try again. You can also use this key for this session only."
    }
}

impl Panel {
    pub(super) fn write_key(&self, identity: &str, key: &ApiKey, cx: &App) -> Task<Result<()>> {
        #[cfg(test)]
        if let Some(store) = &self.credentials.store {
            let mut store = store.lock().unwrap();
            store.writes += 1;
            return Task::ready(if store.refuse {
                Err(anyhow::anyhow!(
                    "Synthetic credential store refused (-25308)"
                ))
            } else {
                store.entries.insert(identity.into(), key.clone());
                Ok(())
            });
        }
        cx.write_credentials(identity, "ApiKey", key.as_bytes())
    }
    pub(super) fn read_key(&self, identity: &str, cx: &App) -> Task<Result<Option<KeyEntry>>> {
        #[cfg(test)]
        if let Some(store) = &self.credentials.store {
            let store = store.lock().unwrap();
            return Task::ready(Ok(store
                .entries
                .get(identity)
                .map(|key| ("ApiKey".into(), key.as_bytes().to_vec()))));
        }
        cx.read_credentials(identity)
    }
    fn retry_credentials(
        &mut self,
        session_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.credential_pending || self.credentials.key.is_none() {
            return;
        }
        let Some(source) = self.requested.clone() else {
            return;
        };
        if source.credential_url(&self.vault) != self.credentials.identity {
            return;
        }
        let replacement = self.credentials.replacement;
        self.credentials.session_only |= session_only;
        self.credentials.retry_save = !session_only;
        self.stop();
        self.submit_query(source, replacement, true, self.generation, window, cx);
    }
    pub(super) fn credential_status(&self) -> &'static str {
        if self.credentials.session_only {
            " · Key not saved — session only"
        } else {
            ""
        }
    }
    pub(super) fn render_credential_recovery(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut section = v_flex().gap_2();
        if (self.credentials.failure.is_some() || self.credentials.session_only) && !self.auth_error
        {
            let mut actions = h_flex().gap_2().flex_wrap();
            if self.credentials.failure.is_some() && self.credentials.key.is_some() {
                actions = actions.child(
                    Button::new("remote-session-key")
                        .debug_selector(|| "remote-session-key".into())
                        .small()
                        .label("Use this key for this session only")
                        .disabled(self.busy || self.credential_pending)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.retry_credentials(true, window, cx)
                        })),
                );
            }
            actions =
                actions.child(
                    Button::new("remote-retry-saving")
                        .debug_selector(|| "remote-retry-saving".into())
                        .small()
                        .label("Retry saving")
                        .disabled(
                            self.credentials.key.is_none() || self.busy || self.credential_pending,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.retry_credentials(false, window, cx)
                        })),
                );
            if let Some(detail) = &self.credentials.failure {
                section = section.child(div().text_sm().child(recovery_message()));
                actions = actions.child(
                    Button::new("remote-credential-detail")
                        .small()
                        .ghost()
                        .label("Credential error details")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.credentials.details = !this.credentials.details;
                            cx.notify();
                        })),
                );
                if self.credentials.details {
                    section = section.child(div().text_xs().child(detail.clone()));
                }
            }
            section = section.child(actions);
        }
        section.into_any_element()
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct TestStore {
    pub(super) refuse: bool,
    pub(super) writes: usize,
    pub(super) entries: std::collections::BTreeMap<String, ApiKey>,
}
