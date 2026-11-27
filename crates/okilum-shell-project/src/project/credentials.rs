//! GPUI owns the platform store (Keychain on macOS); no file fallback.
use super::*;

type Saved = Option<(String, Vec<u8>)>;

#[derive(Clone, Default)]
pub struct Store {
    #[cfg(any(test, feature = "test-support"))]
    pub test: Option<Arc<std::sync::Mutex<TestStore>>>,
}

impl Store {
    pub fn read(&self, key: &str, cx: &App) -> Task<anyhow::Result<Saved>> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(store) = &self.test {
            let mut store = store.lock().unwrap();
            store.reads.push(key.into());
            return Task::ready(Ok(store.entries.get(key).cloned()));
        }
        cx.read_credentials(key)
    }

    #[cfg(native_manager)]
    pub fn write(&self, key: &str, user: &str, bytes: &[u8], cx: &App) -> Task<anyhow::Result<()>> {
        #[cfg(any(test, feature = "test-support"))]
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
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct TestStore {
    pub entries: std::collections::BTreeMap<String, (String, Vec<u8>)>,
    pub reads: Vec<String>,
    pub refuse_write: bool,
}

#[cfg(all(test, native_manager))]
mod tests;

/// Read the two separate OS credential audiences on the UI thread, then build
/// the shared gateway off-thread. Merely reading never pairs or opens a browser.
/// A saved but invalid native bearer is an error, never an MCP fallback.
pub fn restore_gateway(
    env: okilum_t3::Environment,
    mcp: Option<Arc<dyn okilum_t3::ManagerGateway>>,
    access: okilum_t3::NetworkAccess,
    cx: &App,
) -> Task<anyhow::Result<Arc<dyn okilum_t3::ManagerGateway>>> {
    Store::default().gateway(env, mcp, access, cx)
}

/// Board, Projects, icons and read-only summaries use the native bearer on
/// Linux and macOS. MCP remains available when no native credential exists.
pub fn manager_gateway(
    env: okilum_t3::Environment,
    mcp: Arc<dyn okilum_t3::ManagerGateway>,
    native: Option<okilum_t3::stream::NativeToken>,
    access: okilum_t3::NetworkAccess,
) -> anyhow::Result<Arc<dyn okilum_t3::ManagerGateway>> {
    anyhow::ensure!(access.allowed(), "Agents is disabled");
    let _access = access.enter();
    crate::hooks::record_manager_gateway();
    let session = okilum_t3::SessionGateway::native(env, native)?.with_access(access);
    Ok(Arc::new(okilum_t3::PairedGateway::new(session, mcp)))
}

impl Store {
    pub fn gateway(
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
        let saved_native = if cfg!(native_manager) {
            self.read(&okilum_t3::stream::NativeToken::credential_key(&env), cx)
        } else {
            Task::ready(Ok(None))
        };
        cx.background_executor().spawn(async move {
            let token = saved_mcp
                .await?
                .map(|(_, bytes)| okilum_t3::StoredToken::from_secret(&bytes))
                .transpose()?;
            let native = match saved_native.await? {
                Some((_, bytes)) => match okilum_t3::stream::NativeToken::from_secret(&env, &bytes)
                {
                    Ok(token) => Some(token),
                    Err(
                        okilum_t3::stream::StreamError::Expired
                        | okilum_t3::stream::StreamError::PairingRequired,
                    ) => None,
                    Err(error) => return Err(error.into()),
                },
                None => None,
            };
            let mcp = match mcp {
                Some(gateway) => gateway,
                None => Arc::new(
                    okilum_t3::McpGateway::new(env.clone(), token)?.with_access(access.clone()),
                ),
            };
            manager_gateway(env, mcp, native, access)
        })
    }
}
