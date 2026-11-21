//! #1944: per-request proxy resolution for clients whose hosts are not known
//! when the client is built, and that may follow a redirect to another host.
//! reqwest consults the hook for every connection, redirect hops included.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use crate::windows_proxy::{
    env_no_proxy_in, env_proxy_in, exclusions_in, no_proxy_matches, process_var, Lookup,
};

/// A PAC answer stays valid this long for one origin; a network change is
/// therefore seen within five minutes without re-running PAC per image.
const ANSWER_TTL: Duration = Duration::from_secs(300);
/// A failed discovery is retried sooner, and not once per request.
const FAILURE_TTL: Duration = Duration::from_secs(30);
const MAX_ORIGINS: usize = 256;

type LookupFn = dyn Fn(&url::Url) -> anyhow::Result<Lookup> + Send + Sync;
type VarFn = dyn Fn(&str) -> Option<String> + Send + Sync;

pub(crate) struct PerRequest {
    direct: fn(&url::Url) -> bool,
    lookup: Box<LookupFn>,
    /// Environment variable source; the process environment outside tests.
    var: Box<VarFn>,
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
            var: Box::new(process_var),
            answers: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_env(
        mut self,
        var: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.var = Box::new(var);
        self
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
        // An environment proxy for this request's scheme (or `ALL_PROXY`)
        // wins, minus `NO_PROXY`, as reqwest applies it. Only a scheme without
        // one asks WinHTTP, so `HTTP_PROXY` alone still leaves https on PAC.
        if let Some(proxy) = env_proxy_in(url.scheme(), &*self.var) {
            if no_proxy_matches(host, &env_no_proxy_in(&*self.var)) {
                return None;
            }
            let proxy = if proxy.contains("://") {
                proxy
            } else {
                format!("http://{proxy}")
            };
            // Unparseable: direct, and the value (it may hold a password) is not logged.
            return url::Url::parse(&proxy).ok();
        }
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
            Ok(Lookup::Proxy { proxy, bypass })
                if !no_proxy_matches(host, &exclusions_in(&bypass, &*self.var)) =>
            {
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
