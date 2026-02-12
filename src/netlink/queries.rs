use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use futures::TryStreamExt;
use netlink_packet_route::address::AddressAttribute;
use netlink_packet_route::route::{RouteAddress, RouteAttribute};
use rtnetlink::RouteMessageBuilder;
use tracing::{debug, warn};

use rtnetlink::LinkUnspec;

use crate::Result;
use crate::state::{AddrInfo, SharedState};

/// Format a MAC address from raw bytes (e.g. `[0xAA, 0xBB, ...]` → `"AA:BB:..."`).
pub fn format_mac(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Load IP addresses and default gateways into the shared state.
pub async fn load_initial_addresses(handle: &rtnetlink::Handle, shared: &SharedState) -> Result<()> {
    let state = shared.read().await;
    let ifindexes: Vec<i32> = state.devices.keys().copied().collect();
    drop(state);

    for ifindex in ifindexes {
        let idx = ifindex as u32;

        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        let mut addrs = handle.address().get().set_link_index_filter(idx).execute();
        while let Some(msg) = addrs.try_next().await? {
            let prefix_len = msg.header.prefix_len;
            for attr in &msg.attributes {
                match attr {
                    AddressAttribute::Address(IpAddr::V4(v4)) => {
                        ipv4.push(AddrInfo {
                            address: *v4,
                            prefix_len,
                        });
                    }
                    AddressAttribute::Address(IpAddr::V6(v6)) => {
                        ipv6.push(AddrInfo {
                            address: *v6,
                            prefix_len,
                        });
                    }
                    _ => {}
                }
            }
        }
        let mut state = shared.write().await;
        if let Some(dev) = state.devices.get_mut(&ifindex) {
            debug!(iface = %dev.name, ipv4 = ipv4.len(), ipv6 = ipv6.len(), "loaded addresses");
            dev.ipv4_addrs = ipv4;
            dev.ipv6_addrs = ipv6;
        }
    }

    load_default_gateways(handle, shared).await?;
    reload_nameservers(shared).await;

    Ok(())
}

/// Load default gateways for both IPv4 and IPv6.
pub async fn load_default_gateways(handle: &rtnetlink::Handle, shared: &SharedState) -> Result<()> {
    let route_msg = RouteMessageBuilder::<Ipv4Addr>::new().build();
    let mut routes = handle.route().get(route_msg).execute();
    while let Some(msg) = routes.try_next().await? {
        if let Some((gw, idx)) = parse_default_gateway(&msg, |a| match a {
            RouteAddress::Inet(ip) => Some(IpAddr::V4(*ip)),
            _ => None,
        }) {
            let mut state = shared.write().await;
            if let Some(dev) = state.devices.get_mut(&idx)
                && let IpAddr::V4(v4) = gw
            {
                debug!(iface = %dev.name, gateway = %v4, "loaded IPv4 default gateway");
                dev.gateway4 = Some(v4);
            }
        }
    }

    let route_msg = RouteMessageBuilder::<Ipv6Addr>::new().build();
    let mut routes = handle.route().get(route_msg).execute();
    while let Some(msg) = routes.try_next().await? {
        if let Some((gw, idx)) = parse_default_gateway(&msg, |a| match a {
            RouteAddress::Inet6(ip) => Some(IpAddr::V6(*ip)),
            _ => None,
        }) {
            let mut state = shared.write().await;
            if let Some(dev) = state.devices.get_mut(&idx)
                && let IpAddr::V6(v6) = gw
            {
                debug!(iface = %dev.name, gateway = %v6, "loaded IPv6 default gateway");
                dev.gateway6 = Some(v6);
            }
        }
    }

    Ok(())
}

/// Extract (gateway, ifindex) from a default route message (prefix_len == 0).
fn parse_default_gateway(
    msg: &netlink_packet_route::route::RouteMessage,
    extract_gw: impl Fn(&RouteAddress) -> Option<IpAddr>,
) -> Option<(IpAddr, i32)> {
    if msg.header.destination_prefix_length != 0 {
        return None;
    }
    let mut gateway = None;
    let mut oif = None;
    for attr in &msg.attributes {
        match attr {
            RouteAttribute::Gateway(addr) => gateway = extract_gw(addr),
            RouteAttribute::Oif(idx) => oif = Some(*idx as i32),
            _ => {}
        }
    }
    gateway.zip(oif)
}

/// Reload IP addresses for a single interface.
pub async fn reload_addresses_for(handle: &rtnetlink::Handle, ifindex: i32, shared: &SharedState) {
    let mut ipv4 = Vec::new();
    let mut ipv6 = Vec::new();

    let mut addrs = handle
        .address()
        .get()
        .set_link_index_filter(ifindex as u32)
        .execute();

    while let Ok(Some(msg)) = addrs.try_next().await {
        let prefix_len = msg.header.prefix_len;
        for attr in &msg.attributes {
            match attr {
                AddressAttribute::Address(IpAddr::V4(v4)) => {
                    ipv4.push(AddrInfo {
                        address: *v4,
                        prefix_len,
                    });
                }
                AddressAttribute::Address(IpAddr::V6(v6)) => {
                    ipv6.push(AddrInfo {
                        address: *v6,
                        prefix_len,
                    });
                }
                _ => {}
            }
        }
    }

    let mut state = shared.write().await;
    if let Some(dev) = state.devices.get_mut(&ifindex) {
        dev.ipv4_addrs = ipv4;
        dev.ipv6_addrs = ipv6;
        debug!(iface = %dev.name, "reloaded addresses");
    }
}

/// Reload default gateways for all devices.
pub async fn reload_gateways(handle: &rtnetlink::Handle, shared: &SharedState) {
    {
        let mut state = shared.write().await;
        for dev in state.devices.values_mut() {
            dev.gateway4 = None;
            dev.gateway6 = None;
        }
    }

    if let Err(e) = load_default_gateways(handle, shared).await {
        warn!("failed to reload gateways: {e}");
    }
}

/// Set a network interface up or down via rtnetlink.
async fn link_set(handle: &rtnetlink::Handle, ifindex: i32, up: bool) -> Result<()> {
    let builder = rtnetlink::LinkMessageBuilder::<LinkUnspec>::new().index(ifindex as u32);
    let msg = if up { builder.up() } else { builder.down() }.build();
    handle.link().set(msg).execute().await?;
    Ok(())
}

pub async fn link_set_up(handle: &rtnetlink::Handle, ifindex: i32) -> Result<()> {
    link_set(handle, ifindex, true).await
}

pub async fn link_set_down(handle: &rtnetlink::Handle, ifindex: i32) -> Result<()> {
    link_set(handle, ifindex, false).await
}

/// Parse nameservers from resolv.conf files.
/// Tries /run/systemd/resolve/resolv.conf first (systemd-resolved upstream DNS),
/// falls back to /etc/resolv.conf if not available.
pub async fn reload_nameservers(shared: &SharedState) {
    let resolv_paths = ["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"];

    for path in &resolv_paths {
        if let Ok(contents) = tokio::fs::read_to_string(path).await {
            let servers: Vec<String> = contents
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    if line.starts_with("nameserver") {
                        line.split_whitespace().nth(1).map(String::from)
                    } else {
                        None
                    }
                })
                .collect();

            if !servers.is_empty() {
                debug!(path, count = servers.len(), "loaded nameservers");
                shared.write().await.nameservers = servers;
                return;
            }
        }
    }
}
