use futures::stream::StreamExt;
use netlink_packet_core::NetlinkPayload;
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::link::LinkAttribute;
use netlink_sys::AsyncSocket;
use rtnetlink::constants::{
    RTMGRP_IPV4_IFADDR, RTMGRP_IPV4_ROUTE, RTMGRP_IPV6_IFADDR, RTMGRP_IPV6_ROUTE, RTMGRP_LINK,
};
use tracing::{debug, info, warn};
use zbus::Connection;

use crate::Result;
use crate::mapping;
use crate::nm;
use crate::state::SharedState;

use super::queries;

/// Run the event loop: listen for netlink events.
pub async fn run(nm_conn: Connection, shared: SharedState) -> Result<()> {
    tokio::select! {
        result = watch_netlink(nm_conn, shared) => {
            match result {
                Ok(()) => warn!("netlink watcher exited normally"),
                Err(e) => warn!("netlink watcher error: {}", e),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("received SIGINT, shutting down");
        }
    }

    Ok(())
}

/// Watch for netlink events (address/route/link changes).
async fn watch_netlink(nm_conn: Connection, shared: SharedState) -> Result<()> {
    let (mut conn, _handle, mut messages) = rtnetlink::new_connection()?;

    let mgroup_flags = RTMGRP_LINK
        | RTMGRP_IPV4_IFADDR
        | RTMGRP_IPV4_ROUTE
        | RTMGRP_IPV6_IFADDR
        | RTMGRP_IPV6_ROUTE;

    let addr = netlink_sys::SocketAddr::new(0, mgroup_flags);
    conn.socket_mut().socket_mut().bind(&addr)?;

    tokio::spawn(conn);

    debug!("netlink watcher started, groups mask: 0x{:x}", mgroup_flags);

    while let Some((msg, _)) = messages.next().await {
        let NetlinkPayload::InnerMessage(inner) = msg.payload else {
            continue;
        };

        debug!("netlink message received: {:?}", inner);

        match &inner {
            RouteNetlinkMessage::NewAddress(addr_msg)
            | RouteNetlinkMessage::DelAddress(addr_msg) => {
                let ifindex = addr_msg.header.index as i32;

                queries::reload_addresses_for(ifindex, &shared).await;
                queries::reload_nameservers(&shared).await;

                let state_change = {
                    let mut state = shared.write().await;
                    state
                        .devices
                        .get_mut(&ifindex)
                        .and_then(|dev| dev.update_state_on_ip_change())
                        .map(|(new_state, old_state)| {
                            let old_global = state.global_state;
                            state.recompute_global_state();
                            (new_state, old_state, state.global_state, old_global)
                        })
                };

                nm::signals::notify_ip4_config_changed(&nm_conn, ifindex).await;
                nm::signals::notify_ip6_config_changed(&nm_conn, ifindex).await;

                if let Some((new_state, old_state, new_global, old_global)) = state_change {
                    nm::signals::notify_device_state_changed(
                        &nm_conn, ifindex, new_state, old_state,
                    )
                    .await;
                    if old_global != new_global {
                        nm::signals::notify_global_state_changed(&nm_conn, &shared, new_global)
                            .await;
                    }
                }
            }
            RouteNetlinkMessage::NewRoute(_) | RouteNetlinkMessage::DelRoute(_) => {
                queries::reload_gateways(&shared).await;
                let global_state = {
                    let mut state = shared.write().await;
                    state.recompute_global_state();
                    state.global_state
                };
                nm::signals::notify_global_state_changed(&nm_conn, &shared, global_state).await;

                let ifindexes: Vec<i32> = {
                    let st = shared.read().await;
                    st.devices.keys().copied().collect()
                };
                for ifindex in ifindexes {
                    nm::signals::notify_ip4_config_changed(&nm_conn, ifindex).await;
                    nm::signals::notify_ip6_config_changed(&nm_conn, ifindex).await;
                }
            }
            RouteNetlinkMessage::NewLink(link_msg) => {
                let ifindex = link_msg.header.index as i32;
                let mut name = None;
                let mut mac = None;
                for attr in &link_msg.attributes {
                    match attr {
                        LinkAttribute::IfName(n) => name = Some(n.clone()),
                        LinkAttribute::Address(bytes) => {
                            mac = Some(queries::format_mac(bytes));
                        }
                        _ => {}
                    }
                }

                if let Some(ref iface_name) = name {
                    if super::should_ignore_interface(iface_name) {
                        continue;
                    }
                }

                let flags = link_msg.header.flags.bits();

                let is_new_device = {
                    let state = shared.read().await;
                    !state.devices.contains_key(&ifindex)
                };

                if is_new_device {
                    if let Some(iface_name) = name {
                        info!(ifindex, iface = %iface_name, "new device detected");

                        let mut dev = crate::state::DeviceInfo::new(ifindex, iface_name.clone());
                        if let Some(m) = mac {
                            dev.hw_address = m;
                        }
                        dev.nm_state = mapping::netlink_flags_to_nm_device(flags, false, false);

                        {
                            let mut state = shared.write().await;
                            state.devices.insert(ifindex, dev);
                        }

                        queries::reload_addresses_for(ifindex, &shared).await;
                        queries::reload_gateways(&shared).await;
                        queries::reload_nameservers(&shared).await;

                        {
                            let mut state = shared.write().await;
                            if let Some(dev) = state.devices.get_mut(&ifindex) {
                                let has_ipv4 = !dev.ipv4_addrs.is_empty();
                                let has_ipv6 = !dev.ipv6_addrs.is_empty();
                                dev.nm_state =
                                    mapping::netlink_flags_to_nm_device(flags, has_ipv4, has_ipv6);
                            }
                        }

                        if let Err(e) = nm::register_device(&nm_conn, ifindex, shared.clone()).await
                        {
                            warn!(ifindex, "failed to register device: {e}");
                            continue;
                        }

                        nm::signals::notify_device_added(&nm_conn, ifindex).await;
                    }
                } else {
                    let state_change = {
                        let mut state = shared.write().await;
                        if let Some(dev) = state.devices.get_mut(&ifindex) {
                            if let Some(m) = mac {
                                dev.hw_address = m;
                            }

                            if let Some((new_state, old_state)) =
                                dev.update_state_on_link_change(flags)
                            {
                                let iface_name = dev.name.clone();
                                info!(
                                    iface = %iface_name,
                                    old_state,
                                    new_state,
                                    flags,
                                    "link state changed"
                                );

                                let old_global = state.global_state;
                                state.recompute_global_state();
                                Some((new_state, old_state, state.global_state, old_global))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };

                    if let Some((new_state, old_state, new_global, old_global)) = state_change {
                        nm::signals::notify_device_state_changed(
                            &nm_conn, ifindex, new_state, old_state,
                        )
                        .await;

                        if old_global != new_global {
                            debug!("global state changed: {} -> {}", old_global, new_global);
                        }
                        nm::signals::notify_global_state_changed(&nm_conn, &shared, new_global)
                            .await;
                    }
                }
            }
            RouteNetlinkMessage::DelLink(link_msg) => {
                let ifindex = link_msg.header.index as i32;

                let device_exists = {
                    let state = shared.read().await;
                    state.devices.contains_key(&ifindex)
                };

                if device_exists {
                    info!(ifindex, "device removed");

                    if let Err(e) = nm::unregister_device(&nm_conn, ifindex).await {
                        warn!(ifindex, "failed to unregister device: {e}");
                    }

                    let old_global_state = {
                        let mut state = shared.write().await;
                        let old_global = state.global_state;
                        state.devices.remove(&ifindex);
                        state.recompute_global_state();
                        old_global
                    };

                    nm::signals::notify_device_removed(&nm_conn, ifindex).await;

                    let new_global_state = shared.read().await.global_state;
                    if old_global_state != new_global_state {
                        nm::signals::notify_global_state_changed(
                            &nm_conn,
                            &shared,
                            new_global_state,
                        )
                        .await;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}
