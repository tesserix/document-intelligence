use std::sync::Arc;

use futures_util::{stream, StreamExt, TryStreamExt};
use ocr_domain::{
    DocumentId, DocumentPage, DocumentVersion, JobId, JobState, PageWorkflowStatus, ProductId,
    TenantId,
};
use ocr_store::{CommitResultOutcome, PgJobStore};
use thiserror::Error;

use crate::{
    assemble_document_result, assemble_native_document_result, ExtractionSchema,
    PageArtifactReadError, PageArtifactReader, PublishResultError, ResultArtifactWriter,
    ResultAssemblyError, ResultPublisher, MAXIMUM_RESULT_BYTES,
};

#[derive(Debug, Error)]
pub enum DocumentFinalizeError {
    #[error("invalid document finalizer configuration")]
    InvalidConfiguration,
    #[error("page workflow is not terminal")]
    NotReady,
    #[error("page workflow was cancelled")]
    Cancelled,
    #[error("page artifact set is incomplete")]
    IncompleteArtifacts,
    #[error("job names an extraction schema that is no longer registered")]
    UnregisteredSchema,
    #[error("page artifact does not match its locator")]
    InvalidPageArtifact,
    #[error(transparent)]
    PageArtifact(#[from] PageArtifactReadError),
    #[error(transparent)]
    Assembly(#[from] ResultAssemblyError),
    #[error(transparent)]
    Publish(#[from] PublishResultError),
    #[error(transparent)]
    Store(#[from] ocr_store::Error),
}

pub struct DocumentFinalizer<R, W> {
    jobs: PgJobStore,
    pages: Arc<R>,
    publisher: ResultPublisher<W>,
    concurrency: usize,
    native_processing: bool,
}

impl<R, W> DocumentFinalizer<R, W>
where
    R: PageArtifactReader,
    W: ResultArtifactWriter,
{
    pub fn new(
        jobs: PgJobStore,
        pages: Arc<R>,
        publisher: ResultPublisher<W>,
        concurrency: usize,
    ) -> Result<Self, DocumentFinalizeError> {
        if !(1..=64).contains(&concurrency) {
            return Err(DocumentFinalizeError::InvalidConfiguration);
        }
        Ok(Self {
            jobs,
            pages,
            publisher,
            concurrency,
            native_processing: false,
        })
    }

    pub fn with_native_processing(mut self) -> Self {
        self.native_processing = true;
        self
    }

    pub async fn finalize(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
        document_id: DocumentId,
        document_version: DocumentVersion,
        schema: Option<ExtractionSchema>,
    ) -> Result<CommitResultOutcome, DocumentFinalizeError> {
        if self.native_processing
            && matches!(
                self.jobs.find_result(tenant_id, product_id, job_id).await?,
                ocr_store::ResultLookup::Ready(_)
            )
        {
            let job = self
                .jobs
                .find(tenant_id, product_id, job_id)
                .await?
                .ok_or(DocumentFinalizeError::NotReady)?;
            return Ok(CommitResultOutcome::Existing(job));
        }
        let workflow = self
            .jobs
            .load_page_workflow(tenant_id, product_id, job_id)
            .await?
            .ok_or(DocumentFinalizeError::NotReady)?
            .workflow;
        let terminal_state = match workflow.status() {
            PageWorkflowStatus::Completed => JobState::Completed,
            PageWorkflowStatus::Partial => JobState::Partial,
            PageWorkflowStatus::Running => return Err(DocumentFinalizeError::NotReady),
            PageWorkflowStatus::Cancelled => return Err(DocumentFinalizeError::Cancelled),
        };
        let artifacts = self
            .jobs
            .load_page_artifacts(tenant_id, product_id, job_id)
            .await?;
        if artifacts.len() != workflow.successful_page_count() {
            return Err(DocumentFinalizeError::IncompleteArtifacts);
        }
        let total_bytes = artifacts.iter().try_fold(0_usize, |total, artifact| {
            let length = usize::try_from(artifact.content_length)
                .map_err(|_| ResultAssemblyError::TooLarge)?;
            total
                .checked_add(length)
                .filter(|sum| *sum <= MAXIMUM_RESULT_BYTES)
                .ok_or(ResultAssemblyError::TooLarge)
        })?;
        if total_bytes == 0 && !artifacts.is_empty() {
            return Err(DocumentFinalizeError::InvalidPageArtifact);
        }
        let pages = stream::iter(artifacts.into_iter().map(|artifact| async move {
            let bytes = self.pages.read(&artifact, MAXIMUM_RESULT_BYTES).await?;
            let page = serde_json::from_slice::<DocumentPage>(&bytes)
                .map_err(|_| DocumentFinalizeError::InvalidPageArtifact)?;
            if u32::from(page.page) != artifact.page {
                return Err(DocumentFinalizeError::InvalidPageArtifact);
            }
            Ok::<_, DocumentFinalizeError>(page)
        }))
        .buffer_unordered(self.concurrency)
        .try_collect::<Vec<_>>()
        .await?;
        let result = if self.native_processing {
            if let Some(duration) = self
                .jobs
                .load_native_duration_ms(tenant_id, product_id, job_id, "native-text-v1")
                .await?
            {
                assemble_native_document_result(
                    document_id,
                    document_version,
                    pages,
                    schema,
                    duration,
                )?
            } else {
                crate::result_assembly::assemble_legacy_document_result(
                    document_id,
                    document_version,
                    pages,
                    schema,
                )?
            }
        } else {
            assemble_document_result(document_id, document_version, pages, schema)?
        };
        let terminal_state =
            if terminal_state == JobState::Completed && !result.validation_failures.is_empty() {
                JobState::ReviewRequired
            } else {
                terminal_state
            };
        let outcome = self
            .publisher
            .publish(tenant_id, product_id, job_id, terminal_state, &result)
            .await?;
        if self.native_processing
            && matches!(
                outcome,
                CommitResultOutcome::Committed(_) | CommitResultOutcome::Existing(_)
            )
        {
            if let Err(error) = self
                .jobs
                .clear_native_pages(tenant_id, product_id, job_id)
                .await
            {
                tracing::warn!(job_id=job_id.as_str(),%error,"native cache cleanup failed after result publication");
            }
        }
        Ok(outcome)
    }

    pub async fn finalize_stored(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<CommitResultOutcome, DocumentFinalizeError> {
        let identity = self
            .jobs
            .load_document_identity(tenant_id, product_id, job_id)
            .await?
            .ok_or(DocumentFinalizeError::NotReady)?;
        let schema = identity
            .extraction
            .map(|reference| {
                ExtractionSchema::registered(&reference.schema_id, &reference.schema_version)
                    .ok_or(DocumentFinalizeError::UnregisteredSchema)
            })
            .transpose()?;
        self.finalize(
            tenant_id,
            product_id,
            job_id,
            identity.document_id,
            identity.document_version,
            schema,
        )
        .await
    }
}
