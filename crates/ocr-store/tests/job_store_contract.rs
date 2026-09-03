use ocr_domain::{DocumentId, IdempotencyKey, JobId, ProductId, RequestDigest, TenantId, UploadId};
use ocr_store::{
    CancelOutcome, CreateJob, CreateOutcome, CreateUpload, CreateUploadOutcome, Error, PgJobStore,
    ResultLookup,
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
    }
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
async fn idempotency_key_reuse_with_a_different_digest_conflicts() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_CONFLICT_FIRST", "job_CONFLICT_SECOND"]).await;
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
async fn cancellation_is_atomic_and_idempotent() {
    let (store, admin_pool, _) = store().await;
    clear_fixture(&admin_pool, &["job_CANCEL"]).await;
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
