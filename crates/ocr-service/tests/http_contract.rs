use axum::{
    body::Body,
    http::{Request, StatusCode},
    Extension,
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

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn create_replay_read_and_cross_tenant_visibility_are_end_to_end() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let application = router(PgJobStore::new(pool));
    let body = r#"{"source":{"upload_id":"upl_HTTPTEST"},"document_type":"auto"}"#;

    let create = application
        .clone()
        .layer(Extension(TrustedIdentity::new("kora", "ten_HTTP").unwrap()))
        .oneshot(
            Request::post("/v1/ocr/jobs")
                .header("content-type", "application/json")
                .header("idempotency-key", "http-contract-request")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::ACCEPTED);
    let created: Value =
        serde_json::from_slice(&create.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let job_id = created["job_id"].as_str().unwrap();
    assert_eq!(created["status"], "accepted");
    assert_eq!(created["status_url"], format!("/v1/ocr/jobs/{job_id}"));
    assert_eq!(
        created["result_url"],
        format!("/v1/ocr/jobs/{job_id}/result")
    );
    assert!(created["created_at"].as_str().is_some());

    let replay = application
        .clone()
        .layer(Extension(TrustedIdentity::new("kora", "ten_HTTP").unwrap()))
        .oneshot(
            Request::post("/v1/ocr/jobs")
                .header("content-type", "application/json")
                .header("idempotency-key", "http-contract-request")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let replayed: Value =
        serde_json::from_slice(&replay.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(replayed["job_id"], created["job_id"]);
    assert_eq!(replayed["created_at"], created["created_at"]);

    let foreign = application
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_OTHER").unwrap(),
        ))
        .oneshot(
            Request::get(format!("/v1/ocr/jobs/{job_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_requires_an_idempotency_key_before_database_work() {
    let identity = TrustedIdentity::new("kora", "ten_ALPHA").unwrap();
    let response = router(store_without_connection())
        .layer(Extension(identity))
        .oneshot(
            Request::post("/v1/ocr/jobs")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"source":{"upload_id":"upl_TEST"},"document_type":"auto"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], "idempotency_key_required");
}

#[tokio::test]
async fn create_rejects_unknown_command_fields() {
    let identity = TrustedIdentity::new("kora", "ten_ALPHA").unwrap();
    let response = router(store_without_connection())
        .layer(Extension(identity))
        .oneshot(
            Request::post("/v1/ocr/jobs")
                .header("content-type", "application/json")
                .header("idempotency-key", "request-http-1")
                .body(Body::from(
                    r#"{"source":{"upload_id":"upl_TEST"},"document_type":"auto","tenant_id":"ten_ATTACKER"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], "invalid_request");
}
