pub mod monitor;
pub mod queries;

use futures::TryStreamExt;
use netlink_packet_route::link::LinkAttribute;
use tracing::info;

use netlink_packet_route::link::LinkMessage;

use crate::Result;
use crate::mapping;
use crate::state::{DeviceInfo, SharedState};

/// Build a DeviceInfo from a netlink LinkMessage, or None if the interface should be ignored.
pub fn device_from_link_msg(msg: &LinkMessage) -> Option<DeviceInfo> {
    let ifindex = msg.header.index as i32;
    let flags = msg.header.flags.bits();

    let mut name = None;
    let mut mac = None;

    for attr in &msg.attributes {
        match attr {
            LinkAttribute::IfName(n) => name = Some(n.clone()),
            LinkAttribute::Address(bytes) => mac = Some(queries::format_mac(bytes)),
            _ => {}
        }
    }

    let iface_name = name?;
    if should_ignore_interface(&iface_name) {
        return None;
    }

    let mut dev = DeviceInfo::new(ifindex, iface_name);
    if let Some(m) = mac {
        dev.hw_address = m;
    }
    dev.link_flags = flags;
    dev.nm_state = mapping::netlink_flags_to_nm_device(flags, false, false);
    Some(dev)
}

/// Check if interface should be ignored (virtual interfaces, containers, etc.)
pub fn should_ignore_interface(name: &str) -> bool {
    const IGNORED_PREFIXES: &[&str] = &[
        "lo",        // loopback
        "docker",    // docker networks
        "veth",      // virtual ethernet (containers)
        "br-",       // docker bridges
        "virbr",     // libvirt bridges
        "vnet",      // libvirt tap devices
        "wg",        // WireGuard tunnels
        "tun",       // TUN devices
        "tap",       // TAP devices
        "tailscale", // Tailscale VPN
        "podman",    // Podman container networks
    ];

    IGNORED_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Load initial network state from kernel via netlink (no networkd dependency).
pub async fn load_initial_state(shared: &SharedState) -> Result<()> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::spawn(conn);

    // Store handle in shared state for reuse by all reload/query functions
    shared.write().await.netlink_handle = Some(handle.clone());

    // Load all network links
    let mut links = handle.link().get().execute();
    let mut discovered_devices = Vec::new();

    while let Some(msg) = links.try_next().await? {
        if let Some(dev) = device_from_link_msg(&msg) {
            info!(ifindex = dev.ifindex, name = %dev.name, "discovered link");
            discovered_devices.push((dev.ifindex, dev));
        }
    }

    // Insert devices into shared state
    {
        let mut state = shared.write().await;
        for (ifindex, dev) in discovered_devices {
            state.devices.insert(ifindex, dev);
        }
    }

    // Load addresses, gateways, DNS
    queries::load_initial_addresses(&handle, shared).await?;

    // Now update device states based on actual IPs
    {
        let mut state = shared.write().await;
        for dev in state.devices.values_mut() {
            let has_ipv4 = !dev.ipv4_addrs.is_empty();
            let has_ipv6 = !dev.ipv6_addrs.is_empty();
            // Re-evaluate state with IP info
            if has_ipv4 || has_ipv6 {
                if dev.nm_state == mapping::nm_device_state::IP_CONFIG {
                    dev.nm_state = mapping::nm_device_state::ACTIVATED;
                }
            }
        }

        // Compute global state
        state.global_state = mapping::deduce_global_state(&state.devices);
        state.connectivity = mapping::global_state_to_connectivity(state.global_state);
    }

    Ok(())
}
