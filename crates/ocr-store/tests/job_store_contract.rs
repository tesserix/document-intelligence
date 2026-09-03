use ocr_domain::{DocumentId, IdempotencyKey, JobId, ProductId, RequestDigest, TenantId, UploadId};
use ocr_store::{
    AcceptUpload, AcceptUploadOutcome, CancelOutcome, ClaimUploadInspection,
    ClaimUploadInspectionOutcome, CreateJob, CreateOutcome, CreateUpload, CreateUploadOutcome,
    Error, ParserInspectionMetadata, PgJobStore, RecordUpload, RecordUploadOutcome,
    RejectUploadOutcome, ResultLookup, UploadRejectionReason,
};
use sqlx::PgPool;

fn request(job_id: &str, tenant_id: &str, key: &str, digest: char) -> CreateJob {
    CreateJob {
        job_id: JobId::new(job_id).unwrap(),
        tenant_id: TenantId::new(tenant_id).unwrap(),
        product_id: ProductId::new("kora").unwrap(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        request_digest: RequestDigest::new(&format!("sha256:{}", digest.to_string().repeat(64)))
            .unwrap(),
        upload_id: upload_id_for(tenant_id),
    }
}

fn upload_id_for(tenant_id: &str) -> UploadId {
    UploadId::new(&format!(
        "upl_{}",
        tenant_id.strip_prefix("ten_").unwrap_or(tenant_id)
    ))
    .unwrap()
}

async fn seed_uploaded_upload(admin_pool: &PgPool, tenant_id: &str) {
    let upload_id = upload_id_for(tenant_id);
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at) \
         values ($1, $2, 'kora', $3, $4, 'dev-kora-ocr-quarantine', $5, \
          'application/pdf', 8, $6, 'uploaded', now() + interval '10 minutes', 1, \
          'application/pdf', 8, $6, now()) \
         on conflict (upload_id) do nothing",
    )
    .bind(upload_id.as_str())
    .bind(tenant_id)
    .bind(format!("seed-{tenant_id}"))
    .bind(format!("sha256:{}", "e".repeat(64)))
    .bind(format!(
        "products/kora/tenants/{tenant_id}/quarantine/{}",
        upload_id.as_str()
    ))
    .bind(format!("sha256:{}", "f".repeat(64)))
    .execute(admin_pool)
    .await
    .unwrap();
}

async fn set_upload_state(admin_pool: &PgPool, tenant_id: &str, state: &str) {
    sqlx::query(
        "update ocr_uploads set status = $2::ocr_upload_status, \
         source_bucket = case when $2 = 'accepted' then 'dev-kora-ocr-source' end, \
         source_object_name = case when $2 = 'accepted' then object_name || '/accepted' end, \
         source_object_generation = case when $2 = 'accepted' then 2 end, \
         source_digest = case when $2 = 'accepted' then verified_digest end, \
         source_content_length = case when $2 = 'accepted' then verified_content_length end, \
         inspection_attempts = case when $2 = 'accepted' then 1 else 0 end, \
         inspection_lease_owner = null, inspection_lease_expires_at = null, \
         accepted_at = case when $2 = 'accepted' then now() end \
         where tenant_id = $1",
    )
    .bind(tenant_id)
    .bind(state)
    .execute(admin_pool)
    .await
    .unwrap();
}

async fn seed_accepted_upload(admin_pool: &PgPool, tenant_id: &str) {
    seed_uploaded_upload(admin_pool, tenant_id).await;
    set_upload_state(admin_pool, tenant_id, "accepted").await;
}

async fn store() -> (PgJobStore, PgPool, PgPool) {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let pool = PgPool::connect(&url).await.unwrap();
    let admin_pool = PgPool::connect(&admin_url).await.unwrap();
    (PgJobStore::new(pool.clone()), admin_pool, pool)
}

async fn clear_fixture(admin_pool: &PgPool, job_ids: &[&str]) {
    for job_id in job_ids {
        sqlx::query("delete from ocr_results where job_id = $1")
            .bind(job_id)
            .execute(admin_pool)
            .await
            .unwrap();
        sqlx::query("delete from ocr_outbox where job_id = $1")
            .bind(job_id)
            .execute(admin_pool)
            .await
            .unwrap();
        sqlx::query("delete from ocr_jobs where job_id = $1")
            .bind(job_id)
            .execute(admin_pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn create_is_atomic_and_idempotent_per_trusted_scope() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_FIRST", "job_RETRY"]).await;
    seed_accepted_upload(&admin_pool, "ten_ALPHA").await;
    let first = request("job_FIRST", "ten_ALPHA", "request-1", 'a');

    let created = store.create(first).await.unwrap();
    let replayed = store
        .create(request("job_RETRY", "ten_ALPHA", "request-1", 'a'))
        .await
        .unwrap();
    let (CreateOutcome::Created(created), CreateOutcome::Existing(replayed)) = (created, replayed)
    else {
        panic!("expected a created job followed by its existing replay")
    };
    assert_eq!(replayed.job_id, JobId::new("job_FIRST").unwrap());
    assert_eq!(replayed.created_at, created.created_at);

    let outbox_count: i64 =
        sqlx::query_scalar("select count(*) from ocr_outbox where job_id = 'job_FIRST'")
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert_eq!(outbox_count, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn create_requires_an_accepted_source_instead_of_an_uploaded_quarantine_object() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_QUARANTINED", "job_ACCEPTED_SOURCE"]).await;
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_SOURCE_GATE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_SOURCE_GATE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    seed_uploaded_upload(&admin_pool, "ten_SOURCE_GATE").await;

    let quarantined = store
        .create(request(
            "job_QUARANTINED",
            "ten_SOURCE_GATE",
            "quarantined-source",
            'a',
        ))
        .await
        .unwrap_err();
    assert!(matches!(quarantined, Error::UploadSourceUnavailable));

    set_upload_state(&admin_pool, "ten_SOURCE_GATE", "accepted").await;
    let accepted = store
        .create(request(
            "job_ACCEPTED_SOURCE",
            "ten_SOURCE_GATE",
            "accepted-source",
            'b',
        ))
        .await
        .unwrap();
    assert!(matches!(accepted, CreateOutcome::Created(_)));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn idempotency_key_reuse_with_a_different_digest_conflicts() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_CONFLICT_FIRST", "job_CONFLICT_SECOND"]).await;
    seed_accepted_upload(&admin_pool, "ten_CONFLICT").await;
    store
        .create(request(
            "job_CONFLICT_FIRST",
            "ten_CONFLICT",
            "request-conflict",
            'a',
        ))
        .await
        .unwrap();

    let error = store
        .create(request(
            "job_CONFLICT_SECOND",
            "ten_CONFLICT",
            "request-conflict",
            'b',
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::IdempotencyConflict));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn a_different_tenant_cannot_read_a_job() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_PRIVATE"]).await;
    seed_accepted_upload(&admin_pool, "ten_PRIVATE").await;
    store
        .create(request(
            "job_PRIVATE",
            "ten_PRIVATE",
            "private-request",
            'a',
        ))
        .await
        .unwrap();

    assert!(store
        .find(
            &TenantId::new("ten_BRAVO").unwrap(),
            &ProductId::new("kora").unwrap(),
            &JobId::new("job_PRIVATE").unwrap(),
        )
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn row_level_security_fails_closed_without_a_trusted_scope() {
    let (_, _, application_pool) = store().await;

    let visible: i64 = sqlx::query_scalar("select count(*) from ocr_jobs")
        .fetch_one(&application_pool)
        .await
        .unwrap();
    assert_eq!(visible, 0);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn upload_intent_is_idempotent_and_scoped_to_the_verified_tenant() {
    let (store, admin_pool, _) = store().await;
    sqlx::query("delete from ocr_uploads where upload_id in ('upl_FIRST', 'upl_RETRY')")
        .execute(&admin_pool)
        .await
        .unwrap();
    let create = |upload_id: &str, tenant_id: &str| CreateUpload {
        upload_id: UploadId::new(upload_id).unwrap(),
        tenant_id: TenantId::new(tenant_id).unwrap(),
        product_id: ProductId::new("kora").unwrap(),
        idempotency_key: IdempotencyKey::new("upload-request-1").unwrap(),
        request_digest: RequestDigest::new(&format!("sha256:{}", "e".repeat(64))).unwrap(),
        object_bucket: "dev-kora-ocr-quarantine".to_owned(),
        object_name: format!("products/kora/tenants/{tenant_id}/uploads/{upload_id}"),
        expected_content_type: "application/pdf".to_owned(),
        expected_content_length: 1024,
        expected_digest: format!("sha256:{}", "f".repeat(64)),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
    };

    let created = store
        .create_upload(create("upl_FIRST", "ten_UPLOAD"))
        .await
        .unwrap();
    let replayed = store
        .create_upload(create("upl_RETRY", "ten_UPLOAD"))
        .await
        .unwrap();
    let (CreateUploadOutcome::Created(created), CreateUploadOutcome::Existing(replayed)) =
        (created, replayed)
    else {
        panic!("expected a created upload followed by its existing replay")
    };
    assert_eq!(created.upload_id, UploadId::new("upl_FIRST").unwrap());
    assert_eq!(replayed.upload_id, created.upload_id);
    assert_eq!(replayed.object_name, created.object_name);

    let foreign = store
        .find_upload(
            &TenantId::new("ten_OTHER").unwrap(),
            &ProductId::new("kora").unwrap(),
            &created.upload_id,
        )
        .await
        .unwrap();
    assert!(foreign.is_none());
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn verified_upload_generation_and_event_are_recorded_exactly_once() {
    let (store, admin_pool, _) = store().await;
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_RECONCILE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_RECONCILE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    let tenant_id = TenantId::new("ten_RECONCILE").unwrap();
    let product_id = ProductId::new("kora").unwrap();
    store
        .create_upload(CreateUpload {
            upload_id: UploadId::new("upl_RECONCILE").unwrap(),
            tenant_id: tenant_id.clone(),
            product_id: product_id.clone(),
            idempotency_key: IdempotencyKey::new("upload-reconcile-1").unwrap(),
            request_digest: RequestDigest::new(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            object_bucket: "dev-kora-ocr-quarantine".to_owned(),
            object_name: "products/kora/tenants/ten_RECONCILE/quarantine/upl_RECONCILE".to_owned(),
            expected_content_type: "application/pdf".to_owned(),
            expected_content_length: 1024,
            expected_digest: format!("sha256:{}", "b".repeat(64)),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
        })
        .await
        .unwrap();
    let record = || RecordUpload {
        object_generation: 42,
        verified_content_type: "application/pdf".to_owned(),
        verified_content_length: 1024,
        verified_digest: format!("sha256:{}", "b".repeat(64)),
    };

    let first = store
        .record_uploaded(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_RECONCILE").unwrap(),
            record(),
        )
        .await
        .unwrap();
    let replay = store
        .record_uploaded(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_RECONCILE").unwrap(),
            record(),
        )
        .await
        .unwrap();
    assert!(matches!(first, RecordUploadOutcome::Recorded(_)));
    assert!(matches!(replay, RecordUploadOutcome::Existing(_)));

    let event_count: i64 = sqlx::query_scalar(
        "select count(*) from ocr_upload_outbox where upload_id = 'upl_RECONCILE'",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(event_count, 1);

    let foreign = store
        .record_uploaded(
            &TenantId::new("ten_OTHER").unwrap(),
            &product_id,
            &UploadId::new("upl_RECONCILE").unwrap(),
            record(),
        )
        .await
        .unwrap();
    assert!(matches!(foreign, RecordUploadOutcome::NotFound));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn accepted_source_and_event_are_recorded_exactly_once() {
    let (store, admin_pool, _) = store().await;
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_ACCEPT_SOURCE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_ACCEPT_SOURCE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    let tenant_id = TenantId::new("ten_ACCEPT_SOURCE").unwrap();
    let product_id = ProductId::new("kora").unwrap();
    let digest = format!("sha256:{}", "b".repeat(64));
    store
        .create_upload(CreateUpload {
            upload_id: UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            tenant_id: tenant_id.clone(),
            product_id: product_id.clone(),
            idempotency_key: IdempotencyKey::new("upload-accept-source-1").unwrap(),
            request_digest: RequestDigest::new(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            object_bucket: "dev-kora-ocr-quarantine".to_owned(),
            object_name: "products/kora/tenants/ten_ACCEPT_SOURCE/quarantine/upl_ACCEPT_SOURCE"
                .to_owned(),
            expected_content_type: "application/pdf".to_owned(),
            expected_content_length: 1024,
            expected_digest: digest.clone(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
        })
        .await
        .unwrap();
    store
        .record_uploaded(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            RecordUpload {
                object_generation: 42,
                verified_content_type: "application/pdf".to_owned(),
                verified_content_length: 1024,
                verified_digest: digest.clone(),
            },
        )
        .await
        .unwrap();
    let acceptance = |lease_owner: &str| AcceptUpload {
        inspection_lease_owner: lease_owner.to_owned(),
        source_bucket: "dev-kora-ocr-source".to_owned(),
        source_object_name: "products/kora/tenants/ten_ACCEPT_SOURCE/documents/sha256/source"
            .to_owned(),
        source_object_generation: 73,
        source_digest: digest.clone(),
        source_content_length: 1024,
        parser_inspection: ParserInspectionMetadata {
            page_count: 2,
            maximum_page_pixels: 8_500_000,
            total_page_pixels: 16_000_000,
            profile: "intake-v1".to_owned(),
            version: "0.1.0".to_owned(),
        },
    };

    let not_claimed = store
        .accept_upload(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            acceptance("importer-01"),
        )
        .await
        .unwrap();
    assert_eq!(not_claimed, AcceptUploadOutcome::NotAcceptable);

    let claimed = store
        .claim_upload_inspection(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            ClaimUploadInspection {
                lease_owner: "importer-01".to_owned(),
            },
        )
        .await
        .unwrap();
    let replayed_claim = store
        .claim_upload_inspection(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            ClaimUploadInspection {
                lease_owner: "importer-01".to_owned(),
            },
        )
        .await
        .unwrap();
    let competing_claim = store
        .claim_upload_inspection(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            ClaimUploadInspection {
                lease_owner: "importer-02".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(claimed, ClaimUploadInspectionOutcome::Claimed);
    assert_eq!(replayed_claim, ClaimUploadInspectionOutcome::Existing);
    assert_eq!(competing_claim, ClaimUploadInspectionOutcome::Busy);

    sqlx::query(
        "update ocr_uploads set inspection_lease_expires_at = now() - interval '1 second' \
         where upload_id = 'upl_ACCEPT_SOURCE'",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    let reclaimed = store
        .claim_upload_inspection(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            ClaimUploadInspection {
                lease_owner: "importer-02".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(reclaimed, ClaimUploadInspectionOutcome::Claimed);

    let stale_owner = store
        .accept_upload(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            acceptance("importer-01"),
        )
        .await
        .unwrap();
    assert_eq!(stale_owner, AcceptUploadOutcome::NotAcceptable);

    let foreign = store
        .accept_upload(
            &TenantId::new("ten_OTHER").unwrap(),
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            acceptance("importer-02"),
        )
        .await
        .unwrap();
    assert_eq!(foreign, AcceptUploadOutcome::NotFound);

    let accepted = store
        .accept_upload(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            acceptance("importer-02"),
        )
        .await
        .unwrap();
    let replayed = store
        .accept_upload(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            acceptance("importer-02"),
        )
        .await
        .unwrap();
    let mismatched = store
        .accept_upload(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            AcceptUpload {
                source_object_generation: 74,
                ..acceptance("importer-02")
            },
        )
        .await
        .unwrap();
    let mismatched_parser = store
        .accept_upload(
            &tenant_id,
            &product_id,
            &UploadId::new("upl_ACCEPT_SOURCE").unwrap(),
            AcceptUpload {
                parser_inspection: ParserInspectionMetadata {
                    profile: "intake-v2".to_owned(),
                    ..acceptance("importer-02").parser_inspection
                },
                ..acceptance("importer-02")
            },
        )
        .await
        .unwrap();
    assert_eq!(accepted, AcceptUploadOutcome::Accepted);
    assert_eq!(replayed, AcceptUploadOutcome::Existing);
    assert_eq!(mismatched, AcceptUploadOutcome::SourceMismatch);
    assert_eq!(mismatched_parser, AcceptUploadOutcome::SourceMismatch);

    let row: (
        String,
        String,
        i64,
        String,
        i64,
        i32,
        i32,
        i64,
        i64,
        String,
        String,
    ) = sqlx::query_as(
        "select source_bucket, source_object_name, source_object_generation, source_digest, \
         source_content_length, inspection_attempts, parser_page_count, \
         parser_maximum_page_pixels, parser_total_page_pixels, parser_profile, parser_version \
         from ocr_uploads \
         where upload_id = 'upl_ACCEPT_SOURCE'",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(row.0, "dev-kora-ocr-source");
    assert_eq!(
        row.1,
        "products/kora/tenants/ten_ACCEPT_SOURCE/documents/sha256/source"
    );
    assert_eq!(row.2, 73);
    assert_eq!(row.3, digest);
    assert_eq!(row.4, 1024);
    assert_eq!(row.5, 2);
    assert_eq!(row.6, 2);
    assert_eq!(row.7, 8_500_000);
    assert_eq!(row.8, 16_000_000);
    assert_eq!(row.9, "intake-v1");
    assert_eq!(row.10, "0.1.0");

    let events: i64 = sqlx::query_scalar(
        "select count(*) from ocr_upload_outbox where upload_id = 'upl_ACCEPT_SOURCE' \
         and event_type = 'ocr.upload.accepted.v1'",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn exhausted_inspection_attempts_reject_the_upload_once() {
    let (store, admin_pool, _) = store().await;
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_EXHAUSTED'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_EXHAUSTED'")
        .execute(&admin_pool)
        .await
        .unwrap();
    seed_uploaded_upload(&admin_pool, "ten_EXHAUSTED").await;
    sqlx::query(
        "update ocr_uploads set status = 'inspecting', inspection_attempts = 10, \
         inspection_lease_owner = 'dead-importer', \
         inspection_lease_expires_at = now() - interval '1 second' \
         where upload_id = 'upl_EXHAUSTED'",
    )
    .execute(&admin_pool)
    .await
    .unwrap();

    let outcome = store
        .claim_upload_inspection(
            &TenantId::new("ten_EXHAUSTED").unwrap(),
            &ProductId::new("kora").unwrap(),
            &UploadId::new("upl_EXHAUSTED").unwrap(),
            ClaimUploadInspection {
                lease_owner: "importer-recovery".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, ClaimUploadInspectionOutcome::AttemptsExhausted);
    let replayed = store
        .claim_upload_inspection(
            &TenantId::new("ten_EXHAUSTED").unwrap(),
            &ProductId::new("kora").unwrap(),
            &UploadId::new("upl_EXHAUSTED").unwrap(),
            ClaimUploadInspection {
                lease_owner: "importer-recovery".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(replayed, ClaimUploadInspectionOutcome::NotInspectable);

    let state: String = sqlx::query_scalar(
        "select status::text from ocr_uploads where upload_id = 'upl_EXHAUSTED'",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(state, "rejected");
    let events: i64 = sqlx::query_scalar(
        "select count(*) from ocr_upload_outbox where upload_id = 'upl_EXHAUSTED' \
         and event_type = 'ocr.upload.rejected.v1' \
         and payload->>'reason_code' = 'inspection_attempts_exhausted'",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn claimed_upload_rejection_is_scoped_atomic_and_idempotent() {
    let (store, admin_pool, _) = store().await;
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_REJECT_MALWARE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_REJECT_MALWARE'")
        .execute(&admin_pool)
        .await
        .unwrap();
    seed_uploaded_upload(&admin_pool, "ten_REJECT_MALWARE").await;
    let tenant_id = TenantId::new("ten_REJECT_MALWARE").unwrap();
    let product_id = ProductId::new("kora").unwrap();
    let upload_id = UploadId::new("upl_REJECT_MALWARE").unwrap();
    store
        .claim_upload_inspection(
            &tenant_id,
            &product_id,
            &upload_id,
            ClaimUploadInspection {
                lease_owner: "importer-malware".to_owned(),
            },
        )
        .await
        .unwrap();

    let foreign = store
        .reject_upload(
            &TenantId::new("ten_OTHER").unwrap(),
            &product_id,
            &upload_id,
            "importer-malware",
            UploadRejectionReason::MalwareDetected,
        )
        .await
        .unwrap();
    let stale_owner = store
        .reject_upload(
            &tenant_id,
            &product_id,
            &upload_id,
            "other-importer",
            UploadRejectionReason::MalwareDetected,
        )
        .await
        .unwrap();
    let rejected = store
        .reject_upload(
            &tenant_id,
            &product_id,
            &upload_id,
            "importer-malware",
            UploadRejectionReason::MalwareDetected,
        )
        .await
        .unwrap();
    let replayed = store
        .reject_upload(
            &tenant_id,
            &product_id,
            &upload_id,
            "importer-malware",
            UploadRejectionReason::MalwareDetected,
        )
        .await
        .unwrap();

    assert_eq!(foreign, RejectUploadOutcome::NotFound);
    assert_eq!(stale_owner, RejectUploadOutcome::NotRejectable);
    assert_eq!(rejected, RejectUploadOutcome::Rejected);
    assert_eq!(replayed, RejectUploadOutcome::Existing);
    let row: (String, String) = sqlx::query_as(
        "select status::text, rejection_reason from ocr_uploads where upload_id = $1",
    )
    .bind(upload_id.as_str())
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(row, ("rejected".to_owned(), "malware_detected".to_owned()));
    let events: i64 = sqlx::query_scalar(
        "select count(*) from ocr_upload_outbox where upload_id = $1 \
         and event_type = 'ocr.upload.rejected.v1' \
         and payload->>'reason_code' = 'malware_detected'",
    )
    .bind(upload_id.as_str())
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn cancellation_is_atomic_and_idempotent() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_CANCEL"]).await;
    seed_accepted_upload(&admin_pool, "ten_CANCEL").await;
    let job_id = JobId::new("job_CANCEL").unwrap();
    store
        .create(request(
            job_id.as_str(),
            "ten_CANCEL",
            "request-cancel",
            'c',
        ))
        .await
        .unwrap();

    let tenant_id = TenantId::new("ten_CANCEL").unwrap();
    let product_id = ProductId::new("kora").unwrap();
    let first = store
        .cancel(&tenant_id, &product_id, &job_id)
        .await
        .unwrap();
    let replay = store
        .cancel(&tenant_id, &product_id, &job_id)
        .await
        .unwrap();

    assert!(matches!(first, CancelOutcome::Requested(_)));
    assert!(matches!(replay, CancelOutcome::Existing(_)));
    let outbox_count: i64 = sqlx::query_scalar(
        "select count(*) from ocr_outbox where job_id = 'job_CANCEL' and event_type = 'ocr.job.cancellation_requested.v1'",
    )
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(outbox_count, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn result_locator_is_immutable_and_hidden_across_tenant_boundaries() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_RESULT"]).await;
    seed_accepted_upload(&admin_pool, "ten_RESULT").await;
    let job_id = JobId::new("job_RESULT").unwrap();
    store
        .create(request(
            job_id.as_str(),
            "ten_RESULT",
            "request-result",
            'd',
        ))
        .await
        .unwrap();

    sqlx::query("update ocr_jobs set status = 'completed' where job_id = $1")
        .bind(job_id.as_str())
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into ocr_results \
         (job_id, product_id, tenant_id, document_id, document_version, object_bucket, \
          object_name, object_generation, object_digest, content_length) \
         values ($1, 'kora', 'ten_RESULT', 'doc_RESULT', $2, 'ocr-dev-results-au', \
          'products/kora/tenants/ten_RESULT/results/job_RESULT/v1.json', 42, $3, 512)",
    )
    .bind(job_id.as_str())
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(format!("sha256:{}", "b".repeat(64)))
    .execute(&admin_pool)
    .await
    .unwrap();

    let lookup = store
        .find_result(
            &TenantId::new("ten_RESULT").unwrap(),
            &ProductId::new("kora").unwrap(),
            &job_id,
        )
        .await
        .unwrap();
    let ResultLookup::Ready(locator) = lookup else {
        panic!("expected a ready immutable result locator")
    };
    assert_eq!(locator.object_generation, 42);
    assert_eq!(locator.content_length, 512);
    assert_eq!(locator.document_id, DocumentId::new("doc_RESULT").unwrap());

    assert!(matches!(
        store
            .find_result(
                &TenantId::new("ten_OTHER").unwrap(),
                &ProductId::new("kora").unwrap(),
                &job_id,
            )
            .await
            .unwrap(),
        ResultLookup::NotFound
    ));
}
