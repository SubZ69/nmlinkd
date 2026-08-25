use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, LazyLock};
use tokio::sync::RwLock;

use zbus::zvariant::OwnedObjectPath;

use crate::mapping;

const NM_PREFIX: &str = "/org/freedesktop/NetworkManager";

const NMLINKD_UUID_NAMESPACE: uuid::Uuid = uuid::uuid!("90bb69d5-2a09-40fc-96b5-3c0e34f9809c");

/// Generate a stable UUID for a connection based on interface name.
pub fn connection_uuid(iface_name: &str) -> String {
    uuid::Uuid::new_v5(&NMLINKD_UUID_NAMESPACE, iface_name.as_bytes()).to_string()
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

pub fn new_shared_state(config: &crate::config::ConnectivityConfig) -> SharedState {
    Arc::new(RwLock::new(AppState {
        connectivity_check_enabled: config.enabled,
        connectivity_check_uri: config.uri.clone(),
        connectivity_check_response: config.response.clone(),
        connectivity_check_timeout_secs: config.timeout_secs,
        ..AppState::default()
    }))
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

pub struct AppState {
    pub global_state: u32,
    pub connectivity: u32,
    pub connectivity_check_enabled: bool,
    pub connectivity_check_uri: String,
    pub connectivity_check_response: String,
    pub connectivity_check_timeout_secs: u64,
    pub devices: HashMap<i32, DeviceInfo>,
    pub nameservers: Vec<String>,
    pub netlink_handle: Option<rtnetlink::Handle>,
    pub user_disconnect_pending: HashSet<i32>,
    pub user_activate_pending: HashSet<i32>,
}

impl Default for AppState {
    fn default() -> Self {
        let cfg = crate::config::ConnectivityConfig::default();
        Self {
            global_state: 0,
            connectivity: 0,
            connectivity_check_enabled: cfg.enabled,
            connectivity_check_uri: cfg.uri,
            connectivity_check_response: cfg.response,
            connectivity_check_timeout_secs: cfg.timeout_secs,
            devices: HashMap::new(),
            nameservers: Vec::new(),
            netlink_handle: None,
            user_disconnect_pending: HashSet::new(),
            user_activate_pending: HashSet::new(),
        }
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("global_state", &self.global_state)
            .field("connectivity", &self.connectivity)
            .field("devices", &self.devices)
            .field("nameservers", &self.nameservers)
            .field(
                "netlink_handle",
                &self.netlink_handle.as_ref().map(|_| "..."),
            )
            .finish()
    }
}

impl AppState {
    /// Get the shared netlink handle. Panics if not initialized (always set after startup).
    pub fn handle(&self) -> &rtnetlink::Handle {
        self.netlink_handle
            .as_ref()
            .expect("netlink handle not initialized")
    }

    /// Recompute global NM state based on device states and connectivity.
    /// Leaves `connectivity` untouched while staying in a gateway tier (`SITE`/`GLOBAL`),
    /// so an HTTP probe result isn't clobbered by unrelated device/route changes.
    pub fn recompute_global_state(&mut self) {
        let was_gateway = mapping::state_has_gateway(self.global_state);

        self.global_state = match mapping::deduce_route_tier(&self.devices) {
            mapping::RouteTier::Disconnected => {
                self.connectivity = mapping::nm_connectivity::NONE;
                mapping::nm_state::DISCONNECTED
            }
            mapping::RouteTier::Local => {
                self.connectivity = mapping::nm_connectivity::NONE;
                mapping::nm_state::CONNECTED_LOCAL
            }
            mapping::RouteTier::HasGateway if !self.connectivity_check_enabled => {
                self.connectivity = mapping::nm_connectivity::FULL;
                mapping::nm_state::CONNECTED_GLOBAL
            }
            mapping::RouteTier::HasGateway if was_gateway => {
                mapping::gateway_state_for_connectivity(self.connectivity)
            }
            mapping::RouteTier::HasGateway => {
                self.connectivity = mapping::nm_connectivity::UNKNOWN;
                mapping::nm_state::CONNECTED_SITE
            }
        };
    }

    /// The device NM reports as primary: must have a gateway by lowest metric then ifindex.
    pub fn primary_device(&self) -> Option<&DeviceInfo> {
        self.devices
            .values()
            .filter(|d| d.nm_state == mapping::nm_device_state::ACTIVATED && d.has_gateway())
            .min_by_key(|d| (d.best_metric(), d.ifindex))
    }

    /// The active device expected to become primary once it gets a gateway
    /// (`ActivatingConnection`). None if some device is already primary.
    pub fn activating_device(&self) -> Option<&DeviceInfo> {
        if self.primary_device().is_some() {
            return None;
        }
        self.devices
            .values()
            .filter(|d| d.nm_state == mapping::nm_device_state::ACTIVATED)
            .min_by_key(|d| d.ifindex)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub ifindex: i32,
    pub name: String,
    pub device_type: u32,
    pub nm_state: u32,
    pub hw_address: String,
    pub link_flags: u32,
    pub ipv4_addrs: Vec<AddrInfo<Ipv4Addr>>,
    pub ipv6_addrs: Vec<AddrInfo<Ipv6Addr>>,
    pub gateway4: Option<Ipv4Addr>,
    pub gateway6: Option<Ipv6Addr>,
    pub metric4: Option<u32>,
    pub metric6: Option<u32>,
}

impl DeviceInfo {
    pub fn new(ifindex: i32, name: String) -> Self {
        Self {
            ifindex,
            name,
            device_type: mapping::nm_device_type::ETHERNET,
            nm_state: mapping::nm_device_state::UNKNOWN,
            hw_address: String::new(),
            link_flags: 0,
            ipv4_addrs: Vec::new(),
            ipv6_addrs: Vec::new(),
            gateway4: None,
            gateway6: None,
            metric4: None,
            metric6: None,
        }
    }

    /// Lowest (best) default route metric across address families, for primary-device tie-breaking.
    pub fn best_metric(&self) -> u32 {
        [self.metric4, self.metric6]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(0)
    }

    pub fn carrier(&self) -> bool {
        use crate::mapping::netlink_flags;
        (self.link_flags & netlink_flags::IFF_RUNNING) != 0
            || (self.link_flags & netlink_flags::IFF_LOWER_UP) != 0
    }

    pub fn speed(&self) -> u32 {
        if !self.carrier() {
            return 0;
        }
        std::fs::read_to_string(format!("/sys/class/net/{}/speed", self.name))
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .map(|v| if v < 0 { 0 } else { v as u32 })
            .unwrap_or(0)
    }

    fn has_ip_address(&self) -> bool {
        !self.ipv4_addrs.is_empty() || !self.ipv6_addrs.is_empty()
    }

    pub fn has_gateway(&self) -> bool {
        self.gateway4.is_some() || self.gateway6.is_some()
    }

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

    /// `user_activating` masks UNAVAILABLE with PREPARE to keep the toggle visible.
    pub fn update_state_on_link_change(
        &mut self,
        flags: u32,
        user_activating: bool,
    ) -> Option<(u32, u32)> {
        self.link_flags = flags;
        let old_state = self.nm_state;
        let has_ipv4 = !self.ipv4_addrs.is_empty();
        let has_ipv6 = !self.ipv6_addrs.is_empty();
        let mut new_state = mapping::netlink_flags_to_nm_device(flags, has_ipv4, has_ipv6);

        if user_activating && new_state == mapping::nm_device_state::UNAVAILABLE {
            new_state = mapping::nm_device_state::PREPARE;
        }

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
