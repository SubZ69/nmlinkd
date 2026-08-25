use serde::Deserialize;
use tracing::warn;

const CONFIG_PATH: &str = "/etc/nmlinkd/nmlinkd.conf";

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub connectivity: ConnectivityConfig,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct ConnectivityConfig {
    pub enabled: bool,
    pub uri: String,
    pub response: String,
    pub interval_secs: u64,
    pub timeout_secs: u64,
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            uri: "http://nmcheck.gnome.org/check_network_status.txt".to_owned(),
            response: "NetworkManager is online".to_owned(),
            interval_secs: 300,
            timeout_secs: 20,
        }
    }
}

pub fn load() -> Config {
    let Ok(contents) = std::fs::read_to_string(CONFIG_PATH) else {
        return Config::default();
    };
    match toml::from_str(&contents) {
        Ok(config) => config,
        Err(e) => {
            warn!("failed to parse {CONFIG_PATH}: {e}, using defaults");
            Config::default()
        }
    }
}
