use std::time::Duration;

use tracing::debug;
use zbus::Connection;

use crate::mapping::{self, ProbeResult};
use crate::nm::signals::notify_global_state_changed;
use crate::state::SharedState;

pub const PROBE_URL: &str = "http://detectportal.firefox.com/success.txt";
const PROBE_TIMEOUT_SECS: u64 = 5;
const EXPECTED_BODY: &str = "success\n";
const PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Plain HTTP: captive portals intercept it, HTTPS would just fail the handshake.
pub async fn probe() -> ProbeResult {
    tokio::task::spawn_blocking(run)
        .await
        .unwrap_or(ProbeResult::Failed)
}

fn run() -> ProbeResult {
    let response = minreq::get(PROBE_URL)
        .with_timeout(PROBE_TIMEOUT_SECS)
        .with_follow_redirects(false)
        .send();

    match response {
        Ok(resp) if resp.status_code == 200 && resp.as_str().is_ok_and(|b| b == EXPECTED_BODY) => {
            ProbeResult::Full
        }
        Ok(resp) if resp.status_code == 200 || (300..400).contains(&resp.status_code) => {
            ProbeResult::Portal
        }
        Ok(resp) => {
            debug!(status = resp.status_code, "connectivity probe: unexpected status");
            ProbeResult::Failed
        }
        Err(e) => {
            debug!("connectivity probe request failed: {e}");
            ProbeResult::Failed
        }
    }
}

/// Probe and update `state.connectivity`/`state.global_state` (`SITE`/`GLOBAL`),
/// then re-emit the global state signal. No-op if there's no gateway to probe over.
pub async fn probe_and_notify(nm_conn: Connection, shared: SharedState) {
    if !mapping::state_has_gateway(shared.read().await.global_state) {
        return;
    }

    let connectivity = mapping::probe_result_to_connectivity(probe().await);

    let global_state = {
        let mut state = shared.write().await;
        if !mapping::state_has_gateway(state.global_state) {
            return;
        }
        state.connectivity = connectivity;
        state.global_state = mapping::gateway_state_for_connectivity(connectivity);
        state.global_state
    };

    notify_global_state_changed(&nm_conn, &shared, global_state).await;
}

/// Trigger an immediate probe when transitioning into a gateway tier (`SITE`/`GLOBAL`).
pub fn trigger_on_global_transition(
    nm_conn: &Connection,
    shared: &SharedState,
    old_global: u32,
    new_global: u32,
) {
    if !mapping::state_has_gateway(old_global) && mapping::state_has_gateway(new_global) {
        tokio::spawn(probe_and_notify(nm_conn.clone(), shared.clone()));
    }
}

/// Periodically re-probe while connected, to catch portals resolving or connectivity dropping.
pub async fn run_periodic(nm_conn: Connection, shared: SharedState) {
    let mut interval = tokio::time::interval(PROBE_INTERVAL);
    loop {
        interval.tick().await;
        probe_and_notify(nm_conn.clone(), shared.clone()).await;
    }
}
