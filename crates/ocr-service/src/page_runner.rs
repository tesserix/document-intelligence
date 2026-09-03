use std::{future::Future, pin::Pin};

use futures_util::{stream, StreamExt};
use ocr_domain::{JobId, PageTask, PageWorkflowStatus, ProductId, TenantId};
use ocr_store::{PgJobStore, SavePageWorkflowOutcome, StoredPageArtifact};
use thiserror::Error;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PageProcessError {
    Retryable,
    Permanent,
}

pub type PageProcessFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StoredPageArtifact, PageProcessError>> + Send + 'a>>;

pub trait PageProcessor: Send + Sync {
    fn process<'a>(&'a self, task: PageTask) -> PageProcessFuture<'a>;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PageRunnerOutcome {
    Idle(PageWorkflowStatus),
    Progressed(PageWorkflowStatus),
}

#[derive(Debug, Error)]
pub enum PageRunnerError {
    #[error("invalid page runner configuration")]
    InvalidConfiguration,
    #[error("page workflow not found")]
    NotFound,
    #[error("page workflow revision conflict")]
    RetryableConflict,
    #[error(transparent)]
    Domain(#[from] ocr_domain::Error),
    #[error(transparent)]
    Store(#[from] ocr_store::Error),
}

pub struct CheckpointedPageRunner<'a, P> {
    store: &'a PgJobStore,
    processor: &'a P,
    batch_size: usize,
    concurrency: usize,
}

impl<'a, P> CheckpointedPageRunner<'a, P>
where
    P: PageProcessor,
{
    pub fn new(
        store: &'a PgJobStore,
        processor: &'a P,
        batch_size: usize,
        concurrency: usize,
    ) -> Result<Self, PageRunnerError> {
        if !(1..=64).contains(&batch_size) || concurrency == 0 || concurrency > batch_size {
            return Err(PageRunnerError::InvalidConfiguration);
        }
        Ok(Self {
            store,
            processor,
            batch_size,
            concurrency,
        })
    }

    pub async fn run_once(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        job_id: &JobId,
    ) -> Result<PageRunnerOutcome, PageRunnerError> {
        let mut stored = self
            .store
            .load_page_workflow(tenant_id, product_id, job_id)
            .await?
            .ok_or(PageRunnerError::NotFound)?;
        let tasks = stored.workflow.claim_ready(self.batch_size)?;
        if tasks.is_empty() {
            return Ok(PageRunnerOutcome::Idle(stored.workflow.status()));
        }

        stored = match self
            .store
            .save_page_workflow(
                tenant_id,
                product_id,
                job_id,
                stored.revision,
                stored.workflow,
            )
            .await?
        {
            SavePageWorkflowOutcome::Saved(stored) => stored,
            SavePageWorkflowOutcome::Conflict => return Err(PageRunnerError::RetryableConflict),
            SavePageWorkflowOutcome::NotFound => return Err(PageRunnerError::NotFound),
        };

        let outcomes = stream::iter(tasks.into_iter().map(|task| async move {
            let outcome = self.processor.process(task.clone()).await;
            (task, outcome)
        }))
        .buffer_unordered(self.concurrency)
        .collect::<Vec<_>>()
        .await;

        let mut artifacts = Vec::new();
        for (task, outcome) in outcomes {
            match outcome {
                Ok(artifact) => {
                    stored.workflow.record_success(&task)?;
                    artifacts.push(artifact);
                }
                Err(PageProcessError::Retryable) => {
                    stored.workflow.record_retryable_failure(&task)?;
                }
                Err(PageProcessError::Permanent) => {
                    stored.workflow.record_permanent_failure(&task)?;
                }
            }
        }
        let status = stored.workflow.status();
        match self
            .store
            .save_page_workflow_with_artifacts(
                tenant_id,
                product_id,
                job_id,
                stored.revision,
                stored.workflow,
                artifacts,
            )
            .await?
        {
            SavePageWorkflowOutcome::Saved(_) => Ok(PageRunnerOutcome::Progressed(status)),
            SavePageWorkflowOutcome::Conflict => Err(PageRunnerError::RetryableConflict),
            SavePageWorkflowOutcome::NotFound => Err(PageRunnerError::NotFound),
        }
    }
}
