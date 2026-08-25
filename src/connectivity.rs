use std::time::Duration;

use tracing::debug;
use zbus::Connection;

use crate::mapping::{self, ProbeResult};
use crate::nm::signals::notify_global_state_changed;
use crate::state::SharedState;

/// Plain HTTP: captive portals intercept it, HTTPS would just fail the handshake.
pub async fn probe(url: String, expected_body: String, timeout_secs: u64) -> ProbeResult {
    tokio::task::spawn_blocking(move || run(&url, &expected_body, timeout_secs))
        .await
        .unwrap_or(ProbeResult::Failed)
}

/// Matches NM's own `X-NetworkManager-Status: online` short-circuit check.
fn has_online_header(resp: &minreq::Response) -> bool {
    resp.headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("x-networkmanager-status") && v.trim() == "online")
}

fn run(url: &str, expected_body: &str, timeout_secs: u64) -> ProbeResult {
    let response = minreq::get(url)
        .with_timeout(timeout_secs)
        .with_follow_redirects(false)
        .send();

    match response {
        Ok(resp) if has_online_header(&resp) => ProbeResult::Full,
        Ok(resp) if expected_body.is_empty() && resp.status_code == 204 => ProbeResult::Full,
        Ok(resp)
            if expected_body.is_empty()
                && resp.status_code == 200
                && resp.as_str().is_ok_and(str::is_empty) =>
        {
            ProbeResult::Full
        }
        Ok(resp) if resp.status_code == 200 && resp.as_str().is_ok_and(|b| b == expected_body) => {
            ProbeResult::Full
        }
        Ok(resp) if resp.status_code == 200 || (300..400).contains(&resp.status_code) => {
            ProbeResult::Portal
        }
        Ok(resp) => {
            debug!(
                status = resp.status_code,
                "connectivity probe: unexpected status"
            );
            ProbeResult::Failed
        }
        Err(e) => {
            debug!("connectivity probe request failed: {e}");
            ProbeResult::Failed
        }
    }
}

/// Probe, update state, re-emit. No-op without a gateway or if checks are disabled.
pub async fn probe_and_notify(nm_conn: Connection, shared: SharedState) {
    let (url, expected_body, timeout_secs) = {
        let state = shared.read().await;
        if !state.connectivity_check_enabled || !mapping::state_has_gateway(state.global_state) {
            return;
        }
        (
            state.connectivity_check_uri.clone(),
            state.connectivity_check_response.clone(),
            state.connectivity_check_timeout_secs,
        )
    };

    let connectivity =
        mapping::probe_result_to_connectivity(probe(url, expected_body, timeout_secs).await);

    let global_state = {
        let mut state = shared.write().await;
        if !state.connectivity_check_enabled || !mapping::state_has_gateway(state.global_state) {
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
pub async fn run_periodic(nm_conn: Connection, shared: SharedState, interval: Duration) {
    let mut interval = tokio::time::interval(interval);
    loop {
        interval.tick().await;
        probe_and_notify(nm_conn.clone(), shared.clone()).await;
    }
}
