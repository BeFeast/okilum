//! Pairing advertises a real LAN interface, even with no Internet route or a VPN.
use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

fn usable(name: &str, flags: u32, ip: Ipv4Addr) -> bool {
    flags & libc::IFF_UP as u32 != 0
        && flags & (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) as u32 == 0
        && ![
            "utun",
            "tun",
            "tap",
            "wg",
            "tailscale",
            "docker",
            "veth",
            "virbr",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        && !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !ip.is_broadcast()
}

fn select(interfaces: &[(String, u32, Ipv4Addr)], routed: Option<IpAddr>) -> Result<IpAddr> {
    let mut candidates: Vec<_> = interfaces
        .iter()
        .filter(|(name, flags, ip)| usable(name, *flags, *ip))
        .collect();
    candidates.sort_by_key(|(name, _, ip)| {
        // Prefer Ethernet/Wi-Fi to virtual bridges and link-local addresses.
        let physical = ["en", "eth", "wl"].iter().any(|p| name.starts_with(p));
        (
            !physical,
            ip.is_link_local(),
            Some(IpAddr::V4(*ip)) != routed,
            name.clone(),
            *ip,
        )
    });
    candidates.first().map(|(_, _, ip)| IpAddr::V4(*ip))
        .context("No active LAN interface is available. Connect to Wi-Fi or Ethernet and try Add computer again")
}

pub(super) fn address() -> Result<IpAddr> {
    struct Interfaces(*mut libc::ifaddrs);
    impl Drop for Interfaces {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let mut head = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot inspect LAN interfaces");
    }
    let list = Interfaces(head);
    let mut current = list.0;
    let mut interfaces = Vec::new();
    while let Some(entry) = unsafe { current.as_ref() } {
        if !entry.ifa_addr.is_null()
            && !entry.ifa_name.is_null()
            && unsafe { (*entry.ifa_addr).sa_family as i32 } == libc::AF_INET
        {
            let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let address = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
            interfaces.push((
                name,
                entry.ifa_flags,
                Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()),
            ));
        }
        current = entry.ifa_next;
    }
    // UDP connect is only a hint; it sends no packet and its failure is harmless.
    let routed = (|| -> std::io::Result<IpAddr> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect("192.0.2.1:9")?;
        Ok(socket.local_addr()?.ip())
    })()
    .ok();
    select(&interfaces, routed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn issue2589_missing_default_route_and_vpn_still_choose_real_lan() -> Result<()> {
        let up = libc::IFF_UP as u32;
        let lan: Ipv4Addr = "192.168.1.23".parse()?;
        let vpn: Ipv4Addr = "10.7.0.2".parse()?;
        let interfaces = vec![
            ("utun4".into(), up | libc::IFF_POINTOPOINT as u32, vpn),
            ("en0".into(), up, lan),
            (
                "lo0".into(),
                up | libc::IFF_LOOPBACK as u32,
                Ipv4Addr::LOCALHOST,
            ),
        ];
        assert_eq!(select(&interfaces, None)?, IpAddr::V4(lan));
        assert_eq!(select(&interfaces, Some(IpAddr::V4(vpn)))?, IpAddr::V4(lan));
        assert_eq!(select(&interfaces, Some(IpAddr::V4(lan)))?, IpAddr::V4(lan));
        assert!(
            select(&interfaces[..1], None).is_err(),
            "a VPN alone is not a LAN"
        );
        assert!(
            !usable("en0", 0, lan),
            "a down interface cannot be advertised"
        );
        Ok(())
    }
}
