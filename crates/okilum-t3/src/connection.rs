//! Bounded recovery for macOS Local Network permission during connection setup.
//! Never wrap sends, launches, token exchanges, or other feature operations here.
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::Result;

pub const RETRY_NOTICE: &str = "macOS may be asking to allow local network access. Allow Okilum in System Settings → Privacy & Security → Local Network. Retrying once…";
pub const PERMISSION_HELP: &str = "Could not reach T3 on the local network. macOS may need your permission: allow Okilum in System Settings → Privacy & Security → Local Network, then choose Sign in. Okilum already retried once.";

#[derive(Debug)]
struct PermissionHelp {
    diagnostic: String,
}
impl std::fmt::Display for PermissionHelp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(PERMISSION_HELP)
    }
}
impl std::error::Error for PermissionHelp {}

/// User-facing diagnosis is deliberately tentative; route failure alone does
/// not prove that macOS displayed a permission prompt.
pub fn permission_help(error: &anyhow::Error) -> Option<&'static str> {
    error
        .downcast_ref::<PermissionHelp>()
        .map(|_| PERMISSION_HELP)
}

/// The original error chain without the user-facing permission explanation.
pub fn permission_details(error: &anyhow::Error) -> Option<String> {
    error
        .downcast_ref::<PermissionHelp>()
        .map(|help| help.diagnostic.clone())
}

#[derive(Default)]
pub struct Retry {
    used: bool,
}
impl Retry {
    /// Run on a worker thread. Resolve only after the specific macOS route
    /// error, and require every resolved destination to be local, not public.
    pub fn run<T>(
        &mut self,
        environment: &crate::Environment,
        operation: impl FnMut() -> Result<T>,
        notice: impl FnOnce(),
    ) -> Result<T> {
        self.run_using(
            operation,
            notice,
            |error| {
                if !cfg!(target_os = "macos") || !route_error(error) {
                    return false;
                }
                environment
                    .mcp_url()
                    .socket_addrs(|| None)
                    .is_ok_and(|addresses| local_destinations(&addresses))
            },
            || std::thread::sleep(Duration::from_secs(2)),
        )
    }

    fn run_using<T>(
        &mut self,
        mut operation: impl FnMut() -> Result<T>,
        notice: impl FnOnce(),
        eligible: impl FnOnce(&anyhow::Error) -> bool,
        wait: impl FnOnce(),
    ) -> Result<T> {
        let error = match operation() {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if self.used || !eligible(&error) {
            return Err(error);
        }
        self.used = true;
        notice();
        wait();
        operation().map_err(|error| {
            if route_error(&error) {
                let diagnostic = format!("{error:#}");
                error.context(PermissionHelp { diagnostic })
            } else {
                error
            }
        })
    }
}

fn route_error(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        if let Some(io) = source.downcast_ref::<std::io::Error>() {
            return matches!(io.raw_os_error(), Some(51 | 65));
        }
        // MCP preserves its transport chain as an Unreachable detail string.
        source
            .downcast_ref::<crate::GatewayError>()
            .is_some_and(|gateway| {
                matches!(gateway, crate::GatewayError::Unreachable(detail)
                if detail.contains("(os error 65)") || detail.contains("(os error 51)"))
            })
    })
}

fn local_destinations(addresses: &[SocketAddr]) -> bool {
    !addresses.is_empty()
        && addresses.iter().all(|address| match address.ip() {
            IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
            IpAddr::V6(ip) => {
                let first = ip.segments()[0];
                (first & 0xfe00) == 0xfc00
                    || (first & 0xffc0) == 0xfe80
                    || ip
                        .to_ipv4_mapped()
                        .is_some_and(|ip| ip.is_private() || ip.is_link_local())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn mac_route_mapping_requires_local_destinations_and_transport_errors() {
        for code in [51, 65] {
            assert!(route_error(
                &anyhow::Error::new(std::io::Error::from_raw_os_error(code))
                    .context("cannot discover OAuth")
            ));
            assert!(route_error(
                &crate::GatewayError::Unreachable(format!("tcp connect error (os error {code})"))
                    .into()
            ));
        }
        for error in [
            anyhow::anyhow!("HTTP 500 (os error 65)"),
            crate::GatewayError::Revoked.into(),
            std::io::Error::from_raw_os_error(61).into(),
        ] {
            assert!(!route_error(&error));
        }
        for addresses in [
            vec!["10.10.0.23:443"],
            vec!["192.168.1.2:443", "[fd00::1]:443"],
            vec!["[fe80::1]:443"],
        ] {
            assert!(local_destinations(
                &addresses
                    .into_iter()
                    .map(|s| s.parse().unwrap())
                    .collect::<Vec<_>>()
            ));
        }
        for addresses in [
            vec![],
            vec!["8.8.8.8:443"],
            vec!["10.0.0.1:443", "8.8.8.8:443"],
            vec!["127.0.0.1:443"],
            vec!["[::1]:443"],
        ] {
            assert!(!local_destinations(
                &addresses
                    .into_iter()
                    .map(|s| s.parse().unwrap())
                    .collect::<Vec<_>>()
            ));
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn mac_errno_on_another_os_never_retries_or_resolves() {
        let env = crate::Environment::new("lan", "https://10.10.0.23").unwrap();
        let attempts = Cell::new(0);
        let error = Retry::default()
            .run(
                &env,
                || {
                    attempts.set(attempts.get() + 1);
                    Err::<(), _>(std::io::Error::from_raw_os_error(65).into())
                },
                || panic!("macOS guidance must not appear on another OS"),
            )
            .unwrap_err();
        assert_eq!(attempts.get(), 1);
        assert_eq!(permission_help(&error), None);
    }

    #[test]
    fn one_retry_budget_is_shared_and_preserves_failure_details() {
        let mut retry = Retry::default();
        let attempts = Cell::new(0);
        let notices = Cell::new(0);
        let result: Result<()> = retry.run_using(
            || {
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::from_raw_os_error(65).into())
            },
            || notices.set(notices.get() + 1),
            route_error,
            || {},
        );
        let error = result.unwrap_err();
        assert_eq!(permission_help(&error), Some(PERMISSION_HELP));
        assert!(format!("{error:#}").contains("os error 65"));
        let details = permission_details(&error).unwrap();
        assert!(details.contains("os error 65"));
        assert!(
            !details.contains(PERMISSION_HELP),
            "Details must not repeat the headline"
        );
        assert_eq!(attempts.get(), 2);
        assert_eq!(notices.get(), 1);
        let _: Result<()> = retry.run_using(
            || {
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::from_raw_os_error(65).into())
            },
            || panic!("no second retry"),
            |_| panic!("no second resolution"),
            || panic!("no second wait"),
        );
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn permission_retry_can_recover_but_other_errors_do_not_retry() {
        let attempts = Cell::new(0);
        let result = Retry::default().run_using(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Err(std::io::Error::from_raw_os_error(65).into())
                } else {
                    Ok(42)
                }
            },
            || {},
            route_error,
            || {},
        );
        assert_eq!(result.unwrap(), 42);
        let error = Retry::default()
            .run_using(
                || Err::<(), _>(crate::GatewayError::InvalidCredential.into()),
                || panic!("no retry"),
                route_error,
                || panic!("no wait"),
            )
            .unwrap_err();
        assert_eq!(permission_help(&error), None);
        assert_eq!(
            error.downcast_ref::<crate::GatewayError>(),
            Some(&crate::GatewayError::InvalidCredential)
        );
    }
}
