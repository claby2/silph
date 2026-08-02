//! The optional web dashboard: static assets compiled into the binary.

use axum::Router;
use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(app_js))
        .route("/assets/style.css", get(style_css))
        .route("/assets/vendor/uplot.iife.min.js", get(uplot_js))
        .route("/assets/vendor/uplot.min.css", get(uplot_css))
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn app_js() -> impl IntoResponse {
    js(include_str!("../assets/app.js"))
}

async fn style_css() -> impl IntoResponse {
    css(include_str!("../assets/style.css"))
}

async fn uplot_js() -> impl IntoResponse {
    js(include_str!("../assets/vendor/uplot.iife.min.js"))
}

async fn uplot_css() -> impl IntoResponse {
    css(include_str!("../assets/vendor/uplot.min.css"))
}

fn js(body: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript")], body)
}

fn css(body: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], body)
}
