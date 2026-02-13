use zbus::zvariant::OwnedObjectPath;

use crate::state;

pub struct NmSettings {
    pub state: state::SharedState,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
impl NmSettings {
    async fn list_connections(&self) -> Vec<OwnedObjectPath> {
        let state = self.state.read().await;
        state
            .devices
            .keys()
            .map(|&idx| self::state::settings_path(idx))
            .collect()
    }

    async fn load_connections(&self, _filenames: Vec<String>) -> (bool, Vec<String>) {
        (true, Vec::new())
    }

    #[zbus(property)]
    async fn connections(&self) -> Vec<OwnedObjectPath> {
        self.list_connections().await
    }

    #[zbus(property)]
    fn can_modify(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn hostname(&self) -> String {
        tokio::fs::read_to_string("/etc/hostname")
            .await
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}
