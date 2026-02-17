use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedObjectPath;

use crate::mapping::{self, nm_active_connection_state, nm_device_state};
use crate::state::{self, SharedState, SharedStateExt};

pub struct NmActiveConnection {
    pub ifindex: i32,
    pub state: SharedState,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl NmActiveConnection {
    #[zbus(property(emits_changed_signal = "false"))]
    async fn state(&self) -> u32 {
        self.state
            .with_device(self.ifindex, |d| {
                if d.nm_state >= nm_device_state::ACTIVATED {
                    nm_active_connection_state::ACTIVATED
                } else {
                    nm_active_connection_state::DEACTIVATED
                }
            })
            .await
            .unwrap_or(nm_active_connection_state::UNKNOWN)
    }

    #[zbus(property)]
    async fn default(&self) -> bool {
        self.state
            .with_device(self.ifindex, |d| d.gateway4.is_some())
            .await
            .unwrap_or(false)
    }

    #[zbus(property)]
    async fn default6(&self) -> bool {
        self.state
            .with_device(self.ifindex, |d| d.gateway6.is_some())
            .await
            .unwrap_or(false)
    }

    #[zbus(property)]
    async fn r#type(&self) -> String {
        self.state
            .with_device(self.ifindex, |d| {
                mapping::device_type_to_connection_type(d.device_type).to_string()
            })
            .await
            .unwrap_or_else(|| "802-3-ethernet".to_string())
    }

    #[zbus(property)]
    async fn id(&self) -> String {
        self.state
            .with_device(self.ifindex, |d| d.name.clone())
            .await
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn uuid(&self) -> String {
        let name = self.id().await;
        state::connection_uuid(&name)
    }

    #[zbus(property)]
    async fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![state::device_path(self.ifindex)]
    }

    #[zbus(property)]
    fn state_flags(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn vpn(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn controller(&self) -> OwnedObjectPath {
        state::root_path()
    }

    #[zbus(property)]
    fn master(&self) -> OwnedObjectPath {
        state::root_path()
    }

    #[zbus(property)]
    fn ip4_config(&self) -> OwnedObjectPath {
        state::ip4_config_path(self.ifindex)
    }

    #[zbus(property)]
    fn ip6_config(&self) -> OwnedObjectPath {
        state::ip6_config_path(self.ifindex)
    }

    #[zbus(property)]
    fn connection(&self) -> OwnedObjectPath {
        state::settings_path(self.ifindex)
    }

    #[zbus(signal)]
    pub async fn state_changed(
        emitter: &SignalEmitter<'_>,
        state: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}
