pub mod api;
pub mod config;
#[cfg(feature = "dashboard")]
pub mod dashboard;
pub mod scrape;
pub mod storage;

use axum::Router;

/// The full HTTP surface: query API plus (if enabled) the dashboard.
pub fn router(state: api::AppState) -> Router {
    let router = api::router(state);
    #[cfg(feature = "dashboard")]
    let router = router.merge(dashboard::router());
    router
}
