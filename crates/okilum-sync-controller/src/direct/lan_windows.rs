//! Native IPv4 LAN inventory; a VPN default route is never the sole authority.
use anyhow::{ensure, Context, Result};
use std::net::{IpAddr, Ipv4Addr};
use windows::Win32::{Foundation::ERROR_INSUFFICIENT_BUFFER, NetworkManagement::IpHelper::*};

fn usable(row: &MIB_IFROW, ip: Ipv4Addr) -> bool {
    let description =
        String::from_utf8_lossy(&row.bDescr[..(row.dwDescrLen as usize).min(row.bDescr.len())])
            .to_lowercase();
    row.dwOperStatus == IF_OPER_STATUS_OPERATIONAL
        && matches!(row.dwType, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211)
        && ![
            "vpn",
            "tailscale",
            "wireguard",
            "tap-",
            "hyper-v",
            "virtual",
        ]
        .iter()
        .any(|part| description.contains(part))
        && !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !ip.is_broadcast()
}
pub(super) fn address() -> Result<IpAddr> {
    let mut size = 0;
    ensure!(
        unsafe { GetIpAddrTable(None, &mut size, false) } == ERROR_INSUFFICIENT_BUFFER.0,
        "cannot inspect LAN address inventory"
    );
    // u64 alignment covers the native table and its rows; never trust an
    // unbounded count or a growing buffer returned by the system call.
    ensure!(size <= 1024 * 1024, "LAN address inventory exceeds bound");
    let mut allocation = vec![0u64; (size as usize).div_ceil(8)];
    let capacity = allocation.len() * 8;
    let table = allocation.as_mut_ptr().cast::<MIB_IPADDRTABLE>();
    ensure!(
        unsafe { GetIpAddrTable(Some(table), &mut size, false) } == 0,
        "cannot inspect LAN address inventory"
    );
    let count = unsafe { (*table).dwNumEntries } as usize;
    ensure!(
        size as usize <= capacity
            && count <= 4096
            && std::mem::offset_of!(MIB_IPADDRTABLE, table)
                + count * std::mem::size_of::<MIB_IPADDRROW_XP>()
                <= size as usize,
        "invalid LAN address inventory"
    );
    let rows = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*table).table).cast::<MIB_IPADDRROW_XP>(),
            count,
        )
    };
    let mut candidates = vec![];
    for address in rows {
        let mut row = MIB_IFROW {
            dwIndex: address.dwIndex,
            ..Default::default()
        };
        ensure!(
            unsafe { GetIfEntry(&mut row) } == 0,
            "cannot inspect LAN interface"
        );
        let ip = Ipv4Addr::from(address.dwAddr.to_ne_bytes());
        if usable(&row, ip) {
            candidates.push(ip);
        }
    }
    candidates.sort_by_key(|ip| (ip.is_link_local(), *ip));
    candidates.dedup();
    candidates.first().copied().map(IpAddr::V4)
        .context("No active LAN interface is available. Connect to Wi-Fi or Ethernet and try Add computer again")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_lan_selection_requires_live_physical_interfaces() {
        let mut row = MIB_IFROW {
            dwOperStatus: IF_OPER_STATUS_OPERATIONAL,
            dwType: IF_TYPE_ETHERNET_CSMACD,
            ..Default::default()
        };
        let ip = Ipv4Addr::new(192, 168, 20, 5);
        assert!(usable(&row, ip));
        row.dwType = IF_TYPE_IEEE80211;
        assert!(usable(&row, ip));
        assert!(!usable(&row, Ipv4Addr::LOCALHOST));
        row.dwOperStatus = IF_OPER_STATUS_DISCONNECTED;
        assert!(!usable(&row, ip));
        row.dwOperStatus = IF_OPER_STATUS_OPERATIONAL;
        row.dwType = IF_TYPE_TUNNEL;
        assert!(!usable(&row, ip));
        row.dwType = IF_TYPE_ETHERNET_CSMACD;
        row.bDescr[..12].copy_from_slice(b"TAP-Windows ");
        row.dwDescrLen = 12;
        assert!(!usable(&row, ip));
    }
}
