//! Temporal qualification adapter for OCR workflow dispatch.

use std::{future::Future, net::SocketAddr, ops::RangeInclusive, sync::Arc, time::Duration};

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
use temporalio_common::worker::WorkerDeploymentOptions;
use temporalio_common::{protos::temporal::api::enums::v1::WorkflowIdReusePolicy, RetryPolicy};
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::{
    activities::{ActivityContext, ActivityError},
    ActivityCancellationType, ActivityOptions, ContinueAsNewOptions, WorkflowContext,
    WorkflowResult,
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

const WORKFLOW_SCHEMA_VERSION: &str = "1";
const MAX_PAGE_COUNT: u32 = 300;
const MAX_WORKFLOW_INPUT_BYTES: usize = 512;
const TEMPORAL_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const PAGES_PER_RUN: u32 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowInput {
    schema_version: &'static str,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    page_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRunInput {
    schema_version: String,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    page_count: u32,
    next_page: u32,
    run_number: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireWorkflowRunInput {
    schema_version: String,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    page_count: u32,
    next_page: u32,
    run_number: u32,
}

impl<'de> Deserialize<'de> for WorkflowRunInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireWorkflowRunInput::deserialize(deserializer)?;
        let expected_next_page = wire
            .run_number
            .checked_sub(1)
            .and_then(|run| run.checked_mul(PAGES_PER_RUN))
            .and_then(|page| page.checked_add(1));
        if wire.schema_version != WORKFLOW_SCHEMA_VERSION
            || !(1..=MAX_PAGE_COUNT).contains(&wire.page_count)
            || expected_next_page != Some(wire.next_page)
            || wire.next_page > wire.page_count
        {
            return Err(serde::de::Error::custom("invalid workflow run input"));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            product_id: wire.product_id,
            tenant_id: wire.tenant_id,
            job_id: wire.job_id,
            page_count: wire.page_count,
            next_page: wire.next_page,
            run_number: wire.run_number,
        })
    }
}

impl WorkflowRunInput {
    pub fn first(input: WorkflowInput) -> Self {
        Self {
            schema_version: input.schema_version.to_owned(),
            product_id: input.product_id,
            tenant_id: input.tenant_id,
            job_id: input.job_id,
            page_count: input.page_count,
            next_page: 1,
            run_number: 1,
        }
    }

    pub fn run_number(&self) -> u32 {
        self.run_number
    }

    pub fn page_range(&self) -> RangeInclusive<u32> {
        let last_page = self
            .next_page
            .saturating_add(PAGES_PER_RUN - 1)
            .min(self.page_count);
        self.next_page..=last_page
    }

    pub fn next_run(&self) -> Option<Self> {
        let next_page = self.page_range().end().saturating_add(1);
        (next_page <= self.page_count).then(|| Self {
            schema_version: self.schema_version.clone(),
            product_id: self.product_id.clone(),
            tenant_id: self.tenant_id.clone(),
            job_id: self.job_id.clone(),
            page_count: self.page_count,
            next_page,
            run_number: self.run_number.saturating_add(1),
        })
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageActivityInput {
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    page: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePageActivityInput {
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    page: u32,
}

impl<'de> Deserialize<'de> for PageActivityInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WirePageActivityInput::deserialize(deserializer)?;
        if !(1..=MAX_PAGE_COUNT).contains(&wire.page) {
            return Err(serde::de::Error::custom("invalid page activity input"));
        }
        Ok(Self {
            product_id: wire.product_id,
            tenant_id: wire.tenant_id,
            job_id: wire.job_id,
            page: wire.page,
        })
    }
}

impl WorkflowRunInput {
    fn page_activity_input(&self, page: u32) -> PageActivityInput {
        PageActivityInput {
            product_id: self.product_id.clone(),
            tenant_id: self.tenant_id.clone(),
            job_id: self.job_id.clone(),
            page,
        }
    }
}

pub struct QualificationPageActivities {
    started: Option<Arc<tokio::sync::Notify>>,
    started_page: Option<u32>,
    started_endpoint: Option<SocketAddr>,
    heartbeat_steps: u32,
}

impl Default for QualificationPageActivities {
    fn default() -> Self {
        Self {
            started: None,
            started_page: None,
            started_endpoint: None,
            heartbeat_steps: 1,
        }
    }
}

impl QualificationPageActivities {
    pub fn with_started_notifier(started: Arc<tokio::sync::Notify>) -> Self {
        Self {
            started: Some(started),
            started_page: Some(1),
            started_endpoint: None,
            heartbeat_steps: 100,
        }
    }

    pub fn with_page_started_notifier(
        page: u32,
        started: Arc<tokio::sync::Notify>,
    ) -> Option<Self> {
        (1..=MAX_PAGE_COUNT).contains(&page).then_some(Self {
            started: Some(started),
            started_page: Some(page),
            started_endpoint: None,
            heartbeat_steps: 100,
        })
    }

    pub fn held_for_process_loss(started_endpoint: SocketAddr) -> Self {
        Self {
            started: None,
            started_page: None,
            started_endpoint: Some(started_endpoint),
            heartbeat_steps: 1_000,
        }
    }
}

pub fn qualification_deployment_options() -> WorkerDeploymentOptions {
    WorkerDeploymentOptions::from_build_id("ocr-temporal-qualification-v1".to_owned())
}

#[activities]
impl QualificationPageActivities {
    #[activity(name = "ocr_page_v1")]
    pub async fn process_page(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: PageActivityInput,
    ) -> Result<u32, ActivityError> {
        let is_observed_page = self.started_page == Some(input.page);
        if let Some(started) = self.started.as_ref().filter(|_| is_observed_page) {
            started.notify_one();
        }
        if let Some(started_endpoint) = self.started_endpoint {
            if let Ok(mut stream) = tokio::net::TcpStream::connect(started_endpoint).await {
                let _ = stream.write_all(&[1]).await;
            }
        }
        if ctx.is_cancelled() {
            return Err(ActivityError::cancelled());
        }
        let heartbeat_steps = if is_observed_page || self.started_endpoint.is_some() {
            self.heartbeat_steps
        } else {
            1
        };
        for progress in 1..=heartbeat_steps {
            ctx.record_heartbeat((input.page, progress)).await?;
            tokio::time::sleep(Duration::from_millis(10)).await;
            if ctx.is_cancelled() {
                return Err(ActivityError::cancelled());
            }
        }
        Ok(input.page)
    }
}

pub fn page_activity_options(page: u32) -> ActivityOptions {
    let policy = ActivityPolicy::page_ocr();
    let retry_policy = RetryPolicy::builder()
        .initial_interval(policy.initial_backoff())
        .maximum_interval(policy.maximum_backoff())
        .maximum_attempts(policy.maximum_attempts())
        .non_retryable_error_types(policy.non_retryable_errors().iter().copied())
        .build();
    ActivityOptions::with_start_to_close_timeout(policy.start_to_close_timeout())
        .activity_id(format!("ocr-page-{page:04}"))
        .heartbeat_timeout(policy.heartbeat_timeout())
        .cancellation_type(ActivityCancellationType::WaitCancellationCompleted)
        .retry_policy(retry_policy)
        .build()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowResultMetadata {
    pub pages_processed: u32,
    pub runs: u32,
}

#[workflow]
#[derive(Default)]
pub struct OcrDocumentWorkflow;

#[workflow_methods]
impl OcrDocumentWorkflow {
    #[run(name = "ocr_document_v1")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: WorkflowRunInput,
    ) -> WorkflowResult<WorkflowResultMetadata> {
        for page in input.page_range() {
            ctx.execute_activity(
                QualificationPageActivities::process_page,
                input.page_activity_input(page),
                page_activity_options(page),
            )
            .await?;
        }
        if let Some(next) = input.next_run() {
            ctx.continue_as_new(next, ContinueAsNewOptions::default())?;
        }
        Ok(WorkflowResultMetadata {
            pages_processed: input.page_count,
            runs: input.run_number,
        })
    }
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
                    .start_workflow(
                        OcrDocumentWorkflow::run,
                        WorkflowRunInput::first(input),
                        options,
                    )
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
