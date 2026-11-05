//! GPUI owns the platform store (Keychain on macOS); no file fallback.
use super::*;

type Saved = Option<(String, Vec<u8>)>;

#[derive(Default)]
pub(crate) struct Store {
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

#[cfg(test)]
#[derive(Default)]
pub(super) struct TestStore {
    pub(super) entries: std::collections::BTreeMap<String, (String, Vec<u8>)>,
    pub(super) reads: Vec<String>,
    pub(super) refuse_write: bool,
}

#[cfg(all(test, native_manager))]
mod tests;

impl Store {
    pub(crate) fn gateway(
        &self,
        env: Environment,
        mcp: Option<Arc<dyn ManagerGateway>>,
        access: okilum_t3::NetworkAccess,
        cx: &App,
    ) -> Task<anyhow::Result<Arc<dyn ManagerGateway>>> {
        if !access.allowed() {
            return Task::ready(Err(anyhow::anyhow!("Agents is disabled")));
        }
        let saved_mcp = if mcp.is_none() {
            self.read(&env.credential_url(), cx)
        } else {
            Task::ready(Ok(None))
        };
        let saved_native = if cfg!(any(target_os = "linux", target_os = "macos")) {
            self.read(&okilum_t3::stream::NativeToken::credential_key(&env), cx)
        } else {
            Task::ready(Ok(None))
        };
        cx.background_executor().spawn(async move {
            let token = saved_mcp
                .await?
                .map(|(_, bytes)| okilum_t3::StoredToken::from_secret(&bytes))
                .transpose()?;
            let native = saved_native
                .await?
                .map(|(_, bytes)| okilum_t3::stream::NativeToken::from_secret(&env, &bytes))
                .transpose()?;
            let mcp = match mcp {
                Some(gateway) => gateway,
                None => Arc::new(
                    okilum_t3::McpGateway::new(env.clone(), token)?.with_access(access.clone()),
                ),
            };
            reader_project_navigation::manager_gateway(env, mcp, native, access)
        })
    }
}
