//! Native T3 desktop links use its registered single-instance URI handler.
use super::*;

#[cfg(target_os = "linux")]
mod linux;

fn open_desktop_url(url: &str, cx: &mut App) {
    cx.open_url(url);
    #[cfg(all(target_os = "linux", not(test)))]
    cx.background_executor()
        .spawn(async { linux::focus_existing_window() })
        .detach();
}

/// Missing per-user Windows registration is a supported web fallback, never
/// an implicit registry install. Other platforms keep their desktop handler.
#[cfg(windows)]
fn windows_protocol_registered() -> anyhow::Result<bool> {
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_QUERY_VALUE},
    };
    let path: Vec<u16> = r"Software\Classes\t3code\shell\open\command"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut key = std::ptr::null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    match result {
        ERROR_SUCCESS => {
            unsafe {
                RegCloseKey(key);
            }
            Ok(true)
        }
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(false),
        error => Err(std::io::Error::from_raw_os_error(error as i32).into()),
    }
}

fn target_url(
    identity: &okilum_t3::ServerIdentity,
    thread: &str,
    environment: Option<&Environment>,
    windows_missing_handler: bool,
) -> anyhow::Result<String> {
    if windows_missing_handler {
        let environment = environment
            .ok_or_else(|| anyhow::anyhow!("Connect to T3 before opening the web fallback"))?;
        return Ok(okilum_t3::projects::manager_url(environment, identity, thread)?.into());
    }
    Ok(identity.thread_url(thread))
}

impl ProjectView {
    pub(super) fn open_in_t3(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.chat.as_ref().map(|chat| chat.thread_id.clone()) else {
            return;
        };
        if let Some(identity) = &self.identity {
            #[cfg(windows)]
            let missing = match windows_protocol_registered() {
                Ok(registered) => !registered,
                Err(e) => {
                    self.error = Some(format!("Could not check T3 desktop registration: {e}"));
                    cx.notify();
                    return;
                }
            };
            #[cfg(not(windows))]
            let missing = false;
            match target_url(identity, &thread, self.environment.as_ref(), missing) {
                Ok(url) => open_desktop_url(&url, cx),
                Err(e) => {
                    self.error = Some(e.to_string());
                    cx.notify();
                }
            }
            return;
        }
        #[cfg(target_os = "linux")]
        {
            if self.opening_t3 {
                return;
            }
            let Some(client) = self.native.model_client() else {
                return;
            };
            self.opening_t3 = true;
            let generation = self.generation;
            cx.spawn_in(window, async move |this, cx| {
                let identity = cx
                    .background_executor()
                    .spawn(async move { client.server_identity() })
                    .await;
                let _ = this.update_in(cx, |this, _, cx| {
                    if this.generation != generation || !this.opening_t3 {
                        return;
                    }
                    this.opening_t3 = false;
                    if !this.active
                        || this
                            .chat
                            .as_ref()
                            .is_none_or(|chat| chat.thread_id != thread)
                    {
                        return;
                    }
                    match identity {
                        Ok(identity) => {
                            open_desktop_url(&identity.thread_url(&thread), cx);
                            this.identity = Some(identity);
                        }
                        Err(error) => {
                            this.error = Some(format!("Could not open this thread in T3: {error}"))
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
        }
        #[cfg(not(target_os = "linux"))]
        let _ = window;
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn desktop_is_preferred_and_missing_windows_handler_uses_the_configured_server() {
        let identity = okilum_t3::ServerIdentity {
            environment_id: "env".into(),
            label: "T3".into(),
        };
        let env = Environment::new("maestro", "https://t3.oklabs.uk").unwrap();
        assert_eq!(
            target_url(&identity, "mcp:thread", Some(&env), false).unwrap(),
            "t3code://threads/env/mcp:thread"
        );
        assert_eq!(
            target_url(&identity, "mcp:thread", Some(&env), true).unwrap(),
            "https://t3.oklabs.uk/env/mcp:thread"
        );
        assert!(target_url(&identity, "mcp:thread", None, true).is_err());
    }
}
