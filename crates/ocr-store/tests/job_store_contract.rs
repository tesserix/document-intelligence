use ocr_domain::{IdempotencyKey, JobId, ProductId, RequestDigest, TenantId};
use ocr_store::{CreateJob, CreateOutcome, Error, PgJobStore};
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

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn create_is_atomic_and_idempotent_per_trusted_scope() {
    let (store, admin_pool, _) = store().await;
    let first = request("job_FIRST", "ten_ALPHA", "request-1", 'a');

    assert_eq!(store.create(first).await.unwrap(), CreateOutcome::Created);
    assert_eq!(
        store
            .create(request("job_RETRY", "ten_ALPHA", "request-1", 'a'))
            .await
            .unwrap(),
        CreateOutcome::Existing(JobId::new("job_FIRST").unwrap())
    );

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
    let (store, _, _) = store().await;
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
    let (store, _, _) = store().await;
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
