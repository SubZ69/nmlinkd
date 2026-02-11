use std::collections::HashMap;
use std::fmt::Display;
use std::net::{Ipv4Addr, Ipv6Addr};

use zbus::zvariant::{OwnedValue, Str, Value};

use crate::state::{AddrInfo, SharedState, SharedStateExt};

fn address_data_from<A: Display>(addrs: &[AddrInfo<A>]) -> Vec<HashMap<String, OwnedValue>> {
    addrs
        .iter()
        .map(|a| {
            let mut map = HashMap::new();
            map.insert(
                "address".to_string(),
                Value::from(Str::from(a.address.to_string()))
                    .try_into()
                    .unwrap(),
            );
            map.insert(
                "prefix".to_string(),
                Value::from(a.prefix_len as u32).try_into().unwrap(),
            );
            map
        })
        .collect()
}

macro_rules! define_ip_config {
    (
        $struct_name:ident,
        $iface:literal,
        addrs: $addrs_field:ident,
        gateway: $gateway_field:ident,
        nameserver_property: { $($ns_body:tt)* }
    ) => {
        pub struct $struct_name {
            pub ifindex: i32,
            pub state: SharedState,
        }

        #[zbus::interface(name = $iface)]
        impl $struct_name {
            #[zbus(property)]
            async fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
                self.state
                    .with_device(self.ifindex, |d| address_data_from(&d.$addrs_field))
                    .await
                    .unwrap_or_default()
            }

            #[zbus(property)]
            async fn gateway(&self) -> String {
                self.state
                    .with_device(self.ifindex, |d| {
                        d.$gateway_field.map(|g| g.to_string())
                    })
                    .await
                    .flatten()
                    .unwrap_or_default()
            }

            $($ns_body)*
        }
    };
}

define_ip_config!(
    NmIp4Config,
    "org.freedesktop.NetworkManager.IP4Config",
    addrs: ipv4_addrs,
    gateway: gateway4,
    nameserver_property: {
        #[zbus(property)]
        async fn nameserver_data(&self) -> Vec<HashMap<String, OwnedValue>> {
            self.state
                .with_state(|s| {
                    s.nameservers
                        .iter()
                        .filter(|ns| ns.parse::<Ipv4Addr>().is_ok())
                        .map(|ns| {
                            let mut map = HashMap::new();
                            map.insert(
                                "address".to_string(),
                                Value::from(Str::from(ns.as_str())).try_into().unwrap(),
                            );
                            map
                        })
                        .collect()
                })
                .await
        }
    }
);

define_ip_config!(
    NmIp6Config,
    "org.freedesktop.NetworkManager.IP6Config",
    addrs: ipv6_addrs,
    gateway: gateway6,
    nameserver_property: {
        #[zbus(property)]
        async fn nameservers(&self) -> Vec<Vec<u8>> {
            self.state
                .with_state(|s| {
                    s.nameservers
                        .iter()
                        .filter_map(|ns| ns.parse::<Ipv6Addr>().ok())
                        .map(|ip| ip.octets().to_vec())
                        .collect()
                })
                .await
        }
    }
);
