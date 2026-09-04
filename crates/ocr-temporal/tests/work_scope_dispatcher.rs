use std::sync::Arc;

use ocr_domain::{ProductId, TenantId, UploadId};
use ocr_service::{
    ImportOutcome, JobOutboxRelay, WorkflowDispatch, WorkflowDispatchError,
    WorkflowDispatchOutcome, WorkflowStarter,
};
use ocr_store::{PgJobStore, PgWorkScopeDirectory};
use ocr_temporal::{ReconcileFuture, UploadReconciler, WorkScopeDispatcher};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::Mutex;

struct RecordingReconciler {
    uploads: Mutex<Vec<(ProductId, TenantId, UploadId)>>,
}

impl UploadReconciler for RecordingReconciler {
    fn reconcile<'a>(
        &'a self,
        tenant_id: &'a TenantId,
        product_id: &'a ProductId,
        upload_id: &'a UploadId,
        _lease_owner: &'a str,
    ) -> ReconcileFuture<'a> {
        Box::pin(async move {
            self.uploads.lock().await.push((
                product_id.clone(),
                tenant_id.clone(),
                upload_id.clone(),
            ));
            Ok(ImportOutcome::Accepted)
        })
    }
}

struct NoopStarter;

impl WorkflowStarter for NoopStarter {
    async fn dispatch(
        &self,
        _dispatch: WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        Ok(WorkflowDispatchOutcome::Existing)
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn dispatcher_claims_an_opaque_scope_then_reconciles_only_its_upload() {
    let application = PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let admin = PgPool::connect(&std::env::var("TEST_DATABASE_ADMIN_URL").unwrap())
        .await
        .unwrap();
    let product = ProductId::new("kora").unwrap();
    let tenant = TenantId::new("ten_SCOPE_DISPATCH").unwrap();
    let upload = UploadId::new("upl_SCOPE_DISPATCH").unwrap();
    sqlx::query("delete from ocr_work_scopes where product_id = $1 and tenant_id = $2")
        .bind(product.as_str())
        .bind(tenant.as_str())
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_upload_outbox where upload_id = $1")
        .bind(upload.as_str())
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = $1")
        .bind(upload.as_str())
        .execute(&admin)
        .await
        .unwrap();
    let bytes = b"%PDF-1.7";
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at) values \
         ($1, $2, $3, 'scope-dispatch', $4, 'dev-kora-ocr-quarantine', $5, \
          'application/pdf', $6, $7, 'uploaded', now() + interval '10 minutes', 1, \
          'application/pdf', $6, $7, now())",
    )
    .bind(upload.as_str())
    .bind(tenant.as_str())
    .bind(product.as_str())
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(format!(
        "products/{}/tenants/{}/quarantine/{}",
        product.as_str(),
        tenant.as_str(),
        upload.as_str()
    ))
    .bind(i64::try_from(bytes.len()).unwrap())
    .bind(&digest)
    .execute(&admin)
    .await
    .unwrap();
    sqlx::query(
        "insert into ocr_work_scopes (product_id, tenant_id, upload_pending) values ($1, $2, true)",
    )
    .bind(product.as_str())
    .bind(tenant.as_str())
    .execute(&admin)
    .await
    .unwrap();

    let reconciler = Arc::new(RecordingReconciler {
        uploads: Mutex::new(Vec::new()),
    });
    let jobs = PgJobStore::new(application.clone());
    let dispatcher = WorkScopeDispatcher::new(
        PgWorkScopeDirectory::new(application),
        jobs.clone(),
        Arc::clone(&reconciler),
        Arc::new(JobOutboxRelay::new(jobs, Arc::new(NoopStarter))),
        "scope-dispatcher",
        100,
        100,
    )
    .unwrap();

    let outcome = dispatcher.dispatch_once().await.unwrap();
    assert!(outcome.scopes >= 1);
    assert!(outcome.uploads >= 1);
    assert!(reconciler
        .uploads
        .lock()
        .await
        .contains(&(product, tenant, upload)));
}
