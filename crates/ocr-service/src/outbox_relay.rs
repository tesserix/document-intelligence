use std::{future::Future, sync::Arc};

use ocr_domain::{JobId, PageWorkflow, PageWorkflowStatus, ProductId, TenantId};
use ocr_store::{
    ClaimJobOutbox, CreatePageWorkflowOutcome, JobOutboxEventType, PgJobStore,
    PublishJobOutboxOutcome, SavePageWorkflowOutcome, StoredPageWorkflow,
};
use sha2::{Digest, Sha256};
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
    pub page_count: u32,
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

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
#[error("invalid durable workflow configuration")]
pub struct DurableWorkflowConfigurationError;

pub struct DurableWorkflowStarter {
    jobs: PgJobStore,
    max_attempts: u8,
}

impl DurableWorkflowStarter {
    pub fn new(
        jobs: PgJobStore,
        max_attempts: u8,
    ) -> Result<Self, DurableWorkflowConfigurationError> {
        if !(1..=10).contains(&max_attempts) {
            return Err(DurableWorkflowConfigurationError);
        }
        Ok(Self { jobs, max_attempts })
    }

    async fn cancel(
        &self,
        dispatch: &WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        let stored = self
            .jobs
            .load_page_workflow(&dispatch.tenant_id, &dispatch.product_id, &dispatch.job_id)
            .await
            .map_err(|_| WorkflowDispatchError::Unavailable)?;
        let stored = match stored {
            Some(stored) => stored,
            None => {
                let mut workflow = PageWorkflow::new(
                    dispatch.job_id.clone(),
                    dispatch.page_count,
                    self.max_attempts,
                )
                .map_err(|_| WorkflowDispatchError::Unavailable)?;
                workflow.request_cancellation();
                return match self
                    .jobs
                    .create_page_workflow(
                        &dispatch.tenant_id,
                        &dispatch.product_id,
                        &dispatch.job_id,
                        workflow,
                    )
                    .await
                    .map_err(|_| WorkflowDispatchError::Unavailable)?
                {
                    CreatePageWorkflowOutcome::Created(_) => Ok(WorkflowDispatchOutcome::Started),
                    CreatePageWorkflowOutcome::Existing(stored) => {
                        self.cancel_stored(dispatch, stored).await
                    }
                    CreatePageWorkflowOutcome::Conflict | CreatePageWorkflowOutcome::NotFound => {
                        Err(WorkflowDispatchError::Unavailable)
                    }
                };
            }
        };
        self.cancel_stored(dispatch, stored).await
    }

    async fn cancel_stored(
        &self,
        dispatch: &WorkflowDispatch,
        mut stored: StoredPageWorkflow,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        if stored.workflow.status() == PageWorkflowStatus::Cancelled {
            return Ok(WorkflowDispatchOutcome::Existing);
        }
        stored.workflow.request_cancellation();
        match self
            .jobs
            .save_page_workflow(
                &dispatch.tenant_id,
                &dispatch.product_id,
                &dispatch.job_id,
                stored.revision,
                stored.workflow,
            )
            .await
            .map_err(|_| WorkflowDispatchError::Unavailable)?
        {
            SavePageWorkflowOutcome::Saved(_) => Ok(WorkflowDispatchOutcome::Started),
            SavePageWorkflowOutcome::Conflict | SavePageWorkflowOutcome::NotFound => {
                Err(WorkflowDispatchError::Unavailable)
            }
        }
    }
}

impl WorkflowStarter for DurableWorkflowStarter {
    async fn dispatch(
        &self,
        dispatch: WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        match dispatch.action {
            WorkflowAction::Start => {
                let workflow = PageWorkflow::new(
                    dispatch.job_id.clone(),
                    dispatch.page_count,
                    self.max_attempts,
                )
                .map_err(|_| WorkflowDispatchError::Unavailable)?;
                match self
                    .jobs
                    .create_page_workflow(
                        &dispatch.tenant_id,
                        &dispatch.product_id,
                        &dispatch.job_id,
                        workflow,
                    )
                    .await
                    .map_err(|_| WorkflowDispatchError::Unavailable)?
                {
                    CreatePageWorkflowOutcome::Created(_) => Ok(WorkflowDispatchOutcome::Started),
                    CreatePageWorkflowOutcome::Existing(_) => Ok(WorkflowDispatchOutcome::Existing),
                    CreatePageWorkflowOutcome::Conflict | CreatePageWorkflowOutcome::NotFound => {
                        Err(WorkflowDispatchError::Unavailable)
                    }
                }
            }
            WorkflowAction::Cancel => self.cancel(&dispatch).await,
        }
    }
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
                workflow_id: scoped_workflow_id(product_id, tenant_id, &event.job_id),
                product_id: product_id.clone(),
                tenant_id: tenant_id.clone(),
                job_id: event.job_id,
                page_count: event.page_count,
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

pub fn scoped_workflow_id(product_id: &ProductId, tenant_id: &TenantId, job_id: &JobId) -> String {
    let mut digest = Sha256::new();
    digest.update(product_id.as_str());
    digest.update([0]);
    digest.update(tenant_id.as_str());
    digest.update([0]);
    digest.update(job_id.as_str());
    let encoded = format!("{:x}", digest.finalize());
    format!("ocr-v1-{}", &encoded[..32])
}
