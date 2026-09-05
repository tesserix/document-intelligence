//! PostgreSQL persistence for document jobs.

use ocr_domain::{
    DocumentId, DocumentVersion, IdempotencyKey, JobId, JobState, PageGeometry, PageTask,
    PageWorkflow, PageWorkflowStatus, ProductId, RequestDigest, TenantId, UploadId,
    WebhookSubscriptionId,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("idempotency key was reused with different input")]
    IdempotencyConflict,
    #[error("stored job is invalid")]
    InvalidStoredJob,
    #[error("stored result is invalid")]
    InvalidStoredResult,
    #[error("stored upload is invalid")]
    InvalidStoredUpload,
    #[error("upload source is unavailable")]
    UploadSourceUnavailable,
    #[error("stored outbox event is invalid")]
    InvalidOutboxEvent,
    #[error("stored page workflow is invalid")]
    InvalidStoredPageWorkflow,
    #[error("stored page artifact is invalid")]
    InvalidStoredPageArtifact,
    #[error("work scope is invalid")]
    InvalidWorkScope,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct CreateJob {
    pub job_id: JobId,
    pub tenant_id: TenantId,
    pub product_id: ProductId,
    pub idempotency_key: IdempotencyKey,
    pub request_digest: RequestDigest,
    pub upload_id: UploadId,
    pub webhook_subscription_id: Option<WebhookSubscriptionId>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateOutcome {
    Created(StoredJob),
    Existing(StoredJob),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    Requested(StoredJob),
    Existing(StoredJob),
    NotCancellable(StoredJob),
    NotFound,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompleteCancellationOutcome {
    Cancelled(StoredJob),
    Existing(StoredJob),
    NotCancellable(StoredJob),
    NotFound,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StoredJob {
    pub job_id: JobId,
    pub state: JobState,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDocumentIdentity {
    pub document_id: DocumentId,
    pub document_version: DocumentVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAcceptedSource {
    pub bucket: String,
    pub object_name: String,
    pub generation: i64,
    pub digest: String,
    pub content_length: i64,
    pub content_type: String,
    pub page_count: u32,
    pub maximum_page_pixels: u64,
    pub total_page_pixels: u64,
    pub page_geometries: Vec<PageGeometry>,
    pub parser_profile: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPageWorkflow {
    pub workflow: PageWorkflow,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPageArtifact {
    pub page: u32,
    pub attempt: u8,
    pub activity_key: String,
    pub object_bucket: String,
    pub object_name: String,
    pub object_generation: i64,
    pub object_digest: String,
    pub content_length: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreatePageWorkflowOutcome {
    Created(StoredPageWorkflow),
    Existing(StoredPageWorkflow),
    Conflict,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavePageWorkflowOutcome {
    Saved(StoredPageWorkflow),
    Conflict,
    NotFound,
}

#[derive(Debug)]
pub struct ClaimJobOutbox {
    pub lease_owner: String,
    pub limit: i64,
}

#[derive(Debug)]
pub struct ClaimWorkScopes {
    pub lease_owner: String,
    pub limit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredWorkScope {
    pub product_id: ProductId,
    pub tenant_id: TenantId,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum JobOutboxEventType {
    Accepted,
    CancellationRequested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredJobOutboxEvent {
    pub event_id: i64,
    pub job_id: JobId,
    pub event_type: JobOutboxEventType,
    pub page_count: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WebhookOutboxEventType {
    Completed,
    Partial,
    ReviewRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredWebhookOutboxEvent {
    pub event_id: i64,
    pub job_id: JobId,
    pub event_type: WebhookOutboxEventType,
    pub webhook_subscription_id: WebhookSubscriptionId,
    pub document_version: DocumentVersion,
    pub occurred_at: OffsetDateTime,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PublishJobOutboxOutcome {
    Published,
    Existing,
    LeaseLost,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResultLocator {
    pub document_id: DocumentId,
    pub document_version: DocumentVersion,
    pub object_bucket: String,
    pub object_name: String,
    pub object_generation: i64,
    pub object_digest: String,
    pub content_length: i64,
}

#[derive(Debug)]
pub struct CommitResult {
    pub terminal_state: JobState,
    pub locator: StoredResultLocator,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommitResultOutcome {
    Committed(StoredJob),
    Existing(StoredJob),
    Conflict,
    NotCommittable,
    NotFound,
}

#[derive(Debug)]
pub struct CreateUpload {
    pub upload_id: UploadId,
    pub tenant_id: TenantId,
    pub product_id: ProductId,
    pub idempotency_key: IdempotencyKey,
    pub request_digest: RequestDigest,
    pub object_bucket: String,
    pub object_name: String,
    pub expected_content_type: String,
    pub expected_content_length: i64,
    pub expected_digest: String,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateUploadOutcome {
    Created(StoredUpload),
    Existing(StoredUpload),
}

#[derive(Debug)]
pub struct RecordUpload {
    pub object_generation: i64,
    pub verified_content_type: String,
    pub verified_content_length: i64,
    pub verified_digest: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordUploadOutcome {
    Recorded(StoredUpload),
    Existing(StoredUpload),
    Expired,
    VerificationMismatch,
    NotRecordable,
    NotFound,
}

#[derive(Debug)]
pub struct ParserInspectionMetadata {
    pub page_count: i32,
    pub maximum_page_pixels: i64,
    pub total_page_pixels: i64,
    pub page_geometries: Vec<PageGeometry>,
    pub profile: String,
    pub version: String,
}

#[derive(Debug)]
pub struct AcceptUpload {
    pub inspection_lease_owner: String,
    pub source_bucket: String,
    pub source_object_name: String,
    pub source_object_generation: i64,
    pub source_digest: String,
    pub source_content_length: i64,
    pub parser_inspection: ParserInspectionMetadata,
}

#[derive(Debug)]
pub struct ClaimUploadInspection {
    pub lease_owner: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimUploadInspectionOutcome {
    Claimed,
    Existing,
    Busy,
    AttemptsExhausted,
    NotInspectable,
    NotFound,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AcceptUploadOutcome {
    Accepted,
    Existing,
    SourceMismatch,
    NotAcceptable,
    NotFound,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum UploadRejectionReason {
    MalwareDetected,
    InvalidDocument,
    ParserLimitsExceeded,
    PasswordRequired,
    SourceConflict,
}

impl UploadRejectionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::MalwareDetected => "malware_detected",
            Self::InvalidDocument => "invalid_document",
            Self::ParserLimitsExceeded => "parser_limits_exceeded",
            Self::PasswordRequired => "password_required",
            Self::SourceConflict => "source_conflict",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RejectUploadOutcome {
    Rejected,
    Existing,
    ReasonMismatch,
    NotRejectable,
    NotFound,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum UploadState {
    Reserved,
    Uploaded,
    Inspecting,
    Accepted,
    Rejected,
    Expired,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StoredUpload {
    pub upload_id: UploadId,
    pub state: UploadState,
    pub object_bucket: String,
    pub object_name: String,
    pub expected_content_type: String,
    pub expected_content_length: i64,
    pub expected_digest: String,
    pub expires_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub object_generation: Option<i64>,
    pub uploaded_at: Option<OffsetDateTime>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResultLookup {
    Ready(StoredResultLocator),
    NotReady(JobState),
    Unavailable(JobState),
    NotFound,
}

#[derive(Debug, Clone)]
pub struct PgJobStore {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct PgWorkScopeDirectory {
    pool: PgPool,
}

impl PgWorkScopeDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn claim(&self, claim: ClaimWorkScopes) -> Result<Vec<StoredWorkScope>> {
        validate_lease_owner(&claim.lease_owner)?;
        if !(1..=100).contains(&claim.limit) {
            return Err(Error::InvalidWorkScope);
        }
        let rows =
            sqlx::query("select product_id, tenant_id from ocr_claim_work_scopes($1, $2::integer)")
                .bind(&claim.lease_owner)
                .bind(claim.limit)
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter().map(stored_work_scope).collect()
    }

    pub async fn release(&self, scope: &StoredWorkScope, lease_owner: &str) -> Result<bool> {
        validate_lease_owner(lease_owner)?;
        let released = sqlx::query_scalar("select ocr_release_work_scope($1, $2, $3)")
            .bind(scope.product_id.as_str())
            .bind(scope.tenant_id.as_str())
            .bind(lease_owner)
            .fetch_optional(&self.pool)
            .await?;
        Ok(released.unwrap_or(false))
    }
}

impl PgJobStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn ready(&self) -> Result<()> {
        sqlx::query("select 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn create_page_workflow(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
        workflow: PageWorkflow,
    ) -> Result<CreatePageWorkflowOutcome> {
        if workflow.job_id() != job_id {
            return Err(Error::InvalidStoredPageWorkflow);
        }
        let cancellation_checkpoint = workflow.status() == PageWorkflowStatus::Cancelled;
        let checkpoint = serialize_page_workflow(&workflow)?;
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let inserted = sqlx::query(
            "insert into ocr_page_workflows \
             (job_id, product_id, tenant_id, checkpoint) \
             select jobs.job_id, jobs.product_id, jobs.tenant_id, $4::jsonb \
             from ocr_jobs as jobs where jobs.job_id = $1 and jobs.product_id = $2 \
             and jobs.tenant_id = $3 and (jobs.status in ('accepted', 'processing') \
                 or ($5 and jobs.status = 'cancelling')) \
             on conflict (job_id) do nothing \
             returning revision, checkpoint::text as checkpoint",
        )
        .bind(job_id.as_str())
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(&checkpoint)
        .bind(cancellation_checkpoint)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = inserted {
            let stored = stored_page_workflow(row)?;
            if cancellation_checkpoint {
                transaction.commit().await?;
                return Ok(CreatePageWorkflowOutcome::Created(stored));
            }
            let transitioned = sqlx::query(
                "update ocr_jobs set status = 'processing', updated_at = now() \
                 where job_id = $1 and product_id = $2 and tenant_id = $3 \
                 and status in ('accepted', 'processing') returning job_id",
            )
            .bind(job_id.as_str())
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .fetch_optional(&mut *transaction)
            .await?;
            if transitioned.is_none() {
                return Err(Error::InvalidStoredPageWorkflow);
            }
            transaction.commit().await?;
            return Ok(CreatePageWorkflowOutcome::Created(stored));
        }
        let existing =
            load_page_workflow_row(&mut transaction, tenant_id, product_id, job_id).await?;
        transaction.commit().await?;
        Ok(match existing {
            Some(stored) if stored.workflow == workflow => {
                CreatePageWorkflowOutcome::Existing(stored)
            }
            Some(_) => CreatePageWorkflowOutcome::Conflict,
            None => CreatePageWorkflowOutcome::NotFound,
        })
    }

    pub async fn load_page_workflow(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<Option<StoredPageWorkflow>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let stored =
            load_page_workflow_row(&mut transaction, tenant_id, product_id, job_id).await?;
        transaction.commit().await?;
        Ok(stored)
    }

    pub async fn load_page_artifacts(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<Vec<StoredPageArtifact>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let artifacts = sqlx::query(
            "select page_number, attempt, activity_key, object_bucket, object_name, \
             object_generation, object_digest, content_length from ocr_page_artifacts \
             where job_id = $1 and product_id = $2 and tenant_id = $3 order by page_number",
        )
        .bind(job_id.as_str())
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .fetch_all(&mut *transaction)
        .await?
        .into_iter()
        .map(stored_page_artifact)
        .collect::<Result<Vec<_>>>()?;
        transaction.commit().await?;
        Ok(artifacts)
    }

    pub async fn save_page_workflow(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
        expected_revision: i64,
        workflow: PageWorkflow,
    ) -> Result<SavePageWorkflowOutcome> {
        self.save_page_workflow_with_artifacts(
            tenant_id,
            product_id,
            job_id,
            expected_revision,
            workflow,
            Vec::new(),
        )
        .await
    }

    pub async fn save_page_workflow_with_artifacts(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
        expected_revision: i64,
        workflow: PageWorkflow,
        artifacts: Vec<StoredPageArtifact>,
    ) -> Result<SavePageWorkflowOutcome> {
        if expected_revision < 0 || workflow.job_id() != job_id {
            return Err(Error::InvalidStoredPageWorkflow);
        }
        validate_page_artifact_batch(job_id, &workflow, &artifacts)?;
        let checkpoint = serialize_page_workflow(&workflow)?;
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let revision = sqlx::query_scalar::<_, i64>(
            "select revision from ocr_page_workflows where job_id = $1 and product_id = $2 \
             and tenant_id = $3 for update",
        )
        .bind(job_id.as_str())
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(revision) = revision else {
            transaction.commit().await?;
            return Ok(SavePageWorkflowOutcome::NotFound);
        };
        if revision != expected_revision {
            transaction.commit().await?;
            return Ok(SavePageWorkflowOutcome::Conflict);
        }
        if !artifacts.is_empty() {
            let pages = artifacts
                .iter()
                .map(|artifact| i32::try_from(artifact.page))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| Error::InvalidStoredPageArtifact)?;
            let attempts = artifacts
                .iter()
                .map(|artifact| i16::from(artifact.attempt))
                .collect::<Vec<_>>();
            let activity_keys = artifacts
                .iter()
                .map(|artifact| artifact.activity_key.clone())
                .collect::<Vec<_>>();
            let buckets = artifacts
                .iter()
                .map(|artifact| artifact.object_bucket.clone())
                .collect::<Vec<_>>();
            let names = artifacts
                .iter()
                .map(|artifact| artifact.object_name.clone())
                .collect::<Vec<_>>();
            let generations = artifacts
                .iter()
                .map(|artifact| artifact.object_generation)
                .collect::<Vec<_>>();
            let digests = artifacts
                .iter()
                .map(|artifact| artifact.object_digest.clone())
                .collect::<Vec<_>>();
            let lengths = artifacts
                .iter()
                .map(|artifact| artifact.content_length)
                .collect::<Vec<_>>();
            sqlx::query(
                "insert into ocr_page_artifacts (job_id, product_id, tenant_id, page_number, \
                 attempt, activity_key, object_bucket, object_name, object_generation, \
                 object_digest, content_length) select $1, $2, $3, batch.* from unnest( \
                 $4::integer[], $5::smallint[], $6::text[], $7::text[], $8::text[], \
                 $9::bigint[], $10::text[], $11::bigint[]) as batch",
            )
            .bind(job_id.as_str())
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(pages)
            .bind(attempts)
            .bind(activity_keys)
            .bind(buckets)
            .bind(names)
            .bind(generations)
            .bind(digests)
            .bind(lengths)
            .execute(&mut *transaction)
            .await?;
        }
        let updated = sqlx::query(
            "update ocr_page_workflows set checkpoint = $5::jsonb, revision = revision + 1, \
             updated_at = now() where job_id = $1 and product_id = $2 and tenant_id = $3 \
             and revision = $4 returning revision, checkpoint::text as checkpoint",
        )
        .bind(job_id.as_str())
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(expected_revision)
        .bind(&checkpoint)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = updated {
            let stored = stored_page_workflow(row)?;
            transaction.commit().await?;
            return Ok(SavePageWorkflowOutcome::Saved(stored));
        }
        Err(Error::InvalidStoredPageWorkflow)
    }

    pub async fn claim_job_outbox(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        claim: ClaimJobOutbox,
    ) -> Result<Vec<StoredJobOutboxEvent>> {
        validate_lease_owner(&claim.lease_owner)?;
        if !(1..=100).contains(&claim.limit) {
            return Err(Error::InvalidOutboxEvent);
        }
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        sqlx::query(
            "update ocr_outbox set dead_lettered_at = now(), delivery_lease_owner = null, \
             delivery_lease_expires_at = null where product_id = $1 and tenant_id = $2 \
             and published_at is null and dead_lettered_at is null and delivery_attempts = 20 \
             and delivery_lease_expires_at <= now()",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .execute(&mut *transaction)
        .await?;
        let rows = sqlx::query(
            "with candidates as ( \
                select event_id from ocr_outbox where product_id = $1 and tenant_id = $2 \
                and event_type in ('ocr.job.accepted.v1', \
                    'ocr.job.cancellation_requested.v1') \
                and published_at is null and dead_lettered_at is null and delivery_attempts < 20 \
                and (delivery_lease_owner = $3 or delivery_lease_expires_at is null \
                    or delivery_lease_expires_at <= now()) \
                order by event_id for update skip locked limit $4 \
             ), updated as (update ocr_outbox as events set \
                delivery_attempts = case \
                    when events.delivery_lease_owner = $3 \
                        and events.delivery_lease_expires_at > now() \
                    then events.delivery_attempts else events.delivery_attempts + 1 end, \
                delivery_lease_owner = $3, \
                delivery_lease_expires_at = now() + interval '5 minutes' \
             from candidates where events.event_id = candidates.event_id \
             returning events.event_id, events.job_id, events.event_type, events.product_id, \
                 events.tenant_id) \
             select updated.event_id, updated.job_id, updated.event_type, uploads.parser_page_count \
             from updated join ocr_jobs jobs on jobs.job_id = updated.job_id \
                 and jobs.product_id = updated.product_id and jobs.tenant_id = updated.tenant_id \
             join ocr_uploads uploads on uploads.upload_id = jobs.upload_id \
                 and uploads.product_id = jobs.product_id and uploads.tenant_id = jobs.tenant_id",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(&claim.lease_owner)
        .bind(claim.limit)
        .fetch_all(&mut *transaction)
        .await?;
        if rows.is_empty() {
            let pending: bool = sqlx::query_scalar(
                "select exists(select 1 from ocr_outbox where product_id = $1 and tenant_id = $2 \
                 and event_type in ('ocr.job.accepted.v1', \
                     'ocr.job.cancellation_requested.v1') and published_at is null \
                 and dead_lettered_at is null and delivery_attempts < 20)",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .fetch_one(&mut *transaction)
            .await?;
            set_work_scope_pending(&mut transaction, product_id, tenant_id, "dispatch", pending)
                .await?;
        }
        let events = rows
            .into_iter()
            .map(stored_job_outbox_event)
            .collect::<Result<Vec<_>>>()?;
        transaction.commit().await?;
        Ok(events)
    }

    pub async fn claim_webhook_outbox(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        claim: ClaimJobOutbox,
    ) -> Result<Vec<StoredWebhookOutboxEvent>> {
        validate_lease_owner(&claim.lease_owner)?;
        if !(1..=100).contains(&claim.limit) {
            return Err(Error::InvalidOutboxEvent);
        }
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        sqlx::query(
            "update ocr_outbox set dead_lettered_at = now(), delivery_lease_owner = null, \
             delivery_lease_expires_at = null where product_id = $1 and tenant_id = $2 \
             and published_at is null and dead_lettered_at is null and delivery_attempts = 20 \
             and delivery_lease_expires_at <= now()",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .execute(&mut *transaction)
        .await?;
        let rows = sqlx::query(
            "with candidates as ( \
                select event_id from ocr_outbox where product_id = $1 and tenant_id = $2 \
                and event_type in ('ocr.job.completed.v1', 'ocr.job.partial.v1', \
                    'ocr.job.review_required.v1') \
                and published_at is null and dead_lettered_at is null and delivery_attempts < 20 \
                and (delivery_lease_owner = $3 or delivery_lease_expires_at is null \
                    or delivery_lease_expires_at <= now()) \
                order by event_id for update skip locked limit $4 \
             ), updated as (update ocr_outbox as events set \
                delivery_attempts = case \
                    when events.delivery_lease_owner = $3 \
                        and events.delivery_lease_expires_at > now() \
                    then events.delivery_attempts else events.delivery_attempts + 1 end, \
                delivery_lease_owner = $3, \
                delivery_lease_expires_at = now() + interval '5 minutes' \
             from candidates where events.event_id = candidates.event_id \
             returning events.event_id, events.job_id, events.event_type, events.product_id, \
                 events.tenant_id, events.created_at) \
             select updated.event_id, updated.job_id, updated.event_type, updated.created_at, \
                 jobs.webhook_subscription_id, results.document_version \
             from updated join ocr_jobs jobs on jobs.job_id = updated.job_id \
                 and jobs.product_id = updated.product_id and jobs.tenant_id = updated.tenant_id \
             join ocr_results results on results.job_id = jobs.job_id \
                 and results.product_id = jobs.product_id and results.tenant_id = jobs.tenant_id \
             where jobs.webhook_subscription_id is not null",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(&claim.lease_owner)
        .bind(claim.limit)
        .fetch_all(&mut *transaction)
        .await?;
        let events = rows
            .into_iter()
            .map(stored_webhook_outbox_event)
            .collect::<Result<Vec<_>>>()?;
        transaction.commit().await?;
        Ok(events)
    }

    pub async fn publish_job_outbox(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        event_id: i64,
        lease_owner: &str,
    ) -> Result<PublishJobOutboxOutcome> {
        validate_lease_owner(lease_owner)?;
        if event_id <= 0 {
            return Err(Error::InvalidOutboxEvent);
        }
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let updated = sqlx::query(
            "update ocr_outbox set published_at = now(), delivery_lease_owner = null, \
             delivery_lease_expires_at = null where product_id = $1 and tenant_id = $2 \
             and event_id = $3 and published_at is null and dead_lettered_at is null \
             and delivery_lease_owner = $4 and delivery_lease_expires_at > now() \
             returning event_id",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(event_id)
        .bind(lease_owner)
        .fetch_optional(&mut *transaction)
        .await?;
        if updated.is_some() {
            let pending: bool = sqlx::query_scalar(
                "select exists(select 1 from ocr_outbox where product_id = $1 and tenant_id = $2 \
                 and event_type in ('ocr.job.accepted.v1', \
                     'ocr.job.cancellation_requested.v1') and published_at is null \
                 and dead_lettered_at is null and delivery_attempts < 20)",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .fetch_one(&mut *transaction)
            .await?;
            set_work_scope_pending(&mut transaction, product_id, tenant_id, "dispatch", pending)
                .await?;
            transaction.commit().await?;
            return Ok(PublishJobOutboxOutcome::Published);
        }
        let existing = sqlx::query(
            "select published_at from ocr_outbox where product_id = $1 and tenant_id = $2 \
             and event_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(event_id)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(match existing {
            Some(row)
                if row
                    .try_get::<Option<OffsetDateTime>, _>("published_at")?
                    .is_some() =>
            {
                PublishJobOutboxOutcome::Existing
            }
            Some(_) => PublishJobOutboxOutcome::LeaseLost,
            None => PublishJobOutboxOutcome::NotFound,
        })
    }

    pub async fn create_upload(&self, request: CreateUpload) -> Result<CreateUploadOutcome> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, &request.tenant_id, &request.product_id).await?;
        let inserted = sqlx::query(
            "insert into ocr_uploads \
             (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
              object_name, expected_content_type, expected_content_length, expected_digest, expires_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             on conflict (product_id, tenant_id, idempotency_key) do nothing \
             returning upload_id, object_bucket, object_name, expected_content_type, \
             expected_content_length, expected_digest, expires_at, created_at, status::text as status, \
             object_generation, uploaded_at",
        )
        .bind(request.upload_id.as_str())
        .bind(request.tenant_id.as_str())
        .bind(request.product_id.as_str())
        .bind(request.idempotency_key.as_str())
        .bind(request.request_digest.as_str())
        .bind(&request.object_bucket)
        .bind(&request.object_name)
        .bind(&request.expected_content_type)
        .bind(request.expected_content_length)
        .bind(&request.expected_digest)
        .bind(request.expires_at)
        .fetch_optional(&mut *transaction)
        .await?;

        if let Some(inserted) = inserted {
            let upload = stored_upload(inserted)?;
            transaction.commit().await?;
            return Ok(CreateUploadOutcome::Created(upload));
        }

        let existing = sqlx::query(
            "select upload_id, object_bucket, object_name, expected_content_type, \
             expected_content_length, expected_digest, expires_at, created_at, request_digest, \
             status::text as status, object_generation, uploaded_at \
             from ocr_uploads where product_id = $1 and tenant_id = $2 and idempotency_key = $3",
        )
        .bind(request.product_id.as_str())
        .bind(request.tenant_id.as_str())
        .bind(request.idempotency_key.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        let existing_digest: &str = existing.try_get("request_digest")?;
        if existing_digest != request.request_digest.as_str() {
            return Err(Error::IdempotencyConflict);
        }
        let upload = stored_upload(existing)?;
        transaction.commit().await?;
        Ok(CreateUploadOutcome::Existing(upload))
    }

    pub async fn find_upload(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
    ) -> Result<Option<StoredUpload>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select upload_id, object_bucket, object_name, expected_content_type, \
             expected_content_length, expected_digest, expires_at, created_at, status::text as status, \
             object_generation, uploaded_at from ocr_uploads \
             where product_id = $1 and tenant_id = $2 and upload_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        row.map(stored_upload).transpose()
    }

    pub async fn list_reconcilable_uploads(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        limit: i64,
    ) -> Result<Vec<UploadId>> {
        if !(1..=100).contains(&limit) {
            return Err(Error::InvalidStoredUpload);
        }
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let rows = sqlx::query(
            "select upload_id from ocr_uploads where product_id = $1 and tenant_id = $2 \
             and (status = 'uploaded' or (status = 'inspecting' \
                 and inspection_lease_expires_at <= now())) \
             order by uploaded_at, upload_id limit $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        rows.into_iter()
            .map(|row| {
                UploadId::new(row.try_get("upload_id")?).map_err(|_| Error::InvalidStoredUpload)
            })
            .collect()
    }

    pub async fn load_claimed_upload(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
        lease_owner: &str,
    ) -> Result<Option<StoredUpload>> {
        validate_lease_owner(lease_owner)?;
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select upload_id, object_bucket, object_name, expected_content_type, \
             expected_content_length, expected_digest, expires_at, created_at, \
             status::text as status, object_generation, uploaded_at from ocr_uploads \
             where product_id = $1 and tenant_id = $2 and upload_id = $3 \
             and status = 'inspecting' and inspection_lease_owner = $4 \
             and inspection_lease_expires_at > now()",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .bind(lease_owner)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        row.map(stored_upload).transpose()
    }

    pub async fn record_uploaded(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
        record: RecordUpload,
    ) -> Result<RecordUploadOutcome> {
        validate_record(&record)?;
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let updated = sqlx::query(
            "update ocr_uploads set status = 'uploaded', object_generation = $4, \
             verified_content_type = $5, verified_content_length = $6, verified_digest = $7, \
             uploaded_at = now(), updated_at = now() \
             where product_id = $1 and tenant_id = $2 and upload_id = $3 \
             and status = 'reserved' and expires_at > now() \
             and expected_content_type = $5 and expected_content_length = $6 \
             and expected_digest = $7 \
             returning upload_id, object_bucket, object_name, expected_content_type, \
             expected_content_length, expected_digest, expires_at, created_at, status::text as status, \
             object_generation, uploaded_at",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .bind(record.object_generation)
        .bind(&record.verified_content_type)
        .bind(record.verified_content_length)
        .bind(&record.verified_digest)
        .fetch_optional(&mut *transaction)
        .await?;

        if let Some(updated) = updated {
            sqlx::query(
                "insert into ocr_upload_outbox \
                 (product_id, tenant_id, upload_id, event_type, payload) \
                 values ($1, $2, $3, 'ocr.upload.received.v1', \
                 jsonb_build_object('upload_id', $3::text, 'status', 'uploaded'))",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(upload_id.as_str())
            .execute(&mut *transaction)
            .await?;
            register_work_scope(&mut transaction, product_id, tenant_id, "upload").await?;
            let upload = stored_upload(updated)?;
            transaction.commit().await?;
            return Ok(RecordUploadOutcome::Recorded(upload));
        }

        let existing = sqlx::query(
            "select upload_id, object_bucket, object_name, expected_content_type, \
             expected_content_length, expected_digest, expires_at, created_at, status::text as status, \
             object_generation, uploaded_at, verified_content_type, verified_content_length, \
             verified_digest from ocr_uploads \
             where product_id = $1 and tenant_id = $2 and upload_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(existing) = existing else {
            transaction.commit().await?;
            return Ok(RecordUploadOutcome::NotFound);
        };
        let state = parse_upload_state(existing.try_get("status")?)?;
        let outcome = match state {
            UploadState::Uploaded
                if existing.try_get::<Option<i64>, _>("object_generation")?
                    == Some(record.object_generation)
                    && existing.try_get::<Option<&str>, _>("verified_content_type")?
                        == Some(record.verified_content_type.as_str())
                    && existing.try_get::<Option<i64>, _>("verified_content_length")?
                        == Some(record.verified_content_length)
                    && existing.try_get::<Option<&str>, _>("verified_digest")?
                        == Some(record.verified_digest.as_str()) =>
            {
                RecordUploadOutcome::Existing(stored_upload(existing)?)
            }
            UploadState::Reserved
                if existing.try_get::<OffsetDateTime, _>("expires_at")?
                    <= OffsetDateTime::now_utc() =>
            {
                RecordUploadOutcome::Expired
            }
            UploadState::Reserved => RecordUploadOutcome::VerificationMismatch,
            _ => RecordUploadOutcome::NotRecordable,
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn accept_upload(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
        source: AcceptUpload,
    ) -> Result<AcceptUploadOutcome> {
        validate_accepted_source(&source)?;
        let page_geometries = serde_json::to_string(&source.parser_inspection.page_geometries)
            .map_err(|_| Error::InvalidStoredUpload)?;
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let updated = sqlx::query(
            "update ocr_uploads set status = 'accepted', source_bucket = $4, \
             source_object_name = $5, source_object_generation = $6, source_digest = $7, \
             source_content_length = $8, parser_page_count = $10, \
             parser_maximum_page_pixels = $11, parser_total_page_pixels = $12, \
             parser_page_geometries = $13::jsonb, parser_profile = $14, parser_version = $15, accepted_at = now(), \
             inspection_lease_owner = null, \
             inspection_lease_expires_at = null, updated_at = now() \
             where product_id = $1 and tenant_id = $2 and upload_id = $3 \
             and status = 'inspecting' and verified_digest = $7 and verified_content_length = $8 \
             and inspection_lease_owner = $9 and inspection_lease_expires_at > now() \
             returning upload_id",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .bind(&source.source_bucket)
        .bind(&source.source_object_name)
        .bind(source.source_object_generation)
        .bind(&source.source_digest)
        .bind(source.source_content_length)
        .bind(&source.inspection_lease_owner)
        .bind(source.parser_inspection.page_count)
        .bind(source.parser_inspection.maximum_page_pixels)
        .bind(source.parser_inspection.total_page_pixels)
        .bind(&page_geometries)
        .bind(&source.parser_inspection.profile)
        .bind(&source.parser_inspection.version)
        .fetch_optional(&mut *transaction)
        .await?;

        if updated.is_some() {
            sqlx::query(
                "insert into ocr_upload_outbox \
                 (product_id, tenant_id, upload_id, event_type, payload) \
                 values ($1, $2, $3, 'ocr.upload.accepted.v1', \
                 jsonb_build_object('upload_id', $3::text, 'status', 'accepted'))",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(upload_id.as_str())
            .execute(&mut *transaction)
            .await?;
            set_work_scope_pending(&mut transaction, product_id, tenant_id, "upload", false)
                .await?;
            transaction.commit().await?;
            return Ok(AcceptUploadOutcome::Accepted);
        }

        let existing = sqlx::query(
            "select status::text as status, source_bucket, source_object_name, \
             source_object_generation, source_digest, source_content_length, parser_page_count, \
             parser_maximum_page_pixels, parser_total_page_pixels, parser_page_geometries::text as parser_page_geometries, \
             parser_profile, parser_version \
             from ocr_uploads where product_id = $1 and tenant_id = $2 and upload_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(existing) = existing else {
            transaction.commit().await?;
            return Ok(AcceptUploadOutcome::NotFound);
        };
        let state = parse_upload_state(existing.try_get("status")?)?;
        let outcome = if state == UploadState::Accepted {
            if existing.try_get::<Option<&str>, _>("source_bucket")?
                == Some(source.source_bucket.as_str())
                && existing.try_get::<Option<&str>, _>("source_object_name")?
                    == Some(source.source_object_name.as_str())
                && existing.try_get::<Option<i64>, _>("source_object_generation")?
                    == Some(source.source_object_generation)
                && existing.try_get::<Option<&str>, _>("source_digest")?
                    == Some(source.source_digest.as_str())
                && existing.try_get::<Option<i64>, _>("source_content_length")?
                    == Some(source.source_content_length)
                && existing.try_get::<Option<i32>, _>("parser_page_count")?
                    == Some(source.parser_inspection.page_count)
                && existing.try_get::<Option<i64>, _>("parser_maximum_page_pixels")?
                    == Some(source.parser_inspection.maximum_page_pixels)
                && existing.try_get::<Option<i64>, _>("parser_total_page_pixels")?
                    == Some(source.parser_inspection.total_page_pixels)
                && existing
                    .try_get::<Option<String>, _>("parser_page_geometries")?
                    .and_then(|value| serde_json::from_str::<Vec<PageGeometry>>(&value).ok())
                    .as_deref()
                    == Some(source.parser_inspection.page_geometries.as_slice())
                && existing.try_get::<Option<&str>, _>("parser_profile")?
                    == Some(source.parser_inspection.profile.as_str())
                && existing.try_get::<Option<&str>, _>("parser_version")?
                    == Some(source.parser_inspection.version.as_str())
            {
                AcceptUploadOutcome::Existing
            } else {
                AcceptUploadOutcome::SourceMismatch
            }
        } else {
            AcceptUploadOutcome::NotAcceptable
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn claim_upload_inspection(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
        claim: ClaimUploadInspection,
    ) -> Result<ClaimUploadInspectionOutcome> {
        validate_lease_owner(&claim.lease_owner)?;
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let existing = sqlx::query(
            "select status::text as status, inspection_attempts, inspection_lease_owner, \
             coalesce(inspection_lease_expires_at > now(), false) as lease_active \
             from ocr_uploads where product_id = $1 and tenant_id = $2 and upload_id = $3 \
             for update",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(existing) = existing else {
            transaction.commit().await?;
            return Ok(ClaimUploadInspectionOutcome::NotFound);
        };
        let state = parse_upload_state(existing.try_get("status")?)?;
        let attempts: i32 = existing.try_get("inspection_attempts")?;
        let current_owner: Option<&str> = existing.try_get("inspection_lease_owner")?;
        let lease_active: bool = existing.try_get("lease_active")?;
        let outcome = match state {
            UploadState::Uploaded if attempts < 10 => ClaimUploadInspectionOutcome::Claimed,
            UploadState::Inspecting
                if lease_active && current_owner == Some(claim.lease_owner.as_str()) =>
            {
                ClaimUploadInspectionOutcome::Existing
            }
            UploadState::Inspecting if !lease_active && attempts < 10 => {
                ClaimUploadInspectionOutcome::Claimed
            }
            UploadState::Inspecting if !lease_active => {
                ClaimUploadInspectionOutcome::AttemptsExhausted
            }
            UploadState::Inspecting if lease_active => ClaimUploadInspectionOutcome::Busy,
            _ => ClaimUploadInspectionOutcome::NotInspectable,
        };
        match outcome {
            ClaimUploadInspectionOutcome::Claimed => {
                sqlx::query(
                    "update ocr_uploads set status = 'inspecting', \
                     inspection_attempts = inspection_attempts + 1, \
                     inspection_lease_owner = $4, \
                     inspection_lease_expires_at = now() + interval '5 minutes', \
                     updated_at = now() \
                     where product_id = $1 and tenant_id = $2 and upload_id = $3",
                )
                .bind(product_id.as_str())
                .bind(tenant_id.as_str())
                .bind(upload_id.as_str())
                .bind(&claim.lease_owner)
                .execute(&mut *transaction)
                .await?;
            }
            ClaimUploadInspectionOutcome::Existing => {
                sqlx::query(
                    "update ocr_uploads set inspection_lease_expires_at = now() + interval '5 minutes', \
                     updated_at = now() where product_id = $1 and tenant_id = $2 \
                     and upload_id = $3 and inspection_lease_owner = $4",
                )
                .bind(product_id.as_str())
                .bind(tenant_id.as_str())
                .bind(upload_id.as_str())
                .bind(&claim.lease_owner)
                .execute(&mut *transaction)
                .await?;
            }
            ClaimUploadInspectionOutcome::AttemptsExhausted => {
                sqlx::query(
                    "update ocr_uploads set status = 'rejected', inspection_lease_owner = null, \
                     inspection_lease_expires_at = null, \
                     rejection_reason = 'inspection_attempts_exhausted', updated_at = now() \
                     where product_id = $1 and tenant_id = $2 and upload_id = $3",
                )
                .bind(product_id.as_str())
                .bind(tenant_id.as_str())
                .bind(upload_id.as_str())
                .execute(&mut *transaction)
                .await?;
                set_work_scope_pending(&mut transaction, product_id, tenant_id, "upload", false)
                    .await?;
                sqlx::query(
                    "insert into ocr_upload_outbox \
                     (product_id, tenant_id, upload_id, event_type, payload) \
                     values ($1, $2, $3, 'ocr.upload.rejected.v1', \
                     jsonb_build_object('upload_id', $3::text, 'status', 'rejected', \
                     'reason_code', 'inspection_attempts_exhausted'))",
                )
                .bind(product_id.as_str())
                .bind(tenant_id.as_str())
                .bind(upload_id.as_str())
                .execute(&mut *transaction)
                .await?;
            }
            ClaimUploadInspectionOutcome::Busy
            | ClaimUploadInspectionOutcome::NotInspectable
            | ClaimUploadInspectionOutcome::NotFound => {}
        }
        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn reject_upload(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
        lease_owner: &str,
        reason: UploadRejectionReason,
    ) -> Result<RejectUploadOutcome> {
        validate_lease_owner(lease_owner)?;
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let updated = sqlx::query(
            "update ocr_uploads set status = 'rejected', rejection_reason = $5, \
             inspection_lease_owner = null, inspection_lease_expires_at = null, updated_at = now() \
             where product_id = $1 and tenant_id = $2 and upload_id = $3 \
             and status = 'inspecting' and inspection_lease_owner = $4 \
             and inspection_lease_expires_at > now() returning upload_id",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .bind(lease_owner)
        .bind(reason.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        if updated.is_some() {
            sqlx::query(
                "insert into ocr_upload_outbox \
                 (product_id, tenant_id, upload_id, event_type, payload) \
                 values ($1, $2, $3, 'ocr.upload.rejected.v1', \
                 jsonb_build_object('upload_id', $3::text, 'status', 'rejected', \
                 'reason_code', $4::text))",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(upload_id.as_str())
            .bind(reason.as_str())
            .execute(&mut *transaction)
            .await?;
            set_work_scope_pending(&mut transaction, product_id, tenant_id, "upload", false)
                .await?;
            transaction.commit().await?;
            return Ok(RejectUploadOutcome::Rejected);
        }

        let existing = sqlx::query(
            "select status::text as status, rejection_reason from ocr_uploads \
             where product_id = $1 and tenant_id = $2 and upload_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(upload_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        let Some(existing) = existing else {
            return Ok(RejectUploadOutcome::NotFound);
        };
        let state = parse_upload_state(existing.try_get("status")?)?;
        if state != UploadState::Rejected {
            return Ok(RejectUploadOutcome::NotRejectable);
        }
        if existing.try_get::<Option<&str>, _>("rejection_reason")? == Some(reason.as_str()) {
            Ok(RejectUploadOutcome::Existing)
        } else {
            Ok(RejectUploadOutcome::ReasonMismatch)
        }
    }

    pub async fn create(&self, request: CreateJob) -> Result<CreateOutcome> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, &request.tenant_id, &request.product_id).await?;

        let inserted = sqlx::query(
            "insert into ocr_jobs \
             (job_id, tenant_id, product_id, idempotency_key, request_digest, upload_id, \
              webhook_subscription_id) \
             select $1, $2, $3, $4, $5, $6, $7 from ocr_uploads \
             where product_id = $3 and tenant_id = $2 and upload_id = $6 and status = 'accepted' \
             on conflict (product_id, tenant_id, idempotency_key) do nothing \
             returning job_id, status::text as status, created_at",
        )
        .bind(request.job_id.as_str())
        .bind(request.tenant_id.as_str())
        .bind(request.product_id.as_str())
        .bind(request.idempotency_key.as_str())
        .bind(request.request_digest.as_str())
        .bind(request.upload_id.as_str())
        .bind(
            request
                .webhook_subscription_id
                .as_ref()
                .map(WebhookSubscriptionId::as_str),
        )
        .fetch_optional(&mut *transaction)
        .await?;

        if let Some(inserted) = inserted {
            sqlx::query(
                "insert into ocr_outbox \
                 (product_id, tenant_id, job_id, event_type, payload) \
                 values ($1, $2, $3, 'ocr.job.accepted.v1', \
                 jsonb_build_object('job_id', $3::text, 'status', 'accepted'))",
            )
            .bind(request.product_id.as_str())
            .bind(request.tenant_id.as_str())
            .bind(request.job_id.as_str())
            .execute(&mut *transaction)
            .await?;
            register_work_scope(
                &mut transaction,
                &request.product_id,
                &request.tenant_id,
                "dispatch",
            )
            .await?;
            let job = stored_job(inserted)?;
            transaction.commit().await?;
            return Ok(CreateOutcome::Created(job));
        }

        let existing = sqlx::query(
            "select job_id, status::text as status, created_at, request_digest, \
             webhook_subscription_id from ocr_jobs \
             where product_id = $1 and tenant_id = $2 and idempotency_key = $3",
        )
        .bind(request.product_id.as_str())
        .bind(request.tenant_id.as_str())
        .bind(request.idempotency_key.as_str())
        .fetch_optional(&mut *transaction)
        .await?;

        let Some(existing) = existing else {
            return Err(Error::UploadSourceUnavailable);
        };

        let existing_digest: &str = existing.try_get("request_digest")?;
        let existing_webhook: Option<&str> = existing.try_get("webhook_subscription_id")?;
        if existing_digest != request.request_digest.as_str()
            || existing_webhook
                != request
                    .webhook_subscription_id
                    .as_ref()
                    .map(WebhookSubscriptionId::as_str)
        {
            return Err(Error::IdempotencyConflict);
        }

        let job = stored_job(existing)?;
        transaction.commit().await?;
        Ok(CreateOutcome::Existing(job))
    }

    pub async fn find(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<Option<StoredJob>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select job_id, status::text as status, created_at from ocr_jobs \
             where product_id = $1 and tenant_id = $2 and job_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;

        row.map(stored_job).transpose()
    }

    pub async fn load_document_identity(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<Option<StoredDocumentIdentity>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select uploads.source_digest from ocr_jobs as jobs \
             join ocr_uploads as uploads on uploads.upload_id = jobs.upload_id \
               and uploads.product_id = jobs.product_id and uploads.tenant_id = jobs.tenant_id \
             where jobs.product_id = $1 and jobs.tenant_id = $2 and jobs.job_id = $3 \
               and uploads.status = 'accepted'",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let suffix = job_id
            .as_str()
            .strip_prefix("job_")
            .ok_or(Error::InvalidStoredJob)?;
        Ok(Some(StoredDocumentIdentity {
            document_id: DocumentId::new(&format!("doc_{suffix}"))
                .map_err(|_| Error::InvalidStoredJob)?,
            document_version: DocumentVersion::new(row.try_get("source_digest")?)
                .map_err(|_| Error::InvalidStoredUpload)?,
        }))
    }

    pub async fn load_accepted_source(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<Option<StoredAcceptedSource>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select uploads.source_bucket, uploads.source_object_name, \
             uploads.source_object_generation, uploads.source_digest, \
             uploads.source_content_length, uploads.expected_content_type, \
             uploads.parser_page_count, uploads.parser_maximum_page_pixels, \
             uploads.parser_total_page_pixels, uploads.parser_page_geometries::text as parser_page_geometries, \
             uploads.parser_profile, uploads.parser_version \
             from ocr_jobs as jobs join ocr_uploads as uploads \
             on uploads.upload_id = jobs.upload_id and uploads.product_id = jobs.product_id \
             and uploads.tenant_id = jobs.tenant_id \
             where jobs.product_id = $1 and jobs.tenant_id = $2 and jobs.job_id = $3 \
             and uploads.status = 'accepted'",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        row.map(stored_accepted_source).transpose()
    }

    pub async fn cancel(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<CancelOutcome> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let updated = sqlx::query(
            "update ocr_jobs set status = 'cancelled', updated_at = now() \
             where product_id = $1 and tenant_id = $2 and job_id = $3 \
             and status in ('accepted', 'inspecting', 'processing', 'validating') \
             returning job_id, status::text as status, created_at",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;

        if let Some(updated) = updated {
            sqlx::query(
                "insert into ocr_outbox \
                 (product_id, tenant_id, job_id, event_type, payload) \
                 values ($1, $2, $3, 'ocr.job.cancellation_requested.v1', \
                 jsonb_build_object('job_id', $3::text, 'status', 'cancelled'))",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(job_id.as_str())
            .execute(&mut *transaction)
            .await?;
            register_work_scope(&mut transaction, product_id, tenant_id, "dispatch").await?;
            let job = stored_job(updated)?;
            transaction.commit().await?;
            return Ok(CancelOutcome::Requested(job));
        }

        let existing = sqlx::query(
            "select job_id, status::text as status, created_at from ocr_jobs \
             where product_id = $1 and tenant_id = $2 and job_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;

        let Some(job) = existing.map(stored_job).transpose()? else {
            return Ok(CancelOutcome::NotFound);
        };
        if matches!(job.state, JobState::Cancelling | JobState::Cancelled) {
            Ok(CancelOutcome::Existing(job))
        } else {
            Ok(CancelOutcome::NotCancellable(job))
        }
    }

    pub async fn complete_cancellation(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<CompleteCancellationOutcome> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select job_id, status::text as status, created_at from ocr_jobs \
             where product_id = $1 and tenant_id = $2 and job_id = $3 for update",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(CompleteCancellationOutcome::NotFound);
        };
        let job = stored_job(row)?;
        match job.state {
            JobState::Cancelled => {
                transaction.commit().await?;
                Ok(CompleteCancellationOutcome::Existing(job))
            }
            JobState::Cancelling => {
                let cancelled = sqlx::query(
                    "update ocr_jobs set status = 'cancelled', updated_at = now() \
                     where product_id = $1 and tenant_id = $2 and job_id = $3 \
                     and status = 'cancelling' returning job_id, status::text as status, created_at",
                )
                .bind(product_id.as_str())
                .bind(tenant_id.as_str())
                .bind(job_id.as_str())
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(Error::InvalidStoredJob)?;
                let cancelled = stored_job(cancelled)?;
                transaction.commit().await?;
                Ok(CompleteCancellationOutcome::Cancelled(cancelled))
            }
            _ => {
                transaction.commit().await?;
                Ok(CompleteCancellationOutcome::NotCancellable(job))
            }
        }
    }

    pub async fn find_result(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<ResultLookup> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select j.status::text as status, r.document_id, r.document_version, \
             r.object_bucket, r.object_name, r.object_generation, r.object_digest, \
             r.content_length \
             from ocr_jobs j left join ocr_results r on r.job_id = j.job_id \
             where j.product_id = $1 and j.tenant_id = $2 and j.job_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;

        let Some(row) = row else {
            return Ok(ResultLookup::NotFound);
        };
        let state = parse_job_state(row.try_get("status")?)?;
        let document_id: Option<&str> = row.try_get("document_id")?;
        let Some(document_id) = document_id else {
            return Ok(match state {
                JobState::Accepted
                | JobState::Inspecting
                | JobState::Processing
                | JobState::Validating
                | JobState::Cancelling => ResultLookup::NotReady(state),
                JobState::Cancelled | JobState::Rejected => ResultLookup::Unavailable(state),
                JobState::Partial | JobState::ReviewRequired | JobState::Completed => {
                    return Err(Error::InvalidStoredResult)
                }
            });
        };
        if !matches!(
            state,
            JobState::Partial | JobState::ReviewRequired | JobState::Completed
        ) {
            return Err(Error::InvalidStoredResult);
        }
        let locator = StoredResultLocator {
            document_id: DocumentId::new(document_id).map_err(|_| Error::InvalidStoredResult)?,
            document_version: DocumentVersion::new(row.try_get("document_version")?)
                .map_err(|_| Error::InvalidStoredResult)?,
            object_bucket: row.try_get("object_bucket")?,
            object_name: row.try_get("object_name")?,
            object_generation: row.try_get("object_generation")?,
            object_digest: row.try_get("object_digest")?,
            content_length: row.try_get("content_length")?,
        };
        validate_locator(&locator)?;
        Ok(ResultLookup::Ready(locator))
    }

    pub async fn commit_result(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
        command: CommitResult,
    ) -> Result<CommitResultOutcome> {
        validate_locator(&command.locator)?;
        if !matches!(
            command.terminal_state,
            JobState::Partial | JobState::ReviewRequired | JobState::Completed
        ) {
            return Err(Error::InvalidStoredResult);
        }
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let row = sqlx::query(
            "select job_id, status::text as status, created_at, webhook_subscription_id \
             from ocr_jobs \
             where product_id = $1 and tenant_id = $2 and job_id = $3 for update",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(CommitResultOutcome::NotFound);
        };
        let webhook_subscription_id: Option<String> = row.try_get("webhook_subscription_id")?;
        let job = stored_job(row)?;
        let existing = sqlx::query(
            "select document_id, document_version, object_bucket, object_name, \
             object_generation, object_digest, content_length from ocr_results \
             where product_id = $1 and tenant_id = $2 and job_id = $3",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(existing) = existing {
            let locator = stored_result_locator(existing)?;
            transaction.commit().await?;
            return Ok(
                if locator == command.locator && job.state == command.terminal_state {
                    CommitResultOutcome::Existing(job)
                } else {
                    CommitResultOutcome::Conflict
                },
            );
        }
        if !matches!(job.state, JobState::Processing | JobState::Validating) {
            transaction.commit().await?;
            return Ok(CommitResultOutcome::NotCommittable);
        }
        if job.state == JobState::Processing {
            sqlx::query(
                "update ocr_jobs set status = 'validating', updated_at = now() \
                 where product_id = $1 and tenant_id = $2 and job_id = $3",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(job_id.as_str())
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query(
            "insert into ocr_results (job_id, product_id, tenant_id, document_id, \
             document_version, object_bucket, object_name, object_generation, object_digest, \
             content_length) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(job_id.as_str())
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(String::from(command.locator.document_id.clone()))
        .bind(String::from(command.locator.document_version.clone()))
        .bind(&command.locator.object_bucket)
        .bind(&command.locator.object_name)
        .bind(command.locator.object_generation)
        .bind(&command.locator.object_digest)
        .bind(command.locator.content_length)
        .execute(&mut *transaction)
        .await?;
        let terminal_state = match command.terminal_state {
            JobState::Partial => "partial",
            JobState::ReviewRequired => "review_required",
            JobState::Completed => "completed",
            _ => return Err(Error::InvalidStoredResult),
        };
        let completed = sqlx::query(
            "update ocr_jobs set status = $4::ocr_job_status, updated_at = now() \
             where product_id = $1 and tenant_id = $2 and job_id = $3 and status = 'validating' \
             returning job_id, status::text as status, created_at",
        )
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(job_id.as_str())
        .bind(terminal_state)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(Error::InvalidStoredResult)?;
        let completed = stored_job(completed)?;
        if let Some(webhook_subscription_id) = webhook_subscription_id {
            let event_type = match command.terminal_state {
                JobState::Completed => "ocr.job.completed.v1",
                JobState::Partial => "ocr.job.partial.v1",
                JobState::ReviewRequired => "ocr.job.review_required.v1",
                _ => return Err(Error::InvalidStoredResult),
            };
            sqlx::query(
                "insert into ocr_outbox \
                 (product_id, tenant_id, job_id, event_type, payload) values \
                 ($1, $2, $3, $4, jsonb_build_object( \
                    'job_id', $3::text, 'status', $5::text, \
                    'document_version', $6::text, \
                    'webhook_subscription_id', $7::text, \
                    'content_trust', 'untrusted'))",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(job_id.as_str())
            .bind(event_type)
            .bind(terminal_state)
            .bind(String::from(command.locator.document_version.clone()))
            .bind(webhook_subscription_id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(CommitResultOutcome::Committed(completed))
    }
}

async fn set_scope(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: &TenantId,
    product_id: &ProductId,
) -> Result<()> {
    sqlx::query("select set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.as_str())
        .execute(&mut **transaction)
        .await?;
    sqlx::query("select set_config('app.product_id', $1, true)")
        .bind(product_id.as_str())
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn register_work_scope(
    transaction: &mut Transaction<'_, Postgres>,
    product_id: &ProductId,
    tenant_id: &TenantId,
    work_kind: &str,
) -> Result<()> {
    sqlx::query("select ocr_register_work_scope($1, $2, $3)")
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(work_kind)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn set_work_scope_pending(
    transaction: &mut Transaction<'_, Postgres>,
    product_id: &ProductId,
    tenant_id: &TenantId,
    work_kind: &str,
    is_pending: bool,
) -> Result<()> {
    sqlx::query("select ocr_set_work_scope_pending($1, $2, $3, $4)")
        .bind(product_id.as_str())
        .bind(tenant_id.as_str())
        .bind(work_kind)
        .bind(is_pending)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn load_page_workflow_row(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: &TenantId,
    product_id: &ProductId,
    job_id: &JobId,
) -> Result<Option<StoredPageWorkflow>> {
    sqlx::query(
        "select revision, checkpoint::text as checkpoint from ocr_page_workflows \
         where job_id = $1 and product_id = $2 and tenant_id = $3",
    )
    .bind(job_id.as_str())
    .bind(product_id.as_str())
    .bind(tenant_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    .map(stored_page_workflow)
    .transpose()
}

fn serialize_page_workflow(workflow: &PageWorkflow) -> Result<String> {
    let checkpoint =
        serde_json::to_string(workflow).map_err(|_| Error::InvalidStoredPageWorkflow)?;
    if checkpoint.is_empty() || checkpoint.len() > 262_144 {
        return Err(Error::InvalidStoredPageWorkflow);
    }
    Ok(checkpoint)
}

fn stored_page_workflow(row: sqlx::postgres::PgRow) -> Result<StoredPageWorkflow> {
    let revision: i64 = row.try_get("revision")?;
    let checkpoint: &str = row.try_get("checkpoint")?;
    if revision < 0 || checkpoint.is_empty() || checkpoint.len() > 262_144 {
        return Err(Error::InvalidStoredPageWorkflow);
    }
    let workflow =
        serde_json::from_str(checkpoint).map_err(|_| Error::InvalidStoredPageWorkflow)?;
    Ok(StoredPageWorkflow { workflow, revision })
}

fn stored_page_artifact(row: sqlx::postgres::PgRow) -> Result<StoredPageArtifact> {
    let page = u32::try_from(row.try_get::<i32, _>("page_number")?)
        .map_err(|_| Error::InvalidStoredPageArtifact)?;
    let attempt = u8::try_from(row.try_get::<i16, _>("attempt")?)
        .map_err(|_| Error::InvalidStoredPageArtifact)?;
    let artifact = StoredPageArtifact {
        page,
        attempt,
        activity_key: row.try_get("activity_key")?,
        object_bucket: row.try_get("object_bucket")?,
        object_name: row.try_get("object_name")?,
        object_generation: row.try_get("object_generation")?,
        object_digest: row.try_get("object_digest")?,
        content_length: row.try_get("content_length")?,
    };
    validate_page_artifact(&artifact)?;
    Ok(artifact)
}

fn stored_result_locator(row: sqlx::postgres::PgRow) -> Result<StoredResultLocator> {
    let locator = StoredResultLocator {
        document_id: DocumentId::new(row.try_get("document_id")?)
            .map_err(|_| Error::InvalidStoredResult)?,
        document_version: DocumentVersion::new(row.try_get("document_version")?)
            .map_err(|_| Error::InvalidStoredResult)?,
        object_bucket: row.try_get("object_bucket")?,
        object_name: row.try_get("object_name")?,
        object_generation: row.try_get("object_generation")?,
        object_digest: row.try_get("object_digest")?,
        content_length: row.try_get("content_length")?,
    };
    validate_locator(&locator)?;
    Ok(locator)
}

fn stored_job(row: sqlx::postgres::PgRow) -> Result<StoredJob> {
    let job_id = JobId::new(row.try_get("job_id")?).map_err(|_| Error::InvalidStoredJob)?;
    let state = parse_job_state(row.try_get("status")?)?;
    let created_at = row.try_get("created_at")?;
    Ok(StoredJob {
        job_id,
        state,
        created_at,
    })
}

fn stored_work_scope(row: sqlx::postgres::PgRow) -> Result<StoredWorkScope> {
    Ok(StoredWorkScope {
        product_id: ProductId::new(row.try_get("product_id")?)
            .map_err(|_| Error::InvalidWorkScope)?,
        tenant_id: TenantId::new(row.try_get("tenant_id")?).map_err(|_| Error::InvalidWorkScope)?,
    })
}

fn stored_job_outbox_event(row: sqlx::postgres::PgRow) -> Result<StoredJobOutboxEvent> {
    let event_id: i64 = row.try_get("event_id")?;
    if event_id <= 0 {
        return Err(Error::InvalidOutboxEvent);
    }
    let job_id = JobId::new(row.try_get("job_id")?).map_err(|_| Error::InvalidOutboxEvent)?;
    let event_type = match row.try_get::<&str, _>("event_type")? {
        "ocr.job.accepted.v1" => JobOutboxEventType::Accepted,
        "ocr.job.cancellation_requested.v1" => JobOutboxEventType::CancellationRequested,
        _ => return Err(Error::InvalidOutboxEvent),
    };
    let page_count = u32::try_from(row.try_get::<i32, _>("parser_page_count")?)
        .map_err(|_| Error::InvalidOutboxEvent)?;
    if !(1..=300).contains(&page_count) {
        return Err(Error::InvalidOutboxEvent);
    }
    Ok(StoredJobOutboxEvent {
        event_id,
        job_id,
        event_type,
        page_count,
    })
}

fn stored_webhook_outbox_event(row: sqlx::postgres::PgRow) -> Result<StoredWebhookOutboxEvent> {
    let event_id: i64 = row.try_get("event_id")?;
    if event_id <= 0 {
        return Err(Error::InvalidOutboxEvent);
    }
    let event_type = match row.try_get::<&str, _>("event_type")? {
        "ocr.job.completed.v1" => WebhookOutboxEventType::Completed,
        "ocr.job.partial.v1" => WebhookOutboxEventType::Partial,
        "ocr.job.review_required.v1" => WebhookOutboxEventType::ReviewRequired,
        _ => return Err(Error::InvalidOutboxEvent),
    };
    Ok(StoredWebhookOutboxEvent {
        event_id,
        job_id: JobId::new(row.try_get("job_id")?).map_err(|_| Error::InvalidOutboxEvent)?,
        event_type,
        webhook_subscription_id: WebhookSubscriptionId::new(
            row.try_get("webhook_subscription_id")?,
        )
        .map_err(|_| Error::InvalidOutboxEvent)?,
        document_version: DocumentVersion::new(row.try_get("document_version")?)
            .map_err(|_| Error::InvalidOutboxEvent)?,
        occurred_at: row.try_get("created_at")?,
    })
}

fn stored_upload(row: sqlx::postgres::PgRow) -> Result<StoredUpload> {
    let upload = StoredUpload {
        upload_id: UploadId::new(row.try_get("upload_id")?)
            .map_err(|_| Error::InvalidStoredUpload)?,
        state: parse_upload_state(row.try_get("status")?)?,
        object_bucket: row.try_get("object_bucket")?,
        object_name: row.try_get("object_name")?,
        expected_content_type: row.try_get("expected_content_type")?,
        expected_content_length: row.try_get("expected_content_length")?,
        expected_digest: row.try_get("expected_digest")?,
        expires_at: row.try_get("expires_at")?,
        created_at: row.try_get("created_at")?,
        object_generation: row.try_get("object_generation")?,
        uploaded_at: row.try_get("uploaded_at")?,
    };
    validate_upload(&upload)?;
    Ok(upload)
}

fn stored_accepted_source(row: sqlx::postgres::PgRow) -> Result<StoredAcceptedSource> {
    let source = StoredAcceptedSource {
        bucket: row.try_get("source_bucket")?,
        object_name: row.try_get("source_object_name")?,
        generation: row.try_get("source_object_generation")?,
        digest: row.try_get("source_digest")?,
        content_length: row.try_get("source_content_length")?,
        content_type: row.try_get("expected_content_type")?,
        page_count: u32::try_from(row.try_get::<i32, _>("parser_page_count")?)
            .map_err(|_| Error::InvalidStoredUpload)?,
        maximum_page_pixels: u64::try_from(row.try_get::<i64, _>("parser_maximum_page_pixels")?)
            .map_err(|_| Error::InvalidStoredUpload)?,
        total_page_pixels: u64::try_from(row.try_get::<i64, _>("parser_total_page_pixels")?)
            .map_err(|_| Error::InvalidStoredUpload)?,
        page_geometries: serde_json::from_str(
            &row.try_get::<Option<String>, _>("parser_page_geometries")?
                .ok_or(Error::InvalidStoredUpload)?,
        )
        .map_err(|_| Error::InvalidStoredUpload)?,
        parser_profile: row.try_get("parser_profile")?,
        parser_version: row.try_get("parser_version")?,
    };
    validate_accepted_source_locator(&source)?;
    Ok(source)
}

fn parse_upload_state(value: &str) -> Result<UploadState> {
    Ok(match value {
        "reserved" => UploadState::Reserved,
        "uploaded" => UploadState::Uploaded,
        "inspecting" => UploadState::Inspecting,
        "accepted" => UploadState::Accepted,
        "rejected" => UploadState::Rejected,
        "expired" => UploadState::Expired,
        _ => return Err(Error::InvalidStoredUpload),
    })
}

fn validate_record(record: &RecordUpload) -> Result<()> {
    let digest = record.verified_digest.strip_prefix("sha256:");
    let valid_digest = digest.is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let valid_content_type = matches!(
        record.verified_content_type.as_str(),
        "application/pdf" | "image/jpeg" | "image/png" | "image/tiff" | "image/webp"
    );
    if record.object_generation <= 0
        || !(1..=104_857_600).contains(&record.verified_content_length)
        || !valid_digest
        || !valid_content_type
    {
        return Err(Error::InvalidStoredUpload);
    }
    Ok(())
}

fn validate_accepted_source(source: &AcceptUpload) -> Result<()> {
    let page_count = u32::try_from(source.parser_inspection.page_count)
        .map_err(|_| Error::InvalidStoredUpload)?;
    let maximum_page_pixels = u64::try_from(source.parser_inspection.maximum_page_pixels)
        .map_err(|_| Error::InvalidStoredUpload)?;
    let total_page_pixels = u64::try_from(source.parser_inspection.total_page_pixels)
        .map_err(|_| Error::InvalidStoredUpload)?;
    let digest = source.source_digest.strip_prefix("sha256:");
    let valid_digest = digest.is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !(3..=63).contains(&source.source_bucket.len())
        || source.source_object_name.is_empty()
        || source.source_object_name.len() > 1024
        || source.source_object_generation <= 0
        || !(1..=104_857_600).contains(&source.source_content_length)
        || !valid_digest
        || !is_valid_lease_owner(&source.inspection_lease_owner)
        || !(1..=300).contains(&source.parser_inspection.page_count)
        || !(1..=100_000_000).contains(&source.parser_inspection.maximum_page_pixels)
        || !(source.parser_inspection.maximum_page_pixels..=1_000_000_000)
            .contains(&source.parser_inspection.total_page_pixels)
        || !valid_page_geometries(
            &source.parser_inspection.page_geometries,
            page_count,
            maximum_page_pixels,
            total_page_pixels,
        )
        || !is_valid_parser_identifier(&source.parser_inspection.profile)
        || !is_valid_parser_identifier(&source.parser_inspection.version)
    {
        return Err(Error::InvalidStoredUpload);
    }
    Ok(())
}

fn validate_accepted_source_locator(source: &StoredAcceptedSource) -> Result<()> {
    let digest = source.digest.strip_prefix("sha256:");
    let valid_digest = digest.is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let valid_content_type = matches!(
        source.content_type.as_str(),
        "application/pdf" | "image/jpeg" | "image/png" | "image/tiff" | "image/webp"
    );
    if !(3..=63).contains(&source.bucket.len())
        || source.object_name.is_empty()
        || source.object_name.len() > 1024
        || source.generation <= 0
        || !(1..=104_857_600).contains(&source.content_length)
        || !valid_digest
        || !valid_content_type
        || !(1..=300).contains(&source.page_count)
        || !(1..=100_000_000).contains(&source.maximum_page_pixels)
        || !(source.maximum_page_pixels..=1_000_000_000).contains(&source.total_page_pixels)
        || !valid_page_geometries(
            &source.page_geometries,
            source.page_count,
            source.maximum_page_pixels,
            source.total_page_pixels,
        )
        || !is_valid_parser_identifier(&source.parser_profile)
        || !is_valid_parser_identifier(&source.parser_version)
    {
        return Err(Error::InvalidStoredUpload);
    }
    Ok(())
}

fn valid_page_geometries(
    pages: &[PageGeometry],
    page_count: u32,
    maximum_page_pixels: u64,
    total_page_pixels: u64,
) -> bool {
    let Ok(expected_length) = usize::try_from(page_count) else {
        return false;
    };
    if pages.len() != expected_length {
        return false;
    }
    let mut computed_maximum = 0_u64;
    let mut computed_total = 0_u64;
    for (index, page) in pages.iter().enumerate() {
        let Ok(expected_page) = u32::try_from(index + 1) else {
            return false;
        };
        if u32::from(page.page) != expected_page {
            return false;
        }
        computed_maximum = computed_maximum.max(page.pixels());
        let Some(total) = computed_total.checked_add(page.pixels()) else {
            return false;
        };
        computed_total = total;
    }
    computed_maximum == maximum_page_pixels && computed_total == total_page_pixels
}

fn is_valid_parser_identifier(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_lease_owner(owner: &str) -> Result<()> {
    if !is_valid_lease_owner(owner) {
        return Err(Error::InvalidStoredUpload);
    }
    Ok(())
}

fn is_valid_lease_owner(owner: &str) -> bool {
    (1..=128).contains(&owner.len())
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn validate_upload(upload: &StoredUpload) -> Result<()> {
    let digest = upload.expected_digest.strip_prefix("sha256:");
    let valid_digest = digest.is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let valid_content_type = matches!(
        upload.expected_content_type.as_str(),
        "application/pdf" | "image/jpeg" | "image/png" | "image/tiff" | "image/webp"
    );
    let verification_shape_is_valid = match upload.state {
        UploadState::Reserved | UploadState::Expired => {
            upload.object_generation.is_none() && upload.uploaded_at.is_none()
        }
        _ => upload.object_generation.is_some() && upload.uploaded_at.is_some(),
    };
    if upload.object_bucket.is_empty()
        || upload.object_name.is_empty()
        || !(1..=104_857_600).contains(&upload.expected_content_length)
        || !valid_digest
        || !valid_content_type
        || upload.expires_at <= upload.created_at
        || !verification_shape_is_valid
    {
        return Err(Error::InvalidStoredUpload);
    }
    Ok(())
}

fn parse_job_state(value: &str) -> Result<JobState> {
    Ok(match value {
        "accepted" => JobState::Accepted,
        "inspecting" => JobState::Inspecting,
        "processing" => JobState::Processing,
        "validating" => JobState::Validating,
        "cancelling" => JobState::Cancelling,
        "cancelled" => JobState::Cancelled,
        "rejected" => JobState::Rejected,
        "partial" => JobState::Partial,
        "review_required" => JobState::ReviewRequired,
        "completed" => JobState::Completed,
        _ => return Err(Error::InvalidStoredJob),
    })
}

fn validate_locator(locator: &StoredResultLocator) -> Result<()> {
    let digest = locator.object_digest.strip_prefix("sha256:");
    let digest_is_valid = digest.is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if locator.object_bucket.is_empty()
        || locator.object_bucket.len() > 222
        || locator.object_name.is_empty()
        || locator.object_name.len() > 1024
        || locator.object_generation <= 0
        || locator.content_length <= 0
        || !digest_is_valid
    {
        return Err(Error::InvalidStoredResult);
    }
    Ok(())
}

fn validate_page_artifact(artifact: &StoredPageArtifact) -> Result<()> {
    let digest = artifact.object_digest.strip_prefix("sha256:");
    let digest_is_valid = digest.is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !(1..=300).contains(&artifact.page)
        || !(1..=10).contains(&artifact.attempt)
        || artifact.activity_key.is_empty()
        || artifact.activity_key.len() > 160
        || artifact.object_bucket.len() < 3
        || artifact.object_bucket.len() > 222
        || artifact.object_name.is_empty()
        || artifact.object_name.len() > 1024
        || artifact.object_generation <= 0
        || !(1..=16_777_216).contains(&artifact.content_length)
        || !digest_is_valid
    {
        return Err(Error::InvalidStoredPageArtifact);
    }
    Ok(())
}

fn validate_page_artifact_batch(
    job_id: &JobId,
    workflow: &PageWorkflow,
    artifacts: &[StoredPageArtifact],
) -> Result<()> {
    if artifacts.len() > 64 {
        return Err(Error::InvalidStoredPageArtifact);
    }
    let mut pages = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        validate_page_artifact(artifact)?;
        let expected_key = format!(
            "ocr-job-{}-page-{}-attempt-{}",
            job_id.as_str(),
            artifact.page,
            artifact.attempt
        );
        if artifact.activity_key != expected_key {
            return Err(Error::InvalidStoredPageArtifact);
        }
        if !workflow.is_successful_task(&PageTask {
            page: artifact.page,
            attempt: artifact.attempt,
            activity_key: artifact.activity_key.clone(),
        }) {
            return Err(Error::InvalidStoredPageArtifact);
        }
        pages.push(artifact.page);
    }
    pages.sort_unstable();
    if pages.windows(2).any(|pages| pages[0] == pages[1]) {
        return Err(Error::InvalidStoredPageArtifact);
    }
    Ok(())
}
