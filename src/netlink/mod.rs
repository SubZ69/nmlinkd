pub mod monitor;
pub mod queries;

use futures::TryStreamExt;
use netlink_packet_route::link::LinkAttribute;
use tracing::{debug, info};

use crate::Result;
use crate::mapping;
use crate::state::{DeviceInfo, SharedState};

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

    // Load all network links
    let mut links = handle.link().get().execute();
    let mut discovered_devices = Vec::new();

    while let Some(msg) = links.try_next().await? {
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

        // Skip loopback and virtual interfaces
        if let Some(ref iface_name) = name {
            if should_ignore_interface(iface_name) {
                debug!(name = %iface_name, "ignoring interface");
                continue;
            }
        }

        if let Some(iface_name) = name {
            let mut dev = DeviceInfo::new(ifindex, iface_name.clone());
            if let Some(m) = mac {
                dev.hw_address = m;
            }

            // Initial state from flags (will be updated after loading IPs)
            dev.nm_state = mapping::netlink_flags_to_nm_device(flags, false, false);

            discovered_devices.push((ifindex, dev));
            info!(ifindex, name = %iface_name, flags, "discovered link");
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
    queries::load_initial_addresses(shared).await?;

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
