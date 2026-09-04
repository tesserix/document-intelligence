use axum::{
    body::Body,
    http::{Request, StatusCode},
    Extension,
};
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use ocr_domain::{ProductId, TenantId, UploadId};
use ocr_service::{
    router, router_with_job_status_cache, router_with_result_reader, router_with_upload_issuer,
    router_with_upload_services, with_workload_identity, CacheOperationError, CachePolicy,
    CacheReadFuture, CacheRecord, CacheScope, CacheWriteFuture, IssuedUpload, JobStatusCache,
    ResultArtifactReader, ResultReadFuture, StoredUpload, TrustedIdentity,
    UploadArtifactReadFuture, UploadArtifactReader, UploadIntentIssuer, UploadIssueFuture,
    VerifiedUploadArtifact, WorkloadIdentityVerifier,
};
use ocr_store::{
    AcceptUpload, ClaimUploadInspection, ParserInspectionMetadata, PgJobStore, StoredResultLocator,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use std::{
    future::pending,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tower::ServiceExt;

type HmacSha256 = Hmac<Sha256>;

fn sign_workload_identity(
    key_id: &str,
    tenant_id: &str,
    timestamp: i64,
    method: &str,
    path_and_query: &str,
) -> String {
    let mut mac = HmacSha256::new_from_slice(b"0123456789abcdef0123456789abcdef").unwrap();
    mac.update(
        format!("{key_id}\n{tenant_id}\n{timestamp}\n{method}\n{path_and_query}").as_bytes(),
    );
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn signed_workload_identity_allows_only_its_registered_product() {
    let verifier = WorkloadIdentityVerifier::new(
        [(
            "devai-v1",
            "devai",
            b"0123456789abcdef0123456789abcdef".as_slice(),
        )],
        Duration::from_secs(60),
    )
    .unwrap();
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let application = with_workload_identity(router(store_without_connection()), verifier);

    let response = application
        .oneshot(
            Request::get("/v1/ocr/jobs/job_UNKNOWN")
                .header("x-ocr-key-id", "devai-v1")
                .header("x-ocr-tenant-id", "ten_DEVAI")
                .header("x-ocr-timestamp", timestamp)
                .header(
                    "x-ocr-signature",
                    sign_workload_identity(
                        "devai-v1",
                        "ten_DEVAI",
                        timestamp,
                        "GET",
                        "/v1/ocr/jobs/job_UNKNOWN",
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn upload_status_uses_the_verified_scope_before_reading_storage_metadata() {
    let response = router(store_without_connection())
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_UPLOAD_STATUS").unwrap(),
        ))
        .oneshot(
            Request::get("/v1/ocr/uploads/upl_STATUS")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn workload_identity_rejects_signature_replay_on_a_different_route() {
    let verifier = WorkloadIdentityVerifier::new(
        [(
            "devai-v1",
            "devai",
            b"0123456789abcdef0123456789abcdef".as_slice(),
        )],
        Duration::from_secs(60),
    )
    .unwrap();
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let application = with_workload_identity(router(store_without_connection()), verifier);

    let response = application
        .oneshot(
            Request::post("/v1/ocr/jobs")
                .header("x-ocr-key-id", "devai-v1")
                .header("x-ocr-tenant-id", "ten_DEVAI")
                .header("x-ocr-timestamp", timestamp)
                .header(
                    "x-ocr-signature",
                    sign_workload_identity(
                        "devai-v1",
                        "ten_DEVAI",
                        timestamp,
                        "GET",
                        "/v1/ocr/jobs/job_UNKNOWN",
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn workload_identity_configuration_rejects_short_or_malformed_keys() {
    assert!(WorkloadIdentityVerifier::from_encoded_keys(
        "devai-v1=devai:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        Duration::from_secs(60),
    )
    .is_ok());
    assert!(WorkloadIdentityVerifier::from_encoded_keys(
        "devai-v1=devai:not-hex",
        Duration::from_secs(60),
    )
    .is_err());
    assert!(WorkloadIdentityVerifier::from_encoded_keys(
        "devai-v1=devai:0123456789abcdef",
        Duration::from_secs(60),
    )
    .is_err());
}

fn store_without_connection() -> PgJobStore {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(20))
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    PgJobStore::new(pool)
}

struct StaticResultReader(Vec<u8>);

struct StaticJobStatusCache {
    value: Result<Option<CacheRecord>, CacheOperationError>,
    reads: Arc<AtomicUsize>,
}

struct PendingJobStatusCache;

impl JobStatusCache for PendingJobStatusCache {
    fn get<'a>(&'a self, _scope: &'a CacheScope) -> CacheReadFuture<'a> {
        Box::pin(pending())
    }

    fn put<'a>(
        &'a self,
        _scope: &'a CacheScope,
        _record: &'a CacheRecord,
        _ttl: Duration,
    ) -> CacheWriteFuture<'a> {
        Box::pin(pending())
    }
}

struct RecordingJobStatusCache {
    writes: Arc<Mutex<Vec<(ocr_domain::JobState, Duration)>>>,
}

impl JobStatusCache for RecordingJobStatusCache {
    fn get<'a>(&'a self, _scope: &'a CacheScope) -> CacheReadFuture<'a> {
        Box::pin(async { Ok(None) })
    }

    fn put<'a>(
        &'a self,
        _scope: &'a CacheScope,
        record: &'a CacheRecord,
        ttl: Duration,
    ) -> CacheWriteFuture<'a> {
        self.writes.lock().unwrap().push((record.status(), ttl));
        Box::pin(async { Ok(()) })
    }
}

impl JobStatusCache for StaticJobStatusCache {
    fn get<'a>(&'a self, _scope: &'a CacheScope) -> CacheReadFuture<'a> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let value = self.value.clone();
        Box::pin(async move { value })
    }

    fn put<'a>(
        &'a self,
        _scope: &'a CacheScope,
        _record: &'a CacheRecord,
        _ttl: Duration,
    ) -> CacheWriteFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

fn cache_policy() -> CachePolicy {
    CachePolicy::new(
        Duration::from_secs(10),
        Duration::from_secs(300),
        Duration::from_millis(25),
        512,
    )
    .unwrap()
}

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

struct StaticUploadIssuer;

impl UploadIntentIssuer for StaticUploadIssuer {
    fn issue<'a>(&'a self, upload: &'a StoredUpload) -> UploadIssueFuture<'a> {
        let content_type = upload.expected_content_type.clone();
        Box::pin(async move {
            Ok(IssuedUpload {
                upload_url: "https://storage.googleapis.test/signed-upload".to_owned(),
                required_headers: [
                    ("content-type".to_owned(), content_type),
                    ("x-goog-if-generation-match".to_owned(), "0".to_owned()),
                ]
                .into_iter()
                .collect(),
            })
        })
    }
}

struct StaticUploadArtifactReader {
    artifact: VerifiedUploadArtifact,
    calls: Arc<AtomicUsize>,
}

impl UploadArtifactReader for StaticUploadArtifactReader {
    fn verify<'a>(&'a self, _upload: &'a StoredUpload) -> UploadArtifactReadFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let artifact = self.artifact.clone();
        Box::pin(async move { Ok(artifact) })
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
async fn job_status_cache_hit_is_accepted_only_for_the_verified_scope() {
    let scope = CacheScope::new(
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_CACHE").unwrap(),
        ocr_domain::JobId::new("job_CACHE").unwrap(),
    );
    let record = CacheRecord::new(
        &scope,
        ocr_domain::JobState::Processing,
        time::OffsetDateTime::from_unix_timestamp(1_725_000_000).unwrap(),
    );
    let reads = Arc::new(AtomicUsize::new(0));
    let application = router_with_job_status_cache(
        store_without_connection(),
        Arc::new(StaticJobStatusCache {
            value: Ok(Some(record)),
            reads: reads.clone(),
        }),
        cache_policy(),
    );

    let response = application
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_CACHE").unwrap(),
        ))
        .oneshot(
            Request::get("/v1/ocr/jobs/job_CACHE")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["job_id"], "job_CACHE");
    assert_eq!(body["status"], "processing");
}

#[tokio::test]
async fn foreign_scope_or_failed_cache_entry_falls_through_to_the_authoritative_store() {
    let foreign_scope = CacheScope::new(
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_FOREIGN").unwrap(),
        ocr_domain::JobId::new("job_CACHE").unwrap(),
    );
    let foreign_record = CacheRecord::new(
        &foreign_scope,
        ocr_domain::JobState::Completed,
        time::OffsetDateTime::from_unix_timestamp(1_725_000_000).unwrap(),
    );

    for value in [Ok(Some(foreign_record)), Err(CacheOperationError)] {
        let response = router_with_job_status_cache(
            store_without_connection(),
            Arc::new(StaticJobStatusCache {
                value,
                reads: Arc::new(AtomicUsize::new(0)),
            }),
            cache_policy(),
        )
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_CACHE").unwrap(),
        ))
        .oneshot(
            Request::get("/v1/ocr/jobs/job_CACHE")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

#[tokio::test(start_paused = true)]
async fn timed_out_cache_read_falls_through_without_hanging_the_request() {
    let response = router_with_job_status_cache(
        store_without_connection(),
        Arc::new(PendingJobStatusCache),
        cache_policy(),
    )
    .layer(Extension(
        TrustedIdentity::new("kora", "ten_CACHE").unwrap(),
    ))
    .oneshot(
        Request::get("/v1/ocr/jobs/job_CACHE")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
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

#[tokio::test]
async fn create_rejects_an_untrusted_webhook_destination_before_database_work() {
    let response = router(store_without_connection())
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_ALPHA").unwrap(),
        ))
        .oneshot(
            Request::post("/v1/ocr/jobs")
                .header("content-type", "application/json")
                .header("idempotency-key", "invalid-webhook")
                .body(Body::from(
                    r#"{"source":{"upload_id":"upl_ALPHA"},"document_type":"auto","webhook_subscription_id":"https://attacker.invalid/hook"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], "invalid_webhook_subscription_id");
}

#[tokio::test]
async fn create_rejects_unbounded_extraction_inputs_before_database_work() {
    let cases = [
        (
            "too many language hints",
            serde_json::json!({
                "source": {"upload_id": "upl_ALPHA"},
                "document_type": "auto",
                "language_hints": ["en", "fr", "de", "it", "es", "pt", "nl", "sv", "da"]
            }),
        ),
        (
            "duplicate language hints",
            serde_json::json!({
                "source": {"upload_id": "upl_ALPHA"},
                "document_type": "auto",
                "language_hints": ["en-AU", "en-AU"]
            }),
        ),
        (
            "invalid language hint",
            serde_json::json!({
                "source": {"upload_id": "upl_ALPHA"},
                "document_type": "auto",
                "language_hints": ["not_a_language"]
            }),
        ),
        (
            "empty extraction schema id",
            serde_json::json!({
                "source": {"upload_id": "upl_ALPHA"},
                "document_type": "auto",
                "extraction": {"schema_id": "", "schema_version": "1.0"}
            }),
        ),
    ];

    for (index, (name, body)) in cases.into_iter().enumerate() {
        let response = router(store_without_connection())
            .layer(Extension(
                TrustedIdentity::new("kora", "ten_ALPHA").unwrap(),
            ))
            .oneshot(
                Request::post("/v1/ocr/jobs")
                    .header("content-type", "application/json")
                    .header("idempotency-key", format!("invalid-bounds-{index}"))
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "case {name}");
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["code"], "invalid_job_request", "case {name}");
    }
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
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_HTTPTEST'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_HTTPTEST'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at, source_bucket, source_object_name, \
          source_object_generation, source_digest, source_content_length, accepted_at, \
          inspection_attempts) \
         values ('upl_HTTPTEST', 'ten_HTTP', 'kora', 'http-upload-source', $1, \
          'dev-kora-ocr-quarantine', 'products/kora/tenants/ten_HTTP/quarantine/upl_HTTPTEST', \
          'application/pdf', 8, $2, 'accepted', now() + interval '10 minutes', 3, \
          'application/pdf', 8, $2, now(), 'dev-kora-ocr-source', \
          'products/kora/tenants/ten_HTTP/documents/source', 4, $2, 8, now(), 1)",
    )
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(format!("sha256:{}", "b".repeat(64)))
    .execute(&admin_pool)
    .await
    .unwrap();
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let application = router_with_job_status_cache(
        PgJobStore::new(pool),
        Arc::new(RecordingJobStatusCache {
            writes: writes.clone(),
        }),
        cache_policy(),
    );
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
    assert_eq!(
        *writes.lock().unwrap(),
        vec![
            (ocr_domain::JobState::Accepted, Duration::from_secs(10)),
            (ocr_domain::JobState::Accepted, Duration::from_secs(10)),
            (ocr_domain::JobState::Cancelling, Duration::from_secs(10)),
            (ocr_domain::JobState::Cancelling, Duration::from_secs(10)),
        ]
    );
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

#[tokio::test]
async fn upload_intent_rejects_unbounded_or_unsupported_input_before_database_work() {
    for body in [
        r#"{"content_type":"text/html","content_length":1024,"sha256":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        r#"{"content_type":"application/pdf","content_length":104857601,"sha256":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        r#"{"content_type":"application/pdf","content_length":1024,"sha256":"not-a-digest"}"#,
    ] {
        let response = router_with_upload_issuer(
            store_without_connection(),
            [("kora".to_owned(), "dev-kora-ocr-quarantine".to_owned())]
                .into_iter()
                .collect(),
            Arc::new(StaticUploadIssuer),
        )
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_UPLOAD").unwrap(),
        ))
        .oneshot(
            Request::post("/v1/ocr/uploads")
                .header("content-type", "application/json")
                .header("idempotency-key", "upload-http-invalid")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "accepted {body}"
        );
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn upload_intent_is_created_for_the_verified_scope_without_exposing_storage_names() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let admin_pool = PgPoolOptions::new().connect(&admin_url).await.unwrap();
    sqlx::query(
        "delete from ocr_uploads where product_id = 'kora' and tenant_id = 'ten_HTTPUPLOAD' \
         and idempotency_key = 'upload-http-1'",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let application = router_with_upload_issuer(
        PgJobStore::new(pool),
        [("kora".to_owned(), "dev-kora-ocr-quarantine".to_owned())]
            .into_iter()
            .collect(),
        Arc::new(StaticUploadIssuer),
    );
    let body = format!(
        r#"{{"content_type":"application/pdf","content_length":1024,"sha256":"sha256:{}"}}"#,
        "a".repeat(64)
    );

    let response = application
        .clone()
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_HTTPUPLOAD").unwrap(),
        ))
        .oneshot(
            Request::post("/v1/ocr/uploads")
                .header("content-type", "application/json")
                .header("idempotency-key", "upload-http-1")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(created["upload_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("upl_")));
    assert_eq!(created["method"], "PUT");
    assert_eq!(
        created["upload_url"],
        "https://storage.googleapis.test/signed-upload"
    );
    assert_eq!(
        created["required_headers"]["content-type"],
        "application/pdf"
    );
    assert_eq!(
        created["required_headers"]["x-goog-if-generation-match"],
        "0"
    );
    assert!(created.get("object_bucket").is_none());
    assert!(created.get("object_name").is_none());

    let status = application
        .clone()
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_HTTPUPLOAD").unwrap(),
        ))
        .oneshot(
            Request::get(format!(
                "/v1/ocr/uploads/{}",
                created["upload_id"].as_str().unwrap()
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    let status: Value =
        serde_json::from_slice(&status.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status["upload_id"], created["upload_id"]);
    assert_eq!(status["status"], "reserved");
    assert!(status.get("object_bucket").is_none());
    assert!(status.get("object_name").is_none());
    assert!(status.get("object_generation").is_none());
    assert!(status.get("digest").is_none());

    let foreign = application
        .clone()
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_HTTPUPLOAD_OTHER").unwrap(),
        ))
        .oneshot(
            Request::get(format!(
                "/v1/ocr/uploads/{}",
                created["upload_id"].as_str().unwrap()
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

    let replay = application
        .clone()
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_HTTPUPLOAD").unwrap(),
        ))
        .oneshot(
            Request::post("/v1/ocr/uploads")
                .header("content-type", "application/json")
                .header("idempotency-key", "upload-http-1")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replayed: Value =
        serde_json::from_slice(&replay.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(replayed["upload_id"], created["upload_id"]);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn uploaded_object_is_verified_pinned_and_completed_once_for_its_tenant() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let admin_pool = PgPoolOptions::new().connect(&admin_url).await.unwrap();
    sqlx::query(
        "delete from ocr_upload_outbox where upload_id in \
         (select upload_id from ocr_uploads where product_id = 'kora' \
          and tenant_id = 'ten_HTTPCOMPLETE' and idempotency_key = 'upload-complete-1')",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query(
        "delete from ocr_uploads where product_id = 'kora' \
         and tenant_id = 'ten_HTTPCOMPLETE' and idempotency_key = 'upload-complete-1'",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    let bytes = b"%PDF-1.7";
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    let calls = Arc::new(AtomicUsize::new(0));
    let application = router_with_upload_services(
        PgJobStore::new(PgPoolOptions::new().connect(&url).await.unwrap()),
        [("kora".to_owned(), "dev-kora-ocr-quarantine".to_owned())]
            .into_iter()
            .collect(),
        Arc::new(StaticUploadIssuer),
        Arc::new(StaticUploadArtifactReader {
            artifact: VerifiedUploadArtifact {
                object_generation: 73,
                content_type: "application/pdf".to_owned(),
                content_length: i64::try_from(bytes.len()).unwrap(),
                digest: digest.clone(),
            },
            calls: Arc::clone(&calls),
        }),
    );
    let create_body = format!(
        r#"{{"content_type":"application/pdf","content_length":{},"sha256":"{digest}"}}"#,
        bytes.len()
    );
    let create = application
        .clone()
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_HTTPCOMPLETE").unwrap(),
        ))
        .oneshot(
            Request::post("/v1/ocr/uploads")
                .header("content-type", "application/json")
                .header("idempotency-key", "upload-complete-1")
                .body(Body::from(create_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let created: Value =
        serde_json::from_slice(&create.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let upload_id = created["upload_id"].as_str().unwrap();

    let foreign = application
        .clone()
        .layer(Extension(
            TrustedIdentity::new("kora", "ten_OTHER").unwrap(),
        ))
        .oneshot(
            Request::post(format!("/v1/ocr/uploads/{upload_id}/complete"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    for expected_status in [StatusCode::OK, StatusCode::OK] {
        let response = application
            .clone()
            .layer(Extension(
                TrustedIdentity::new("kora", "ten_HTTPCOMPLETE").unwrap(),
            ))
            .oneshot(
                Request::post(format!("/v1/ocr/uploads/{upload_id}/complete"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["upload_id"], upload_id);
        assert_eq!(body["status"], "uploaded");
        assert!(body.get("object_generation").is_none());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let event_count: i64 =
        sqlx::query_scalar("select count(*) from ocr_upload_outbox where upload_id = $1")
            .bind(upload_id)
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert_eq!(event_count, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn job_creation_rejects_missing_reserved_and_foreign_uploads_as_not_found() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let admin_pool = PgPoolOptions::new().connect(&admin_url).await.unwrap();
    sqlx::query(
        "delete from ocr_outbox where job_id in \
         (select job_id from ocr_jobs where idempotency_key like 'job-source-boundary-%')",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query("delete from ocr_jobs where idempotency_key like 'job-source-boundary-%'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_FOREIGNJOB'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id in ('upl_FOREIGNJOB', 'upl_RESERVEDJOB')")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at) \
         values ('upl_FOREIGNJOB', 'ten_OWNER', 'kora', 'foreign-job-upload', $1, \
          'dev-kora-ocr-quarantine', \
          'products/kora/tenants/ten_OWNER/quarantine/upl_FOREIGNJOB', 'application/pdf', 8, $2, \
          'uploaded', now() + interval '10 minutes', 9, 'application/pdf', 8, $2, now())",
    )
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(format!("sha256:{}", "b".repeat(64)))
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, expires_at) \
         values ('upl_RESERVEDJOB', 'ten_CALLER', 'kora', 'reserved-job-upload', $1, \
          'dev-kora-ocr-quarantine', \
          'products/kora/tenants/ten_CALLER/quarantine/upl_RESERVEDJOB', 'application/pdf', 8, $2, \
          now() + interval '10 minutes')",
    )
    .bind(format!("sha256:{}", "c".repeat(64)))
    .bind(format!("sha256:{}", "d".repeat(64)))
    .execute(&admin_pool)
    .await
    .unwrap();
    let application = router(PgJobStore::new(
        PgPoolOptions::new().connect(&url).await.unwrap(),
    ));

    for (case, tenant, upload_id) in [
        ("missing", "ten_CALLER", "upl_MISSINGJOB"),
        ("reserved", "ten_CALLER", "upl_RESERVEDJOB"),
        ("foreign", "ten_CALLER", "upl_FOREIGNJOB"),
    ] {
        let response = application
            .clone()
            .layer(Extension(TrustedIdentity::new("kora", tenant).unwrap()))
            .oneshot(
                Request::post("/v1/ocr/jobs")
                    .header("content-type", "application/json")
                    .header("idempotency-key", format!("job-source-boundary-{case}"))
                    .body(Body::from(format!(
                        r#"{{"source":{{"upload_id":"{upload_id}"}},"document_type":"auto"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "case {case}");
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["code"], "upload_not_found", "case {case}");
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn a_second_product_uses_the_same_contract_without_cross_product_visibility() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let admin_pool = PgPoolOptions::new().connect(&admin_url).await.unwrap();
    sqlx::query(
        "delete from ocr_outbox where job_id in \
         (select job_id from ocr_jobs where tenant_id = 'ten_COMPAT')",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query("delete from ocr_jobs where tenant_id = 'ten_COMPAT'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query(
        "delete from ocr_upload_outbox where upload_id in \
         (select upload_id from ocr_uploads where tenant_id = 'ten_COMPAT')",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query("delete from ocr_uploads where tenant_id = 'ten_COMPAT'")
        .execute(&admin_pool)
        .await
        .unwrap();

    let bytes = b"%PDF-1.7";
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    let calls = Arc::new(AtomicUsize::new(0));
    let store = PgJobStore::new(PgPoolOptions::new().connect(&url).await.unwrap());
    let application = router_with_upload_services(
        store.clone(),
        [
            ("kora".to_owned(), "dev-kora-ocr-quarantine".to_owned()),
            ("atlas".to_owned(), "dev-atlas-ocr-quarantine".to_owned()),
        ]
        .into_iter()
        .collect(),
        Arc::new(StaticUploadIssuer),
        Arc::new(StaticUploadArtifactReader {
            artifact: VerifiedUploadArtifact {
                object_generation: 81,
                content_type: "application/pdf".to_owned(),
                content_length: i64::try_from(bytes.len()).unwrap(),
                digest: digest.clone(),
            },
            calls: Arc::clone(&calls),
        }),
    );
    let request_body = format!(
        r#"{{"content_type":"application/pdf","content_length":{},"sha256":"{digest}"}}"#,
        bytes.len()
    );
    let mut upload_ids = std::collections::BTreeMap::new();
    for product in ["kora", "atlas"] {
        let response = application
            .clone()
            .layer(Extension(
                TrustedIdentity::new(product, "ten_COMPAT").unwrap(),
            ))
            .oneshot(
                Request::post("/v1/ocr/uploads")
                    .header("content-type", "application/json")
                    .header("idempotency-key", format!("compat-upload-{product}"))
                    .body(Body::from(request_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "product {product}");
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(body.get("object_bucket").is_none());
        upload_ids.insert(product, body["upload_id"].as_str().unwrap().to_owned());
    }
    assert_ne!(upload_ids["kora"], upload_ids["atlas"]);

    for (caller, foreign_product) in [("kora", "atlas"), ("atlas", "kora")] {
        let response = application
            .clone()
            .layer(Extension(
                TrustedIdentity::new(caller, "ten_COMPAT").unwrap(),
            ))
            .oneshot(
                Request::post(format!(
                    "/v1/ocr/uploads/{}/complete",
                    upload_ids[foreign_product]
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "caller {caller}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    for product in ["kora", "atlas"] {
        let upload_id = &upload_ids[product];
        let completion = application
            .clone()
            .layer(Extension(
                TrustedIdentity::new(product, "ten_COMPAT").unwrap(),
            ))
            .oneshot(
                Request::post(format!("/v1/ocr/uploads/{upload_id}/complete"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(completion.status(), StatusCode::OK, "product {product}");

        store
            .claim_upload_inspection(
                &TenantId::new("ten_COMPAT").unwrap(),
                &ProductId::new(product).unwrap(),
                &UploadId::new(upload_id).unwrap(),
                ClaimUploadInspection {
                    lease_owner: "compat-importer".to_owned(),
                },
            )
            .await
            .unwrap();
        store
            .accept_upload(
                &TenantId::new("ten_COMPAT").unwrap(),
                &ProductId::new(product).unwrap(),
                &UploadId::new(upload_id).unwrap(),
                AcceptUpload {
                    inspection_lease_owner: "compat-importer".to_owned(),
                    source_bucket: format!("dev-{product}-ocr-source"),
                    source_object_name: format!(
                        "products/{product}/tenants/ten_COMPAT/documents/{digest}/source"
                    ),
                    source_object_generation: 82,
                    source_digest: digest.clone(),
                    source_content_length: i64::try_from(bytes.len()).unwrap(),
                    parser_inspection: ParserInspectionMetadata {
                        page_count: 1,
                        maximum_page_pixels: 1,
                        total_page_pixels: 1,
                        profile: "compat-v1".to_owned(),
                        version: "0.1.0".to_owned(),
                    },
                },
            )
            .await
            .unwrap();

        let job = application
            .clone()
            .layer(Extension(
                TrustedIdentity::new(product, "ten_COMPAT").unwrap(),
            ))
            .oneshot(
                Request::post("/v1/ocr/jobs")
                    .header("content-type", "application/json")
                    .header("idempotency-key", format!("compat-job-{product}"))
                    .body(Body::from(format!(
                        r#"{{"source":{{"upload_id":"{upload_id}"}},"document_type":"auto"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(job.status(), StatusCode::ACCEPTED, "product {product}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
