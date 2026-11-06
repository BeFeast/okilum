//! Origin-bound Manager onboarding shared by Settings and project panels.
//! Only an explicit Connect action arms clipboard reads, only on activation.
use super::*;
use okilum_t3::{
    stream::{NativeClient, NativeToken, StreamError},
    Environment,
};
use std::{collections::BTreeMap, time::Duration};
mod pairing;
pub(crate) use pairing::PairLink;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Stage {
    SignedOut,
    #[default]
    NotConnected,
    Waiting,
    Connecting,
    Connected,
    Offline,
    Expired,
    Error,
}
impl Stage {
    pub(crate) fn sentence(self) -> &'static str {
        match self {
            Self::SignedOut => "Sign in to T3 once to use your manager in every project.",
            Self::NotConnected => "T3 opens in your browser: create a link, then Share → Copy link and switch back here.",
            Self::Waiting => "In T3, create a link, then Share → Copy link and switch back here.",
            Self::Connecting => "Connecting your manager to T3…",
            Self::Connected => "Your manager is connected to T3.",
            Self::Offline => "Can't reach T3; your history and draft are kept while Okilum retries automatically.",
            Self::Expired => "Your manager connection expired; reconnect to continue.",
            Self::Error => "T3 could not connect your manager; try again.",
        }
    }
    pub(crate) fn action(self) -> &'static str {
        match self {
            Self::SignedOut => "Sign in to T3",
            Self::NotConnected => "Connect manager",
            Self::Waiting => "Open T3 again",
            Self::Connecting => "Connecting…",
            Self::Connected => "Open T3",
            Self::Offline => "Retry now",
            Self::Expired => "Reconnect",
            Self::Error => "Try again",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SignedOut | Self::NotConnected => "Not connected",
            Self::Waiting | Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Offline => "Offline",
            Self::Expired => "Reconnect needed",
            Self::Error => "Error",
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Entry {
    pub stage: Stage,
    pub client: Option<Arc<NativeClient>>,
    pub expiry: Option<u64>,
    pub error: Option<String>,
    pub error_at: Option<chrono::DateTime<chrono::Local>>,
    generation: u64,
    pub retry_epoch: u64,
    previous_stage: Stage,
}
#[derive(Clone, Default)]
pub(crate) struct Connections(BTreeMap<String, Entry>);
impl Global for Connections {}

pub(crate) fn entry(env: &Environment, cx: &App) -> Entry {
    cx.try_global::<Connections>()
        .and_then(|c| c.0.get(&env.origin()))
        .cloned()
        .unwrap_or_default()
}
fn edit(env: &Environment, cx: &mut App, f: impl FnOnce(&mut Entry)) {
    let mut connections = cx.try_global::<Connections>().cloned().unwrap_or_default();
    f(connections.0.entry(env.origin()).or_default());
    cx.set_global(connections);
}
pub(crate) fn suspend(cx: &mut App) {
    let origins: Vec<_> = cx
        .try_global::<Connections>()
        .into_iter()
        .flat_map(|c| c.0.keys())
        .cloned()
        .collect();
    for origin in origins {
        if let Ok(env) = Environment::new("app", &origin) {
            forget(&env, cx);
        }
    }
}

pub(crate) fn forget(env: &Environment, cx: &mut App) {
    edit(env, cx, |e| {
        *e = Entry {
            generation: e.generation.wrapping_add(1),
            ..Default::default()
        }
    });
}
pub(crate) fn connections_url(env: &Environment) -> String {
    format!("{}/settings/connections#authorized-clients", env.origin())
}
pub(crate) fn begin(env: &Environment, cx: &mut App) {
    if !reader_features::enabled(reader_features::Feature::Agents, cx)
        || entry(env, cx).stage == Stage::Connecting
    {
        return;
    }
    edit(env, cx, |e| {
        e.generation = e.generation.wrapping_add(1);
        if e.stage != Stage::Waiting {
            e.previous_stage = e.stage;
        }
        e.stage = Stage::Waiting;
    });
    cx.open_url(&connections_url(env));
}
pub(crate) fn retry(env: &Environment, cx: &mut App) {
    edit(env, cx, |e| e.retry_epoch = e.retry_epoch.wrapping_add(1));
}
pub(crate) fn transport(
    env: &Environment,
    client: Arc<NativeClient>,
    expiry: u64,
    stage: Stage,
    error: Option<String>,
    cx: &mut App,
) {
    let current = entry(env, cx);
    if matches!(current.stage, Stage::Waiting | Stage::Connecting) {
        return;
    }
    if current.stage == stage
        && current.error == error
        && current
            .client
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &client))
    {
        return;
    }
    edit(env, cx, |e| {
        e.stage = stage;
        e.client = Some(client);
        e.expiry = Some(expiry);
        e.error = error;
        e.error_at = e.error.as_ref().map(|_| chrono::Local::now());
    });
}

pub(crate) fn cancel(env: &Environment, cx: &mut App) {
    edit(env, cx, |e| {
        if e.stage == Stage::Waiting {
            e.stage = e.previous_stage;
            e.generation = e.generation.wrapping_add(1);
        }
    });
}

pub(crate) fn activated(window: &Window, cx: &mut App) {
    activated_with_store(window, project::credentials::Store::default(), cx);
}
fn waiting_generations(cx: &App) -> BTreeMap<String, u64> {
    cx.try_global::<Connections>()
        .into_iter()
        .flat_map(|c| c.0.iter())
        .filter(|(_, e)| e.stage == Stage::Waiting)
        .map(|(origin, e)| (origin.clone(), e.generation))
        .collect()
}
fn activated_with_store(window: &Window, store: project::credentials::Store, cx: &mut App) {
    let waiting = waiting_generations(cx);
    if waiting.is_empty() {
        return;
    }
    // Wayland delivers keyboard Enter before the new clipboard Selection offer.
    // Reading synchronously in the activation callback sees the previous offer.
    // Let the platform pump finish, then retry briefly while this window and
    // the explicitly armed flow are still current. Never poll in the background.
    window
        .spawn(cx, async move |cx| {
            for millis in [50, 100, 200, 400] {
                cx.background_executor()
                    .timer(Duration::from_millis(millis))
                    .await;
                let pending = cx
                    .update(|window, cx| {
                        window.is_window_active()
                            && consume_clipboard_at(store.clone(), &waiting, cx)
                    })
                    .unwrap_or(false);
                if !pending {
                    break;
                }
            }
        })
        .detach();
}
#[cfg(test)]
fn consume_clipboard(store: project::credentials::Store, cx: &mut App) {
    consume_clipboard_at(store, &waiting_generations(cx), cx);
}
fn consume_clipboard_at(
    store: project::credentials::Store,
    generations: &BTreeMap<String, u64>,
    cx: &mut App,
) -> bool {
    if !reader_features::enabled(reader_features::Feature::Agents, cx) {
        return false;
    }
    let waiting: Vec<_> = cx
        .try_global::<Connections>()
        .into_iter()
        .flat_map(|c| c.0.iter())
        .filter(|(origin, e)| {
            e.stage == Stage::Waiting && generations.get(*origin) == Some(&e.generation)
        })
        .filter_map(|(origin, _)| Environment::new("app", origin).ok())
        .collect();
    if waiting.is_empty() {
        return false;
    }
    let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
        return true;
    };
    for env in waiting {
        if let Some(link) = PairLink::recognize(&env, &text) {
            pair(env, link, store, cx);
            return false;
        }
    }
    true
}

pub(crate) fn paste(env: Environment, text: &str, cx: &mut App) {
    match PairLink::manual(&env, text) {
        Some(link) => pair(env, link, project::credentials::Store::default(), cx),
        None => edit(&env, cx, |e| {
            e.stage = Stage::Error;
            e.error = Some(
                "Use a pair link from this T3 server or its 12 character pairing code.".into(),
            );
            e.error_at = Some(chrono::Local::now());
        }),
    }
}
fn pair(env: Environment, link: PairLink, store: project::credentials::Store, cx: &mut App) {
    if !reader_features::enabled(reader_features::Feature::Agents, cx)
        || entry(&env, cx).stage == Stage::Connecting
    {
        return;
    }
    let access = reader_features::access(reader_features::Feature::Agents, cx);
    edit(&env, cx, |e| {
        e.generation = e.generation.wrapping_add(1);
        e.stage = Stage::Connecting;
        e.error = None;
        e.error_at = None;
    });
    let generation = entry(&env, cx).generation;
    // No await between comparison and clearing. A different/new clipboard is kept.
    if cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .as_deref()
        .is_some_and(|text| link.matches_clipboard(text))
    {
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    }
    let previous = store.read(&NativeToken::credential_key(&env), cx);
    cx.spawn(async move |cx| {
        // GPUI replaces items by URL AND username. Keep the existing username
        // so reconnect replaces the credential restored after an app restart.
        let username = match previous.await {
            Ok(saved) => saved.map(|(username, _)| username).unwrap_or_else(|| "Okilum Manager".into()),
            Err(_) => {
                cx.update(|cx| {
                    if access.allowed() && entry(&env, cx).generation == generation {
                        edit(&env, cx, |e| {
                            e.stage = Stage::Error;
                            e.error = Some("The saved manager connection could not be read.".into());
                            e.error_at = Some(chrono::Local::now());
                        });
                    }
                });
                return;
            }
        };
        if !cx.update(|cx| access.allowed() && entry(&env, cx).generation == generation) { return; }
        let worker_env = env.clone();
        let worker_access = access.clone();
        let result = cx
            .background_executor()
            .spawn(async move {
                let _lease = worker_access.enter();
                let token = NativeToken::pair(&worker_env, link.credential(), Duration::from_secs(10))?;
                if !token.can_operate() {
                    return Err(StreamError::Protocol("the manager connection needs permission to read threads and control agents"));
                }
                Ok(token)
            })
            .await;
        let result = match result {
            Ok(token) => {
                let current =
                    cx.update(|cx| access.allowed() && entry(&env, cx).generation == generation);
                if !current {
                    return;
                }
                let saved = cx
                    .update(|cx| {
                        store.write(
                            &NativeToken::credential_key(&env),
                            &username,
                            &token.to_secret(),
                            cx,
                        )
                    })
                    .await;
                if saved.is_err() {
                    Err(StreamError::Protocol(
                        "the manager connection could not be saved",
                    ))
                } else {
                    let expiry = token.expires_at();
                    let worker_env = env.clone();
                    let worker_access = access.clone();
                    cx.background_executor()
                        .spawn(async move {
                            NativeClient::new(worker_env, token, Duration::from_secs(10))
                                .map(|client| (Arc::new(client.with_access(worker_access)), expiry))
                        })
                        .await
                }
            }
            Err(error) => Err(error),
        };
        cx.update(|cx| {
            if !access.allowed() || entry(&env, cx).generation != generation {
                return;
            }
            edit(&env, cx, |e| match result {
                Ok((client, expiry)) => {
                    e.stage = Stage::Connected;
                    e.client = Some(client);
                    e.expiry = Some(expiry);
                }
                Err(error) => {
                    e.stage = Stage::Error;
                    e.error = Some(error.to_string());
                    e.error_at = Some(chrono::Local::now());
                }
            });
        });
    })
    .detach();
}

/// Settings can inspect the saved Manager credential without consuming a link
/// or opening a live thread. Project panels own their stream/retry lifecycle.
pub(crate) fn restore(env: Environment, cx: &mut App) {
    if !reader_features::enabled(reader_features::Feature::Agents, cx)
        || cx
            .try_global::<Connections>()
            .is_some_and(|c| c.0.contains_key(&env.origin()))
    {
        return;
    }
    edit(&env, cx, |_| {});
    let generation = entry(&env, cx).generation;
    let access = reader_features::access(reader_features::Feature::Agents, cx);
    let saved = cx.read_credentials(&NativeToken::credential_key(&env));
    cx.spawn(async move |cx| {
        let worker_env = env.clone();
        let mut saved_expiry = None;
        let result = match saved.await {
            Ok(Some((_, bytes))) => {
                saved_expiry = NativeToken::saved_expiry(&env, &bytes);
                cx.background_executor()
                    .spawn(async move {
                        let token = NativeToken::from_secret(&worker_env, &bytes)?;
                        Ok(Some(token.expires_at()))
                    })
                    .await
            }
            Ok(None) => Ok(None),
            Err(_) => Err(StreamError::Protocol(
                "the saved manager connection could not be read",
            )),
        };
        cx.update(|cx| {
            if !access.allowed() || entry(&env, cx).generation != generation {
                return;
            }
            edit(&env, cx, |e| {
                e.expiry = saved_expiry;
                match result {
                    Ok(Some(expiry)) => {
                        e.stage = Stage::Connected;
                        e.expiry = Some(expiry);
                    }
                    Ok(None) => {}
                    Err(StreamError::Expired | StreamError::PairingRequired) => {
                        e.stage = Stage::Expired;
                        e.error = Some(StreamError::Expired.to_string());
                    }
                    Err(error) => {
                        e.stage = Stage::Error;
                        e.error = Some(error.to_string());
                    }
                }
            });
        });
    })
    .detach();
}

#[cfg(test)]
mod tests;
