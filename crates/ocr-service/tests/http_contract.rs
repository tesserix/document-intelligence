use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use ocr_service::{router, TrustedIdentity};
use ocr_store::PgJobStore;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

fn store_without_connection() -> PgJobStore {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    PgJobStore::new(pool)
}

#[tokio::test]
async fn health_is_public_and_does_not_probe_dependencies() {
    let response = router(store_without_connection())
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn job_routes_fail_closed_without_verified_identity() {
    let response = router(store_without_connection())
        .oneshot(
            Request::get("/v1/ocr/jobs/job_PRIVATE")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], "authentication_required");
    assert!(body["request_id"].as_str().is_some_and(|id| !id.is_empty()));
}

#[test]
fn trusted_identity_rejects_noncanonical_scope() {
    assert!(TrustedIdentity::new("prod/kora", "ten_ALPHA").is_err());
    assert!(TrustedIdentity::new("kora", "../../other").is_err());
    assert!(TrustedIdentity::new("kora", "ten_ALPHA").is_ok());
}
