//! GPUI owns the platform store (Keychain on macOS); no file fallback.
use super::*;

type Saved = Option<(String, Vec<u8>)>;

#[derive(Default)]
pub(super) struct Store {
    #[cfg(test)]
    pub(super) test: Option<Arc<std::sync::Mutex<TestStore>>>,
}

impl Store {
    pub(super) fn read(&self, key: &str, cx: &App) -> Task<anyhow::Result<Saved>> {
        #[cfg(test)]
        if let Some(store) = &self.test {
            let mut store = store.lock().unwrap();
            store.reads.push(key.into());
            return Task::ready(Ok(store.entries.get(key).cloned()));
        }
        cx.read_credentials(key)
    }

    #[cfg(native_manager)]
    pub(super) fn write(
        &self,
        key: &str,
        user: &str,
        bytes: &[u8],
        cx: &App,
    ) -> Task<anyhow::Result<()>> {
        #[cfg(test)]
        if let Some(store) = &self.test {
            let mut store = store.lock().unwrap();
            if store.refuse_write {
                return Task::ready(Err(anyhow::anyhow!("Synthetic locked credential store")));
            }
            store
                .entries
                .insert(key.into(), (user.into(), bytes.into()));
            return Task::ready(Ok(()));
        }
        cx.write_credentials(key, user, bytes)
    }

    #[cfg(native_manager)]
    pub(super) fn delete(&self, key: &str, cx: &App) -> Task<anyhow::Result<()>> {
        #[cfg(test)]
        if let Some(store) = &self.test {
            store.lock().unwrap().entries.remove(key);
            return Task::ready(Ok(()));
        }
        cx.delete_credentials(key)
    }
}

impl ProjectView {
    pub(super) fn restore_stored_token(
        &self,
        environment: &Environment,
        cx: &App,
    ) -> Task<anyhow::Result<Option<StoredToken>>> {
        #[cfg(native_manager)]
        let native_panel = self.threads.enabled;
        #[cfg(not(native_manager))]
        let native_panel = false;
        // A retained ProjectView must not prompt for Keychain access behind the
        // Mac WKWebView. #2522 selects the native panel through threads.enabled.
        if cfg!(target_os = "macos") && !native_panel {
            return Task::ready(Ok(None));
        }
        let read = self.credentials.read(&environment.credential_url(), cx);
        cx.background_executor().spawn(async move {
            read.await?
                .map(|(_, bytes)| StoredToken::from_secret(&bytes))
                .transpose()
        })
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct TestStore {
    pub(super) entries: std::collections::BTreeMap<String, (String, Vec<u8>)>,
    pub(super) reads: Vec<String>,
    pub(super) refuse_write: bool,
}

#[cfg(all(test, native_manager))]
mod tests;
