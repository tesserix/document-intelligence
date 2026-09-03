//! PostgreSQL persistence for document jobs.

use ocr_domain::{IdempotencyKey, JobId, JobState, ProductId, RequestDigest, TenantId};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("idempotency key was reused with different input")]
    IdempotencyConflict,
    #[error("stored job is invalid")]
    InvalidStoredJob,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct CreateJob {
    pub job_id: JobId,
    pub tenant_id: TenantId,
    pub product_id: ProductId,
    pub idempotency_key: IdempotencyKey,
    pub request_digest: RequestDigest,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateOutcome {
    Created,
    Existing(JobId),
}

#[derive(Debug, PartialEq, Eq)]
pub struct StoredJob {
    pub job_id: JobId,
    pub state: JobState,
}

#[derive(Debug, Clone)]
pub struct PgJobStore {
    pool: PgPool,
}

impl PgJobStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, request: CreateJob) -> Result<CreateOutcome> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, &request.tenant_id, &request.product_id).await?;

        let inserted = sqlx::query(
            "insert into ocr_jobs \
             (job_id, tenant_id, product_id, idempotency_key, request_digest) \
             values ($1, $2, $3, $4, $5) \
             on conflict (product_id, tenant_id, idempotency_key) do nothing \
             returning job_id",
        )
        .bind(request.job_id.as_str())
        .bind(request.tenant_id.as_str())
        .bind(request.product_id.as_str())
        .bind(request.idempotency_key.as_str())
        .bind(request.request_digest.as_str())
        .fetch_optional(&mut *transaction)
        .await?;

        if inserted.is_some() {
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
            transaction.commit().await?;
            return Ok(CreateOutcome::Created);
        }

        let existing = sqlx::query(
            "select job_id, request_digest from ocr_jobs \
             where product_id = $1 and tenant_id = $2 and idempotency_key = $3",
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

        let existing_id: &str = existing.try_get("job_id")?;
        let job_id = JobId::new(existing_id).map_err(|_| Error::InvalidStoredJob)?;
        transaction.commit().await?;
        Ok(CreateOutcome::Existing(job_id))
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
            "select job_id, status::text as status from ocr_jobs \
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
    let state = match row.try_get("status")? {
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
    };
    Ok(StoredJob { job_id, state })
}
