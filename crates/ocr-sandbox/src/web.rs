use crate::config::{MAX_BYTES, MAX_PAGES, MAX_PIXELS};
use crate::App;
use axum::{response::Html, routing::get, Json, Router};
use std::sync::Arc;
pub fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/limits.json", get(|| async { Json(serde_json::json!({"max_bytes":MAX_BYTES,"max_pages":MAX_PAGES,"max_pixels":MAX_PIXELS})) }))
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.css",
            get(|| async {
                (
                    [("content-type", "text/css; charset=utf-8")],
                    include_str!("../web/app.css"),
                )
            }),
        )
        .route(
            "/app.mjs",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../web/app.mjs"),
                )
            }),
        )
        .route(
            "/status.mjs",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../web/status.mjs"),
                )
            }),
        )
        .route(
            "/document.mjs",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../web/document.mjs"),
                )
            }),
        )
        .route(
            "/score.mjs",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../web/score.mjs"),
                )
            }),
        )
        .route(
            "/samples/manifest.json",
            get(|| async {
                (
                    [("content-type", "application/json")],
                    include_str!("../../../benchmarks/v1/quick-manifest.json"),
                )
            }),
        )
        .route(
            "/samples/nutrition_clear.png",
            get(|| async {
                (
                    [("content-type", "image/png")],
                    include_bytes!("../../../benchmarks/v1/images/clear.png").as_slice(),
                )
            }),
        )
        .route(
            "/samples/receipt_clear.png",
            get(|| async {
                (
                    [("content-type", "image/png")],
                    include_bytes!("../../../benchmarks/v1/images/receipt_clear.png").as_slice(),
                )
            }),
        )
        .route(
            "/samples/no_text.png",
            get(|| async {
                (
                    [("content-type", "image/png")],
                    include_bytes!("../../../benchmarks/v1/images/no_text.png").as_slice(),
                )
            }),
        )
}
