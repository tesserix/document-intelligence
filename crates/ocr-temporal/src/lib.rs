//! Temporal qualification adapter for OCR workflow dispatch.

use std::{future::Future, ops::RangeInclusive, sync::Arc, time::Duration};

use ocr_domain::{JobId, ProductId, TenantId};
use ocr_service::{
    scoped_workflow_id, WorkflowAction, WorkflowDispatch, WorkflowDispatchError,
    WorkflowDispatchOutcome, WorkflowStarter,
};
use serde::{Deserialize, Deserializer, Serialize};
use temporalio_client::{
    errors::{WorkflowInteractionError, WorkflowStartError},
    Client, RpcOptions, UntypedWorkflow, WorkflowCancelOptions, WorkflowStartOptions,
};
use temporalio_common::{
    protos::temporal::api::enums::v1::WorkflowIdReusePolicy, HasWorkflowDefinition,
    WorkflowDefinition,
};
use thiserror::Error;

const WORKFLOW_SCHEMA_VERSION: &str = "1";
const MAX_PAGE_COUNT: u32 = 300;
const MAX_WORKFLOW_INPUT_BYTES: usize = 512;
const TEMPORAL_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const OCR_WORKFLOW_TYPE: &str = "ocr_document_v1";
const PAGES_PER_RUN: u32 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowInput {
    schema_version: &'static str,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    page_count: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireWorkflowInput {
    schema_version: String,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    page_count: u32,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
#[error("invalid workflow input")]
pub struct WorkflowInputError;

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
#[error("invalid workflow plan")]
pub struct WorkflowPlanError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRun {
    first_page: u32,
    last_page: u32,
    continues_as_new: bool,
}

impl WorkflowRun {
    pub fn page_range(&self) -> RangeInclusive<u32> {
        self.first_page..=self.last_page
    }

    pub fn continues_as_new(&self) -> bool {
        self.continues_as_new
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPlan {
    page_count: u32,
    runs: Vec<WorkflowRun>,
}

impl WorkflowPlan {
    pub fn new(page_count: u32) -> Result<Self, WorkflowPlanError> {
        if !(1..=MAX_PAGE_COUNT).contains(&page_count) {
            return Err(WorkflowPlanError);
        }
        let mut runs = Vec::new();
        let mut first_page = 1;
        while first_page <= page_count {
            let last_page = first_page.saturating_add(PAGES_PER_RUN - 1).min(page_count);
            runs.push(WorkflowRun {
                first_page,
                last_page,
                continues_as_new: last_page < page_count,
            });
            first_page = last_page + 1;
        }
        Ok(Self { page_count, runs })
    }

    pub fn runs(&self) -> &[WorkflowRun] {
        &self.runs
    }

    pub fn activity_id(&self, page: u32) -> Result<String, WorkflowPlanError> {
        if !(1..=self.page_count).contains(&page) {
            return Err(WorkflowPlanError);
        }
        Ok(format!("ocr-page-{page:04}"))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ActivityPolicy {
    start_to_close_timeout: Duration,
    heartbeat_timeout: Duration,
    maximum_attempts: u32,
    initial_backoff: Duration,
    maximum_backoff: Duration,
}

impl ActivityPolicy {
    pub fn page_ocr() -> Self {
        Self {
            start_to_close_timeout: Duration::from_secs(120),
            heartbeat_timeout: Duration::from_secs(10),
            maximum_attempts: 3,
            initial_backoff: Duration::from_secs(1),
            maximum_backoff: Duration::from_secs(30),
        }
    }

    pub fn start_to_close_timeout(&self) -> Duration {
        self.start_to_close_timeout
    }

    pub fn heartbeat_timeout(&self) -> Duration {
        self.heartbeat_timeout
    }

    pub fn maximum_attempts(&self) -> u32 {
        self.maximum_attempts
    }

    pub fn initial_backoff(&self) -> Duration {
        self.initial_backoff
    }

    pub fn maximum_backoff(&self) -> Duration {
        self.maximum_backoff
    }

    pub fn non_retryable_errors(&self) -> &'static [&'static str] {
        &["invalid_document", "scope_violation"]
    }
}

impl WorkflowInput {
    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    fn new(
        product_id: ProductId,
        tenant_id: TenantId,
        job_id: JobId,
        page_count: u32,
    ) -> Result<Self, WorkflowInputError> {
        if !(1..=MAX_PAGE_COUNT).contains(&page_count) {
            return Err(WorkflowInputError);
        }
        let input = Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            product_id,
            tenant_id,
            job_id,
            page_count,
        };
        let encoded = serde_json::to_vec(&input).map_err(|_| WorkflowInputError)?;
        if encoded.len() > MAX_WORKFLOW_INPUT_BYTES {
            return Err(WorkflowInputError);
        }
        Ok(input)
    }
}

impl TryFrom<&WorkflowDispatch> for WorkflowInput {
    type Error = WorkflowInputError;

    fn try_from(dispatch: &WorkflowDispatch) -> Result<Self, Self::Error> {
        Self::new(
            dispatch.product_id.clone(),
            dispatch.tenant_id.clone(),
            dispatch.job_id.clone(),
            dispatch.page_count,
        )
    }
}

impl<'de> Deserialize<'de> for WorkflowInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireWorkflowInput::deserialize(deserializer)?;
        if wire.schema_version != WORKFLOW_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(WorkflowInputError));
        }
        Self::new(
            wire.product_id,
            wire.tenant_id,
            wire.job_id,
            wire.page_count,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemporalCommand {
    Start {
        workflow_id: String,
        request_id: String,
        task_queue: String,
        input: WorkflowInput,
    },
    Cancel {
        workflow_id: String,
        request_id: String,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum GatewayOutcome {
    Accepted,
    AlreadyExists,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum GatewayError {
    #[error("workflow gateway is unavailable")]
    Unavailable,
}

struct OcrDocumentWorkflow;

impl WorkflowDefinition for OcrDocumentWorkflow {
    type Input = WorkflowInput;
    type Output = ();

    fn name(&self) -> &str {
        OCR_WORKFLOW_TYPE
    }
}

impl HasWorkflowDefinition for OcrDocumentWorkflow {
    type Run = Self;
}

pub struct OfficialTemporalGateway {
    client: Client,
}

impl OfficialTemporalGateway {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    fn rpc_options() -> RpcOptions {
        RpcOptions::builder().timeout(TEMPORAL_RPC_TIMEOUT).build()
    }
}

impl TemporalGateway for OfficialTemporalGateway {
    async fn execute(&self, command: TemporalCommand) -> Result<GatewayOutcome, GatewayError> {
        match command {
            TemporalCommand::Start {
                workflow_id,
                request_id: _,
                task_queue,
                input,
            } => {
                let options = WorkflowStartOptions::new(task_queue, workflow_id)
                    .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
                    .rpc_options(Self::rpc_options())
                    .build();
                match self
                    .client
                    .start_workflow(OcrDocumentWorkflow, input, options)
                    .await
                {
                    Ok(_) => Ok(GatewayOutcome::Accepted),
                    Err(WorkflowStartError::AlreadyStarted { .. }) => {
                        Ok(GatewayOutcome::AlreadyExists)
                    }
                    Err(_) => Err(GatewayError::Unavailable),
                }
            }
            TemporalCommand::Cancel {
                workflow_id,
                request_id,
            } => {
                let handle = self
                    .client
                    .get_workflow_handle::<UntypedWorkflow>(workflow_id);
                let options = WorkflowCancelOptions::builder()
                    .request_id(request_id)
                    .rpc_options(Self::rpc_options())
                    .build();
                match handle.cancel(options).await {
                    Ok(()) => Ok(GatewayOutcome::Accepted),
                    Err(WorkflowInteractionError::NotFound(_)) => Ok(GatewayOutcome::AlreadyExists),
                    Err(_) => Err(GatewayError::Unavailable),
                }
            }
        }
    }
}

pub trait TemporalGateway: Send + Sync {
    fn execute(
        &self,
        command: TemporalCommand,
    ) -> impl Future<Output = Result<GatewayOutcome, GatewayError>> + Send;
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
#[error("invalid Temporal starter configuration")]
pub struct TemporalStarterConfigurationError;

pub struct TemporalStarter<G> {
    gateway: Arc<G>,
    task_queue: String,
}

impl<G> TemporalStarter<G> {
    pub fn new(
        gateway: Arc<G>,
        task_queue: &str,
    ) -> Result<Self, TemporalStarterConfigurationError> {
        let valid = !task_queue.is_empty()
            && task_queue.len() <= 127
            && task_queue
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !valid {
            return Err(TemporalStarterConfigurationError);
        }
        Ok(Self {
            gateway,
            task_queue: task_queue.to_owned(),
        })
    }
}

impl<G> WorkflowStarter for TemporalStarter<G>
where
    G: TemporalGateway,
{
    async fn dispatch(
        &self,
        dispatch: WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        if dispatch.workflow_id
            != scoped_workflow_id(&dispatch.product_id, &dispatch.tenant_id, &dispatch.job_id)
        {
            return Err(WorkflowDispatchError::Unavailable);
        }
        let input =
            WorkflowInput::try_from(&dispatch).map_err(|_| WorkflowDispatchError::Unavailable)?;
        let request_id = format!("ocr-outbox-{}", dispatch.event_id);
        let command = match dispatch.action {
            WorkflowAction::Start => TemporalCommand::Start {
                workflow_id: dispatch.workflow_id,
                request_id,
                task_queue: self.task_queue.clone(),
                input,
            },
            WorkflowAction::Cancel => TemporalCommand::Cancel {
                workflow_id: dispatch.workflow_id,
                request_id,
            },
        };
        match self.gateway.execute(command).await {
            Ok(GatewayOutcome::Accepted) => Ok(WorkflowDispatchOutcome::Started),
            Ok(GatewayOutcome::AlreadyExists) => Ok(WorkflowDispatchOutcome::Existing),
            Err(GatewayError::Unavailable) => Err(WorkflowDispatchError::Unavailable),
        }
    }
}
