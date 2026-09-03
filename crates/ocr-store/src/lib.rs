//! PostgreSQL persistence for document jobs.

use ocr_domain::{
    DocumentId, DocumentVersion, IdempotencyKey, JobId, JobState, ProductId, RequestDigest,
    TenantId, UploadId,
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
pub struct StoredJob {
    pub job_id: JobId,
    pub state: JobState,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, PartialEq, Eq)]
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
pub struct AcceptUpload {
    pub source_bucket: String,
    pub source_object_name: String,
    pub source_object_generation: i64,
    pub source_digest: String,
    pub source_content_length: i64,
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

impl PgJobStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn ready(&self) -> Result<()> {
        sqlx::query("select 1").execute(&self.pool).await?;
        Ok(())
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
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let updated = sqlx::query(
            "update ocr_uploads set status = 'accepted', source_bucket = $4, \
             source_object_name = $5, source_object_generation = $6, source_digest = $7, \
             source_content_length = $8, accepted_at = now(), updated_at = now() \
             where product_id = $1 and tenant_id = $2 and upload_id = $3 \
             and status = 'uploaded' and verified_digest = $7 and verified_content_length = $8 \
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
            transaction.commit().await?;
            return Ok(AcceptUploadOutcome::Accepted);
        }

        let existing = sqlx::query(
            "select status::text as status, source_bucket, source_object_name, \
             source_object_generation, source_digest, source_content_length \
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

    pub async fn create(&self, request: CreateJob) -> Result<CreateOutcome> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, &request.tenant_id, &request.product_id).await?;

        let inserted = sqlx::query(
            "insert into ocr_jobs \
             (job_id, tenant_id, product_id, idempotency_key, request_digest, upload_id) \
             select $1, $2, $3, $4, $5, $6 from ocr_uploads \
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
            let job = stored_job(inserted)?;
            transaction.commit().await?;
            return Ok(CreateOutcome::Created(job));
        }

        let existing = sqlx::query(
            "select job_id, status::text as status, created_at, request_digest from ocr_jobs \
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
        if existing_digest != request.request_digest.as_str() {
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

    pub async fn cancel(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<CancelOutcome> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant_id, product_id).await?;
        let updated = sqlx::query(
            "update ocr_jobs set status = 'cancelling', updated_at = now() \
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
                 jsonb_build_object('job_id', $3::text, 'status', 'cancelling'))",
            )
            .bind(product_id.as_str())
            .bind(tenant_id.as_str())
            .bind(job_id.as_str())
            .execute(&mut *transaction)
            .await?;
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
    {
        return Err(Error::InvalidStoredUpload);
    }
    Ok(())
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
