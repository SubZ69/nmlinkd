/// NetworkManager global state (NMState).
pub mod nm_state {
    pub const DISCONNECTED: u32 = 20;
    pub const CONNECTED_LOCAL: u32 = 50;
    pub const CONNECTED_SITE: u32 = 60;
    pub const CONNECTED_GLOBAL: u32 = 70;
}

/// NetworkManager device state (NMDeviceState).
pub mod nm_device_state {
    pub const UNKNOWN: u32 = 0;
    pub const UNAVAILABLE: u32 = 20;
    pub const DISCONNECTED: u32 = 30;
    pub const PREPARE: u32 = 40;
    pub const IP_CONFIG: u32 = 70;
    pub const ACTIVATED: u32 = 100;
    pub const DEACTIVATING: u32 = 110;
}

/// NetworkManager device type (NMDeviceType).
pub mod nm_device_type {
    pub const ETHERNET: u32 = 1;
    pub const WIREGUARD: u32 = 29;
}

/// NetworkManager connectivity state (NMConnectivityState).
pub mod nm_connectivity {
    pub const UNKNOWN: u32 = 0;
    pub const NONE: u32 = 1;
    pub const PORTAL: u32 = 2;
    pub const LIMITED: u32 = 3;
    pub const FULL: u32 = 4;
}

/// NetworkManager device state reason (NMDeviceStateReason).
pub mod nm_device_state_reason {
    pub const NONE: u32 = 0;
    pub const USER_REQUESTED: u32 = 39;
}

/// NetworkManager active connection state (NMActiveConnectionState).
pub mod nm_active_connection_state {
    pub const UNKNOWN: u32 = 0;
    pub const ACTIVATED: u32 = 2;
    pub const DEACTIVATED: u32 = 4;
}

/// NetworkManager active connection state reason (NMActiveConnectionStateReason).
pub mod nm_active_connection_state_reason {
    pub const UNKNOWN: u32 = 0;
    pub const USER_DISCONNECTED: u32 = 2;
}

/// Linux netlink interface flags.
pub mod netlink_flags {
    pub const IFF_UP: u32 = 0x1;
    pub const IFF_RUNNING: u32 = 0x40;
    pub const IFF_LOWER_UP: u32 = 0x10000;
    pub const IFF_DORMANT: u32 = 0x20000;
}

/// Connectivity tier from routes alone, before factoring in the HTTP probe result.
pub enum RouteTier {
    Disconnected,
    Local,
    HasGateway,
}

/// Deduce the route-based connectivity tier from device states and routes.
pub fn deduce_route_tier(
    devices: &std::collections::HashMap<i32, crate::state::DeviceInfo>,
) -> RouteTier {
    let mut has_local = false;

    for dev in devices.values() {
        let has_ip = !dev.ipv4_addrs.is_empty() || !dev.ipv6_addrs.is_empty();
        if has_ip {
            has_local = true;
            if dev.has_gateway() {
                return RouteTier::HasGateway;
            }
        }
    }

    if has_local {
        RouteTier::Local
    } else {
        RouteTier::Disconnected
    }
}

/// Whether an `NMState` means a gateway is present (`SITE` or `GLOBAL`).
pub fn state_has_gateway(global_state: u32) -> bool {
    global_state == nm_state::CONNECTED_SITE || global_state == nm_state::CONNECTED_GLOBAL
}

/// `SITE` if the probe hasn't confirmed full connectivity yet, `GLOBAL` otherwise.
pub fn gateway_state_for_connectivity(connectivity: u32) -> u32 {
    if connectivity == nm_connectivity::FULL {
        nm_state::CONNECTED_GLOBAL
    } else {
        nm_state::CONNECTED_SITE
    }
}

/// Outcome of an HTTP connectivity probe against the configured check URI.
pub enum ProbeResult {
    Full,
    Portal,
    Failed,
}

/// Map an HTTP connectivity probe outcome to `NMConnectivityState`.
pub fn probe_result_to_connectivity(result: ProbeResult) -> u32 {
    match result {
        ProbeResult::Full => nm_connectivity::FULL,
        ProbeResult::Portal => nm_connectivity::PORTAL,
        ProbeResult::Failed => nm_connectivity::LIMITED,
    }
}

/// Map device type to NM connection type string.
pub fn device_type_to_connection_type(device_type: u32) -> &'static str {
    if device_type == nm_device_type::WIREGUARD {
        "wireguard"
    } else {
        "802-3-ethernet"
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
