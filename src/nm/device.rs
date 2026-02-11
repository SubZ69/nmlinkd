use tracing::warn;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedObjectPath;

use crate::mapping::{nm_device_state, nm_device_type};
use crate::netlink::queries;
use crate::state::{self, SharedState, SharedStateExt};

pub struct NmDevice {
    pub ifindex: i32,
    pub state: SharedState,
}

pub struct NmDeviceWired {
    pub ifindex: i32,
    pub state: SharedState,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wired")]
impl NmDeviceWired {
    #[zbus(property)]
    async fn hw_address(&self) -> String {
        self.state
            .with_device(self.ifindex, |d| d.hw_address.clone())
            .await
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn perm_hw_address(&self) -> String {
        self.hw_address().await
    }

    #[zbus(property)]
    fn speed(&self) -> u32 {
        1000
    }

    #[zbus(property)]
    fn carrier(&self) -> bool {
        true
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl NmDevice {
    #[zbus(property(emits_changed_signal = "false"))]
    async fn state(&self) -> u32 {
        self.state
            .with_device(self.ifindex, |d| d.nm_state)
            .await
            .unwrap_or(0)
    }

    #[zbus(property)]
    async fn state_reason(&self) -> (u32, u32) {
        let nm_state = self.state().await;
        (nm_state, 0) // reason 0 = NM_DEVICE_STATE_REASON_NONE
    }

    #[zbus(property)]
    async fn hw_address(&self) -> String {
        self.state
            .with_device(self.ifindex, |d| d.hw_address.clone())
            .await
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn interface(&self) -> String {
        self.state
            .with_device(self.ifindex, |d| d.name.clone())
            .await
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn ip_interface(&self) -> String {
        self.interface().await
    }

    #[zbus(property)]
    async fn device_type(&self) -> u32 {
        nm_device_type::ETHERNET
    }

    #[zbus(property)]
    async fn managed(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn real(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn autoconnect(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn available_connections(&self) -> Vec<OwnedObjectPath> {
        vec![state::settings_path(self.ifindex)]
    }

    #[zbus(property)]
    async fn active_connection(&self) -> OwnedObjectPath {
        let is_activated = self
            .state
            .with_device(self.ifindex, |d| d.nm_state >= nm_device_state::ACTIVATED)
            .await
            .unwrap_or(false);
        if is_activated {
            state::active_connection_path(self.ifindex)
        } else {
            state::root_path()
        }
    }

    #[zbus(property)]
    async fn ip4_config(&self) -> OwnedObjectPath {
        state::ip4_config_path(self.ifindex)
    }

    #[zbus(property)]
    async fn ip6_config(&self) -> OwnedObjectPath {
        state::ip6_config_path(self.ifindex)
    }

    async fn disconnect(&self) -> zbus::fdo::Result<()> {
        if let Err(e) = queries::link_set_down(self.ifindex).await {
            warn!(ifindex = self.ifindex, "disconnect failed: {e}");
            return Err(zbus::fdo::Error::Failed(format!(
                "Failed to disconnect: {e}"
            )));
        }
        Ok(())
    }

    #[zbus(signal)]
    pub async fn state_changed(
        emitter: &SignalEmitter<'_>,
        new_state: u32,
        old_state: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}
