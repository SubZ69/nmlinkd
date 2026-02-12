use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, LazyLock};
use tokio::sync::RwLock;

use zbus::zvariant::OwnedObjectPath;

use crate::mapping;

const NM_PREFIX: &str = "/org/freedesktop/NetworkManager";

/// Generate a stable UUID for a connection based on interface name.
pub fn connection_uuid(iface_name: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h1 = DefaultHasher::new();
    "nmlinkd".hash(&mut h1);
    iface_name.hash(&mut h1);
    let hash1 = h1.finish();

    let mut h2 = DefaultHasher::new();
    "nmlinkd2".hash(&mut h2);
    iface_name.hash(&mut h2);
    let hash2 = h2.finish();

    let bytes = [hash1.to_le_bytes(), hash2.to_le_bytes()].concat();
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_le_bytes([bytes[4], bytes[5]]),
        u16::from_le_bytes([bytes[6], bytes[7]]),
        u16::from_le_bytes([bytes[8], bytes[9]]),
        u64::from_le_bytes([
            bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], 0, 0
        ]),
    )
}

fn nm_path(kind: &str, ifindex: i32) -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("{NM_PREFIX}/{kind}/{ifindex}")).unwrap()
}

pub fn device_path(ifindex: i32) -> OwnedObjectPath {
    nm_path("Devices", ifindex)
}

pub fn active_connection_path(ifindex: i32) -> OwnedObjectPath {
    nm_path("ActiveConnection", ifindex)
}

pub fn ip4_config_path(ifindex: i32) -> OwnedObjectPath {
    nm_path("IP4Config", ifindex)
}

pub fn ip6_config_path(ifindex: i32) -> OwnedObjectPath {
    nm_path("IP6Config", ifindex)
}

pub fn settings_path(ifindex: i32) -> OwnedObjectPath {
    nm_path("Settings", ifindex)
}

static ROOT_PATH: LazyLock<OwnedObjectPath> =
    LazyLock::new(|| OwnedObjectPath::try_from("/").unwrap());

pub fn root_path() -> OwnedObjectPath {
    ROOT_PATH.clone()
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn new_shared_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}

/// Extension trait for ergonomic access on SharedState.
pub trait SharedStateExt {
    async fn with_device<T>(&self, ifindex: i32, f: impl FnOnce(&DeviceInfo) -> T) -> Option<T>;
    async fn with_state<T>(&self, f: impl FnOnce(&AppState) -> T) -> T;
}

impl SharedStateExt for SharedState {
    async fn with_device<T>(&self, ifindex: i32, f: impl FnOnce(&DeviceInfo) -> T) -> Option<T> {
        let state = self.read().await;
        state.devices.get(&ifindex).map(f)
    }

    async fn with_state<T>(&self, f: impl FnOnce(&AppState) -> T) -> T {
        let state = self.read().await;
        f(&state)
    }
}

#[derive(Default)]
pub struct AppState {
    pub global_state: u32,
    pub connectivity: u32,
    pub devices: HashMap<i32, DeviceInfo>,
    pub nameservers: Vec<String>,
    pub netlink_handle: Option<rtnetlink::Handle>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("global_state", &self.global_state)
            .field("connectivity", &self.connectivity)
            .field("devices", &self.devices)
            .field("nameservers", &self.nameservers)
            .field("netlink_handle", &self.netlink_handle.as_ref().map(|_| "..."))
            .finish()
    }
}

impl AppState {
    /// Get the shared netlink handle. Panics if not initialized (always set after startup).
    pub fn handle(&self) -> &rtnetlink::Handle {
        self.netlink_handle.as_ref().expect("netlink handle not initialized")
    }

    /// Recompute global NM state based on device states and connectivity.
    pub fn recompute_global_state(&mut self) {
        self.global_state = mapping::deduce_global_state(&self.devices);
        self.connectivity = mapping::global_state_to_connectivity(self.global_state);
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub ifindex: i32,
    pub name: String,
    pub nm_state: u32,
    pub hw_address: String,
    pub ipv4_addrs: Vec<AddrInfo<Ipv4Addr>>,
    pub ipv6_addrs: Vec<AddrInfo<Ipv6Addr>>,
    pub gateway4: Option<Ipv4Addr>,
    pub gateway6: Option<Ipv6Addr>,
}

impl DeviceInfo {
    pub fn new(ifindex: i32, name: String) -> Self {
        Self {
            ifindex,
            name,
            nm_state: mapping::nm_device_state::UNKNOWN,
            hw_address: String::new(),
            ipv4_addrs: Vec::new(),
            ipv6_addrs: Vec::new(),
            gateway4: None,
            gateway6: None,
        }
    }

    fn has_ip_address(&self) -> bool {
        !self.ipv4_addrs.is_empty() || !self.ipv6_addrs.is_empty()
    }

    /// Update device state when IP addresses change.
    /// Returns (new_state, old_state) if state changed, None otherwise.
    pub fn update_state_on_ip_change(&mut self) -> Option<(u32, u32)> {
        let old_state = self.nm_state;

        if old_state < mapping::nm_device_state::IP_CONFIG {
            return None;
        }

        let has_ip = self.has_ip_address();
        let new_state = if has_ip {
            mapping::nm_device_state::ACTIVATED
        } else {
            mapping::nm_device_state::IP_CONFIG
        };

        if old_state != new_state {
            self.nm_state = new_state;
            Some((new_state, old_state))
        } else {
            None
        }
    }

    /// Update device state when link flags change.
    /// Returns (new_state, old_state) if state changed, None otherwise.
    pub fn update_state_on_link_change(&mut self, flags: u32) -> Option<(u32, u32)> {
        let old_state = self.nm_state;
        let has_ipv4 = !self.ipv4_addrs.is_empty();
        let has_ipv6 = !self.ipv6_addrs.is_empty();
        let new_state = mapping::netlink_flags_to_nm_device(flags, has_ipv4, has_ipv6);

        if old_state != new_state {
            self.nm_state = new_state;

            if new_state == mapping::nm_device_state::DISCONNECTED
                || new_state == mapping::nm_device_state::UNAVAILABLE
            {
                self.gateway4 = None;
                self.gateway6 = None;
            }

            Some((new_state, old_state))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct AddrInfo<A> {
    pub address: A,
    pub prefix_len: u8,
}
