use ocr_domain::{JobId, PageWorkflow, PageWorkflowStatus};

#[test]
fn replay_retries_only_the_failed_page_with_a_stable_activity_key() {
    let mut workflow = PageWorkflow::new(JobId::new("job_PAGE_RECOVERY").unwrap(), 3, 3).unwrap();

    let first = workflow.claim_ready(3).unwrap();
    assert_eq!(
        first.iter().map(|task| task.page).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    workflow.record_success(&first[0]).unwrap();
    workflow.record_retryable_failure(&first[1]).unwrap();
    workflow.record_success(&first[2]).unwrap();

    let checkpoint = serde_json::to_vec(&workflow).unwrap();
    let mut resumed: PageWorkflow = serde_json::from_slice(&checkpoint).unwrap();
    let retry = resumed.claim_ready(3).unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].page, 2);
    assert_eq!(retry[0].attempt, 2);
    assert_eq!(
        retry[0].activity_key,
        "ocr-job-job_PAGE_RECOVERY-page-2-attempt-2"
    );

    let running_checkpoint = serde_json::to_vec(&resumed).unwrap();
    let mut crash_replay: PageWorkflow = serde_json::from_slice(&running_checkpoint).unwrap();
    assert_eq!(crash_replay.claim_ready(3).unwrap(), retry);
    crash_replay.record_success(&retry[0]).unwrap();

    assert_eq!(crash_replay.status(), PageWorkflowStatus::Completed);
    assert!(crash_replay.claim_ready(3).unwrap().is_empty());
}

#[test]
fn cancellation_is_idempotent_and_stops_new_or_stale_page_work() {
    let mut workflow = PageWorkflow::new(JobId::new("job_PAGE_CANCEL").unwrap(), 2, 3).unwrap();
    let active = workflow.claim_ready(1).unwrap().remove(0);

    workflow.request_cancellation();
    workflow.request_cancellation();

    assert_eq!(workflow.status(), PageWorkflowStatus::Cancelled);
    assert!(workflow.claim_ready(2).unwrap().is_empty());
    assert!(workflow.record_success(&active).is_err());

    let checkpoint = serde_json::to_vec(&workflow).unwrap();
    let resumed: PageWorkflow = serde_json::from_slice(&checkpoint).unwrap();
    assert_eq!(resumed.status(), PageWorkflowStatus::Cancelled);
}
