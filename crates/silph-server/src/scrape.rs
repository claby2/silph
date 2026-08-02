use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use silph_core::{METRICS, MetricsResponse, RawSnapshot};

use crate::config::Target;
use crate::storage::Store;

#[derive(Debug, Default)]
pub struct HostState {
    /// Previous raw snapshot, used for counter-delta processing.
    pub last_raw: Option<RawSnapshot>,
    pub last_ok_ms: Option<i64>,
    pub last_error: Option<String>,
    /// Known instances per output series (e.g. mount points for `disk_used`),
    /// learned from scrapes; used by the query API to fan out instanced
    /// metrics. Rebuilt after restart on the first successful scrape.
    pub instances: BTreeMap<&'static str, BTreeSet<String>>,
}

/// How stale a host's last successful scrape may be, in scrape intervals,
/// before it's reported down; 2.5 tolerates one missed scrape without flapping.
const UP_THRESHOLD_INTERVALS: f64 = 2.5;

impl HostState {
    /// A host is up if its last successful scrape is recent enough.
    pub fn up(&self, now_ms: i64, scrape_interval: Duration) -> bool {
        let threshold_ms = (scrape_interval.as_millis() as f64 * UP_THRESHOLD_INTERVALS) as i64;
        self.last_ok_ms.is_some_and(|ok| now_ms - ok < threshold_ms)
    }
}

pub type Hosts = Arc<RwLock<BTreeMap<String, HostState>>>;

/// Current time in milliseconds since the Unix epoch. Signed because
/// timestamps get subtracted (staleness checks, bucket indexing) and unsigned
/// differences would underflow on clock skew; i64 also matches tsink's
/// DataPoint timestamp type, so no conversion at the storage boundary.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as i64
}

/// Scrape one target forever. Runs as its own tokio task.
pub async fn scrape_loop(
    target: Target,
    hosts: Hosts,
    store: Store,
    client: reqwest::Client,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match scrape_once(&target, &hosts, &store, &client).await {
            Ok(point_count) => {
                tracing::debug!(host = target.name, points = point_count, "scrape ok");
            }
            Err(e) => {
                tracing::warn!(host = target.name, error = %e, "scrape failed");
                let mut hosts = hosts.write().unwrap();
                hosts.entry(target.name.clone()).or_default().last_error = Some(e);
            }
        }
    }
}

pub async fn scrape_once(
    target: &Target,
    hosts: &Hosts,
    store: &Store,
    client: &reqwest::Client,
) -> Result<usize, String> {
    let url = format!("{}/metrics", target.url.trim_end_matches('/'));
    let mut request = client.get(&url);
    if let Some(token) = &target.token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("collector returned {}", response.status()));
    }
    let body: MetricsResponse = response.json().await.map_err(|e| e.to_string())?;

    let curr = RawSnapshot {
        ts_ms: now_ms(),
        values: body.values,
    };
    // Take (not clone) the previous snapshot; it's replaced below either way.
    let prev = {
        let mut hosts = hosts.write().unwrap();
        hosts
            .entry(target.name.clone())
            .or_default()
            .last_raw
            .take()
    };
    let points: Vec<silph_core::Point> = METRICS
        .iter()
        .flat_map(|m| m.process(prev.as_ref(), &curr))
        .collect();

    let point_count = points.len();
    if !points.is_empty() {
        store
            .insert(&target.name, curr.ts_ms, points.clone())
            .await
            .map_err(|e| format!("storage insert: {e}"))?;
    }

    let mut hosts = hosts.write().unwrap();
    let state = hosts.entry(target.name.clone()).or_default();
    for point in &points {
        if let Some(instance) = &point.instance {
            state
                .instances
                .entry(point.name)
                .or_default()
                .insert(instance.clone());
        }
    }
    state.last_ok_ms = Some(curr.ts_ms);
    state.last_error = None;
    state.last_raw = Some(curr);
    Ok(point_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_requires_recent_success() {
        let interval = Duration::from_secs(15);
        let mut state = HostState::default();
        assert!(!state.up(100_000, interval));
        state.last_ok_ms = Some(100_000);
        assert!(state.up(110_000, interval)); // 10s ago
        assert!(state.up(100_000 + 37_000, interval)); // just under 2.5 intervals
        assert!(!state.up(100_000 + 38_000, interval)); // past the threshold
    }
}
