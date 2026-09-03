use std::sync::Arc;

use ocr_domain::{
    IdempotencyKey, JobId, PageTask, PageWorkflow, ProductId, RequestDigest, TenantId, UploadId,
};
use ocr_service::{PageProcessError, PageProcessFuture, PageProcessor};
use ocr_store::{CreateJob, CreatePageWorkflowOutcome, PgJobStore, StoredPageArtifact};
use ocr_temporal::{
    CheckpointedPageExecutor, DurableActivityInput, DurableActivityStatus, DurableDocumentWorkflow,
    DurableExecutionErrorKind, DurablePageActivities, DurablePageExecution,
    DurableWorkflowResultMetadata, DurableWorkflowRunInput,
};
use sqlx::PgPool;
use temporalio_client::{WorkflowGetResultOptions, WorkflowStartOptions};
use temporalio_sdk::{
    testing::{
        DevServerLogLevel, EphemeralExe, LocalWorkflowEnvironmentOptions, WorkflowEnvironment,
    },
    Runtime, Worker, WorkerOptions,
};

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

async fn seed_job(store: &PgJobStore, admin: &PgPool, page_count: u32) {
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
    let workflow = PageWorkflow::new(job_id.clone(), page_count, 3).unwrap();
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
    seed_job(&store, &admin, 1).await;
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

struct PageTwoExhaustingProcessor;

impl PageProcessor for PageTwoExhaustingProcessor {
    fn process<'a>(&'a self, task: PageTask) -> PageProcessFuture<'a> {
        Box::pin(async move {
            if task.page == 2 {
                return Err(PageProcessError::Retryable);
            }
            Ok(StoredPageArtifact {
                page: task.page,
                attempt: task.attempt,
                activity_key: task.activity_key,
                object_bucket: "dev-kora-ocr-pages".to_owned(),
                object_name: format!("page-artifacts/{}.json", task.page),
                object_generation: i64::from(task.attempt),
                object_digest: format!("sha256:{}", "d".repeat(64)),
                content_length: 256,
            })
        })
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires TEST_DATABASE_URL and TEMPORAL_CLI_PATH"]
async fn temporal_sdk_activity_exhaustion_preserves_successful_page_artifacts() {
    let Ok(cli) = std::env::var("TEMPORAL_CLI_PATH") else {
        return;
    };
    let (store, admin) = stores().await;
    seed_job(&store, &admin, 3).await;
    let executor =
        CheckpointedPageExecutor::new(store.clone(), PageTwoExhaustingProcessor, 1, 1).unwrap();
    let input = DurableActivityInput::new(
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_TEMPORAL_BRIDGE").unwrap(),
        JobId::new("job_TEMPORAL_BRIDGE").unwrap(),
    );
    let env = WorkflowEnvironment::start_local(
        LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(cli))
            .log_level(DevServerLogLevel::Never)
            .build(),
    )
    .await
    .unwrap();
    let runtime = Runtime::new_assume_tokio(Default::default()).unwrap();
    let worker_options = WorkerOptions::new("ocr-temporal-durable-postgres")
        .register_workflow::<DurableDocumentWorkflow>()
        .unwrap()
        .register_activities(DurablePageActivities::new(Arc::new(executor)))
        .build();
    let mut worker = Worker::new(&runtime, env.client().clone(), worker_options).unwrap();
    let shutdown = worker.shutdown_handle();

    tokio::task::LocalSet::new()
        .run_until(async move {
            let worker_task = tokio::task::spawn_local(async move { worker.run().await });
            let handle = env
                .client()
                .start_workflow(
                    DurableDocumentWorkflow::run,
                    DurableWorkflowRunInput::first(input),
                    WorkflowStartOptions::new(
                        "ocr-temporal-durable-postgres",
                        "ocr-temporal-durable-postgres-exhaustion",
                    )
                    .build(),
                )
                .await
                .unwrap();
            let result: DurableWorkflowResultMetadata = handle
                .get_result(WorkflowGetResultOptions::default())
                .await
                .unwrap();
            assert_eq!(result.status, DurableActivityStatus::Partial);
            assert_eq!(result.runner_iterations, 5);

            shutdown();
            worker_task.await.unwrap().unwrap();
            env.shutdown().await.unwrap();
        })
        .await;

    let tenant = TenantId::new("ten_TEMPORAL_BRIDGE").unwrap();
    let product = ProductId::new("kora").unwrap();
    let job = JobId::new("job_TEMPORAL_BRIDGE").unwrap();
    let artifacts = store
        .load_page_artifacts(&tenant, &product, &job)
        .await
        .unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0].page, 1);
    assert_eq!(artifacts[1].page, 3);
}
