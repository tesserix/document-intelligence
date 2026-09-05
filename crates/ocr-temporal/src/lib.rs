//! Temporal qualification adapter for OCR workflow dispatch.

mod work_scope_dispatcher;

pub use work_scope_dispatcher::{
    ReconcileFuture, UploadReconciler, WorkScopeDispatchError, WorkScopeDispatchOutcome,
    WorkScopeDispatcher,
};

use std::{
    future::Future, net::SocketAddr, ops::RangeInclusive, pin::Pin, sync::Arc, time::Duration,
};

use ocr_domain::{JobId, ProductId, TenantId, MAXIMUM_PAGE_ATTEMPTS, MAXIMUM_PAGE_COUNT};
use ocr_service::{
    scoped_workflow_id, CheckpointedPageRunner, DocumentFinalizeError, DocumentFinalizer,
    PageArtifactReader, PageProcessor, PageRunnerError, PageRunnerOutcome, ResultArtifactWriter,
    WorkflowAction, WorkflowDispatch, WorkflowDispatchError, WorkflowDispatchOutcome,
    WorkflowStarter,
};
use ocr_store::{CommitResultOutcome, PgJobStore};
use serde::{Deserialize, Deserializer, Serialize};
use temporalio_client::{
    errors::{WorkflowInteractionError, WorkflowStartError},
    Client, RpcOptions, UntypedWorkflow, WorkflowCancelOptions, WorkflowStartOptions,
};
use temporalio_common::error::ApplicationFailure;
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
const MAX_PAGE_COUNT: u32 = MAXIMUM_PAGE_COUNT;
const MAX_WORKFLOW_INPUT_BYTES: usize = 512;
const TEMPORAL_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const PAGES_PER_RUN: u32 = 50;
const DURABLE_ITERATIONS_PER_RUN: u32 = 50;
const MAX_DURABLE_RUNNER_ITERATIONS: u32 = MAX_PAGE_COUNT * MAXIMUM_PAGE_ATTEMPTS as u32;

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
pub struct DurableActivityInput {
    schema_version: &'static str,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
}

impl DurableActivityInput {
    pub fn new(product_id: ProductId, tenant_id: TenantId, job_id: JobId) -> Self {
        Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            product_id,
            tenant_id,
            job_id,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDurableActivityInput {
    schema_version: String,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
}

impl<'de> Deserialize<'de> for DurableActivityInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireDurableActivityInput::deserialize(deserializer)?;
        if wire.schema_version != WORKFLOW_SCHEMA_VERSION {
            return Err(serde::de::Error::custom("invalid durable activity input"));
        }
        Ok(Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            product_id: wire.product_id,
            tenant_id: wire.tenant_id,
            job_id: wire.job_id,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableActivityStatus {
    Running,
    Completed,
    Partial,
    Cancelled,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableActivityOutput {
    status: DurableActivityStatus,
}

impl DurableActivityOutput {
    pub fn new(status: DurableActivityStatus) -> Self {
        Self { status }
    }

    pub fn status(&self) -> DurableActivityStatus {
        self.status
    }
}

impl From<PageRunnerOutcome> for DurableActivityOutput {
    fn from(value: PageRunnerOutcome) -> Self {
        let status = match value {
            PageRunnerOutcome::Idle(status) | PageRunnerOutcome::Progressed(status) => match status
            {
                ocr_domain::PageWorkflowStatus::Running => DurableActivityStatus::Running,
                ocr_domain::PageWorkflowStatus::Completed => DurableActivityStatus::Completed,
                ocr_domain::PageWorkflowStatus::Partial => DurableActivityStatus::Partial,
                ocr_domain::PageWorkflowStatus::Cancelled => DurableActivityStatus::Cancelled,
            },
        };
        Self::new(status)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DurableExecutionErrorKind {
    DependencyUnavailable,
    RevisionConflict,
    InvalidInput,
    ScopeNotFound,
    IterationLimit,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
#[error("durable activity execution failed")]
pub struct DurableExecutionError {
    kind: DurableExecutionErrorKind,
}

impl DurableExecutionError {
    pub fn new(kind: DurableExecutionErrorKind) -> Self {
        Self { kind }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            DurableExecutionErrorKind::DependencyUnavailable
                | DurableExecutionErrorKind::RevisionConflict
        )
    }

    pub fn kind(&self) -> DurableExecutionErrorKind {
        self.kind
    }

    pub fn into_activity_error(self) -> ActivityError {
        if self.is_retryable() {
            ApplicationFailure::new(self).into()
        } else {
            ApplicationFailure::non_retryable(self).into()
        }
    }
}

impl From<PageRunnerError> for DurableExecutionError {
    fn from(value: PageRunnerError) -> Self {
        let kind = match value {
            PageRunnerError::RetryableConflict => DurableExecutionErrorKind::RevisionConflict,
            PageRunnerError::NotFound => DurableExecutionErrorKind::ScopeNotFound,
            PageRunnerError::InvalidConfiguration | PageRunnerError::Domain(_) => {
                DurableExecutionErrorKind::InvalidInput
            }
            PageRunnerError::Store(_) => DurableExecutionErrorKind::DependencyUnavailable,
        };
        Self::new(kind)
    }
}

pub type DurablePageExecutionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DurableActivityOutput, DurableExecutionError>> + Send + 'a>>;

pub trait DurablePageExecution: Send + Sync {
    fn execute<'a>(&'a self, input: DurableActivityInput) -> DurablePageExecutionFuture<'a>;
}

pub type DurableFinalizationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), DurableExecutionError>> + Send + 'a>>;

pub trait DurableFinalizationExecution: Send + Sync {
    fn finalize<'a>(&'a self, input: DurableActivityInput) -> DurableFinalizationFuture<'a>;
}

pub struct DocumentFinalizerExecutor<R, W> {
    finalizer: DocumentFinalizer<R, W>,
}

impl<R, W> DocumentFinalizerExecutor<R, W>
where
    R: PageArtifactReader,
    W: ResultArtifactWriter,
{
    pub fn new(finalizer: DocumentFinalizer<R, W>) -> Self {
        Self { finalizer }
    }

    async fn run(&self, input: &DurableActivityInput) -> Result<(), DurableExecutionError> {
        match self
            .finalizer
            .finalize_stored(&input.tenant_id, &input.product_id, &input.job_id)
            .await
        {
            Ok(CommitResultOutcome::Committed(_) | CommitResultOutcome::Existing(_)) => Ok(()),
            Ok(CommitResultOutcome::Conflict | CommitResultOutcome::NotCommittable) => Err(
                DurableExecutionError::new(DurableExecutionErrorKind::InvalidInput),
            ),
            Ok(CommitResultOutcome::NotFound) => Err(DurableExecutionError::new(
                DurableExecutionErrorKind::ScopeNotFound,
            )),
            Err(error) => Err(DurableExecutionError::new(match error {
                DocumentFinalizeError::Cancelled => DurableExecutionErrorKind::ScopeNotFound,
                DocumentFinalizeError::InvalidConfiguration
                | DocumentFinalizeError::InvalidPageArtifact
                | DocumentFinalizeError::Assembly(_) => DurableExecutionErrorKind::InvalidInput,
                DocumentFinalizeError::NotReady
                | DocumentFinalizeError::IncompleteArtifacts
                | DocumentFinalizeError::PageArtifact(_)
                | DocumentFinalizeError::Publish(_)
                | DocumentFinalizeError::Store(_) => {
                    DurableExecutionErrorKind::DependencyUnavailable
                }
            })),
        }
    }
}

impl<R, W> DurableFinalizationExecution for DocumentFinalizerExecutor<R, W>
where
    R: PageArtifactReader,
    W: ResultArtifactWriter,
{
    fn finalize<'a>(&'a self, input: DurableActivityInput) -> DurableFinalizationFuture<'a> {
        Box::pin(async move { self.run(&input).await })
    }
}

pub struct CheckpointedPageExecutor<P> {
    store: PgJobStore,
    processor: P,
    batch_size: usize,
    concurrency: usize,
}

impl<P> CheckpointedPageExecutor<P>
where
    P: PageProcessor,
{
    pub fn new(
        store: PgJobStore,
        processor: P,
        batch_size: usize,
        concurrency: usize,
    ) -> Result<Self, DurableExecutionError> {
        CheckpointedPageRunner::new(&store, &processor, batch_size, concurrency)
            .map_err(DurableExecutionError::from)?;
        Ok(Self {
            store,
            processor,
            batch_size,
            concurrency,
        })
    }

    pub async fn run(
        &self,
        input: &DurableActivityInput,
    ) -> Result<DurableActivityOutput, DurableExecutionError> {
        let runner = CheckpointedPageRunner::new(
            &self.store,
            &self.processor,
            self.batch_size,
            self.concurrency,
        )
        .map_err(DurableExecutionError::from)?;
        runner
            .run_once(&input.tenant_id, &input.product_id, &input.job_id)
            .await
            .map(DurableActivityOutput::from)
            .map_err(DurableExecutionError::from)
    }
}

impl<P> DurablePageExecution for CheckpointedPageExecutor<P>
where
    P: PageProcessor,
{
    fn execute<'a>(&'a self, input: DurableActivityInput) -> DurablePageExecutionFuture<'a> {
        Box::pin(async move { self.run(&input).await })
    }
}

pub fn durable_activity_options(iteration: u32) -> Option<ActivityOptions> {
    if !(1..=MAX_PAGE_COUNT * u32::from(MAXIMUM_PAGE_ATTEMPTS)).contains(&iteration) {
        return None;
    }
    let policy = ActivityPolicy::page_ocr();
    let retry_policy = RetryPolicy::builder()
        .initial_interval(policy.initial_backoff())
        .maximum_interval(policy.maximum_backoff())
        .maximum_attempts(policy.maximum_attempts())
        .build();
    Some(
        ActivityOptions::with_start_to_close_timeout(policy.start_to_close_timeout())
            .activity_id(format!("ocr-runner-{iteration:04}"))
            .heartbeat_timeout(policy.heartbeat_timeout())
            .cancellation_type(ActivityCancellationType::WaitCancellationCompleted)
            .retry_policy(retry_policy)
            .build(),
    )
}

pub struct DurablePageActivities {
    execution: Arc<dyn DurablePageExecution>,
}

pub struct DurableFinalizationActivities {
    execution: Arc<dyn DurableFinalizationExecution>,
}

impl DurableFinalizationActivities {
    pub fn new(execution: Arc<dyn DurableFinalizationExecution>) -> Self {
        Self { execution }
    }
}

impl DurablePageActivities {
    pub fn new(execution: Arc<dyn DurablePageExecution>) -> Self {
        Self { execution }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DurableWorkflowRunInput {
    schema_version: String,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    next_iteration: u32,
    run_number: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDurableWorkflowRunInput {
    schema_version: String,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    next_iteration: u32,
    run_number: u32,
}

impl<'de> Deserialize<'de> for DurableWorkflowRunInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireDurableWorkflowRunInput::deserialize(deserializer)?;
        let expected_iteration = wire
            .run_number
            .checked_sub(1)
            .and_then(|run| run.checked_mul(DURABLE_ITERATIONS_PER_RUN))
            .and_then(|iteration| iteration.checked_add(1));
        if wire.schema_version != WORKFLOW_SCHEMA_VERSION
            || expected_iteration != Some(wire.next_iteration)
            || !(1..=MAX_DURABLE_RUNNER_ITERATIONS).contains(&wire.next_iteration)
        {
            return Err(serde::de::Error::custom(
                "invalid durable workflow run input",
            ));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            product_id: wire.product_id,
            tenant_id: wire.tenant_id,
            job_id: wire.job_id,
            next_iteration: wire.next_iteration,
            run_number: wire.run_number,
        })
    }
}

impl DurableWorkflowRunInput {
    pub fn first(input: DurableActivityInput) -> Self {
        Self {
            schema_version: WORKFLOW_SCHEMA_VERSION.to_owned(),
            product_id: input.product_id,
            tenant_id: input.tenant_id,
            job_id: input.job_id,
            next_iteration: 1,
            run_number: 1,
        }
    }

    pub fn run_number(&self) -> u32 {
        self.run_number
    }

    pub fn iteration_range(&self) -> RangeInclusive<u32> {
        let last_iteration = self
            .next_iteration
            .saturating_add(DURABLE_ITERATIONS_PER_RUN - 1)
            .min(MAX_DURABLE_RUNNER_ITERATIONS);
        self.next_iteration..=last_iteration
    }

    pub fn next_run(&self) -> Option<Self> {
        let next_iteration = self.iteration_range().end().saturating_add(1);
        (next_iteration <= MAX_DURABLE_RUNNER_ITERATIONS).then(|| Self {
            schema_version: self.schema_version.clone(),
            product_id: self.product_id.clone(),
            tenant_id: self.tenant_id.clone(),
            job_id: self.job_id.clone(),
            next_iteration,
            run_number: self.run_number.saturating_add(1),
        })
    }

    fn activity_input(&self) -> DurableActivityInput {
        DurableActivityInput::new(
            self.product_id.clone(),
            self.tenant_id.clone(),
            self.job_id.clone(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableWorkflowResultMetadata {
    pub status: DurableActivityStatus,
    pub runner_iterations: u32,
    pub runs: u32,
}

#[workflow]
#[derive(Default)]
pub struct DurableDocumentWorkflow;

#[workflow_methods]
impl DurableDocumentWorkflow {
    #[run(name = "ocr_durable_document_v1")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: DurableWorkflowRunInput,
    ) -> WorkflowResult<DurableWorkflowResultMetadata> {
        for iteration in input.iteration_range() {
            let options = match durable_activity_options(iteration) {
                Some(options) => options,
                None => {
                    return Err(
                        ApplicationFailure::non_retryable(DurableExecutionError::new(
                            DurableExecutionErrorKind::InvalidInput,
                        ))
                        .into(),
                    )
                }
            };
            let outcome = ctx
                .execute_activity(
                    DurablePageActivities::run_checkpointed_pages,
                    input.activity_input(),
                    options,
                )
                .await?;
            if outcome.status() != DurableActivityStatus::Running {
                if matches!(
                    outcome.status(),
                    DurableActivityStatus::Completed | DurableActivityStatus::Partial
                ) {
                    ctx.execute_activity(
                        DurableFinalizationActivities::finalize_document,
                        input.activity_input(),
                        durable_activity_options(iteration).ok_or_else(|| {
                            ApplicationFailure::non_retryable(DurableExecutionError::new(
                                DurableExecutionErrorKind::InvalidInput,
                            ))
                        })?,
                    )
                    .await?;
                }
                return Ok(DurableWorkflowResultMetadata {
                    status: outcome.status(),
                    runner_iterations: iteration,
                    runs: input.run_number,
                });
            }
        }
        if let Some(next) = input.next_run() {
            ctx.continue_as_new(next, ContinueAsNewOptions::default())?;
        }
        Err(
            ApplicationFailure::non_retryable(DurableExecutionError::new(
                DurableExecutionErrorKind::IterationLimit,
            ))
            .into(),
        )
    }
}

#[activities]
impl DurablePageActivities {
    #[activity(name = "run_checkpointed_pages_v1")]
    pub async fn run_checkpointed_pages(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: DurableActivityInput,
    ) -> Result<DurableActivityOutput, ActivityError> {
        let execution = self.execution.execute(input);
        tokio::pin!(execution);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                result = &mut execution => {
                    return result.map_err(DurableExecutionError::into_activity_error);
                }
                _ = ctx.cancelled() => return Err(ActivityError::cancelled()),
                _ = heartbeat.tick() => ctx.record_heartbeat(()).await?,
            }
        }
    }
}

#[activities]
impl DurableFinalizationActivities {
    #[activity(name = "finalize_document_v1")]
    pub async fn finalize_document(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: DurableActivityInput,
    ) -> Result<(), ActivityError> {
        let execution = self.execution.finalize(input);
        tokio::pin!(execution);
        tokio::select! {
            result = &mut execution => result.map_err(DurableExecutionError::into_activity_error),
            _ = ctx.cancelled() => Err(ActivityError::cancelled()),
        }
    }
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
                        DurableDocumentWorkflow::run,
                        DurableWorkflowRunInput::first(DurableActivityInput::new(
                            input.product_id,
                            input.tenant_id,
                            input.job_id,
                        )),
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

#[cfg(test)]
mod checkpointed_temporal_starter_tests {
    use super::{
        CheckpointedTemporalStarter, GatewayError, GatewayOutcome, TemporalCommand,
        TemporalGateway, TemporalStarter,
    };
    use ocr_domain::{JobId, ProductId, TenantId};
    use ocr_service::{
        scoped_workflow_id, WorkflowAction, WorkflowDispatch, WorkflowDispatchError,
        WorkflowDispatchOutcome, WorkflowStarter,
    };
    use std::sync::{Arc, Mutex};

    struct RecordingCheckpoint {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl WorkflowStarter for RecordingCheckpoint {
        async fn dispatch(
            &self,
            _dispatch: WorkflowDispatch,
        ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
            self.events.lock().unwrap().push("checkpoint");
            Ok(WorkflowDispatchOutcome::Started)
        }
    }

    struct RecordingGateway {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TemporalGateway for RecordingGateway {
        async fn execute(&self, _command: TemporalCommand) -> Result<GatewayOutcome, GatewayError> {
            self.events.lock().unwrap().push("temporal");
            Ok(GatewayOutcome::Accepted)
        }
    }

    #[tokio::test]
    async fn checkpoint_is_persisted_before_starting_temporal_workflow() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let product_id = ProductId::new("kora").unwrap();
        let tenant_id = TenantId::new("ten_CHECKPOINT").unwrap();
        let job_id = JobId::new("job_CHECKPOINT").unwrap();
        let dispatch = WorkflowDispatch {
            event_id: 1,
            workflow_id: scoped_workflow_id(&product_id, &tenant_id, &job_id),
            product_id,
            tenant_id,
            job_id,
            page_count: 1,
            action: WorkflowAction::Start,
        };
        let temporal = TemporalStarter::new(
            Arc::new(RecordingGateway {
                events: events.clone(),
            }),
            "document-intelligence-test",
        )
        .unwrap();
        let starter = CheckpointedTemporalStarter::new(
            RecordingCheckpoint {
                events: events.clone(),
            },
            temporal,
        );

        assert_eq!(
            starter.dispatch(dispatch).await.unwrap(),
            WorkflowDispatchOutcome::Started
        );
        assert_eq!(*events.lock().unwrap(), ["checkpoint", "temporal"]);
    }
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

pub struct CheckpointedTemporalStarter<C, G> {
    checkpoint: C,
    temporal: TemporalStarter<G>,
}

impl<C, G> CheckpointedTemporalStarter<C, G> {
    pub fn new(checkpoint: C, temporal: TemporalStarter<G>) -> Self {
        Self {
            checkpoint,
            temporal,
        }
    }
}

impl<C, G> WorkflowStarter for CheckpointedTemporalStarter<C, G>
where
    C: WorkflowStarter,
    G: TemporalGateway,
{
    async fn dispatch(
        &self,
        dispatch: WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        let checkpointed = self.checkpoint.dispatch(dispatch.clone()).await?;
        let started = self.temporal.dispatch(dispatch).await?;
        Ok(
            if matches!(checkpointed, WorkflowDispatchOutcome::Started)
                || matches!(started, WorkflowDispatchOutcome::Started)
            {
                WorkflowDispatchOutcome::Started
            } else {
                WorkflowDispatchOutcome::Existing
            },
        )
    }
}
