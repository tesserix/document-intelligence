use ocr_domain::{JobId, ProductId, TenantId};
use ocr_service::{WorkflowAction, WorkflowDispatch};
use ocr_temporal::{
    OcrDocumentWorkflow, QualificationPageActivities, WorkflowInput, WorkflowResultMetadata,
    WorkflowRunInput,
};
use temporalio_client::{
    WorkflowFetchHistoryOptions, WorkflowGetResultOptions, WorkflowStartOptions,
};
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires scripts/test-temporal-qualification.sh"]
async fn fifty_one_pages_continue_as_new_and_replay() {
    let cli = std::env::var("TEMPORAL_CLI_PATH").expect("verified Temporal CLI path is required");
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
        .register_activities(QualificationPageActivities)
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
