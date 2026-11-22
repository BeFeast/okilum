//! Resolve the current user's Windows proxy, including PAC/WPAD, for this URL.
//! Credentials are not copied from Windows or placed in headers/logs.
#[cfg(windows)]
pub(super) enum Policy {
    Inherit,
    Direct,
    Proxy(reqwest::Proxy),
}

/// The proxy decision for one URL before it becomes a reqwest policy.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Lookup {
    /// An environment proxy variable is set; it keeps reqwest's precedence.
    Inherit,
    Direct,
    /// Proxy URL for this scheme, and the system bypass list for redirects.
    Proxy {
        proxy: String,
        bypass: String,
    },
}

/// Per-endpoint policy: the endpoint's proxy also serves its redirects, minus
/// the system bypass list and `NO_PROXY`.
#[cfg(windows)]
pub(super) fn resolve(url: &url::Url) -> anyhow::Result<Policy> {
    Ok(match lookup(url)? {
        Lookup::Inherit => Policy::Inherit,
        Lookup::Direct => Policy::Direct,
        Lookup::Proxy { proxy, bypass } => {
            let no_proxy = exclusions(&bypass);
            Policy::Proxy(
                reqwest::Proxy::all(proxy)?.no_proxy(reqwest::NoProxy::from_string(&no_proxy)),
            )
        }
    })
}

/// Discovery failures are typed, so classification never reads a URL or a
/// context string to recognise them.
#[cfg(windows)]
pub(super) fn lookup(url: &url::Url) -> anyhow::Result<Lookup> {
    lookup_system(url).map_err(|error| anyhow::Error::new(crate::ProxyDiscoveryFailed(error)))
}

/// The environment proxy for `scheme` (`http`/`https`), as reqwest reads it.
#[cfg(any(windows, test))]
pub(super) fn env_proxy(scheme: &str) -> Option<String> {
    env_proxy_in(scheme, &process_var)
}

/// [`env_proxy`] over an injected variable source (tests).
#[cfg(any(windows, test))]
pub(super) fn env_proxy_in(scheme: &str, var: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    [
        format!("{scheme}_proxy"),
        format!("{}_PROXY", scheme.to_ascii_uppercase()),
        "all_proxy".into(),
        "ALL_PROXY".into(),
    ]
    .iter()
    .find_map(|name| var(name).filter(|v| !v.is_empty()))
}

#[cfg(any(windows, test))]
pub(super) fn env_no_proxy_in(var: &dyn Fn(&str) -> Option<String>) -> String {
    var("NO_PROXY")
        .or_else(|| var("no_proxy"))
        .unwrap_or_default()
}

#[cfg(any(windows, test))]
pub(super) fn process_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The exclusions a proxy answer carries: `NO_PROXY` plus the system bypass
/// list. The per-endpoint policy, the per-request hook and the socket route
/// all use this one string, so a ticket and its socket agree.
#[cfg(any(windows, test))]
pub(super) fn exclusions(bypass: &str) -> String {
    exclusions_in(bypass, &process_var)
}

#[cfg(any(windows, test))]
pub(super) fn exclusions_in(bypass: &str, var: &dyn Fn(&str) -> Option<String>) -> String {
    [env_no_proxy_in(var), bypass.replace(';', ",")].join(",")
}

/// reqwest 0.12's `NoProxy`, i.e. hyper-util 0.1.20 `NoProxy::from_string`
/// plus `contains`, so the hook and the socket decide as the ticket client
/// does. Comma separated. An entry that parses as an address or a CIDR range
/// goes to the IP list; every other entry, `*` and `[::1]` included, is a
/// domain. An IP host (brackets stripped) is checked against the IP list
/// only, so `*` never matches an IP and a bracketed entry matches nothing. A
/// domain entry (leading dot optional) matches that name and its subdomains;
/// `*` matches every domain host.
#[cfg(any(windows, test))]
pub(super) fn no_proxy_matches(host: &str, list: &str) -> bool {
    use std::net::IpAddr;
    let host = if host.starts_with('[') {
        host.trim_matches(&['[', ']'][..])
    } else {
        host
    };
    let mut entries = list.split(',').map(str::trim).filter(|e| !e.is_empty());
    match host.parse::<IpAddr>() {
        Ok(ip) => entries.any(|entry| match cidr(entry) {
            Some((network, prefix)) => in_network(ip, network, prefix),
            None => entry.parse::<IpAddr>().ok() == Some(ip),
        }),
        Err(_) => entries
            .filter(|entry| cidr(entry).is_none() && entry.parse::<IpAddr>().is_err())
            .any(|entry| domain_matches(host, entry)),
    }
}

/// hyper-util's `DomainMatcher::contains` for one entry.
#[cfg(any(windows, test))]
fn domain_matches(domain: &str, entry: &str) -> bool {
    if entry.eq_ignore_ascii_case(domain)
        || entry
            .strip_prefix('.')
            .is_some_and(|bare| bare.eq_ignore_ascii_case(domain))
    {
        return true;
    }
    let len = domain.len();
    if domain
        .get(len.saturating_sub(entry.len())..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(entry))
    {
        // `len > entry.len()` here: equal lengths matched above.
        return entry.starts_with('.')
            || domain.as_bytes().get(len - entry.len() - 1) == Some(&b'.');
    }
    entry == "*"
}

/// An entry `ipnet::IpNet` parses: a bare address, `/`, then a decimal prefix
/// length of at most 2 (IPv4) or 3 (IPv6) digits within the address size.
#[cfg(any(windows, test))]
fn cidr(entry: &str) -> Option<(std::net::IpAddr, u8)> {
    let (network, prefix) = entry.split_once('/')?;
    let network: std::net::IpAddr = network.parse().ok()?;
    let (digits, max) = if network.is_ipv4() { (2, 32) } else { (3, 128) };
    if prefix.is_empty() || prefix.len() > digits || !prefix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let prefix: u8 = prefix.parse().ok()?;
    (prefix <= max).then_some((network, prefix))
}

#[cfg(any(windows, test))]
fn in_network(ip: std::net::IpAddr, network: std::net::IpAddr, prefix: u8) -> bool {
    use std::net::IpAddr;
    let (ip, network, bits) = match (ip, network) {
        (IpAddr::V4(a), IpAddr::V4(b)) => (u32::from(a) as u128, u32::from(b) as u128, 32),
        (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a), u128::from(b), 128),
        _ => return false,
    };
    let shift = bits - u32::from(prefix);
    let mask = if shift >= 128 { 0 } else { u128::MAX << shift };
    ip & mask == network & mask
}

#[cfg(windows)]
fn lookup_system(url: &url::Url) -> anyhow::Result<Lookup> {
    use anyhow::{ensure, Context};
    use std::{ffi::c_void, ptr};
    use windows_sys::Win32::{Foundation::GlobalFree, Networking::WinHttp::*};
    // Explicit environment settings keep reqwest's existing precedence/NO_PROXY behavior.
    if env_proxy(url.scheme()).is_some() {
        return Ok(Lookup::Inherit);
    }
    struct Strings(Vec<*mut u16>);
    impl Drop for Strings {
        fn drop(&mut self) {
            for p in &self.0 {
                if !p.is_null() {
                    unsafe {
                        GlobalFree(*p as *mut c_void);
                    }
                }
            }
        }
    }
    unsafe fn text(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
    let mut config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("Read Windows system proxy configuration");
    }
    let _config_strings = Strings(vec![
        config.lpszAutoConfigUrl,
        config.lpszProxy,
        config.lpszProxyBypass,
    ]);
    let pac = unsafe { text(config.lpszAutoConfigUrl) };
    let mut proxy = unsafe { text(config.lpszProxy) };
    let mut bypass = unsafe { text(config.lpszProxyBypass) };
    if !pac.is_empty() || config.fAutoDetect != 0 {
        let agent: Vec<u16> = "Okilum proxy discovery\0".encode_utf16().collect();
        let handle = unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        };
        ensure!(
            !handle.is_null(),
            "Open Windows proxy resolver: {}",
            std::io::Error::last_os_error()
        );
        struct Session(*mut c_void);
        impl Drop for Session {
            fn drop(&mut self) {
                unsafe {
                    WinHttpCloseHandle(self.0);
                }
            }
        }
        let _session = Session(handle);
        ensure!(
            unsafe { WinHttpSetTimeouts(handle, 5000, 5000, 5000, 5000) } != 0,
            "Set proxy discovery timeout: {}",
            std::io::Error::last_os_error()
        );
        let mut options = WINHTTP_AUTOPROXY_OPTIONS {
            fAutoLogonIfChallenged: 0,
            ..Default::default()
        };
        if !pac.is_empty() {
            options.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
            options.lpszAutoConfigUrl = config.lpszAutoConfigUrl;
        } else {
            options.dwFlags = WINHTTP_AUTOPROXY_AUTO_DETECT;
            options.dwAutoDetectFlags =
                WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
        }
        let wide_url: Vec<u16> = url.as_str().encode_utf16().chain(Some(0)).collect();
        let mut info = WINHTTP_PROXY_INFO::default();
        if unsafe { WinHttpGetProxyForUrl(handle, wide_url.as_ptr(), &mut options, &mut info) } == 0
        {
            let error = std::io::Error::last_os_error();
            // WPAD being unavailable is normal on a direct/static-proxy network.
            if !pac.is_empty()
                || error.raw_os_error() != Some(ERROR_WINHTTP_AUTODETECTION_FAILED as i32)
            {
                return Err(error).context("Resolve Windows PAC/system proxy");
            }
        } else {
            let _info_strings = Strings(vec![info.lpszProxy, info.lpszProxyBypass]);
            proxy = unsafe { text(info.lpszProxy) };
            bypass = unsafe { text(info.lpszProxyBypass) };
            if info.dwAccessType == WINHTTP_ACCESS_TYPE_NO_PROXY {
                proxy.clear();
            }
        }
    }
    if bypasses(url.host_str().unwrap_or_default(), &bypass) {
        return Ok(Lookup::Direct);
    }
    Ok(match select_proxy(&proxy, url.scheme()) {
        Some(proxy) => Lookup::Proxy { proxy, bypass },
        None => Lookup::Direct,
    })
}

#[cfg(any(windows, test))]
fn select_proxy(value: &str, scheme: &str) -> Option<String> {
    let entries: Vec<_> = value
        .split(';')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect();
    let selected = if entries.iter().any(|v| v.contains('=')) {
        entries.iter().find_map(|v| {
            v.split_once('=')
                .filter(|(s, _)| s.eq_ignore_ascii_case(scheme))
                .map(|(_, p)| p.trim())
        })
    } else {
        entries.first().copied()
    }?;
    if selected.eq_ignore_ascii_case("DIRECT") {
        return None;
    }
    Some(if selected.contains("://") {
        selected.into()
    } else {
        format!("http://{selected}")
    })
}

#[cfg(any(windows, test))]
fn bypasses(host: &str, bypass: &str) -> bool {
    let host = host.to_ascii_lowercase();
    bypass.split(';').any(|pattern| {
        let pattern = pattern.trim().to_ascii_lowercase();
        if pattern == "<local>" {
            return !host.is_empty() && !host.contains('.');
        }
        if let Some(suffix) = pattern.strip_prefix("*.") {
            return host.ends_with(&format!(".{suffix}")) || host == suffix;
        }
        !pattern.is_empty() && pattern == host
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn per_protocol_proxy_is_not_treated_as_one_url() {
        assert_eq!(
            select_proxy("http=one:80;https=two:81", "https"),
            Some("http://two:81".into())
        );
        assert_eq!(
            select_proxy("one:80", "https"),
            Some("http://one:80".into())
        );
        assert_eq!(select_proxy("http=one:80", "https"), None);
        assert_eq!(select_proxy("DIRECT", "https"), None);
    }
    #[test]
    fn system_bypass_matches_hosts_without_substring_leaks() {
        assert!(bypasses("printer", "<local>"));
        assert!(bypasses("api.example.test", "*.example.test"));
        assert!(!bypasses("example.test.attacker.test", "*.example.test"));
        assert!(!bypasses("remote.test", "<local>"));
    }
    #[test]
    fn no_proxy_follows_reqwest_rules() {
        let list = "intranet.test, .corp.test,10.1.2.3,::1,192.168.1.0/24,fd00::/8,2.3";
        assert!(no_proxy_matches("intranet.test", list));
        assert!(no_proxy_matches("a.corp.test", list));
        assert!(no_proxy_matches("corp.test", list));
        assert!(no_proxy_matches("Intranet.TEST", list));
        assert!(no_proxy_matches("10.1.2.3", list));
        assert!(no_proxy_matches("[::1]", list));
        assert!(no_proxy_matches("192.168.1.42", list));
        assert!(no_proxy_matches("[fd12::1]", list));
        assert!(!no_proxy_matches("192.168.2.1", list));
        // A numeric entry is not a string suffix of an IP host.
        assert!(!no_proxy_matches("10.1.2.3", "2.3"));
        assert!(!no_proxy_matches("10.9.2.3", list));
        assert!(!no_proxy_matches("evilcorp.test", list));
        assert!(!no_proxy_matches("intranet.test.attacker.test", list));
        assert!(!no_proxy_matches("example.test", ""));
        // hyper-util 0.1.20 stores `*` as a domain: every name, never an IP.
        assert!(no_proxy_matches("example.test", "*"));
        assert!(!no_proxy_matches("10.0.0.1", "*"));
        assert!(!no_proxy_matches("[fd00::1]", "*"));
        // A bracketed entry is a domain too, so it matches no IPv6 host.
        assert!(!no_proxy_matches("[::1]", "[::1]"));
        assert!(!no_proxy_matches("::1", "[::1]"));
        assert!(!no_proxy_matches("[fd12::1]", "[fd00::]/8"));
        // Only a plain decimal prefix makes a range.
        assert!(!no_proxy_matches("192.168.1.42", "192.168.1.0/+24"));
        assert!(!no_proxy_matches("192.168.1.42", "192.168.1.0/33"));
        assert!(!no_proxy_matches("192.168.1.42", "192.168.1.0/024"));
        assert!(no_proxy_matches("10.200.0.1", "10.0.0.0/8"));
    }
}
