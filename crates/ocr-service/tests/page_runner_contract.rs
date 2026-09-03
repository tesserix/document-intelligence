use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use futures_util::FutureExt;
use ocr_domain::{
    IdempotencyKey, JobId, PageTask, PageWorkflow, PageWorkflowStatus, ProductId, RequestDigest,
    TenantId, UploadId,
};
use ocr_service::{
    CheckpointedPageRunner, PageProcessError, PageProcessFuture, PageProcessor, PageRunnerOutcome,
};
use ocr_store::{CreateJob, CreateOutcome, CreatePageWorkflowOutcome, PgJobStore};
use sqlx::PgPool;

#[derive(Clone, Default)]
struct RecordingProcessor {
    calls: Arc<Mutex<Vec<PageTask>>>,
    failures_remaining: Arc<Mutex<HashMap<u32, usize>>>,
}

impl RecordingProcessor {
    fn fail_retryably_once(page: u32) -> Self {
        Self {
            failures_remaining: Arc::new(Mutex::new(HashMap::from([(page, 1)]))),
            ..Self::default()
        }
    }

    fn pages(&self) -> Vec<u32> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|task| task.page)
            .collect()
    }
}

impl PageProcessor for RecordingProcessor {
    fn process<'a>(&'a self, task: PageTask) -> PageProcessFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(task.clone());
            let mut failures = self.failures_remaining.lock().unwrap();
            let remaining = failures.entry(task.page).or_default();
            if *remaining > 0 {
                *remaining -= 1;
                Err(PageProcessError::Retryable)
            } else {
                Ok(())
            }
        })
    }
}

#[derive(Default)]
struct CrashOnceProcessor {
    crashed: AtomicBool,
    calls: Mutex<Vec<PageTask>>,
}

impl PageProcessor for CrashOnceProcessor {
    fn process<'a>(&'a self, task: PageTask) -> PageProcessFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(task.clone());
            if task.page == 2 && !self.crashed.swap(true, Ordering::SeqCst) {
                panic!("injected worker crash");
            }
            Ok(())
        })
    }
}

struct CancelOnProcess<'a> {
    store: &'a PgJobStore,
    tenant: &'a TenantId,
    product: &'a ProductId,
    job: &'a JobId,
}

impl PageProcessor for CancelOnProcess<'_> {
    fn process<'a>(&'a self, _task: PageTask) -> PageProcessFuture<'a> {
        Box::pin(async move {
            let mut stored = self
                .store
                .load_page_workflow(self.tenant, self.product, self.job)
                .await
                .unwrap()
                .unwrap();
            stored.workflow.request_cancellation();
            self.store
                .save_page_workflow(
                    self.tenant,
                    self.product,
                    self.job,
                    stored.revision,
                    stored.workflow,
                )
                .await
                .unwrap();
            Ok(())
        })
    }
}

async fn store() -> (PgJobStore, PgPool) {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set");
    let admin_url =
        std::env::var("TEST_DATABASE_ADMIN_URL").expect("TEST_DATABASE_ADMIN_URL must be set");
    let pool = PgPool::connect(&url).await.unwrap();
    let admin_pool = PgPool::connect(&admin_url).await.unwrap();
    (PgJobStore::new(pool), admin_pool)
}

async fn seed_job(store: &PgJobStore, admin_pool: &PgPool, job_id: &str, tenant_id: &str) {
    let upload_id =
        UploadId::new(&format!("upl_{}", tenant_id.trim_start_matches("ten_"))).unwrap();
    sqlx::query("delete from ocr_page_workflows where job_id = $1")
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
    sqlx::query("delete from ocr_uploads where upload_id = $1")
        .bind(upload_id.as_str())
        .execute(admin_pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into ocr_uploads (upload_id, tenant_id, product_id, idempotency_key, \
         request_digest, object_bucket, object_name, expected_content_type, \
         expected_content_length, expected_digest, status, expires_at, object_generation, \
         verified_content_type, verified_content_length, verified_digest, uploaded_at, \
         source_bucket, source_object_name, source_object_generation, source_digest, \
         source_content_length, inspection_attempts, accepted_at) values \
         ($1, $2, 'kora', $3, $4, 'dev-kora-ocr-quarantine', $5, 'application/pdf', 8, $6, \
          'accepted', now() + interval '10 minutes', 1, 'application/pdf', 8, $6, now(), \
          'dev-kora-ocr-source', $7, 2, $6, 8, 1, now())",
    )
    .bind(upload_id.as_str())
    .bind(tenant_id)
    .bind(format!("seed-{tenant_id}"))
    .bind(format!("sha256:{}", "e".repeat(64)))
    .bind(format!(
        "products/kora/tenants/{tenant_id}/{}",
        upload_id.as_str()
    ))
    .bind(format!("sha256:{}", "f".repeat(64)))
    .bind(format!(
        "products/kora/tenants/{tenant_id}/{}/accepted",
        upload_id.as_str()
    ))
    .execute(admin_pool)
    .await
    .unwrap();

    let created = store
        .create(CreateJob {
            job_id: JobId::new(job_id).unwrap(),
            tenant_id: TenantId::new(tenant_id).unwrap(),
            product_id: ProductId::new("kora").unwrap(),
            idempotency_key: IdempotencyKey::new(&format!("runner-{job_id}")).unwrap(),
            request_digest: RequestDigest::new(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            upload_id,
        })
        .await
        .unwrap();
    assert!(matches!(created, CreateOutcome::Created(_)));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn retries_only_failed_page_and_preserves_stable_activity_key() {
    let (store, admin_pool) = store().await;
    let tenant = TenantId::new("ten_PAGE_RUNNER").unwrap();
    let product = ProductId::new("kora").unwrap();
    let job = JobId::new("job_PAGE_RUNNER").unwrap();
    seed_job(&store, &admin_pool, job.as_str(), tenant.as_str()).await;
    let workflow = PageWorkflow::new(job.clone(), 3, 3).unwrap();
    assert!(matches!(
        store
            .create_page_workflow(&tenant, &product, &job, workflow)
            .await
            .unwrap(),
        CreatePageWorkflowOutcome::Created(_)
    ));
    let processor = RecordingProcessor::fail_retryably_once(2);
    let runner = CheckpointedPageRunner::new(&store, &processor, 3, 2).unwrap();

    assert_eq!(
        runner.run_once(&tenant, &product, &job).await.unwrap(),
        PageRunnerOutcome::Progressed(PageWorkflowStatus::Running)
    );
    assert_eq!(
        runner.run_once(&tenant, &product, &job).await.unwrap(),
        PageRunnerOutcome::Progressed(PageWorkflowStatus::Completed)
    );
    assert_eq!(processor.pages(), vec![1, 2, 3, 2]);

    let calls = processor.calls.lock().unwrap();
    assert_eq!(
        calls[1].activity_key,
        "ocr-job-job_PAGE_RUNNER-page-2-attempt-1"
    );
    assert_eq!(
        calls[3].activity_key,
        "ocr-job-job_PAGE_RUNNER-page-2-attempt-2"
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn crash_replays_the_same_activity_keys_without_advancing_attempts() {
    let (store, admin_pool) = store().await;
    let tenant = TenantId::new("ten_PAGE_CRASH").unwrap();
    let product = ProductId::new("kora").unwrap();
    let job = JobId::new("job_PAGE_CRASH").unwrap();
    seed_job(&store, &admin_pool, job.as_str(), tenant.as_str()).await;
    store
        .create_page_workflow(
            &tenant,
            &product,
            &job,
            PageWorkflow::new(job.clone(), 3, 3).unwrap(),
        )
        .await
        .unwrap();
    let processor = CrashOnceProcessor::default();
    let runner = CheckpointedPageRunner::new(&store, &processor, 3, 1).unwrap();

    assert!(
        std::panic::AssertUnwindSafe(runner.run_once(&tenant, &product, &job))
            .catch_unwind()
            .await
            .is_err()
    );
    assert_eq!(
        runner.run_once(&tenant, &product, &job).await.unwrap(),
        PageRunnerOutcome::Progressed(PageWorkflowStatus::Completed)
    );

    let calls = processor.calls.lock().unwrap();
    let first_page_one = calls.iter().find(|task| task.page == 1).unwrap();
    let replayed_page_one = calls.iter().rfind(|task| task.page == 1).unwrap();
    assert_eq!(first_page_one.activity_key, replayed_page_one.activity_key);
    assert_eq!(first_page_one.attempt, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn concurrent_cancellation_wins_and_rejects_stale_page_result() {
    let (store, admin_pool) = store().await;
    let tenant = TenantId::new("ten_PAGE_CANCEL_RUNNER").unwrap();
    let product = ProductId::new("kora").unwrap();
    let job = JobId::new("job_PAGE_CANCEL_RUNNER").unwrap();
    seed_job(&store, &admin_pool, job.as_str(), tenant.as_str()).await;
    store
        .create_page_workflow(
            &tenant,
            &product,
            &job,
            PageWorkflow::new(job.clone(), 2, 3).unwrap(),
        )
        .await
        .unwrap();
    let processor = CancelOnProcess {
        store: &store,
        tenant: &tenant,
        product: &product,
        job: &job,
    };
    let runner = CheckpointedPageRunner::new(&store, &processor, 1, 1).unwrap();

    assert!(matches!(
        runner.run_once(&tenant, &product, &job).await,
        Err(ocr_service::PageRunnerError::RetryableConflict)
    ));
    assert_eq!(
        runner.run_once(&tenant, &product, &job).await.unwrap(),
        PageRunnerOutcome::Idle(PageWorkflowStatus::Cancelled)
    );
}
