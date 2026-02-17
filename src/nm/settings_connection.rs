use std::collections::HashMap;
use zbus::zvariant::Value;

use crate::mapping::{self, nm_device_type};
use crate::state::{self, SharedState, SharedStateExt};

pub struct NmSettingsConnection {
    pub ifindex: i32,
    pub state: SharedState,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl NmSettingsConnection {
    async fn get_settings(&self) -> HashMap<String, HashMap<String, Value<'_>>> {
        let mut settings = HashMap::new();
        let mut connection = HashMap::new();
        let (iface_name, device_type) = self
            .state
            .with_device(self.ifindex, |d| (d.name.clone(), d.device_type))
            .await
            .unwrap_or_else(|| (format!("eth{}", self.ifindex), nm_device_type::ETHERNET));

        let conn_type = mapping::device_type_to_connection_type(device_type);

        let uuid = state::connection_uuid(&iface_name);
        connection.insert("id".to_string(), Value::new(iface_name.clone()));
        connection.insert("uuid".to_string(), Value::new(uuid));
        connection.insert("type".to_string(), Value::new(conn_type));
        connection.insert("interface-name".to_string(), Value::new(iface_name));

        settings.insert("connection".to_string(), connection);

        // Empty 802-3-ethernet section — required for libnm's
        // nm_device_filter_connections() to consider this connection
        // compatible with an ethernet device.
        if device_type != nm_device_type::WIREGUARD {
            settings.insert("802-3-ethernet".to_string(), HashMap::new());
        }

        settings
    }

    #[zbus(property)]
    fn unsaved(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn flags(&self) -> u32 {
        0 // NM_SETTINGS_CONNECTION_FLAG_NONE
    }

    #[zbus(property)]
    fn filename(&self) -> String {
        String::new()
    }
}
