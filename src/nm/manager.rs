use std::collections::HashMap;

use tracing::warn;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedObjectPath;

use crate::mapping::{self, nm_device_state};
use crate::netlink::queries;
use crate::state::{self, SharedState};

pub struct NmManager {
    pub state: SharedState,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl NmManager {
    #[zbus(property(emits_changed_signal = "false"))]
    async fn state(&self) -> u32 {
        self.state.read().await.global_state
    }

    #[zbus(property)]
    async fn connectivity(&self) -> u32 {
        self.state.read().await.connectivity
    }

    #[zbus(property)]
    async fn version(&self) -> String {
        "1.52.0".to_owned()
    }

    #[zbus(property)]
    async fn networking_enabled(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn devices(&self) -> Vec<OwnedObjectPath> {
        self.device_paths().await
    }

    #[zbus(property)]
    async fn active_connections(&self) -> Vec<OwnedObjectPath> {
        self.active_connection_paths().await
    }

    #[zbus(property)]
    async fn primary_connection(&self) -> OwnedObjectPath {
        let state = self.state.read().await;
        for dev in state.devices.values() {
            if dev.nm_state >= nm_device_state::ACTIVATED && dev.has_gateway() {
                return state::active_connection_path(dev.ifindex);
            }
        }
        state::root_path()
    }

    #[zbus(property)]
    async fn primary_connection_type(&self) -> String {
        let state = self.state.read().await;
        state
            .devices
            .values()
            .find(|dev| dev.nm_state >= nm_device_state::ACTIVATED && dev.has_gateway())
            .map(|dev| mapping::device_type_to_connection_type(dev.device_type).to_string())
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn metered(&self) -> u32 {
        4 // NM_METERED_GUESS_NO
    }

    async fn get_devices(&self) -> Vec<OwnedObjectPath> {
        self.device_paths().await
    }

    async fn get_all_devices(&self) -> Vec<OwnedObjectPath> {
        self.device_paths().await
    }

    async fn get_permissions(&self) -> HashMap<String, String> {
        let mut perms = HashMap::new();
        perms.insert(
            "org.freedesktop.NetworkManager.network-control".to_string(),
            "yes".to_string(),
        );
        for key in [
            "org.freedesktop.NetworkManager.checkpoint-rollback",
            "org.freedesktop.NetworkManager.enable-disable-connectivity-check",
            "org.freedesktop.NetworkManager.enable-disable-network",
            "org.freedesktop.NetworkManager.enable-disable-statistics",
            "org.freedesktop.NetworkManager.enable-disable-wifi",
            "org.freedesktop.NetworkManager.enable-disable-wimax",
            "org.freedesktop.NetworkManager.enable-disable-wwan",
            "org.freedesktop.NetworkManager.reload",
            "org.freedesktop.NetworkManager.settings.modify.global-dns",
            "org.freedesktop.NetworkManager.settings.modify.hostname",
            "org.freedesktop.NetworkManager.settings.modify.own",
            "org.freedesktop.NetworkManager.settings.modify.system",
            "org.freedesktop.NetworkManager.sleep-wake",
            "org.freedesktop.NetworkManager.wifi.scan",
            "org.freedesktop.NetworkManager.wifi.share.open",
            "org.freedesktop.NetworkManager.wifi.share.protected",
        ] {
            perms.insert(key.to_string(), "no".to_string());
        }
        perms
    }

    async fn add_and_activate_connection(
        &self,
        _connection: HashMap<String, HashMap<String, zbus::zvariant::Value<'_>>>,
        device: OwnedObjectPath,
        _specific_object: OwnedObjectPath,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let ifindex = self.resolve_device_ifindex(&device).await?;
        let handle = self.state.read().await.handle().clone();

        if let Err(e) = queries::link_set_up(&handle, ifindex).await {
            warn!(ifindex, "add_and_activate failed: {e}");
            return Err(zbus::fdo::Error::Failed(format!("Failed to activate: {e}")));
        }

        Ok((
            state::settings_path(ifindex),
            state::active_connection_path(ifindex),
        ))
    }

    async fn activate_connection(
        &self,
        connection: OwnedObjectPath,
        device: OwnedObjectPath,
        _specific_object: OwnedObjectPath,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        // For VPNs, GNOME passes device="/", resolve via connection path instead
        let ifindex = if device.as_str() == "/" {
            self.resolve_ifindex_from_path(&connection).await?
        } else {
            self.resolve_device_ifindex(&device).await?
        };
        let handle = self.state.read().await.handle().clone();

        if let Err(e) = queries::link_set_up(&handle, ifindex).await {
            warn!(ifindex, "activate connection failed: {e}");
            return Err(zbus::fdo::Error::Failed(format!("Failed to activate: {e}")));
        }

        Ok(state::active_connection_path(ifindex))
    }

    async fn deactivate_connection(
        &self,
        active_connection: OwnedObjectPath,
    ) -> zbus::fdo::Result<()> {
        let ifindex = self.resolve_ifindex_from_path(&active_connection).await?;
        let handle = {
            let mut state = self.state.write().await;
            state.user_disconnect_pending.insert(ifindex);
            state.handle().clone()
        };

        if let Err(e) = queries::link_set_down(&handle, ifindex).await {
            warn!(ifindex, "deactivate connection failed: {e}");
            return Err(zbus::fdo::Error::Failed(format!(
                "Failed to deactivate: {e}"
            )));
        }

        Ok(())
    }

    async fn get_device_by_ip_iface(&self, iface: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        let state = self.state.read().await;
        for dev in state.devices.values() {
            if dev.name == iface {
                return Ok(state::device_path(dev.ifindex));
            }
        }
        Err(zbus::fdo::Error::UnknownObject(format!(
            "No device for interface {iface}"
        )))
    }

    #[zbus(signal)]
    pub async fn state_changed(emitter: &SignalEmitter<'_>, state: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn device_added(
        emitter: &SignalEmitter<'_>,
        device_path: OwnedObjectPath,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn device_removed(
        emitter: &SignalEmitter<'_>,
        device_path: OwnedObjectPath,
    ) -> zbus::Result<()>;
}

impl NmManager {
    /// Parse ifindex from a D-Bus path like /org/.../Devices/{ifindex} and validate the device exists.
    async fn resolve_device_ifindex(&self, device: &OwnedObjectPath) -> zbus::fdo::Result<i32> {
        self.resolve_ifindex_from_path(device).await
    }

    /// Parse ifindex from any NM object path (Devices, ActiveConnection, Settings, etc.).
    async fn resolve_ifindex_from_path(&self, path: &OwnedObjectPath) -> zbus::fdo::Result<i32> {
        let ifindex: i32 = path
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| zbus::fdo::Error::UnknownObject(format!("Invalid path {path}")))?;
        let state = self.state.read().await;
        if state.devices.contains_key(&ifindex) {
            Ok(ifindex)
        } else {
            Err(zbus::fdo::Error::UnknownObject(format!(
                "No device for path {path}"
            )))
        }
    }

    async fn device_paths(&self) -> Vec<OwnedObjectPath> {
        let state = self.state.read().await;
        state
            .devices
            .keys()
            .map(|&idx| state::device_path(idx))
            .collect()
    }

    async fn active_connection_paths(&self) -> Vec<OwnedObjectPath> {
        let state = self.state.read().await;
        state
            .devices
            .values()
            .filter(|d| d.nm_state >= nm_device_state::ACTIVATED)
            .map(|d| state::active_connection_path(d.ifindex))
            .collect()
    }
}
