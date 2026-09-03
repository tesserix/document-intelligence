use std::{future::Future, sync::Arc};

use ocr_domain::{JobId, ProductId, TenantId};
use ocr_store::{ClaimJobOutbox, JobOutboxEventType, PgJobStore, PublishJobOutboxOutcome};
use thiserror::Error;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WorkflowAction {
    Start,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDispatch {
    pub event_id: i64,
    pub workflow_id: String,
    pub product_id: ProductId,
    pub tenant_id: TenantId,
    pub job_id: JobId,
    pub action: WorkflowAction,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WorkflowDispatchOutcome {
    Started,
    Existing,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum WorkflowDispatchError {
    #[error("workflow service is unavailable")]
    Unavailable,
}

pub trait WorkflowStarter: Send + Sync {
    fn dispatch<'a>(
        &'a self,
        dispatch: WorkflowDispatch,
    ) -> impl Future<Output = Result<WorkflowDispatchOutcome, WorkflowDispatchError>> + Send + 'a;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RelayOutcome {
    Idle,
    Published(usize),
    Retryable { published: usize },
    LeaseLost { published: usize },
}

#[derive(Debug, Error)]
pub enum RelayError {
    #[error(transparent)]
    Store(#[from] ocr_store::Error),
}

pub struct JobOutboxRelay<W> {
    jobs: PgJobStore,
    workflows: Arc<W>,
}

impl<W> JobOutboxRelay<W>
where
    W: WorkflowStarter,
{
    pub fn new(jobs: PgJobStore, workflows: Arc<W>) -> Self {
        Self { jobs, workflows }
    }

    pub async fn relay_scope(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        lease_owner: &str,
        limit: i64,
    ) -> Result<RelayOutcome, RelayError> {
        let events = self
            .jobs
            .claim_job_outbox(
                tenant_id,
                product_id,
                ClaimJobOutbox {
                    lease_owner: lease_owner.to_owned(),
                    limit,
                },
            )
            .await?;
        if events.is_empty() {
            return Ok(RelayOutcome::Idle);
        }
        let mut published = 0_usize;
        for event in events {
            let dispatch = WorkflowDispatch {
                event_id: event.event_id,
                workflow_id: format!("ocr-job-{}", event.job_id.as_str()),
                product_id: product_id.clone(),
                tenant_id: tenant_id.clone(),
                job_id: event.job_id,
                action: match event.event_type {
                    JobOutboxEventType::Accepted => WorkflowAction::Start,
                    JobOutboxEventType::CancellationRequested => WorkflowAction::Cancel,
                },
            };
            if self.workflows.dispatch(dispatch).await.is_err() {
                return Ok(RelayOutcome::Retryable { published });
            }
            match self
                .jobs
                .publish_job_outbox(tenant_id, product_id, event.event_id, lease_owner)
                .await?
            {
                PublishJobOutboxOutcome::Published | PublishJobOutboxOutcome::Existing => {
                    published += 1;
                }
                PublishJobOutboxOutcome::LeaseLost | PublishJobOutboxOutcome::NotFound => {
                    return Ok(RelayOutcome::LeaseLost { published });
                }
            }
        }
        Ok(RelayOutcome::Published(published))
    }
}
