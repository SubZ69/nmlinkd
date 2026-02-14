/// NetworkManager global state (NMState).
pub mod nm_state {
    pub const DISCONNECTED: u32 = 20;
    pub const CONNECTED_LOCAL: u32 = 50;
    pub const CONNECTED_GLOBAL: u32 = 70;
}

/// NetworkManager device state (NMDeviceState).
pub mod nm_device_state {
    pub const UNKNOWN: u32 = 0;
    pub const UNAVAILABLE: u32 = 20;
    pub const DISCONNECTED: u32 = 30;
    pub const IP_CONFIG: u32 = 70;
    pub const ACTIVATED: u32 = 100;
}

/// NetworkManager device type (NMDeviceType).
pub mod nm_device_type {
    pub const ETHERNET: u32 = 1;
}

/// NetworkManager connectivity state (NMConnectivityState).
pub mod nm_connectivity {
    pub const UNKNOWN: u32 = 0;
    pub const NONE: u32 = 1;
    pub const FULL: u32 = 4;
}

/// NetworkManager active connection state (NMActiveConnectionState).
pub mod nm_active_connection_state {
    pub const UNKNOWN: u32 = 0;
    pub const ACTIVATED: u32 = 2;
    pub const DEACTIVATED: u32 = 4;
}

/// Linux netlink interface flags.
pub mod netlink_flags {
    pub const IFF_UP: u32 = 0x1;
    pub const IFF_RUNNING: u32 = 0x40;
    pub const IFF_LOWER_UP: u32 = 0x10000;
    pub const IFF_DORMANT: u32 = 0x20000;
}

/// Deduce global NM state from device states and routes.
pub fn deduce_global_state(
    devices: &std::collections::HashMap<i32, crate::state::DeviceInfo>,
) -> u32 {
    let mut has_local = false;

    for dev in devices.values() {
        let has_ip = !dev.ipv4_addrs.is_empty() || !dev.ipv6_addrs.is_empty();
        if has_ip {
            has_local = true;
            if dev.has_gateway() {
                return nm_state::CONNECTED_GLOBAL;
            }
        }
    }

    if has_local {
        nm_state::CONNECTED_LOCAL
    } else {
        nm_state::DISCONNECTED
    }
}

/// Deduce connectivity from global state.
/// For a read-only bridge, we assume full connectivity if connected,
/// since we don't perform actual connectivity checks.
pub fn global_state_to_connectivity(global_state: u32) -> u32 {
    match global_state {
        nm_state::CONNECTED_LOCAL..=nm_state::CONNECTED_GLOBAL => nm_connectivity::FULL,
        nm_state::DISCONNECTED => nm_connectivity::NONE,
        _ => nm_connectivity::UNKNOWN,
    }
}

/// Map netlink link flags to NM device state.
pub fn netlink_flags_to_nm_device(flags: u32, has_ipv4: bool, has_ipv6: bool) -> u32 {
    use netlink_flags::*;

    let is_up = (flags & IFF_UP) != 0;
    let is_running = (flags & IFF_RUNNING) != 0;
    let is_lower_up = (flags & IFF_LOWER_UP) != 0;
    let is_dormant = (flags & IFF_DORMANT) != 0;

    if !is_up {
        return nm_device_state::DISCONNECTED;
    }

    if is_dormant {
        return nm_device_state::UNAVAILABLE;
    }

    let has_carrier = is_running || is_lower_up;
    let has_ip = has_ipv4 || has_ipv6;

    match (has_carrier, has_ip) {
        (false, _) => nm_device_state::UNAVAILABLE,
        (true, false) => nm_device_state::IP_CONFIG,
        (true, true) => nm_device_state::ACTIVATED,
    }
}
