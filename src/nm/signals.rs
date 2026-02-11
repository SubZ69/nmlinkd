use std::collections::HashMap;

use tracing::warn;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};

use crate::mapping::{nm_active_connection_state, nm_device_state};
use crate::state::{self, SharedState};

const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const NM_DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const NM_AC_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const NM_IP4_CONFIG_IFACE: &str = "org.freedesktop.NetworkManager.IP4Config";
const NM_IP6_CONFIG_IFACE: &str = "org.freedesktop.NetworkManager.IP6Config";

/// Emit a PropertiesChanged signal with a mix of changed and invalidated properties.
async fn emit_properties_changed(
    conn: &Connection,
    path: ObjectPath<'_>,
    interface: &str,
    changed: HashMap<&str, Value<'_>>,
    invalidated: &[&str],
) {
    let Some(sender) = conn.unique_name() else {
        warn!("no unique name on connection, cannot emit PropertiesChanged");
        return;
    };
    let Ok(msg) = zbus::message::Message::signal(
        path,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
    )
    .and_then(|b| b.sender(sender))
    .and_then(|b| b.build(&(interface, changed, invalidated))) else {
        warn!("failed to build PropertiesChanged message");
        return;
    };

    if let Err(e) = conn.send(&msg).await {
        warn!("failed to emit PropertiesChanged: {e}");
    }
}

/// Notify D-Bus clients that the global NM state changed.
/// Emits PropertiesChanged + StateChanged signal on the Manager.
pub async fn notify_global_state_changed(
    nm_conn: &Connection,
    shared: &SharedState,
    new_global_state: u32,
) {
    let Ok(path) = ObjectPath::try_from("/org/freedesktop/NetworkManager") else {
        return;
    };

    let iface_ref = nm_conn
        .object_server()
        .interface::<_, super::manager::NmManager>(path.clone())
        .await;

    let (connectivity, active_connections, primary_connection) = {
        let st = shared.read().await;
        let ac: Vec<OwnedObjectPath> = st
            .devices
            .values()
            .filter(|d| d.nm_state == crate::mapping::nm_device_state::ACTIVATED)
            .map(|d| state::active_connection_path(d.ifindex))
            .collect();
        let primary: OwnedObjectPath = st
            .devices
            .values()
            .find(|d| {
                d.nm_state == crate::mapping::nm_device_state::ACTIVATED
                    && (d.gateway4.is_some() || d.gateway6.is_some())
            })
            .map(|d| state::active_connection_path(d.ifindex))
            .unwrap_or_else(state::root_path);
        (st.connectivity, ac, primary)
    };

    let mut changed: HashMap<&str, Value> = HashMap::new();
    changed.insert("State", Value::U32(new_global_state));
    changed.insert("Connectivity", Value::U32(connectivity));
    changed.insert("ActiveConnections", Value::from(active_connections));
    changed.insert(
        "PrimaryConnection",
        Value::ObjectPath(primary_connection.into()),
    );
    emit_properties_changed(nm_conn, path.clone(), NM_IFACE, changed, &[]).await;

    if let Ok(iface) = iface_ref
        && let Err(e) =
            super::manager::NmManager::state_changed(iface.signal_emitter(), new_global_state).await
    {
        warn!("failed to emit Manager.StateChanged: {e}");
    }
}

/// Notify D-Bus clients that a device's state changed.
/// Emits PropertiesChanged + StateChanged signals on Device and ActiveConnection.
pub async fn notify_device_state_changed(
    nm_conn: &Connection,
    ifindex: i32,
    new_state: u32,
    old_state: u32,
) {
    let dev_path = state::device_path(ifindex);
    let ac_path = state::active_connection_path(ifindex);

    let active_conn_path = if new_state >= nm_device_state::ACTIVATED {
        state::active_connection_path(ifindex)
    } else {
        state::root_path()
    };

    if let Ok(path) = ObjectPath::try_from(dev_path.as_str()) {
        let mut changed: HashMap<&str, Value> = HashMap::new();
        changed.insert("State", Value::U32(new_state));
        changed.insert("StateReason", Value::from((new_state, 0u32)));
        changed.insert(
            "ActiveConnection",
            Value::ObjectPath(active_conn_path.into()),
        );
        emit_properties_changed(nm_conn, path, NM_DEVICE_IFACE, changed, &[]).await;
    }

    if let Ok(iface) = nm_conn
        .object_server()
        .interface::<_, super::device::NmDevice>(dev_path.as_ref())
        .await
        && let Err(e) = super::device::NmDevice::state_changed(
            iface.signal_emitter(),
            new_state,
            old_state,
            0, // NM_DEVICE_STATE_REASON_NONE
        )
        .await
    {
        warn!("failed to emit Device.StateChanged: {e}");
    }

    let ac_state = if new_state >= nm_device_state::ACTIVATED {
        nm_active_connection_state::ACTIVATED
    } else {
        nm_active_connection_state::DEACTIVATED
    };
    let old_ac_state = if old_state >= nm_device_state::ACTIVATED {
        nm_active_connection_state::ACTIVATED
    } else {
        nm_active_connection_state::DEACTIVATED
    };

    if let Ok(path) = ObjectPath::try_from(ac_path.as_str()) {
        let mut changed: HashMap<&str, Value> = HashMap::new();
        changed.insert("State", Value::U32(ac_state));
        emit_properties_changed(nm_conn, path, NM_AC_IFACE, changed, &[]).await;
    }

    if ac_state != old_ac_state
        && let Ok(iface) = nm_conn
            .object_server()
            .interface::<_, super::active_connection::NmActiveConnection>(ac_path.as_ref())
            .await
        && let Err(e) = super::active_connection::NmActiveConnection::state_changed(
            iface.signal_emitter(),
            ac_state,
            0, // reason
        )
        .await
    {
        warn!("failed to emit ActiveConnection.StateChanged: {e}");
    }
}

/// Notify D-Bus clients that IP4Config properties changed (addresses, gateway, DNS).
pub async fn notify_ip4_config_changed(nm_conn: &Connection, ifindex: i32) {
    let path = state::ip4_config_path(ifindex);
    if let Ok(obj_path) = ObjectPath::try_from(path.as_str()) {
        // Invalidate all IP config properties to force clients to re-read them
        let changed: HashMap<&str, Value> = HashMap::new();
        let invalidated = &["AddressData", "Gateway", "NameserverData"];
        emit_properties_changed(nm_conn, obj_path, NM_IP4_CONFIG_IFACE, changed, invalidated).await;
    }
}

/// Notify D-Bus clients that IP6Config properties changed (addresses, gateway).
pub async fn notify_ip6_config_changed(nm_conn: &Connection, ifindex: i32) {
    let path = state::ip6_config_path(ifindex);
    if let Ok(obj_path) = ObjectPath::try_from(path.as_str()) {
        let changed: HashMap<&str, Value> = HashMap::new();
        let invalidated = &["AddressData", "Gateway"];
        emit_properties_changed(nm_conn, obj_path, NM_IP6_CONFIG_IFACE, changed, invalidated).await;
    }
}

/// Notify D-Bus clients that a device was added (hotplug).
pub async fn notify_device_added(nm_conn: &Connection, ifindex: i32) {
    let dev_path = state::device_path(ifindex);

    if let Ok(path) = ObjectPath::try_from("/org/freedesktop/NetworkManager")
        && let Ok(iface) = nm_conn
            .object_server()
            .interface::<_, super::manager::NmManager>(path)
            .await
        && let Err(e) =
            super::manager::NmManager::device_added(iface.signal_emitter(), dev_path.into()).await
    {
        warn!("failed to emit Manager.DeviceAdded: {e}");
    }
}

/// Notify D-Bus clients that a device was removed (hotplug).
pub async fn notify_device_removed(nm_conn: &Connection, ifindex: i32) {
    let dev_path = state::device_path(ifindex);

    if let Ok(path) = ObjectPath::try_from("/org/freedesktop/NetworkManager")
        && let Ok(iface) = nm_conn
            .object_server()
            .interface::<_, super::manager::NmManager>(path)
            .await
        && let Err(e) =
            super::manager::NmManager::device_removed(iface.signal_emitter(), dev_path.into()).await
    {
        warn!("failed to emit Manager.DeviceRemoved: {e}");
    }
}
