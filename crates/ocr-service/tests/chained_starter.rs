use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ocr_domain::{JobId, ProductId, TenantId};
use ocr_service::{
    ChainedWorkflowStarter, WorkflowAction, WorkflowDispatch, WorkflowDispatchError,
    WorkflowDispatchOutcome, WorkflowStarter,
};
use tokio::sync::Mutex;

struct ScriptedStarter {
    name: &'static str,
    result: Result<WorkflowDispatchOutcome, WorkflowDispatchError>,
    order: Arc<Mutex<Vec<&'static str>>>,
    sequence: Arc<AtomicUsize>,
}

impl WorkflowStarter for ScriptedStarter {
    async fn dispatch(
        &self,
        _dispatch: WorkflowDispatch,
    ) -> Result<WorkflowDispatchOutcome, WorkflowDispatchError> {
        self.sequence.fetch_add(1, Ordering::SeqCst);
        self.order.lock().await.push(self.name);
        self.result
    }
}

fn dispatch() -> WorkflowDispatch {
    WorkflowDispatch {
        event_id: 1,
        workflow_id: "ocr-v1-chained".to_owned(),
        product_id: ProductId::new("kora").unwrap(),
        tenant_id: TenantId::new("ten_chained").unwrap(),
        job_id: JobId::new("job_chained").unwrap(),
        page_count: 2,
        action: WorkflowAction::Start,
    }
}

fn chained(
    checkpoint: Result<WorkflowDispatchOutcome, WorkflowDispatchError>,
    workflow: Result<WorkflowDispatchOutcome, WorkflowDispatchError>,
) -> (
    ChainedWorkflowStarter<ScriptedStarter, ScriptedStarter>,
    Arc<Mutex<Vec<&'static str>>>,
) {
    let order = Arc::new(Mutex::new(Vec::new()));
    let sequence = Arc::new(AtomicUsize::new(0));
    let starter = ChainedWorkflowStarter::new(
        Arc::new(ScriptedStarter {
            name: "checkpoint",
            result: checkpoint,
            order: order.clone(),
            sequence: sequence.clone(),
        }),
        Arc::new(ScriptedStarter {
            name: "workflow",
            result: workflow,
            order: order.clone(),
            sequence,
        }),
    );
    (starter, order)
}

#[tokio::test]
async fn checkpoint_is_written_before_the_workflow_starts() {
    let (starter, order) = chained(
        Ok(WorkflowDispatchOutcome::Started),
        Ok(WorkflowDispatchOutcome::Started),
    );
    let outcome = starter.dispatch(dispatch()).await.unwrap();
    assert_eq!(outcome, WorkflowDispatchOutcome::Started);
    assert_eq!(*order.lock().await, vec!["checkpoint", "workflow"]);
}

#[tokio::test]
async fn workflow_is_not_started_when_the_checkpoint_fails() {
    let (starter, order) = chained(
        Err(WorkflowDispatchError::Unavailable),
        Ok(WorkflowDispatchOutcome::Started),
    );
    let error = starter.dispatch(dispatch()).await.unwrap_err();
    assert_eq!(error, WorkflowDispatchError::Unavailable);
    assert_eq!(*order.lock().await, vec!["checkpoint"]);
}

#[tokio::test]
async fn outcome_is_existing_only_when_both_sides_already_exist() {
    let cases = [
        (
            WorkflowDispatchOutcome::Existing,
            WorkflowDispatchOutcome::Existing,
            WorkflowDispatchOutcome::Existing,
        ),
        (
            WorkflowDispatchOutcome::Existing,
            WorkflowDispatchOutcome::Started,
            WorkflowDispatchOutcome::Started,
        ),
        (
            WorkflowDispatchOutcome::Started,
            WorkflowDispatchOutcome::Existing,
            WorkflowDispatchOutcome::Started,
        ),
    ];
    for (checkpoint, workflow, expected) in cases {
        let (starter, _) = chained(Ok(checkpoint), Ok(workflow));
        let outcome = starter.dispatch(dispatch()).await.unwrap();
        assert_eq!(
            outcome, expected,
            "checkpoint={checkpoint:?} workflow={workflow:?}"
        );
    }
}

#[tokio::test]
async fn workflow_start_failure_is_surfaced_after_the_checkpoint() {
    let (starter, order) = chained(
        Ok(WorkflowDispatchOutcome::Started),
        Err(WorkflowDispatchError::Unavailable),
    );
    let error = starter.dispatch(dispatch()).await.unwrap_err();
    assert_eq!(error, WorkflowDispatchError::Unavailable);
    assert_eq!(*order.lock().await, vec!["checkpoint", "workflow"]);
}
