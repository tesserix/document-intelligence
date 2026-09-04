use std::sync::Arc;

use futures_util::{stream, StreamExt};
use ocr_domain::{
    DocumentId, DocumentPage, DocumentVersion, JobId, JobState, PageWorkflowStatus, ProductId,
    TenantId,
};
use ocr_store::{CommitResultOutcome, PgJobStore};
use thiserror::Error;

use crate::{
    assemble_document_result, PageArtifactReadError, PageArtifactReader, PublishResultError,
    ResultArtifactWriter, ResultAssemblyError, ResultPublisher, MAXIMUM_RESULT_BYTES,
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
        })
    }

    pub async fn finalize(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
        document_id: DocumentId,
        document_version: DocumentVersion,
    ) -> Result<CommitResultOutcome, DocumentFinalizeError> {
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
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        let result = assemble_document_result(document_id, document_version, pages)?;
        self.publisher
            .publish(tenant_id, product_id, job_id, terminal_state, &result)
            .await
            .map_err(Into::into)
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
        self.finalize(
            tenant_id,
            product_id,
            job_id,
            identity.document_id,
            identity.document_version,
        )
        .await
    }
}
