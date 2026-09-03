use axum::{
    body::Body,
    http::{Request, StatusCode},
    Extension,
};
use http_body_util::BodyExt;
use ocr_service::{
    router, router_with_result_reader, ResultArtifactReader, ResultReadFuture, TrustedIdentity,
};
use ocr_store::{PgJobStore, StoredResultLocator};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

fn store_without_connection() -> PgJobStore {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(20))
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    PgJobStore::new(pool)
}

struct StaticResultReader(Vec<u8>);

impl ResultArtifactReader for StaticResultReader {
    fn read<'a>(
        &'a self,
        _locator: &'a StoredResultLocator,
        _maximum_bytes: usize,
    ) -> ResultReadFuture<'a> {
        let bytes = self.0.clone();
        Box::pin(async move { Ok(bytes) })
    }
}

#[tokio::test]
async fn readiness_fails_when_the_database_is_unavailable() {
    let response = router(store_without_connection())
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], "service_unavailable");
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

#[tokio::test]
async fn result_route_rejects_an_invalid_job_identifier_without_database_work() {
    let response = router(store_without_connection())
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_ALPHA").unwrap(),
        ))
        .oneshot(
            Request::get("/v1/ocr/jobs/not-a-job/result")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], "job_not_found");
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
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let admin_pool = PgPoolOptions::new().connect(&admin_url).await.unwrap();
    sqlx::query(
        "delete from ocr_outbox where job_id in \
         (select job_id from ocr_jobs where product_id = 'kora' and tenant_id = 'ten_HTTP' \
          and idempotency_key = 'http-contract-request')",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query(
        "delete from ocr_jobs where product_id = 'kora' and tenant_id = 'ten_HTTP' \
         and idempotency_key = 'http-contract-request'",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
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

    let pending_result = application
        .clone()
        .layer(Extension(TrustedIdentity::new("kora", "ten_HTTP").unwrap()))
        .oneshot(
            Request::get(format!("/v1/ocr/jobs/{job_id}/result"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pending_result.status(), StatusCode::CONFLICT);
    let pending_body: Value = serde_json::from_slice(
        &pending_result
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(pending_body["code"], "result_not_ready");

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

    let cancel = application
        .clone()
        .layer(Extension(TrustedIdentity::new("kora", "ten_HTTP").unwrap()))
        .oneshot(
            Request::post(format!("/v1/ocr/jobs/{job_id}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel.status(), StatusCode::OK);
    let cancelled: Value =
        serde_json::from_slice(&cancel.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(cancelled["status"], "cancelling");

    let cancel_replay = application
        .clone()
        .layer(Extension(TrustedIdentity::new("kora", "ten_HTTP").unwrap()))
        .oneshot(
            Request::post(format!("/v1/ocr/jobs/{job_id}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel_replay.status(), StatusCode::OK);

    let foreign = application
        .clone()
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

    let foreign_cancel = application
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_OTHER").unwrap(),
        ))
        .oneshot(
            Request::post(format!("/v1/ocr/jobs/{job_id}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign_cancel.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn ready_result_is_verified_and_returned_without_exposing_its_storage_locator() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let admin_pool = PgPoolOptions::new().connect(&admin_url).await.unwrap();
    sqlx::query("delete from ocr_results where job_id = 'job_HTTPRESULT'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_outbox where job_id = 'job_HTTPRESULT'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_jobs where job_id = 'job_HTTPRESULT'")
        .execute(&admin_pool)
        .await
        .unwrap();
    let body = format!(
        r#"{{"schema_version":"1.0","document_id":"doc_HTTPRESULT","document_version":"sha256:{}","content_trust":"untrusted","fields":{{}}}}"#,
        "a".repeat(64)
    )
    .into_bytes();
    let object_digest = format!("sha256:{:x}", Sha256::digest(&body));

    sqlx::query(
        "insert into ocr_jobs \
         (job_id, tenant_id, product_id, idempotency_key, request_digest, status) \
         values ('job_HTTPRESULT', 'ten_HTTPRESULT', 'kora', 'result-http', $1, 'completed')",
    )
    .bind(format!("sha256:{}", "c".repeat(64)))
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into ocr_results \
         (job_id, product_id, tenant_id, document_id, document_version, object_bucket, \
          object_name, object_generation, object_digest, content_length) \
         values ('job_HTTPRESULT', 'kora', 'ten_HTTPRESULT', 'doc_HTTPRESULT', $1, \
          'ocr-dev-results-au', 'products/kora/tenants/ten_HTTPRESULT/results/job_HTTPRESULT/v1.json', \
          7, $2, $3)",
    )
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(&object_digest)
    .bind(i64::try_from(body.len()).unwrap())
    .execute(&admin_pool)
    .await
    .unwrap();

    let store = PgJobStore::new(pool);
    let application =
        router_with_result_reader(store.clone(), Arc::new(StaticResultReader(body.clone())));
    let foreign = application
        .clone()
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_OTHER").unwrap(),
        ))
        .oneshot(
            Request::get("/v1/ocr/jobs/job_HTTPRESULT/result")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

    let corrupted = router_with_result_reader(store, Arc::new(StaticResultReader(b"{}".to_vec())))
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_HTTPRESULT").unwrap(),
        ))
        .oneshot(
            Request::get("/v1/ocr/jobs/job_HTTPRESULT/result")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(corrupted.status(), StatusCode::SERVICE_UNAVAILABLE);

    let response = application
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_HTTPRESULT").unwrap(),
        ))
        .oneshot(
            Request::get("/v1/ocr/jobs/job_HTTPRESULT/result")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let result: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(result["document_id"], "doc_HTTPRESULT");
    assert_eq!(result["content_trust"], "untrusted");
    assert!(result.get("object_bucket").is_none());
    assert!(result.get("object_name").is_none());
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
