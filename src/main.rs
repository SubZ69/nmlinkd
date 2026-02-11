mod mapping;
mod netlink;
mod nm;
mod state;

use tracing::{error, info};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),

    #[error("D-Bus fdo error: {0}")]
    Fdo(#[from] zbus::fdo::Error),

    #[error("Netlink error: {0}")]
    Rtnetlink(#[from] rtnetlink::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nmlinkd=info".parse().unwrap()),
        )
        .init();

    if let Err(e) = run().await {
        error!("fatal: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    info!("starting nmlinkd");

    let shared = state::new_shared_state();

    // Load initial state from kernel via netlink
    netlink::load_initial_state(&shared).await?;

    // Serve NetworkManager D-Bus API
    let nm_conn = nm::serve(shared.clone()).await?;
    info!("claimed org.freedesktop.NetworkManager on system bus");

    // Run netlink event loop
    netlink::monitor::run(nm_conn, shared).await
}
