use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use silph_core::{METRICS, OutputSpec};

use crate::scrape::{Hosts, now_ms};
use crate::storage::Store;

/// Refuse queries that would produce absurd numbers of buckets.
const MAX_BUCKETS: i64 = 100_000;

#[derive(Clone)]
pub struct AppState {
    pub hosts: Hosts,
    pub store: Store,
    pub scrape_interval: Duration,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/hosts", get(hosts))
        .route("/api/metrics", get(metrics))
        .route("/api/query", get(query))
        .with_state(state)
}

#[derive(Debug, Serialize)]
struct HostSummary {
    name: String,
    up: bool,
    last_scrape_ms: Option<i64>,
    error: Option<String>,
}

async fn hosts(State(state): State<AppState>) -> Json<Vec<HostSummary>> {
    let now = now_ms();
    let hosts = state.hosts.read().unwrap();
    Json(
        hosts
            .iter()
            .map(|(name, host)| HostSummary {
                name: name.clone(),
                up: host.up(now, state.scrape_interval),
                last_scrape_ms: host.last_ok_ms,
                error: host.last_error.clone(),
            })
            .collect(),
    )
}

#[derive(Debug, Serialize)]
struct MetricInfo {
    name: &'static str,
    unit: &'static str,
    instanced: bool,
}

async fn metrics() -> Json<Vec<MetricInfo>> {
    Json(
        METRICS
            .iter()
            .flat_map(|m| m.outputs())
            .map(|spec| MetricInfo {
                name: spec.name,
                unit: spec.unit.as_str(),
                instanced: spec.instanced,
            })
            .collect(),
    )
}

#[derive(Debug, Deserialize)]
struct QueryParams {
    host: String,
    metric: String,
    /// Milliseconds since the Unix epoch.
    start: i64,
    end: i64,
    /// Downsample bucket width in milliseconds.
    step: i64,
}

#[derive(Debug, Serialize)]
struct QueryResponse {
    /// Bucket timestamps (ms), shared by all series.
    t: Vec<i64>,
    series: Vec<Series>,
}

#[derive(Debug, Serialize)]
struct Series {
    /// Null for plain metrics; the instance (e.g. mount point) otherwise.
    instance: Option<String>,
    /// One entry per bucket; null where no data.
    values: Vec<Option<f64>>,
}

async fn query(
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<QueryResponse>, (StatusCode, String)> {
    let bad_request = |msg: &str| (StatusCode::BAD_REQUEST, msg.to_string());
    let spec: &OutputSpec = METRICS
        .iter()
        .flat_map(|m| m.outputs())
        .find(|spec| spec.name == params.metric)
        .ok_or((
            StatusCode::NOT_FOUND,
            format!("unknown metric: {}", params.metric),
        ))?;
    if params.step <= 0 {
        return Err(bad_request("step must be positive"));
    }
    if params.end <= params.start {
        return Err(bad_request("end must be after start"));
    }
    if (params.end - params.start) / params.step > MAX_BUCKETS {
        return Err(bad_request("too many buckets; increase step"));
    }

    // Instanced metrics fan out into one series per known instance.
    let instances: Vec<Option<String>> = {
        let hosts = state.hosts.read().unwrap();
        let host = hosts.get(&params.host).ok_or((
            StatusCode::NOT_FOUND,
            format!("unknown host: {}", params.host),
        ))?;
        if spec.instanced {
            host.instances
                .get(params.metric.as_str())
                .map(|set| set.iter().cloned().map(Some).collect())
                .unwrap_or_default()
        } else {
            vec![None]
        }
    };

    // Bucket timestamps aligned to epoch multiples of step, matching tsink's
    // default downsample bucket origin, so all series share one time axis.
    let t0 = params.start - params.start.rem_euclid(params.step);
    let t: Vec<i64> = (0..)
        .map(|i| t0 + i * params.step)
        .take_while(|ts| *ts < params.end)
        .collect();

    let mut series = Vec::with_capacity(instances.len());
    for instance in instances {
        let points = state
            .store
            .query(
                &params.metric,
                &params.host,
                instance.as_deref(),
                params.start,
                params.end,
                params.step,
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("query: {e}")))?;
        let mut values: Vec<Option<f64>> = vec![None; t.len()];
        for (ts, value) in points {
            let index = (ts - t0) / params.step;
            if (0..t.len() as i64).contains(&index) {
                values[index as usize] = Some(value);
            }
        }
        series.push(Series { instance, values });
    }
    Ok(Json(QueryResponse { t, series }))
}
