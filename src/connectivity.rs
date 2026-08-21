use crate::mapping::ProbeResult;

const PROBE_URL: &str = "http://detectportal.firefox.com/success.txt";
const PROBE_TIMEOUT_SECS: u64 = 5;
const EXPECTED_BODY: &str = "success\n";

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
        _ => ProbeResult::Failed,
    }
}
