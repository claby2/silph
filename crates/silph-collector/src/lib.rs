pub mod config;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Json, Response};
use axum::routing::get;
use silph_core::{CollectConfig, METRICS, MetricsResponse};

#[derive(Clone)]
struct AppState {
    token: Arc<str>,
    collect_cfg: Arc<CollectConfig>,
}

/// Build the collector's HTTP router. Factored out of `main` so the server's
/// integration tests can run a collector in-process.
pub fn router(token: &str, collect_cfg: CollectConfig) -> Router {
    let state = AppState {
        token: token.into(),
        collect_cfg: Arc::new(collect_cfg),
    };
    Router::new()
        .route("/metrics", get(metrics))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}

async fn require_bearer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !constant_time_eq(presented.as_bytes(), state.token.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Length is not secret; contents are compared without early exit.
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn metrics(State(state): State<AppState>) -> Json<MetricsResponse> {
    let mut response = MetricsResponse::default();
    for metric in METRICS {
        match metric.collect(&state.collect_cfg) {
            Ok(values) => {
                response
                    .values
                    .extend(values.into_iter().map(|(k, v)| (k.to_string(), v)));
            }
            // Omit a failing category rather than failing the scrape; the flat
            // response tolerates missing keys.
            Err(e) => tracing::warn!(category = metric.category(), error = %e, "collect failed"),
        }
    }
    Json(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
        assert!(constant_time_eq(b"", b""));
    }
}
