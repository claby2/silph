//! The optional web dashboard: static assets compiled into the binary.

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse};
use axum::routing::get;

const JS: &str = "text/javascript";
const CSS: &str = "text/css";

/// Everything served under `/assets/`, by path suffix. Adding a dashboard
/// file means adding one line here.
const ASSETS: &[(&str, &str, &str)] = &[
    ("app.js", JS, include_str!("../assets/app.js")),
    ("chart.js", JS, include_str!("../assets/chart.js")),
    ("format.js", JS, include_str!("../assets/format.js")),
    ("state.js", JS, include_str!("../assets/state.js")),
    ("style.css", CSS, include_str!("../assets/style.css")),
    (
        "vendor/uplot.iife.min.js",
        JS,
        include_str!("../assets/vendor/uplot.iife.min.js"),
    ),
    (
        "vendor/uplot.min.css",
        CSS,
        include_str!("../assets/vendor/uplot.min.css"),
    ),
];

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn asset(Path(path): Path<String>) -> impl IntoResponse {
    match ASSETS.iter().find(|(name, _, _)| *name == path) {
        Some((_, content_type, body)) => {
            ([(header::CONTENT_TYPE, *content_type)], *body).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
