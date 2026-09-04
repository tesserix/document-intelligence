use std::{
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use ocr_domain::{JobId, ProductId, TenantId};
use ocr_service::{WorkflowAction, WorkflowDispatch};
use ocr_temporal::{
    qualification_deployment_options, DurableActivityInput, DurableActivityOutput,
    DurableActivityStatus, DurableDocumentWorkflow, DurableFinalizationActivities,
    DurableFinalizationExecution, DurableFinalizationFuture, DurablePageActivities,
    DurablePageExecution, DurablePageExecutionFuture, DurableWorkflowResultMetadata,
    DurableWorkflowRunInput, GatewayOutcome, OcrDocumentWorkflow, OfficialTemporalGateway,
    QualificationPageActivities, TemporalCommand, TemporalGateway, WorkflowInput,
    WorkflowResultMetadata, WorkflowRunInput,
};
use temporalio_client::{
    errors::WorkflowGetResultError, WorkflowCancelOptions, WorkflowFetchHistoryOptions,
    WorkflowGetResultOptions, WorkflowStartOptions,
};
use temporalio_common::protos::temporal::api::enums::v1::EventType;
use temporalio_common::protos::temporal::api::history::v1::history_event::Attributes;
use temporalio_sdk::{
    testing::{
        DevServerLogLevel, EphemeralExe, LocalWorkflowEnvironmentOptions, WorkflowEnvironment,
    },
    workflow_replayer::{WorkflowReplayer, WorkflowReplayerOptions},
    Runtime, Worker, WorkerOptions,
};

fn workflow_input(page_count: u32) -> WorkflowRunInput {
    let product_id = ProductId::new("qualification").unwrap();
    let tenant_id = TenantId::new("ten_TEMPORAL").unwrap();
    let job_id = JobId::new("job_TEMPORAL").unwrap();
    let dispatch = WorkflowDispatch {
        event_id: 1,
        workflow_id: ocr_service::scoped_workflow_id(&product_id, &tenant_id, &job_id),
        product_id,
        tenant_id,
        job_id,
        page_count,
        action: WorkflowAction::Start,
    };
    WorkflowRunInput::first(WorkflowInput::try_from(&dispatch).unwrap())
}

fn durable_workflow_input() -> DurableWorkflowRunInput {
    DurableWorkflowRunInput::first(DurableActivityInput::new(
        ProductId::new("qualification").unwrap(),
        TenantId::new("ten_TEMPORAL").unwrap(),
        JobId::new("job_TEMPORAL_DURABLE").unwrap(),
    ))
}

#[derive(Default)]
struct PartialAfterFiftyIterations {
    iterations: AtomicU32,
}

impl DurablePageExecution for PartialAfterFiftyIterations {
    fn execute<'a>(&'a self, _input: DurableActivityInput) -> DurablePageExecutionFuture<'a> {
        let iteration = self.iterations.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            let status = if iteration == 51 {
                DurableActivityStatus::Partial
            } else {
                DurableActivityStatus::Running
            };
            Ok(DurableActivityOutput::new(status))
        })
    }
}

struct SuccessfulFinalization;

impl DurableFinalizationExecution for SuccessfulFinalization {
    fn finalize<'a>(&'a self, _input: DurableActivityInput) -> DurableFinalizationFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

struct ActivityWorkerProcess(Child);

impl ActivityWorkerProcess {
    fn start(target: &str, task_queue: &str, started_endpoint: Option<SocketAddr>) -> Self {
        let mode = if started_endpoint.is_some() {
            "hold"
        } else {
            "normal"
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_temporal-qualification-worker"));
        command.arg(target).arg(task_queue).arg(mode);
        if let Some(endpoint) = started_endpoint {
            command.arg(endpoint.to_string());
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Self(child)
    }

    fn terminate(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for ActivityWorkerProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn unused_loopback_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires scripts/test-temporal-qualification.sh"]
async fn fifty_one_pages_continue_as_new_and_replay() {
    let Ok(cli) = std::env::var("TEMPORAL_CLI_PATH") else {
        return;
    };
    let env = WorkflowEnvironment::start_local(
        LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(cli))
            .log_level(DevServerLogLevel::Never)
            .build(),
    )
    .await
    .unwrap();
    let runtime = Runtime::new_assume_tokio(Default::default()).unwrap();
    let worker_options = WorkerOptions::new("ocr-temporal-qualification")
        .register_workflow::<OcrDocumentWorkflow>()
        .unwrap()
        .register_activities(QualificationPageActivities::default())
        .build();
    let mut worker = Worker::new(&runtime, env.client().clone(), worker_options).unwrap();
    let shutdown = worker.shutdown_handle();
    tokio::task::LocalSet::new()
        .run_until(async move {
            let worker_task = tokio::task::spawn_local(async move { worker.run().await });

            let handle = env
                .client()
                .start_workflow(
                    OcrDocumentWorkflow::run,
                    workflow_input(51),
                    WorkflowStartOptions::new(
                        "ocr-temporal-qualification",
                        "ocr-temporal-qualification-51-pages",
                    )
                    .build(),
                )
                .await
                .unwrap();
            let result: WorkflowResultMetadata = handle
                .get_result(WorkflowGetResultOptions::default())
                .await
                .unwrap();
            assert_eq!(result.pages_processed, 51);
            assert_eq!(result.runs, 2);

            let history = handle.fetch_history(WorkflowFetchHistoryOptions::default());
            WorkflowReplayer::new(
                WorkflowReplayerOptions::new()
                    .register_workflow::<OcrDocumentWorkflow>()
                    .unwrap()
                    .build(),
            )
            .unwrap()
            .replay_workflow(history)
            .await
            .unwrap();

            shutdown();
            worker_task.await.unwrap().unwrap();
            env.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires scripts/test-temporal-qualification.sh"]
async fn durable_runner_activity_continues_as_new_and_replays_without_document_content() {
    let Ok(cli) = std::env::var("TEMPORAL_CLI_PATH") else {
        return;
    };
    let env = WorkflowEnvironment::start_local(
        LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(cli))
            .log_level(DevServerLogLevel::Never)
            .build(),
    )
    .await
    .unwrap();
    let runtime = Runtime::new_assume_tokio(Default::default()).unwrap();
    let worker_options = WorkerOptions::new("ocr-temporal-durable")
        .register_workflow::<DurableDocumentWorkflow>()
        .unwrap()
        .register_activities(DurablePageActivities::new(Arc::new(
            PartialAfterFiftyIterations::default(),
        )))
        .register_activities(DurableFinalizationActivities::new(Arc::new(
            SuccessfulFinalization,
        )))
        .build();
    let mut worker = Worker::new(&runtime, env.client().clone(), worker_options).unwrap();
    let shutdown = worker.shutdown_handle();

    tokio::task::LocalSet::new()
        .run_until(async move {
            let worker_task = tokio::task::spawn_local(async move { worker.run().await });
            let handle = env
                .client()
                .start_workflow(
                    DurableDocumentWorkflow::run,
                    durable_workflow_input(),
                    WorkflowStartOptions::new(
                        "ocr-temporal-durable",
                        "ocr-temporal-durable-fifty-one-iterations",
                    )
                    .build(),
                )
                .await
                .unwrap();
            let result: DurableWorkflowResultMetadata = handle
                .get_result(WorkflowGetResultOptions::default())
                .await
                .unwrap();
            assert_eq!(result.status, DurableActivityStatus::Partial);
            assert_eq!(result.runner_iterations, 51);
            assert_eq!(result.runs, 2);

            let history = handle.fetch_history(WorkflowFetchHistoryOptions::default());
            WorkflowReplayer::new(
                WorkflowReplayerOptions::new()
                    .register_workflow::<DurableDocumentWorkflow>()
                    .unwrap()
                    .build(),
            )
            .unwrap()
            .replay_workflow(history)
            .await
            .unwrap();

            shutdown();
            worker_task.await.unwrap().unwrap();
            env.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires scripts/test-temporal-qualification.sh"]
async fn cancellation_during_page_activity_stops_following_pages() {
    let Ok(cli) = std::env::var("TEMPORAL_CLI_PATH") else {
        return;
    };
    let env = WorkflowEnvironment::start_local(
        LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(cli))
            .log_level(DevServerLogLevel::Never)
            .build(),
    )
    .await
    .unwrap();
    let runtime = Runtime::new_assume_tokio(Default::default()).unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let worker_options = WorkerOptions::new("ocr-temporal-cancellation")
        .register_workflow::<OcrDocumentWorkflow>()
        .unwrap()
        .register_activities(QualificationPageActivities::with_started_notifier(
            Arc::clone(&started),
        ))
        .build();
    let mut worker = Worker::new(&runtime, env.client().clone(), worker_options).unwrap();
    let shutdown = worker.shutdown_handle();

    tokio::task::LocalSet::new()
        .run_until(async move {
            let worker_task = tokio::task::spawn_local(async move { worker.run().await });
            let handle = env
                .client()
                .start_workflow(
                    OcrDocumentWorkflow::run,
                    workflow_input(300),
                    WorkflowStartOptions::new(
                        "ocr-temporal-cancellation",
                        "ocr-temporal-qualification-cancellation",
                    )
                    .build(),
                )
                .await
                .unwrap();

            started.notified().await;
            handle
                .cancel(
                    WorkflowCancelOptions::builder()
                        .request_id("ocr-outbox-cancellation-test")
                        .reason("qualification")
                        .build(),
                )
                .await
                .unwrap();
            assert!(matches!(
                handle.get_result(WorkflowGetResultOptions::default()).await,
                Err(WorkflowGetResultError::Cancelled { .. })
            ));
            let events = handle
                .fetch_history(WorkflowFetchHistoryOptions::default())
                .into_events()
                .await
                .unwrap();
            let event_types = events
                .iter()
                .map(|event| EventType::try_from(event.event_type).unwrap())
                .collect::<Vec<_>>();
            let cancellation_requested = event_types
                .iter()
                .position(|event| *event == EventType::WorkflowExecutionCancelRequested)
                .unwrap();
            let activity_cancel_requested = event_types
                .iter()
                .position(|event| *event == EventType::ActivityTaskCancelRequested)
                .unwrap();
            assert!(cancellation_requested < activity_cancel_requested);
            assert_eq!(
                event_types
                    .iter()
                    .filter(|event| **event == EventType::ActivityTaskScheduled)
                    .count(),
                1
            );
            assert_eq!(
                event_types.last(),
                Some(&EventType::WorkflowExecutionCanceled)
            );

            shutdown();
            worker_task.await.unwrap().unwrap();
            env.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires scripts/test-temporal-qualification.sh"]
async fn activity_worker_process_loss_retries_only_the_incomplete_page() {
    let Ok(cli) = std::env::var("TEMPORAL_CLI_PATH") else {
        return;
    };
    let port = unused_loopback_port();
    let target = format!("http://127.0.0.1:{port}");
    let env = WorkflowEnvironment::start_local(
        LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(cli))
            .port(port)
            .log_level(DevServerLogLevel::Never)
            .build(),
    )
    .await
    .unwrap();
    let runtime = Runtime::new_assume_tokio(Default::default()).unwrap();
    let task_queue = "ocr-temporal-process-loss";
    let worker_options = WorkerOptions::new(task_queue)
        .deployment_options(qualification_deployment_options())
        .register_workflow::<OcrDocumentWorkflow>()
        .unwrap()
        .build();
    let mut workflow_worker = Worker::new(&runtime, env.client().clone(), worker_options).unwrap();
    let shutdown = workflow_worker.shutdown_handle();

    tokio::task::LocalSet::new()
        .run_until(async move {
            let workflow_worker_task =
                tokio::task::spawn_local(async move { workflow_worker.run().await });
            let started_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let started_endpoint = started_listener.local_addr().unwrap();
            let mut first_activity_worker =
                ActivityWorkerProcess::start(&target, task_queue, Some(started_endpoint));
            let handle = env
                .client()
                .start_workflow(
                    OcrDocumentWorkflow::run,
                    workflow_input(300),
                    WorkflowStartOptions::new(
                        task_queue,
                        "ocr-temporal-qualification-process-loss",
                    )
                    .build(),
                )
                .await
                .unwrap();

            let (mut started_stream, _) =
                tokio::time::timeout(Duration::from_secs(15), started_listener.accept())
                    .await
                    .expect("activity worker did not start")
                    .unwrap();
            let mut signal = [0_u8; 1];
            tokio::io::AsyncReadExt::read_exact(&mut started_stream, &mut signal)
                .await
                .unwrap();
            assert_eq!(signal, [1]);
            first_activity_worker.terminate();

            let _replacement_activity_worker =
                ActivityWorkerProcess::start(&target, task_queue, None);
            let result: WorkflowResultMetadata = tokio::time::timeout(
                Duration::from_secs(90),
                handle.get_result(WorkflowGetResultOptions::default()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(result.pages_processed, 300);
            assert_eq!(result.runs, 6);

            let events = handle
                .fetch_history(WorkflowFetchHistoryOptions::default())
                .into_events()
                .await
                .unwrap();
            let final_attempt = events
                .iter()
                .find_map(|event| match event.attributes.as_ref() {
                    Some(Attributes::ActivityTaskStartedEventAttributes(attributes)) => {
                        Some(attributes)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(final_attempt.attempt, 2);
            assert!(final_attempt.last_failure.is_some());
            assert_eq!(
                events
                    .iter()
                    .filter(|event| {
                        EventType::try_from(event.event_type)
                            == Ok(EventType::ActivityTaskCompleted)
                    })
                    .count(),
                50
            );

            shutdown();
            workflow_worker_task.await.unwrap().unwrap();
            env.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires scripts/test-temporal-qualification.sh"]
async fn cancellation_before_start_and_after_completion_is_idempotent() {
    let Ok(cli) = std::env::var("TEMPORAL_CLI_PATH") else {
        return;
    };
    let env = WorkflowEnvironment::start_local(
        LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(cli))
            .log_level(DevServerLogLevel::Never)
            .build(),
    )
    .await
    .unwrap();
    let runtime = Runtime::new_assume_tokio(Default::default()).unwrap();
    let task_queue = "ocr-temporal-cancel-boundaries";
    let workflow_id = "ocr-temporal-qualification-cancel-boundaries";
    let worker_options = WorkerOptions::new(task_queue)
        .register_workflow::<OcrDocumentWorkflow>()
        .unwrap()
        .register_activities(QualificationPageActivities::default())
        .build();
    let mut worker = Worker::new(&runtime, env.client().clone(), worker_options).unwrap();
    let shutdown = worker.shutdown_handle();

    tokio::task::LocalSet::new()
        .run_until(async move {
            let worker_task = tokio::task::spawn_local(async move { worker.run().await });
            let gateway = OfficialTemporalGateway::new(env.client().clone());
            assert_eq!(
                gateway
                    .execute(TemporalCommand::Cancel {
                        workflow_id: workflow_id.to_owned(),
                        request_id: "ocr-outbox-before-start".to_owned(),
                    })
                    .await
                    .unwrap(),
                GatewayOutcome::AlreadyExists
            );

            let handle = env
                .client()
                .start_workflow(
                    OcrDocumentWorkflow::run,
                    workflow_input(1),
                    WorkflowStartOptions::new(task_queue, workflow_id).build(),
                )
                .await
                .unwrap();
            let result: WorkflowResultMetadata = handle
                .get_result(WorkflowGetResultOptions::default())
                .await
                .unwrap();
            assert_eq!(result.pages_processed, 1);

            for request_id in [
                "ocr-outbox-after-completion-1",
                "ocr-outbox-after-completion-2",
            ] {
                assert_eq!(
                    gateway
                        .execute(TemporalCommand::Cancel {
                            workflow_id: workflow_id.to_owned(),
                            request_id: request_id.to_owned(),
                        })
                        .await
                        .unwrap(),
                    GatewayOutcome::Accepted
                );
            }

            shutdown();
            worker_task.await.unwrap().unwrap();
            env.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires scripts/test-temporal-qualification.sh"]
async fn cancellation_after_continue_as_new_stops_the_second_run() {
    let Ok(cli) = std::env::var("TEMPORAL_CLI_PATH") else {
        return;
    };
    let env = WorkflowEnvironment::start_local(
        LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(cli))
            .log_level(DevServerLogLevel::Never)
            .build(),
    )
    .await
    .unwrap();
    let runtime = Runtime::new_assume_tokio(Default::default()).unwrap();
    let task_queue = "ocr-temporal-cancel-second-run";
    let workflow_id = "ocr-temporal-qualification-cancel-second-run";
    let page_51_started = Arc::new(tokio::sync::Notify::new());
    let activities =
        QualificationPageActivities::with_page_started_notifier(51, Arc::clone(&page_51_started))
            .unwrap();
    let worker_options = WorkerOptions::new(task_queue)
        .register_workflow::<OcrDocumentWorkflow>()
        .unwrap()
        .register_activities(activities)
        .build();
    let mut worker = Worker::new(&runtime, env.client().clone(), worker_options).unwrap();
    let shutdown = worker.shutdown_handle();

    tokio::task::LocalSet::new()
        .run_until(async move {
            let worker_task = tokio::task::spawn_local(async move { worker.run().await });
            let initial = env
                .client()
                .start_workflow(
                    OcrDocumentWorkflow::run,
                    workflow_input(300),
                    WorkflowStartOptions::new(task_queue, workflow_id).build(),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(30), page_51_started.notified())
                .await
                .expect("page 51 did not start");

            let current = env
                .client()
                .get_workflow_handle::<OcrDocumentWorkflow>(workflow_id);
            current
                .cancel(
                    WorkflowCancelOptions::builder()
                        .request_id("ocr-outbox-second-run-cancel")
                        .reason("qualification")
                        .build(),
                )
                .await
                .unwrap();
            assert!(matches!(
                initial
                    .get_result(WorkflowGetResultOptions::default())
                    .await,
                Err(WorkflowGetResultError::Cancelled { .. })
            ));

            let first_run_events = initial
                .fetch_history(WorkflowFetchHistoryOptions::default())
                .into_events()
                .await
                .unwrap();
            assert_eq!(
                first_run_events
                    .iter()
                    .filter(|event| {
                        EventType::try_from(event.event_type)
                            == Ok(EventType::ActivityTaskCompleted)
                    })
                    .count(),
                50
            );
            assert_eq!(
                EventType::try_from(first_run_events.last().unwrap().event_type).unwrap(),
                EventType::WorkflowExecutionContinuedAsNew
            );

            let second_run_events = current
                .fetch_history(WorkflowFetchHistoryOptions::default())
                .into_events()
                .await
                .unwrap();
            assert_eq!(
                second_run_events
                    .iter()
                    .filter(|event| {
                        EventType::try_from(event.event_type)
                            == Ok(EventType::ActivityTaskScheduled)
                    })
                    .count(),
                1
            );
            assert_eq!(
                EventType::try_from(second_run_events.last().unwrap().event_type).unwrap(),
                EventType::WorkflowExecutionCanceled
            );

            shutdown();
            worker_task.await.unwrap().unwrap();
            env.shutdown().await.unwrap();
        })
        .await;
}
