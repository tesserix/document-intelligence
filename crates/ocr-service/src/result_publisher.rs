use std::sync::Arc;

use ocr_domain::{DocumentResult, JobId, JobState, ProductId, TenantId};
use ocr_store::{CommitResult, CommitResultOutcome, PgJobStore};
use thiserror::Error;

use crate::{ResultArtifactWriteError, ResultArtifactWriter};

#[derive(Debug, Error)]
pub enum PublishResultError {
    #[error(transparent)]
    Artifact(#[from] ResultArtifactWriteError),
    #[error(transparent)]
    Store(#[from] ocr_store::Error),
}

pub struct ResultPublisher<W> {
    jobs: PgJobStore,
    writer: Arc<W>,
}

impl<W> ResultPublisher<W>
where
    W: ResultArtifactWriter,
{
    pub fn new(jobs: PgJobStore, writer: Arc<W>) -> Self {
        Self { jobs, writer }
    }

    pub async fn publish(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
        terminal_state: JobState,
        result: &DocumentResult,
    ) -> Result<CommitResultOutcome, PublishResultError> {
        let locator = self
            .writer
            .write(product_id, tenant_id, job_id, result)
            .await?;
        if locator.document_id != result.document_id
            || locator.document_version != result.document_version
        {
            return Err(ResultArtifactWriteError::Invalid.into());
        }
        self.jobs
            .commit_result(
                tenant_id,
                product_id,
                job_id,
                CommitResult {
                    terminal_state,
                    locator,
                },
            )
            .await
            .map_err(Into::into)
    }
}
