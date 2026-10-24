//! Resolve the current user's Windows proxy, including PAC/WPAD, for this URL.
//! Credentials are not copied from Windows or placed in headers/logs.
#[cfg(windows)]
pub(super) fn configure(
    builder: reqwest::blocking::ClientBuilder,
    url: &url::Url,
) -> anyhow::Result<reqwest::blocking::ClientBuilder> {
    use anyhow::{ensure, Context};
    use std::{ffi::c_void, ptr};
    use windows_sys::Win32::{Foundation::GlobalFree, Networking::WinHttp::*};
    // Explicit environment settings keep reqwest's existing precedence/NO_PROXY behavior.
    let names = [
        format!("{}_proxy", url.scheme()),
        format!("{}_PROXY", url.scheme().to_ascii_uppercase()),
        "all_proxy".into(),
        "ALL_PROXY".into(),
    ];
    if names
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|v| !v.is_empty()))
    {
        return Ok(builder);
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
        return Ok(builder.no_proxy());
    }
    match select_proxy(&proxy, url.scheme()) {
        Some(proxy) => {
            let bypass = bypass.replace(';', ",");
            let no_proxy = [
                std::env::var("NO_PROXY")
                    .or_else(|_| std::env::var("no_proxy"))
                    .unwrap_or_default(),
                bypass,
            ]
            .join(",");
            Ok(builder.proxy(
                reqwest::Proxy::all(proxy)?.no_proxy(reqwest::NoProxy::from_string(&no_proxy)),
            ))
        }
        None => Ok(builder.no_proxy()),
    }
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
}
