use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use ocr_domain::{IdempotencyKey, JobId, ProductId, RequestDigest, TenantId, UploadId};
use ocr_service::{
    DurableWorkflowStarter, JobOutboxRelay, RelayOutcome, WorkflowAction, WorkflowDispatch,
    WorkflowDispatchError, WorkflowDispatchOutcome, WorkflowStarter,
};
use ocr_store::{CancelOutcome, CreateJob, PgJobStore};
use sqlx::PgPool;
use tokio::sync::Mutex;

struct RecordingStarter {
    dispatches: Mutex<Vec<WorkflowDispatch>>,
    inner: DurableWorkflowStarter,
}

impl WorkflowStarter for RecordingStarter {
    async fn dispatch(
        &self,
        dispatch: WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        self.dispatches.lock().await.push(dispatch.clone());
        self.inner.dispatch(dispatch).await
    }
}

struct FailOnceStarter {
    fail: AtomicBool,
    dispatches: Mutex<Vec<WorkflowDispatch>>,
}

impl WorkflowStarter for FailOnceStarter {
    async fn dispatch(
        &self,
        dispatch: WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        self.dispatches.lock().await.push(dispatch);
        if self.fail.swap(false, Ordering::SeqCst) {
            Err(WorkflowDispatchError::Unavailable)
        } else {
            Ok(WorkflowDispatchOutcome::Existing)
        }
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn relay_dispatches_a_deterministic_workflow_once_then_acknowledges() {
    let application = PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let admin = PgPool::connect(&std::env::var("TEST_DATABASE_ADMIN_URL").unwrap())
        .await
        .unwrap();
    sqlx::query("delete from ocr_page_workflows where job_id = 'job_RELAY'")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_outbox where job_id = 'job_RELAY'")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_jobs where job_id = 'job_RELAY'")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_RELAY'")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at, source_bucket, source_object_name, \
          source_object_generation, source_digest, source_content_length, accepted_at, \
          inspection_attempts, parser_page_count, parser_maximum_page_pixels, \
          parser_total_page_pixels, parser_page_geometries, parser_profile, parser_version) values \
         ('upl_RELAY', 'ten_RELAY', 'kora', 'relay-upload', $1, 'dev-kora-ocr-quarantine', \
          'products/kora/tenants/ten_RELAY/quarantine/upl_RELAY', 'application/pdf', 8, $2, \
          'accepted', now() + interval '10 minutes', 42, 'application/pdf', 8, $2, now(), \
          'dev-kora-ocr-source', 'products/kora/tenants/ten_RELAY/documents/source', 43, $2, 8, \
          now(), 1, 3, 1000000, 3000000, \
          '[{\"page\":1,\"width\":1000,\"height\":1000},{\"page\":2,\"width\":1000,\"height\":1000},{\"page\":3,\"width\":1000,\"height\":1000}]'::jsonb, \
          'strict-v1', '1.0.0')",
    )
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(format!("sha256:{}", "b".repeat(64)))
    .execute(&admin)
    .await
    .unwrap();
    let store = PgJobStore::new(application.clone());
    store
        .create(CreateJob {
            job_id: JobId::new("job_RELAY").unwrap(),
            tenant_id: TenantId::new("ten_RELAY").unwrap(),
            product_id: ProductId::new("kora").unwrap(),
            idempotency_key: IdempotencyKey::new("relay-job").unwrap(),
            request_digest: RequestDigest::new(&format!("sha256:{}", "c".repeat(64))).unwrap(),
            upload_id: UploadId::new("upl_RELAY").unwrap(),
            webhook_subscription_id: None,
        })
        .await
        .unwrap();
    let starter = Arc::new(RecordingStarter {
        dispatches: Mutex::new(Vec::new()),
        inner: DurableWorkflowStarter::new(store.clone(), 3).unwrap(),
    });
    let relay = JobOutboxRelay::new(store.clone(), Arc::clone(&starter));
    let tenant = TenantId::new("ten_RELAY").unwrap();
    let product = ProductId::new("kora").unwrap();

    assert_eq!(
        relay
            .relay_scope(&tenant, &product, "relay-01", 10)
            .await
            .unwrap(),
        RelayOutcome::Published(1)
    );
    assert_eq!(
        relay
            .relay_scope(&tenant, &product, "relay-01", 10)
            .await
            .unwrap(),
        RelayOutcome::Idle
    );
    let dispatches = starter.dispatches.lock().await;
    assert_eq!(dispatches.len(), 1);
    assert_eq!(
        dispatches[0].workflow_id,
        "ocr-v1-93c8e4e4759aa062d8f7e317c3278149"
    );
    assert_eq!(dispatches[0].job_id, JobId::new("job_RELAY").unwrap());
    assert_eq!(dispatches[0].page_count, 3);
    assert_eq!(dispatches[0].action, WorkflowAction::Start);
    drop(dispatches);
    assert!(store
        .load_page_workflow(&tenant, &product, &JobId::new("job_RELAY").unwrap())
        .await
        .unwrap()
        .is_some());

    assert!(matches!(
        store
            .cancel(&tenant, &product, &JobId::new("job_RELAY").unwrap())
            .await
            .unwrap(),
        CancelOutcome::Requested(_)
    ));
    let fail_once = Arc::new(FailOnceStarter {
        fail: AtomicBool::new(true),
        dispatches: Mutex::new(Vec::new()),
    });
    let recovering_relay =
        JobOutboxRelay::new(PgJobStore::new(application), Arc::clone(&fail_once));
    assert_eq!(
        recovering_relay
            .relay_scope(&tenant, &product, "relay-02", 10)
            .await
            .unwrap(),
        RelayOutcome::Retryable { published: 0 }
    );
    assert_eq!(
        recovering_relay
            .relay_scope(&tenant, &product, "relay-02", 10)
            .await
            .unwrap(),
        RelayOutcome::Published(1)
    );
    let retried = fail_once.dispatches.lock().await;
    assert_eq!(retried.len(), 2);
    assert_eq!(retried[0].workflow_id, retried[1].workflow_id);
    assert_eq!(retried[1].action, WorkflowAction::Cancel);
    let cancellation = retried[1].clone();
    drop(retried);

    let durable = DurableWorkflowStarter::new(store.clone(), 3).unwrap();
    assert_eq!(
        durable.dispatch(cancellation.clone()).await.unwrap(),
        WorkflowDispatchOutcome::Started
    );
    assert_eq!(
        durable.dispatch(cancellation).await.unwrap(),
        WorkflowDispatchOutcome::Existing
    );
    assert_eq!(
        store
            .load_page_workflow(&tenant, &product, &JobId::new("job_RELAY").unwrap())
            .await
            .unwrap()
            .unwrap()
            .workflow
            .status(),
        ocr_domain::PageWorkflowStatus::Cancelled
    );
    assert_eq!(
        store
            .find(&tenant, &product, &JobId::new("job_RELAY").unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ocr_domain::JobState::Cancelled
    );
}
