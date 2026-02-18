pub mod active_connection;
pub mod device;
pub mod ip_config;
pub mod manager;
pub mod settings;
pub mod settings_connection;
pub mod signals;

use tracing::{error, info};
use zbus::Connection;
use zbus::connection::Builder;
use zbus::zvariant::OwnedObjectPath;

use crate::Result;
use crate::mapping::nm_device_type;
use crate::state::{self, SharedState};

use active_connection::NmActiveConnection;
use device::{NmDevice, NmDeviceWireGuard, NmDeviceWired};
use ip_config::{NmIp4Config, NmIp6Config};
use manager::NmManager;
use settings::NmSettings;
use settings_connection::NmSettingsConnection;

struct DevicePaths {
    dev: OwnedObjectPath,
    ip4: OwnedObjectPath,
    ip6: OwnedObjectPath,
    active: OwnedObjectPath,
    settings: OwnedObjectPath,
}

impl DevicePaths {
    fn new(ifindex: i32) -> Self {
        Self {
            dev: state::device_path(ifindex),
            ip4: state::ip4_config_path(ifindex),
            ip6: state::ip6_config_path(ifindex),
            active: state::active_connection_path(ifindex),
            settings: state::settings_path(ifindex),
        }
    }
}

/// Build the NM D-Bus server: register all interfaces and claim the bus name.
pub async fn serve(shared: SharedState) -> Result<Connection> {
    let state = shared.read().await;
    let devices: Vec<(i32, u32)> = state
        .devices
        .values()
        .map(|d| (d.ifindex, d.device_type))
        .collect();
    drop(state);

    let device_paths: Vec<(i32, u32, DevicePaths)> = devices
        .iter()
        .map(|&(idx, dt)| (idx, dt, DevicePaths::new(idx)))
        .collect();

    let mut builder = Builder::system()?
        .name("org.freedesktop.NetworkManager")?
        .serve_at("/org/freedesktop", zbus::fdo::ObjectManager)?
        .serve_at(
            "/org/freedesktop/NetworkManager",
            NmManager {
                state: shared.clone(),
            },
        )?
        .serve_at(
            "/org/freedesktop/NetworkManager/Settings",
            NmSettings {
                state: shared.clone(),
            },
        )?;

    for (ifindex, device_type, p) in &device_paths {
        info!(ifindex, path = %p.dev, "registering device");

        builder = builder.serve_at(
            &p.dev,
            NmDevice {
                ifindex: *ifindex,
                state: shared.clone(),
            },
        )?;

        if *device_type == nm_device_type::WIREGUARD {
            builder = builder.serve_at(&p.dev, NmDeviceWireGuard)?;
        } else {
            builder = builder.serve_at(
                &p.dev,
                NmDeviceWired {
                    ifindex: *ifindex,
                    state: shared.clone(),
                },
            )?;
        }

        builder = builder
            .serve_at(
                &p.ip4,
                NmIp4Config {
                    ifindex: *ifindex,
                    state: shared.clone(),
                },
            )?
            .serve_at(
                &p.ip6,
                NmIp6Config {
                    ifindex: *ifindex,
                    state: shared.clone(),
                },
            )?
            .serve_at(
                &p.active,
                NmActiveConnection {
                    ifindex: *ifindex,
                    state: shared.clone(),
                },
            )?
            .serve_at(
                &p.settings,
                NmSettingsConnection {
                    ifindex: *ifindex,
                    state: shared.clone(),
                },
            )?;
    }

    let conn = builder.build().await.inspect_err(|_| {
        error!(
            "failed to claim org.freedesktop.NetworkManager bus name — is NetworkManager running?"
        );
    })?;

    Ok(conn)
}

/// Register all D-Bus interfaces for a single device (hotplug support).
pub async fn register_device(conn: &Connection, ifindex: i32, state: SharedState) -> Result<()> {
    let p = DevicePaths::new(ifindex);
    let obj = conn.object_server();

    let device_type = state
        .read()
        .await
        .devices
        .get(&ifindex)
        .map(|d| d.device_type)
        .unwrap_or(nm_device_type::ETHERNET);

    info!(ifindex, path = %p.dev, "registering device");

    obj.at(
        &p.dev,
        NmDevice {
            ifindex,
            state: state.clone(),
        },
    )
    .await?;

    if device_type == nm_device_type::WIREGUARD {
        obj.at(&p.dev, NmDeviceWireGuard).await?;
    } else {
        obj.at(
            &p.dev,
            NmDeviceWired {
                ifindex,
                state: state.clone(),
            },
        )
        .await?;
    }

    obj.at(
        &p.ip4,
        NmIp4Config {
            ifindex,
            state: state.clone(),
        },
    )
    .await?;
    obj.at(
        &p.ip6,
        NmIp6Config {
            ifindex,
            state: state.clone(),
        },
    )
    .await?;
    obj.at(
        &p.active,
        NmActiveConnection {
            ifindex,
            state: state.clone(),
        },
    )
    .await?;
    obj.at(&p.settings, NmSettingsConnection { ifindex, state })
        .await?;

    Ok(())
}

/// Unregister all D-Bus interfaces for a device (hotplug removal).
pub async fn unregister_device(conn: &Connection, ifindex: i32, device_type: u32) -> Result<()> {
    let p = DevicePaths::new(ifindex);
    let obj = conn.object_server();

    info!(ifindex, path = %p.dev, "unregistering device");

    obj.remove::<NmDevice, _>(&p.dev).await?;
    if device_type == nm_device_type::WIREGUARD {
        obj.remove::<NmDeviceWireGuard, _>(&p.dev).await?;
    } else {
        obj.remove::<NmDeviceWired, _>(&p.dev).await?;
    }
    obj.remove::<NmIp4Config, _>(&p.ip4).await?;
    obj.remove::<NmIp6Config, _>(&p.ip6).await?;
    obj.remove::<NmActiveConnection, _>(&p.active).await?;
    obj.remove::<NmSettingsConnection, _>(&p.settings).await?;

    Ok(())
}
