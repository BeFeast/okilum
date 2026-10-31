//! One origin-bound authorization per application, independent of any form.
use super::*;
use okilum_t3::oauth::{Authorization, Input as AuthInput, Progress, DEFAULT_WAIT};
use okilum_t3::{Environment, GatewayStatus, ManagerGateway, McpGateway, StoredToken};

#[derive(Clone)]
pub(super) struct Pending {
    pub url: String,
    pub input: std::sync::mpsc::Sender<AuthInput>,
    pub remaining: Option<u64>,
}
#[derive(Clone, Default)]
pub(super) struct Entry {
    pub gateway: Option<Arc<dyn ManagerGateway>>,
    pub pending: Option<Pending>,
    pub busy: bool,
    pub error: Option<String>,
    pub error_details: Option<String>,
    pub notice: Option<String>,
    pub signed_out: bool,
}
#[derive(Clone, Default)]
pub(super) struct Sessions {
    entries: std::collections::BTreeMap<String, Entry>,
    selected: Option<String>,
}
impl Global for Sessions {}

pub(super) fn entry(origin: &str, cx: &App) -> Entry {
    cx.try_global::<Sessions>()
        .and_then(|s| s.entries.get(origin))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn fixture_connection(
    env: &Environment,
    gateway: Arc<dyn ManagerGateway>,
    cx: &mut App,
) {
    edit(&env.origin(), cx, |entry| entry.gateway = Some(gateway));
}
pub(super) fn selected_origin(cx: &App) -> Option<String> {
    cx.try_global::<Sessions>().and_then(|s| s.selected.clone())
}
pub(super) fn pending_origin(cx: &App) -> Option<String> {
    let sessions = cx.try_global::<Sessions>()?;
    let origin = sessions.selected.as_ref()?;
    sessions
        .entries
        .get(origin)
        .filter(|e| e.busy)
        .map(|_| origin.clone())
}
fn edit(origin: &str, cx: &mut App, change: impl FnOnce(&mut Entry)) {
    let mut sessions = cx.try_global::<Sessions>().cloned().unwrap_or_default();
    change(sessions.entries.entry(origin.to_owned()).or_default());
    cx.set_global(sessions);
}
pub(super) fn status(origin: &str, cx: &App) -> String {
    let entry = entry(origin, cx);
    if let Some(pending) = entry.pending {
        return match pending.remaining {
            Some(seconds) => format!("Finish sign-in · {}:{:02} remaining", seconds / 60, seconds % 60),
            None => "Browser return timed out. Wait longer, paste the callback URL, or cancel and sign in again.".into(),
        };
    }
    if entry.busy {
        return entry
            .notice
            .unwrap_or_else(|| "Checking your T3 connection…".into());
    }
    match entry.gateway.as_ref().map(|g| g.status()) {
        Some(GatewayStatus::Connected { .. }) => {
            format!("Signed in to {origin} · shared by all projects")
        }
        Some(GatewayStatus::ExpiresSoon { .. }) => format!("Signed in to {origin} · renew soon"),
        _ => format!("Server address: {origin} · sign-in required for this app"),
    }
}

/// An explicit sign-in may authorize. Restoring Settings only checks the saved
/// credential; neither path starts another attempt while one is pending.
pub(super) fn connect(env: Environment, state: Option<PathBuf>, authorize: bool, cx: &mut App) {
    if !reader_features::enabled(reader_features::Feature::Agents, cx) {
        return;
    }
    let access = reader_features::access(reader_features::Feature::Agents, cx);
    let origin = env.origin();
    let current = entry(&origin, cx);
    if current.busy || current.pending.is_some() {
        return;
    }
    if current.signed_out && !authorize {
        return;
    }
    let renew = authorize
        && matches!(
            current.gateway.as_ref().map(|g| g.status()),
            Some(GatewayStatus::ExpiresSoon { .. } | GatewayStatus::Expired)
        );
    if authorize {
        let mut sessions = cx.try_global::<Sessions>().cloned().unwrap_or_default();
        sessions.selected = Some(origin.clone());
        cx.set_global(sessions);
    }
    edit(&origin, cx, |e| {
        e.busy = true;
        e.error = None;
        e.error_details = None;
        e.notice = None;
        e.signed_out = false;
    });
    cx.spawn(async move |cx| {
        let mut publish = false;
        let result = async {
            let saved = cx
                .update(|cx| cx.read_credentials(&env.credential_url()))
                .await?;
            let token = saved
                .map(|(_, bytes)| StoredToken::from_secret(&bytes))
                .transpose()?;
            let worker_env = env.clone();
            let (events, updates) = async_channel::unbounded();
            let progress_origin = origin.clone();
            let updates_job = cx.spawn(async move |cx| {
                while let Ok(event) = updates.recv().await {
                    cx.update(|cx| {
                        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
                            if let Event::Ready(pending) = event {
                                let _ = pending.input.send(AuthInput::Cancel);
                            }
                            return;
                        }
                        #[cfg(not(target_os = "macos"))]
                        if let Event::Ready(pending) = &event {
                            cx.open_url(&pending.url);
                        }
                        edit(&progress_origin, cx, |entry| match event {
                            Event::NetworkRetry => {
                                entry.notice = Some(okilum_t3::connection::RETRY_NOTICE.into());
                            }
                            Event::Ready(pending) => {
                                entry.pending = Some(pending);
                                entry.error = None;
                            }
                            Event::Progress(Progress::Waiting(time)) => {
                                if let Some(p) = &mut entry.pending {
                                    p.remaining = Some(time.as_secs());
                                }
                            }
                            Event::Progress(Progress::TimedOut) => {
                                if let Some(p) = &mut entry.pending {
                                    p.remaining = None;
                                }
                            }
                            Event::Progress(Progress::InvalidCallback(message)) => {
                                entry.error = Some(message)
                            }
                        })
                    });
                }
            });
            let final_events = events.clone();
            let worker_access = access.clone();
            let (connected, mut retry) = cx
                .background_executor()
                .spawn(async move {
                    let _access = worker_access.enter();
                    anyhow::ensure!(worker_access.allowed(), "Agents is disabled");
                    let mut retry = okilum_t3::connection::Retry::default();
                    let notice = || {
                        let _ = events.try_send(Event::NetworkRetry);
                    };
                    if !authorize {
                        let gateway = McpGateway::new(worker_env.clone(), token)?;
                        if matches!(
                            gateway.status(),
                            GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. }
                        ) {
                            retry.run(
                                &worker_env,
                                || gateway.connect().map_err(Into::into),
                                notice,
                            )?;
                        }
                        return Ok((
                            okilum_t3::reconnect::Connected {
                                gateway,
                                issued_token: None,
                            },
                            retry,
                        ));
                    }
                    let saved = token.map(|t| (worker_env.credential_url(), t));
                    let connected = okilum_t3::reconnect::connect_with_retry(
                        &worker_env,
                        saved,
                        renew,
                        |retry| {
                            let authorization = retry.run(
                                &worker_env,
                                || Authorization::prepare(&worker_env, "Okilum"),
                                notice,
                            )?;
                            let (send, receive) = std::sync::mpsc::channel();
                            events.try_send(Event::Ready(Pending {
                                url: authorization.url().to_string(),
                                input: send,
                                remaining: Some(DEFAULT_WAIT.as_secs()),
                            }))?;
                            authorization.wait(
                                receive,
                                |p| {
                                    let _ = events.try_send(Event::Progress(p));
                                },
                                DEFAULT_WAIT,
                            )
                        },
                        &mut retry,
                        notice,
                    )?;
                    Ok::<_, anyhow::Error>((connected, retry))
                })
                .await?;
            anyhow::ensure!(access.allowed(), "Agents is disabled");
            if let Some(token) = &connected.issued_token {
                cx.update(|cx| {
                    cx.write_credentials(&env.credential_url(), "Okilum", &token.to_secret())
                })
                .await?;
            }
            // Persist only after a real authenticated connection. Merely typing
            // an address must never become a misleading "Connected" state.
            let connected = cx
                .background_executor()
                .spawn(async move {
                    if authorize {
                        retry.run(
                            connected.gateway.environment(),
                            || connected.gateway.connect().map_err(Into::into),
                            || {
                                let _ = final_events.try_send(Event::NetworkRetry);
                            },
                        )?;
                    }
                    Ok::<_, anyhow::Error>(connected)
                })
                .await?;
            // Both setup workers have dropped their progress senders.
            updates_job.await;
            publish = authorize && cx.update(|cx| selected_origin(cx).as_deref() == Some(&origin));
            if publish {
                let saved_env = env.clone();
                cx.background_executor()
                    .spawn(async move {
                        let state = state
                            .ok_or_else(|| anyhow::anyhow!("Application state is unavailable"))?;
                        project::save_browser_environment(&state, &saved_env)
                    })
                    .await?;
            }
            Ok::<_, anyhow::Error>(Arc::new(connected.gateway) as Arc<dyn ManagerGateway>)
        }
        .await;
        if !access.allowed() {
            return;
        }
        cx.update(|cx| finish_if_current(&origin, result, publish, cx));
    })
    .detach();
}

pub(super) fn suspend(cx: &mut App) {
    if let Some(mut sessions) = cx.try_global::<Sessions>().cloned() {
        for entry in sessions.entries.values_mut() {
            if let Some(pending) = entry.pending.take() {
                let _ = pending.input.send(AuthInput::Cancel);
            }
            entry.gateway = None;
            entry.busy = false;
        }
        cx.set_global(sessions);
    }
}

/// Local sign-out is shared by every panel at this origin. It does not revoke
/// another application's session or remove drafts and history.
pub(super) fn sign_out(env: Environment, cx: &mut App) {
    let origin = env.origin();
    let current = entry(&origin, cx);
    if current.busy || current.pending.is_some() {
        return;
    }
    edit(&origin, cx, |entry| {
        entry.gateway = None;
        entry.busy = true;
        entry.signed_out = true;
        entry.error = None;
        entry.error_details = None;
        entry.notice = None;
    });
    cx.set_global(Connection {
        origin: Some(origin.clone()),
    });
    cx.spawn(async move |cx| {
        let result = async {
            cx.update(|cx| cx.delete_credentials(&env.credential_url()))
                .await?;
            #[cfg(target_os = "linux")]
            cx.update(|cx| {
                cx.delete_credentials(&okilum_t3::stream::NativeToken::credential_key(&env))
            })
            .await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        cx.update(|cx| {
            let failed = result.is_err();
            edit(&origin, cx, |entry| {
                entry.busy = false;
                entry.error = result.err().map(|e| {
                    format!("Sign-out could not remove the saved connection: {e:#}. Try again.")
                });
            });
            if failed {
                cx.set_global(Connection {
                    origin: Some(origin.clone()),
                });
            }
        });
    })
    .detach();
}

fn finish_if_current(
    origin: &str,
    result: anyhow::Result<Arc<dyn ManagerGateway>>,
    publish: bool,
    cx: &mut App,
) {
    if !reader_features::enabled(reader_features::Feature::Agents, cx) {
        return;
    }
    let publish = publish && selected_origin(cx).as_deref() == Some(origin);
    finish(origin, result, publish, cx);
}

fn finish(
    origin: &str,
    result: anyhow::Result<Arc<dyn ManagerGateway>>,
    publish: bool,
    cx: &mut App,
) {
    edit(origin, cx, |entry| {
        entry.busy = false;
        entry.pending = None;
        entry.notice = None;
        entry.error_details = None;
        match result {
            Ok(gateway) => {
                entry.gateway = Some(gateway);
                entry.error = None;
            }
            Err(error) => {
                entry.gateway = None;
                if let Some(help) = okilum_t3::connection::permission_help(&error) {
                    entry.error = Some(help.into());
                    entry.error_details = okilum_t3::connection::permission_details(&error);
                } else {
                    entry.error = Some(format!("{error:#}. Choose Sign in to continue."));
                }
            }
        }
    });
    if publish && entry(origin, cx).gateway.is_some() {
        cx.set_global(Connection {
            origin: Some(origin.to_owned()),
        });
    }
}

enum Event {
    NetworkRetry,
    Ready(Pending),
    Progress(Progress),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn local_network_diagnostics_stay_under_details(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let form = cx.new(|cx| {
                let mut form = T3Settings::new(None, window, cx);
                form.edited = true;
                form.origin.update(cx, |input, cx| {
                    input.set_value("https://10.10.0.23", window, cx)
                });
                form
            });
            Root::new(form, window, cx)
        });
        visual.update(|_, cx| {
            edit("https://10.10.0.23", cx, |entry| {
                entry.error = Some(okilum_t3::connection::PERMISSION_HELP.into());
                entry.error_details = Some("No route to host (os error 65)".into());
            })
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("t3-connection-error").is_some());
        assert!(visual.debug_bounds("t3-connection-error-details").is_none());
        let details = visual.debug_bounds("t3-connection-details").unwrap();
        visual.simulate_click(details.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("t3-connection-error-details").is_some());
        let details = visual.debug_bounds("t3-connection-details").unwrap();
        visual.simulate_click(details.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("t3-connection-error-details").is_none());
        root.read_with(visual, |_, cx| {
            assert!(entry("https://10.10.0.23", cx).pending.is_none())
        });
    }

    #[gpui::test]
    fn pending_sign_in_is_shared_across_aliases_and_survives_the_form(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let form = cx.new(|cx| T3Settings::new(None, window, cx));
            Root::new(form, window, cx)
        });
        let initiating =
            visual.update(|window, cx| cx.new(|cx| T3Settings::new(None, window, cx)).downgrade());
        visual.run_until_parked();
        assert!(
            initiating.upgrade().is_none(),
            "the initiating form has closed before authorization completes"
        );
        let env = Environment::new("first-project", "https://t3.example").unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        visual.update(|_, cx| {
            edit(&env.origin(), cx, |entry| {
                entry.busy = true;
                entry.pending = Some(Pending {
                    url: "https://t3.example/authorize".into(),
                    input: send,
                    remaining: None,
                });
            });
            let mut sessions = cx.global::<Sessions>().clone();
            sessions.selected = Some(env.origin());
            cx.set_global(sessions);
            let other = Environment::new("another-project", &env.origin()).unwrap();
            connect(other, None, true, cx);
            let entry = entry(&env.origin(), cx);
            assert!(entry.pending.is_some());
            assert_eq!(cx.global::<Sessions>().entries.len(), 1);
            assert!(status(&env.origin(), cx).contains("timed out"));
            entry
                .pending
                .unwrap()
                .input
                .send(AuthInput::WaitLonger)
                .unwrap();
        });
        assert!(
            matches!(receive.try_recv(), Ok(AuthInput::WaitLonger)),
            "the original attempt, not a replacement, owns the controls"
        );
        let reopened = visual.update(|window, cx| cx.new(|cx| T3Settings::new(None, window, cx)));
        visual.run_until_parked();
        reopened.read_with(visual, |form, cx| {
            assert_eq!(
                form.origin_value(cx),
                env.origin(),
                "reopened Settings resumes the selected pending sign-in"
            )
        });
        visual.update(|_, cx| {
            let token = StoredToken {
                access_token: "fixture-only".into(),
                expires_at: u64::MAX,
                scope: okilum_t3::oauth::SCOPE.into(),
                client_id: "fixture".into(),
            };
            let gateway = Arc::new(McpGateway::new(env.clone(), Some(token)).unwrap());
            finish_if_current(&env.origin(), Ok(gateway.clone()), true, cx);
            assert_eq!(
                cx.global::<Connection>().origin.as_deref(),
                Some("https://t3.example")
            );
            assert!(status(&env.origin(), cx).starts_with("Signed in"));
            assert!(
                !status("https://another.example", cx).starts_with("Signed in"),
                "an origin alone is never authentication"
            );
            assert!(entry(&env.origin(), cx).pending.is_none());
            let mut sessions = cx.global::<Sessions>().clone();
            sessions.selected = Some("https://another.example".into());
            cx.set_global(sessions);
            cx.set_global(Connection {
                origin: Some("https://another.example".into()),
            });
            finish_if_current(&env.origin(), Ok(gateway), true, cx);
            assert_eq!(
                cx.global::<Connection>().origin.as_deref(),
                Some("https://another.example"),
                "a late callback must not replace a newer selected server"
            );
        });
    }
}
