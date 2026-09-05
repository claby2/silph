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
/// Refuse queries whose host x metric x instance fan-out would swamp the
/// storage layer (and any chart drawing the result).
const MAX_SERIES: usize = 512;

#[derive(Clone)]
pub struct AppState {
    pub hosts: Hosts,
    pub store: Store,
    pub scrape_interval: Duration,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/config", get(config))
        .route("/api/hosts", get(hosts))
        .route("/api/metrics", get(metrics))
        .route("/api/query", get(query))
        .with_state(state)
}

#[derive(Debug, Serialize)]
struct ConfigInfo {
    scrape_interval_ms: u128,
}

/// Server settings the dashboard needs; the client aligns its query step to
/// the scrape interval so downsample buckets are never narrower than the
/// sample cadence (which would render as spurious gaps).
async fn config(State(state): State<AppState>) -> Json<ConfigInfo> {
    Json(ConfigInfo {
        scrape_interval_ms: state.scrape_interval.as_millis(),
    })
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
    /// One or more host names, comma separated. Config validation rejects
    /// commas in target names so the split is unambiguous.
    host: String,
    /// One or more metric names, comma separated.
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
    metric: &'static str,
    host: String,
    /// Null for plain metrics; the instance (e.g. mount point) otherwise.
    instance: Option<String>,
    /// One entry per bucket; null where no data.
    values: Vec<Option<f64>>,
}

/// Splits a comma-separated parameter, trimming blanks and dropping repeats
/// while preserving the caller's order.
fn split_list(raw: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Range query for any combination of hosts and metrics over one shared time
/// axis. The dashboard draws a whole chart (several metrics across every
/// selected host) from a single call, so one panel is one request no matter
/// how many hosts are selected.
async fn query(
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<QueryResponse>, (StatusCode, String)> {
    let bad_request = |msg: String| (StatusCode::BAD_REQUEST, msg);
    let not_found = |msg: String| (StatusCode::NOT_FOUND, msg);

    let specs: Vec<&OutputSpec> = split_list(&params.metric)
        .into_iter()
        .map(|name| {
            METRICS
                .iter()
                .flat_map(|m| m.outputs())
                .find(|spec| spec.name == name)
                .ok_or_else(|| not_found(format!("unknown metric: {name}")))
        })
        .collect::<Result<_, _>>()?;
    if specs.is_empty() {
        return Err(bad_request("metric must name at least one metric".into()));
    }
    let host_names = split_list(&params.host);
    if host_names.is_empty() {
        return Err(bad_request("host must name at least one host".into()));
    }
    if params.step <= 0 {
        return Err(bad_request("step must be positive".into()));
    }
    if params.end <= params.start {
        return Err(bad_request("end must be after start".into()));
    }
    if (params.end - params.start) / params.step > MAX_BUCKETS {
        return Err(bad_request("too many buckets; increase step".into()));
    }

    // Resolve the full (metric, host, instance) fan-out up front, under one
    // lock, so the storage queries below need no access to host state.
    let targets: Vec<(&'static str, String, Option<String>)> = {
        let hosts = state.hosts.read().unwrap();
        let mut targets = Vec::new();
        for name in &host_names {
            let host = hosts
                .get(*name)
                .ok_or_else(|| not_found(format!("unknown host: {name}")))?;
            for spec in &specs {
                if spec.instanced {
                    let instances = host.instances.get(spec.name);
                    for instance in instances.into_iter().flatten() {
                        targets.push((spec.name, name.to_string(), Some(instance.clone())));
                    }
                } else {
                    targets.push((spec.name, name.to_string(), None));
                }
            }
        }
        targets
    };
    if targets.len() > MAX_SERIES {
        return Err(bad_request(format!(
            "query fans out into {} series; select fewer hosts or metrics",
            targets.len()
        )));
    }

    // Bucket timestamps aligned to epoch multiples of step, matching the
    // storage downsample bucket origin, so all series share one time axis.
    let t0 = params.start - params.start.rem_euclid(params.step);
    let t: Vec<i64> = (0..)
        .map(|i| t0 + i * params.step)
        .take_while(|ts| *ts < params.end)
        .collect();

    let mut series = Vec::with_capacity(targets.len());
    for (metric, host, instance) in targets {
        let points = state
            .store
            .query(
                metric,
                &host,
                instance.as_deref(),
                params.start,
                params.end,
                params.step,
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("query: {e}")))?;
        // A series with nothing in the window is omitted rather than sent as
        // a column of nulls: with several hosts selected those would pile up
        // as empty legend entries for metrics a host doesn't even collect.
        if points.is_empty() {
            continue;
        }
        let mut values: Vec<Option<f64>> = vec![None; t.len()];
        for (ts, value) in points {
            let index = (ts - t0) / params.step;
            if (0..t.len() as i64).contains(&index) {
                values[index as usize] = Some(value);
            }
        }
        series.push(Series {
            metric,
            host,
            instance,
            values,
        });
    }
    Ok(Json(QueryResponse { t, series }))
}
