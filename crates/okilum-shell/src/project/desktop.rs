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

#[cfg(target_os = "macos")]
fn macos_protocol_registered() -> bool {
    extern "C" {
        fn okilum_t3_protocol_registered() -> bool;
    }
    // SAFETY: read-only NSWorkspace lookup, called on the application UI thread.
    unsafe { okilum_t3_protocol_registered() }
}

// TestPlatform does not install T3. Tests supply the OS lookup result explicitly.
#[cfg(test)]
pub(in crate::project) struct TestHandler(pub bool);
#[cfg(test)]
impl Global for TestHandler {}

fn missing_handler(cx: &App) -> anyhow::Result<bool> {
    #[cfg(test)]
    if let Some(handler) = cx.try_global::<TestHandler>() {
        return Ok(!handler.0);
    }
    let _ = cx;
    #[cfg(windows)]
    return windows_protocol_registered().map(|registered| !registered);
    #[cfg(target_os = "macos")]
    return Ok(!macos_protocol_registered());
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(false)
}

fn open_thread(
    identity: &ServerIdentity,
    thread: &str,
    env: Option<&Environment>,
    cx: &mut App,
) -> anyhow::Result<()> {
    let url = target_url(identity, thread, env, missing_handler(cx)?)?;
    open_desktop_url(&url, cx);
    Ok(())
}

fn target_url(
    identity: &okilum_t3::ServerIdentity,
    thread: &str,
    environment: Option<&Environment>,
    missing_handler: bool,
) -> anyhow::Result<String> {
    if missing_handler {
        let environment = environment
            .ok_or_else(|| anyhow::anyhow!("Connect to T3 before opening the web fallback"))?;
        return Ok(okilum_t3::projects::manager_url(environment, identity, thread)?.into());
    }
    Ok(identity.thread_url(thread))
}

fn parse_thread_url(url: &str) -> anyhow::Result<(&str, &str)> {
    let rest = url
        .strip_prefix("t3code://threads/")
        .ok_or_else(|| anyhow::anyhow!("Unsupported T3 thread URL"))?;
    let (env_id, thread_id) = rest
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Malformed T3 thread URL"))?;
    let valid_part = |part: &str| {
        !part.is_empty()
            && !part.contains(['/', '?', '#'])
            && !part.chars().any(char::is_whitespace)
    };
    anyhow::ensure!(
        valid_part(env_id) && valid_part(thread_id),
        "Malformed T3 thread URL"
    );
    Ok((env_id, thread_id))
}

fn resolve_thread_url(
    url: &str,
    state: Option<&Path>,
    missing_handler: bool,
) -> anyhow::Result<String> {
    let (env_id, thread_id) = parse_thread_url(url)?;
    if !missing_handler {
        return Ok(url.to_owned());
    }
    let origin = connections::last_origin(state)?
        .ok_or_else(|| anyhow::anyhow!("Connect to T3 before opening the web fallback"))?;
    let environment = Environment::new("t3", &origin)?;
    let identity = ServerIdentity {
        environment_id: env_id.to_owned(),
        label: "T3".into(),
    };
    Ok(okilum_t3::projects::manager_url(&environment, &identity, thread_id)?.into())
}

/// Open a canonical `t3code://threads/<env>/<thread>` link through the same
/// desktop URI handler, Linux window reveal, and web fallback as «Open in T3».
pub(crate) fn open_thread_url(url: &str, state: Option<&Path>, cx: &mut App) -> anyhow::Result<()> {
    let target = resolve_thread_url(url, state, missing_handler(cx)?)?;
    open_desktop_url(&target, cx);
    Ok(())
}

impl ProjectView {
    pub(super) fn open_in_t3(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.chat.as_ref().map(|chat| chat.thread_id.clone()) else {
            return;
        };
        if let Some(identity) = &self.identity {
            if let Err(error) = open_thread(identity, &thread, self.environment.as_ref(), cx) {
                self.error = Some(format!("Could not open this thread in T3: {error}"));
                cx.notify();
            }
            return;
        }
        #[cfg(native_manager)]
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
                            if let Err(error) =
                                open_thread(&identity, &thread, this.environment.as_ref(), cx)
                            {
                                this.error =
                                    Some(format!("Could not open this thread in T3: {error}"));
                            }
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
        #[cfg(not(native_manager))]
        let _ = window;
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn desktop_is_preferred_and_missing_handler_uses_the_configured_server() {
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

    #[test]
    fn view_thread_url_validates_scheme_and_falls_back_to_saved_origin() {
        let url = "t3code://threads/env-fixture/thread-views";
        assert_eq!(resolve_thread_url(url, None, false).unwrap(), url);
        for invalid in [
            "https://example.com/env/thread",
            "t3code://threads/only-env",
            "t3code://threads/env/thread/extra",
            "t3code://threads/env/thread?x=1",
            "t3code://threads/env/thread#frag",
            "t3code://threads/env/bad thread",
        ] {
            assert!(
                resolve_thread_url(invalid, None, false).is_err(),
                "{invalid}"
            );
        }
        let temp = tempfile::tempdir().unwrap();
        let env = Environment::new("maestro", "https://t3.oklabs.uk").unwrap();
        connections::save_last_origin(temp.path(), &env).unwrap();
        assert_eq!(
            resolve_thread_url(url, Some(temp.path()), true).unwrap(),
            "https://t3.oklabs.uk/env-fixture/thread-views"
        );
        assert!(resolve_thread_url(url, None, true).is_err());
    }
}
