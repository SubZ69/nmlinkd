use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::stream::StreamExt;
use netlink_packet_core::NetlinkPayload;
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::link::{LinkAttribute, LinkMessage};
use netlink_sys::AsyncSocket;
use rtnetlink::constants::{
    RTMGRP_IPV4_IFADDR, RTMGRP_IPV4_ROUTE, RTMGRP_IPV6_IFADDR, RTMGRP_IPV6_ROUTE, RTMGRP_LINK,
};
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};
use zbus::Connection;

use crate::Result;
use crate::mapping;
use crate::nm;
use crate::state::SharedState;

use super::queries;

const DEBOUNCE_DURATION: Duration = Duration::from_millis(50);

/// Accumulated netlink events during a debounce window.
#[derive(Default)]
struct PendingEvents {
    /// ifindexes that received NewAddress/DelAddress events.
    address_changed: HashSet<i32>,
    /// Whether any NewRoute/DelRoute was received.
    routes_changed: bool,
    /// NewLink messages, keyed by ifindex (last message wins for flag updates).
    new_links: HashMap<i32, LinkMessage>,
    /// DelLink messages, keyed by ifindex.
    del_links: HashMap<i32, LinkMessage>,
}

impl PendingEvents {
    fn is_empty(&self) -> bool {
        self.address_changed.is_empty()
            && !self.routes_changed
            && self.new_links.is_empty()
            && self.del_links.is_empty()
    }
}

/// Dispatch a netlink message into the pending events accumulator.
fn accumulate(msg: &RouteNetlinkMessage, pending: &mut PendingEvents) {
    match msg {
        RouteNetlinkMessage::NewAddress(addr_msg) | RouteNetlinkMessage::DelAddress(addr_msg) => {
            pending.address_changed.insert(addr_msg.header.index as i32);
        }
        RouteNetlinkMessage::NewRoute(_) | RouteNetlinkMessage::DelRoute(_) => {
            pending.routes_changed = true;
        }
        RouteNetlinkMessage::NewLink(link_msg) => {
            let ifindex = link_msg.header.index as i32;
            pending.new_links.insert(ifindex, link_msg.clone());
        }
        RouteNetlinkMessage::DelLink(link_msg) => {
            let ifindex = link_msg.header.index as i32;
            pending.del_links.insert(ifindex, link_msg.clone());
        }
        _ => {}
    }
}

/// Run the event loop: listen for netlink events.
pub async fn run(nm_conn: Connection, shared: SharedState) -> Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

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
        _ = sigterm.recv() => {
            info!("received SIGTERM, shutting down");
        }
    }

    Ok(())
}

/// Watch for netlink events (address/route/link changes) with debouncing.
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

    loop {
        let Some((msg, _)) = messages.next().await else {
            break;
        };

        let mut pending = PendingEvents::default();

        if let NetlinkPayload::InnerMessage(inner) = msg.payload {
            debug!("netlink message received: {:?}", inner);
            accumulate(&inner, &mut pending);
        }

        let deadline = Instant::now() + DEBOUNCE_DURATION;
        loop {
            tokio::select! {
                biased;
                Some((msg, _)) = messages.next() => {
                    if let NetlinkPayload::InnerMessage(inner) = msg.payload {
                        debug!("netlink message received: {:?}", inner);
                        accumulate(&inner, &mut pending);
                    }
                }
                () = sleep_until(deadline) => break,
            }
        }

        if !pending.is_empty() {
            process_batch(&nm_conn, &shared, pending).await;
        }
    }

    Ok(())
}

/// Process a batch of accumulated netlink events.
///
/// Order: DelLink → NewLink → Addresses → Routes, then emit D-Bus signals.
async fn process_batch(nm_conn: &Connection, shared: &SharedState, pending: PendingEvents) {
    debug!(
        del_links = pending.del_links.len(),
        new_links = pending.new_links.len(),
        address_changed = pending.address_changed.len(),
        routes_changed = pending.routes_changed,
        "processing debounced batch"
    );

    for link_msg in pending.del_links.values() {
        handle_del_link(nm_conn, shared, link_msg).await;
    }

    for link_msg in pending.new_links.values() {
        let _ = handle_new_link(nm_conn, shared, link_msg).await;
    }

    let mut ip_config_notify: HashSet<i32> = HashSet::new();

    if !pending.address_changed.is_empty() {
        let handle = shared.read().await.handle().clone();
        for &ifindex in &pending.address_changed {
            queries::reload_addresses_for(&handle, ifindex, shared).await;
        }
        queries::reload_nameservers(shared).await;

        let (device_changes, old_global, new_global) = {
            let mut state = shared.write().await;
            let old_global = state.global_state;
            let changes: Vec<_> = pending
                .address_changed
                .iter()
                .filter_map(|&ifindex| {
                    state
                        .devices
                        .get_mut(&ifindex)
                        .and_then(|dev| dev.update_state_on_ip_change())
                        .map(|(new_state, old_state)| (ifindex, new_state, old_state))
                })
                .collect();
            state.recompute_global_state();
            (changes, old_global, state.global_state)
        };

        ip_config_notify.extend(&pending.address_changed);

        for (ifindex, new_state, old_state) in device_changes {
            nm::signals::notify_device_state_changed(nm_conn, ifindex, new_state, old_state).await;
        }

        if old_global != new_global {
            nm::signals::notify_global_state_changed(nm_conn, shared, new_global).await;
        }
    }

    if pending.routes_changed {
        let handle = shared.read().await.handle().clone();
        queries::reload_gateways(&handle, shared).await;
        let global_state = {
            let mut state = shared.write().await;
            state.recompute_global_state();
            state.global_state
        };
        nm::signals::notify_global_state_changed(nm_conn, shared, global_state).await;

        let ifindexes: Vec<i32> = {
            let st = shared.read().await;
            st.devices.keys().copied().collect()
        };
        ip_config_notify.extend(ifindexes);
    }

    for ifindex in ip_config_notify {
        nm::signals::notify_device_ip_config_changed(nm_conn, ifindex).await;
    }
}

/// Handle NewLink: detect new devices (hotplug) or update existing device state.
///
/// Returns `Err(())` if the caller should `continue` (skip further processing),
/// i.e. when the interface is ignored or device registration fails.
async fn handle_new_link(
    nm_conn: &Connection,
    shared: &SharedState,
    link_msg: &LinkMessage,
) -> std::result::Result<(), ()> {
    let ifindex = link_msg.header.index as i32;
    let flags = link_msg.header.flags.bits();

    let is_new_device = {
        let state = shared.read().await;
        !state.devices.contains_key(&ifindex)
    };

    if is_new_device {
        let dev = super::device_from_link_msg(link_msg).ok_or(())?;
        info!(ifindex, iface = %dev.name, "new device detected");

        {
            let mut state = shared.write().await;
            state.devices.insert(ifindex, dev);
        }

        let handle = shared.read().await.handle().clone();
        queries::reload_addresses_for(&handle, ifindex, shared).await;
        queries::reload_gateways(&handle, shared).await;
        queries::reload_nameservers(shared).await;

        {
            let mut state = shared.write().await;
            if let Some(dev) = state.devices.get_mut(&ifindex) {
                let has_ipv4 = !dev.ipv4_addrs.is_empty();
                let has_ipv6 = !dev.ipv6_addrs.is_empty();
                dev.nm_state = mapping::netlink_flags_to_nm_device(flags, has_ipv4, has_ipv6);
            }
        }

        if let Err(e) = nm::register_device(nm_conn, ifindex, shared.clone()).await {
            warn!(ifindex, "failed to register device: {e}");
            return Err(());
        }

        nm::signals::notify_device_added(nm_conn, ifindex).await;
    } else {
        let mac = link_msg.attributes.iter().find_map(|attr| match attr {
            LinkAttribute::Address(bytes) => Some(queries::format_mac(bytes)),
            _ => None,
        });

        let state_change = {
            let mut state = shared.write().await;
            if let Some(dev) = state.devices.get_mut(&ifindex) {
                if let Some(m) = mac {
                    dev.hw_address = m;
                }

                if let Some((new_state, old_state)) = dev.update_state_on_link_change(flags) {
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
            nm::signals::notify_device_state_changed(nm_conn, ifindex, new_state, old_state).await;

            if old_global != new_global {
                debug!("global state changed: {} -> {}", old_global, new_global);
            }
            nm::signals::notify_global_state_changed(nm_conn, shared, new_global).await;
        }
    }

    Ok(())
}

/// Handle DelLink: unregister removed devices and update global state.
async fn handle_del_link(nm_conn: &Connection, shared: &SharedState, link_msg: &LinkMessage) {
    let ifindex = link_msg.header.index as i32;

    let device_type = {
        let state = shared.read().await;
        state.devices.get(&ifindex).map(|d| d.device_type)
    };

    let Some(device_type) = device_type else {
        return;
    };

    info!(ifindex, "device removed");

    if let Err(e) = nm::unregister_device(nm_conn, ifindex, device_type).await {
        warn!(ifindex, "failed to unregister device: {e}");
    }

    let old_global_state = {
        let mut state = shared.write().await;
        let old_global = state.global_state;
        state.devices.remove(&ifindex);
        state.recompute_global_state();
        old_global
    };

    nm::signals::notify_device_removed(nm_conn, ifindex).await;

    let new_global_state = shared.read().await.global_state;
    if old_global_state != new_global_state {
        nm::signals::notify_global_state_changed(nm_conn, shared, new_global_state).await;
    }
}
