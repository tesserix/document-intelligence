use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use ocr_domain::WebhookSubscriptionId;
use ocr_service::{
    SignedWebhook, TerminalWebhookEvent, WebhookOutboxRelay, WebhookPublishError,
    WebhookPublishFuture, WebhookPublishOutcome, WebhookPublisher, WebhookRelayOutcome,
    WebhookSigner, WebhookSigningSecret,
};
use ocr_store::PgJobStore;
use sqlx::PgPool;
use tokio::sync::Mutex;

struct RecordingPublisher {
    signer: WebhookSigner,
    fail: AtomicBool,
    attempts: Mutex<Vec<(WebhookSubscriptionId, SignedWebhook)>>,
}

impl WebhookPublisher for RecordingPublisher {
    fn publish<'a>(
        &'a self,
        subscription_id: &'a WebhookSubscriptionId,
        event: &'a TerminalWebhookEvent,
    ) -> WebhookPublishFuture<'a> {
        Box::pin(async move {
            let signed = self
                .signer
                .sign(event)
                .map_err(|_| WebhookPublishError::Unavailable)?;
            self.attempts
                .lock()
                .await
                .push((subscription_id.clone(), signed));
            if self.fail.swap(false, Ordering::SeqCst) {
                Err(WebhookPublishError::Unavailable)
            } else {
                Ok(WebhookPublishOutcome::Delivered)
            }
        })
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn webhook_relay_replays_identically_and_acknowledges_only_success() {
    let application = PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let admin = PgPool::connect(&std::env::var("TEST_DATABASE_ADMIN_URL").unwrap())
        .await
        .unwrap();
    clear(&admin).await;
    seed(&admin).await;
    let publisher = Arc::new(RecordingPublisher {
        signer: WebhookSigner::new(
            WebhookSigningSecret::new(b"32-byte-webhook-replay-test-secret").unwrap(),
        ),
        fail: AtomicBool::new(true),
        attempts: Mutex::new(Vec::new()),
    });
    let relay = WebhookOutboxRelay::new(PgJobStore::new(application), Arc::clone(&publisher));
    let tenant = ocr_domain::TenantId::new("ten_WEBHOOK_RELAY").unwrap();
    let product = ocr_domain::ProductId::new("kora").unwrap();

    assert_eq!(
        relay
            .relay_scope(&tenant, &product, "webhook-relay", 10)
            .await
            .unwrap(),
        WebhookRelayOutcome::Retryable { published: 0 }
    );
    assert_eq!(
        relay
            .relay_scope(&tenant, &product, "webhook-relay", 10)
            .await
            .unwrap(),
        WebhookRelayOutcome::Published(1)
    );
    assert_eq!(
        relay
            .relay_scope(&tenant, &product, "webhook-relay", 10)
            .await
            .unwrap(),
        WebhookRelayOutcome::Idle
    );
    let attempts = publisher.attempts.lock().await;
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0].0,
        WebhookSubscriptionId::new("whs_RELAY").unwrap()
    );
    assert_eq!(attempts[0], attempts[1]);
}

async fn clear(admin: &PgPool) {
    sqlx::query("delete from ocr_outbox where job_id = 'job_WEBHOOK_RELAY'")
        .execute(admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_results where job_id = 'job_WEBHOOK_RELAY'")
        .execute(admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_jobs where job_id = 'job_WEBHOOK_RELAY'")
        .execute(admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_WEBHOOK_RELAY'")
        .execute(admin)
        .await
        .unwrap();
}

async fn seed(admin: &PgPool) {
    let digest = format!("sha256:{}", "a".repeat(64));
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at, source_bucket, source_object_name, \
          source_object_generation, source_digest, source_content_length, accepted_at, \
          inspection_attempts, parser_page_count, parser_maximum_page_pixels, \
          parser_total_page_pixels, parser_profile, parser_version) values \
         ('upl_WEBHOOK_RELAY', 'ten_WEBHOOK_RELAY', 'kora', 'webhook-relay-upload', $1, \
          'dev-kora-ocr-quarantine', \
          'products/kora/tenants/ten_WEBHOOK_RELAY/quarantine/upl_WEBHOOK_RELAY', \
          'application/pdf', 8, $1, 'accepted', now() + interval '10 minutes', 1, \
          'application/pdf', 8, $1, now(), 'dev-kora-ocr-source', \
          'products/kora/tenants/ten_WEBHOOK_RELAY/source/upl_WEBHOOK_RELAY', 2, $1, 8, now(), \
          1, 1, 1000000, 1000000, 'strict-v1', '1.0.0')",
    )
    .bind(&digest)
    .execute(admin)
    .await
    .unwrap();
    sqlx::query(
        "insert into ocr_jobs \
         (job_id, tenant_id, product_id, idempotency_key, request_digest, status, upload_id, \
          webhook_subscription_id) values \
         ('job_WEBHOOK_RELAY', 'ten_WEBHOOK_RELAY', 'kora', 'webhook-relay-job', $1, \
          'completed', 'upl_WEBHOOK_RELAY', 'whs_RELAY')",
    )
    .bind(&digest)
    .execute(admin)
    .await
    .unwrap();
    sqlx::query(
        "insert into ocr_results \
         (job_id, product_id, tenant_id, document_id, document_version, object_bucket, \
          object_name, object_generation, object_digest, content_length) values \
         ('job_WEBHOOK_RELAY', 'kora', 'ten_WEBHOOK_RELAY', 'doc_WEBHOOK_RELAY', $1, \
          'dev-kora-ocr-results', \
          'products/kora/tenants/ten_WEBHOOK_RELAY/results/job_WEBHOOK_RELAY/v1.json', \
          1, $1, 256)",
    )
    .bind(&digest)
    .execute(admin)
    .await
    .unwrap();
    sqlx::query(
        "insert into ocr_outbox (product_id, tenant_id, job_id, event_type, payload) values \
         ('kora', 'ten_WEBHOOK_RELAY', 'job_WEBHOOK_RELAY', 'ocr.job.completed.v1', \
          jsonb_build_object('job_id', 'job_WEBHOOK_RELAY', 'status', 'completed', \
          'document_version', $1::text, 'webhook_subscription_id', 'whs_RELAY', \
          'content_trust', 'untrusted'))",
    )
    .bind(digest)
    .execute(admin)
    .await
    .unwrap();
}
