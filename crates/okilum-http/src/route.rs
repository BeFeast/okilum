//! #1944: per-request proxy resolution for clients whose hosts are not known
//! when the client is built, and that may follow a redirect to another host.
//! reqwest consults the hook for every connection, redirect hops included.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use crate::windows_proxy::{env_no_proxy, no_proxy_matches, Lookup};

/// A PAC answer stays valid this long for one origin; a network change is
/// therefore seen within five minutes without re-running PAC per image.
const ANSWER_TTL: Duration = Duration::from_secs(300);
/// A failed discovery is retried sooner, and not once per request.
const FAILURE_TTL: Duration = Duration::from_secs(30);
const MAX_ORIGINS: usize = 256;

type LookupFn = dyn Fn(&url::Url) -> anyhow::Result<Lookup> + Send + Sync;

pub(crate) struct PerRequest {
    direct: fn(&url::Url) -> bool,
    lookup: Box<LookupFn>,
    answers: Mutex<HashMap<String, (Instant, Option<url::Url>)>>,
}

impl PerRequest {
    pub(crate) fn new(
        direct: fn(&url::Url) -> bool,
        lookup: impl Fn(&url::Url) -> anyhow::Result<Lookup> + Send + Sync + 'static,
    ) -> Self {
        Self {
            direct,
            lookup: Box::new(lookup),
            answers: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn into_proxy(self) -> reqwest::Proxy {
        let resolver = Arc::new(self);
        reqwest::Proxy::custom(move |url| resolver.proxy_for(url))
    }

    /// The proxy for this request's origin, or `None` for a direct connection.
    pub(crate) fn proxy_for(&self, url: &url::Url) -> Option<url::Url> {
        if (self.direct)(url) {
            return None;
        }
        let host = url.host_str().unwrap_or_default();
        let key = format!(
            "{}://{host}:{}",
            url.scheme(),
            url.port_or_known_default().unwrap_or_default()
        );
        let now = Instant::now();
        let answers = || self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((until, answer)) = answers().get(&key) {
            if *until > now {
                return answer.clone();
            }
        }
        let (ttl, answer) = match (self.lookup)(url) {
            Ok(Lookup::Proxy { proxy, .. }) if !no_proxy_matches(host, &env_no_proxy()) => {
                (ANSWER_TTL, url::Url::parse(&proxy).ok())
            }
            Ok(_) => (ANSWER_TTL, None),
            Err(error) => {
                // No URL in the log: a pasted link may carry a token.
                eprintln!(
                    "Proxy discovery failed for one request; connecting directly: {}",
                    crate::reason(&error)
                );
                (FAILURE_TTL, None)
            }
        };
        let mut answers = answers();
        if answers.len() >= MAX_ORIGINS {
            answers.retain(|_, (until, _)| *until > now);
            if answers.len() >= MAX_ORIGINS {
                answers.clear();
            }
        }
        answers.insert(key, (now + ttl, answer.clone()));
        answer
    }
}

#[cfg(test)]
mod tests;
