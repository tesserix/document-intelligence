use ocr_domain::{JobId, ProductId, TenantId};
use ocr_service::{
    WorkflowAction, WorkflowDispatch, WorkflowDispatchError, WorkflowDispatchOutcome,
    WorkflowStarter,
};
use ocr_temporal::{
    GatewayError, GatewayOutcome, TemporalCommand, TemporalGateway, TemporalStarter, WorkflowInput,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingGateway {
    commands: Mutex<Vec<TemporalCommand>>,
    outcome: Mutex<Option<Result<GatewayOutcome, GatewayError>>>,
}

impl TemporalGateway for RecordingGateway {
    async fn execute(&self, command: TemporalCommand) -> Result<GatewayOutcome, GatewayError> {
        self.commands.lock().unwrap().push(command);
        self.outcome
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Ok(GatewayOutcome::Accepted))
    }
}

fn dispatch(action: WorkflowAction) -> WorkflowDispatch {
    WorkflowDispatch {
        event_id: 42,
        workflow_id: "ocr-v1-93c8e4e4759aa062d8f7e317c3278149".to_owned(),
        product_id: ProductId::new("kora").unwrap(),
        tenant_id: TenantId::new("ten_RELAY").unwrap(),
        job_id: JobId::new("job_RELAY").unwrap(),
        page_count: 300,
        action,
    }
}

#[test]
fn workflow_input_is_versioned_bounded_and_content_free() {
    let input = WorkflowInput::try_from(&dispatch(WorkflowAction::Start)).unwrap();
    let encoded = serde_json::to_vec(&input).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

    assert!(encoded.len() <= 512);
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [
            "job_id",
            "page_count",
            "product_id",
            "schema_version",
            "tenant_id"
        ]
    );
    assert!(serde_json::from_str::<WorkflowInput>(
        r#"{"schema_version":"1","product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page_count":301}"#,
    )
    .is_err());
    assert!(serde_json::from_str::<WorkflowInput>(
        r#"{"schema_version":"1","product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page_count":3,"document_text":"untrusted"}"#,
    )
    .is_err());
}

#[tokio::test]
async fn start_and_duplicate_delivery_map_to_one_deterministic_workflow() {
    for (gateway_outcome, expected) in [
        (GatewayOutcome::Accepted, WorkflowDispatchOutcome::Started),
        (
            GatewayOutcome::AlreadyExists,
            WorkflowDispatchOutcome::Existing,
        ),
    ] {
        let gateway = Arc::new(RecordingGateway {
            commands: Mutex::new(Vec::new()),
            outcome: Mutex::new(Some(Ok(gateway_outcome))),
        });
        let starter = TemporalStarter::new(Arc::clone(&gateway), "ocr-interactive").unwrap();

        assert_eq!(
            starter
                .dispatch(dispatch(WorkflowAction::Start))
                .await
                .unwrap(),
            expected
        );
        let commands = gateway.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        let TemporalCommand::Start {
            workflow_id,
            request_id,
            task_queue,
            input,
        } = &commands[0]
        else {
            panic!("expected start command");
        };
        assert_eq!(workflow_id, "ocr-v1-93c8e4e4759aa062d8f7e317c3278149");
        assert_eq!(request_id, "ocr-outbox-42");
        assert_eq!(task_queue, "ocr-interactive");
        assert_eq!(input.page_count(), 300);
    }
}

#[tokio::test]
async fn cancellation_is_idempotent_and_gateway_failures_remain_retryable() {
    let gateway = Arc::new(RecordingGateway::default());
    let starter = TemporalStarter::new(Arc::clone(&gateway), "ocr-interactive").unwrap();
    assert_eq!(
        starter
            .dispatch(dispatch(WorkflowAction::Cancel))
            .await
            .unwrap(),
        WorkflowDispatchOutcome::Started
    );
    assert!(matches!(
        &gateway.commands.lock().unwrap()[0],
        TemporalCommand::Cancel { workflow_id, request_id }
            if workflow_id == "ocr-v1-93c8e4e4759aa062d8f7e317c3278149"
                && request_id == "ocr-outbox-42"
    ));

    let unavailable = Arc::new(RecordingGateway {
        commands: Mutex::new(Vec::new()),
        outcome: Mutex::new(Some(Err(GatewayError::Unavailable))),
    });
    let starter = TemporalStarter::new(unavailable, "ocr-interactive").unwrap();
    assert_eq!(
        starter.dispatch(dispatch(WorkflowAction::Start)).await,
        Err(WorkflowDispatchError::Unavailable)
    );
}
