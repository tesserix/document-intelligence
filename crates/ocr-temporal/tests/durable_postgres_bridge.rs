use ocr_domain::{
    IdempotencyKey, JobId, PageTask, PageWorkflow, ProductId, RequestDigest, TenantId, UploadId,
};
use ocr_service::{PageProcessError, PageProcessFuture, PageProcessor};
use ocr_store::{CreateJob, CreatePageWorkflowOutcome, PgJobStore};
use ocr_temporal::{
    CheckpointedPageExecutor, DurableActivityInput, DurableActivityStatus,
    DurableExecutionErrorKind, DurablePageExecution,
};
use sqlx::PgPool;

struct ExhaustingProcessor;

impl PageProcessor for ExhaustingProcessor {
    fn process<'a>(&'a self, _task: PageTask) -> PageProcessFuture<'a> {
        Box::pin(async { Err(PageProcessError::Retryable) })
    }
}

async fn stores() -> (PgJobStore, PgPool) {
    let application_url =
        std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let application_pool = PgPool::connect(&application_url).await.unwrap();
    let admin_pool = PgPool::connect(&admin_url).await.unwrap();
    (PgJobStore::new(application_pool), admin_pool)
}

async fn seed_job(store: &PgJobStore, admin: &PgPool) {
    let job = "job_TEMPORAL_BRIDGE";
    let tenant = "ten_TEMPORAL_BRIDGE";
    let upload = "upl_TEMPORAL_BRIDGE";
    for statement in [
        "delete from ocr_page_artifacts where job_id = $1",
        "delete from ocr_page_workflows where job_id = $1",
        "delete from ocr_outbox where job_id = $1",
        "delete from ocr_jobs where job_id = $1",
    ] {
        sqlx::query(statement)
            .bind(job)
            .execute(admin)
            .await
            .unwrap();
    }
    sqlx::query("delete from ocr_uploads where upload_id = $1")
        .bind(upload)
        .execute(admin)
        .await
        .unwrap();
    sqlx::query(
        "insert into ocr_uploads (upload_id, tenant_id, product_id, idempotency_key, \
         request_digest, object_bucket, object_name, expected_content_type, \
         expected_content_length, expected_digest, status, expires_at, object_generation, \
         verified_content_type, verified_content_length, verified_digest, uploaded_at, \
         source_bucket, source_object_name, source_object_generation, source_digest, \
         source_content_length, inspection_attempts, accepted_at) values \
         ($1, $2, 'kora', 'seed-temporal-bridge', $3, 'dev-kora-ocr-quarantine', \
          'products/kora/tenants/ten_TEMPORAL_BRIDGE/upl_TEMPORAL_BRIDGE', \
          'application/pdf', 8, $4, 'accepted', now() + interval '10 minutes', 1, \
          'application/pdf', 8, $4, now(), 'dev-kora-ocr-source', \
          'products/kora/tenants/ten_TEMPORAL_BRIDGE/upl_TEMPORAL_BRIDGE/accepted', \
          2, $4, 8, 1, now())",
    )
    .bind(upload)
    .bind(tenant)
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(format!("sha256:{}", "b".repeat(64)))
    .execute(admin)
    .await
    .unwrap();
    store
        .create(CreateJob {
            job_id: JobId::new(job).unwrap(),
            tenant_id: TenantId::new(tenant).unwrap(),
            product_id: ProductId::new("kora").unwrap(),
            idempotency_key: IdempotencyKey::new("temporal-bridge-job").unwrap(),
            request_digest: RequestDigest::new(&format!("sha256:{}", "c".repeat(64))).unwrap(),
            upload_id: UploadId::new(upload).unwrap(),
            webhook_subscription_id: None,
        })
        .await
        .unwrap();
    let job_id = JobId::new(job).unwrap();
    let workflow = PageWorkflow::new(job_id.clone(), 1, 3).unwrap();
    assert!(matches!(
        store
            .create_page_workflow(
                &TenantId::new(tenant).unwrap(),
                &ProductId::new("kora").unwrap(),
                &job_id,
                workflow,
            )
            .await
            .unwrap(),
        CreatePageWorkflowOutcome::Created(_)
    ));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn cnpg_owns_attempt_exhaustion_and_cross_tenant_access_is_denied() {
    let (store, admin) = stores().await;
    seed_job(&store, &admin).await;
    let executor = CheckpointedPageExecutor::new(store, ExhaustingProcessor, 1, 1).unwrap();
    let input = DurableActivityInput::new(
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_TEMPORAL_BRIDGE").unwrap(),
        JobId::new("job_TEMPORAL_BRIDGE").unwrap(),
    );

    let execution: &dyn DurablePageExecution = &executor;
    assert_eq!(
        execution.execute(input.clone()).await.unwrap().status(),
        DurableActivityStatus::Running
    );
    assert_eq!(
        executor.run(&input).await.unwrap().status(),
        DurableActivityStatus::Running
    );
    assert_eq!(
        executor.run(&input).await.unwrap().status(),
        DurableActivityStatus::Partial
    );

    let wrong_tenant = DurableActivityInput::new(
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_OTHER_PRODUCT_USER").unwrap(),
        JobId::new("job_TEMPORAL_BRIDGE").unwrap(),
    );
    let error = executor.run(&wrong_tenant).await.unwrap_err();
    assert_eq!(error.kind(), DurableExecutionErrorKind::ScopeNotFound);
    assert!(!error.is_retryable());
}
