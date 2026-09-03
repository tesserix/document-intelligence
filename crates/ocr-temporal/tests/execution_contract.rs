use std::{sync::Arc, time::Duration};

use ocr_domain::{JobId, ProductId, TenantId};
use ocr_service::{WorkflowAction, WorkflowDispatch};
use ocr_temporal::{
    page_activity_options, ActivityPolicy, PageActivityInput, QualificationPageActivities,
    WorkflowInput, WorkflowPlan, WorkflowRunInput,
};
use temporalio_sdk::{ActivityCancellationType, ActivityCloseTimeouts};

#[test]
fn three_hundred_pages_roll_over_before_history_can_grow_unbounded() {
    let plan = WorkflowPlan::new(300).unwrap();

    assert_eq!(plan.runs().len(), 6);
    assert_eq!(plan.runs()[0].page_range(), 1..=50);
    assert!(plan.runs()[0].continues_as_new());
    assert_eq!(plan.runs()[5].page_range(), 251..=300);
    assert!(!plan.runs()[5].continues_as_new());
}

#[test]
fn retry_identity_targets_only_the_failed_page() {
    let plan = WorkflowPlan::new(300).unwrap();

    assert_eq!(plan.activity_id(184).unwrap(), "ocr-page-0184");
    assert!(plan.activity_id(0).is_err());
    assert!(plan.activity_id(301).is_err());
}

#[test]
fn page_activity_policy_has_bounded_retries_heartbeats_and_timeouts() {
    let policy = ActivityPolicy::page_ocr();

    assert_eq!(policy.start_to_close_timeout(), Duration::from_secs(120));
    assert_eq!(policy.heartbeat_timeout(), Duration::from_secs(10));
    assert_eq!(policy.maximum_attempts(), 3);
    assert_eq!(policy.initial_backoff(), Duration::from_secs(1));
    assert_eq!(policy.maximum_backoff(), Duration::from_secs(30));
    assert_eq!(
        policy.non_retryable_errors(),
        &["invalid_document", "scope_violation"]
    );
}

#[test]
fn workflow_run_state_advances_without_document_content() {
    let dispatch = WorkflowDispatch {
        event_id: 42,
        workflow_id: "ocr-v1-93c8e4e4759aa062d8f7e317c3278149".to_owned(),
        product_id: ProductId::new("kora").unwrap(),
        tenant_id: TenantId::new("ten_RELAY").unwrap(),
        job_id: JobId::new("job_RELAY").unwrap(),
        page_count: 51,
        action: WorkflowAction::Start,
    };
    let input = WorkflowInput::try_from(&dispatch).unwrap();
    let first = WorkflowRunInput::first(input);
    let second = first.next_run().unwrap();

    assert_eq!(first.run_number(), 1);
    assert_eq!(first.page_range(), 1..=50);
    assert_eq!(second.run_number(), 2);
    assert_eq!(second.page_range(), 51..=51);
    assert!(second.next_run().is_none());

    let encoded = serde_json::to_value(second).unwrap();
    assert_eq!(
        encoded
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [
            "job_id",
            "next_page",
            "page_count",
            "product_id",
            "run_number",
            "schema_version",
            "tenant_id"
        ]
    );
}

#[test]
fn workflow_run_state_rejects_impossible_or_injected_values() {
    for json in [
        r#"{"schema_version":"1","product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page_count":51,"next_page":0,"run_number":1}"#,
        r#"{"schema_version":"1","product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page_count":51,"next_page":51,"run_number":1}"#,
        r#"{"schema_version":"1","product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page_count":51,"next_page":51,"run_number":2,"document_text":"untrusted"}"#,
    ] {
        assert!(
            serde_json::from_str::<WorkflowRunInput>(json).is_err(),
            "{json}"
        );
    }
}

#[test]
fn sdk_page_activity_options_match_the_bounded_policy() {
    let options = page_activity_options(184);
    let retry = options.retry_policy.unwrap();

    assert_eq!(options.activity_id.as_deref(), Some("ocr-page-0184"));
    assert_eq!(
        options.close_timeouts,
        ActivityCloseTimeouts::StartToClose(Duration::from_secs(120))
    );
    assert_eq!(options.heartbeat_timeout, Some(Duration::from_secs(10)));
    assert_eq!(
        options.cancellation_type,
        ActivityCancellationType::WaitCancellationCompleted
    );
    assert_eq!(retry.maximum_attempts(), 3);
    assert_eq!(retry.initial_interval(), Duration::from_secs(1));
    assert_eq!(retry.maximum_interval(), Some(Duration::from_secs(30)));
    assert_eq!(
        retry.non_retryable_error_types(),
        &["invalid_document", "scope_violation"]
    );
}

#[test]
fn page_activity_input_rejects_out_of_range_or_injected_values() {
    for json in [
        r#"{"product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page":0}"#,
        r#"{"product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page":301}"#,
        r#"{"product_id":"kora","tenant_id":"ten_RELAY","job_id":"job_RELAY","page":1,"document_text":"untrusted"}"#,
    ] {
        assert!(
            serde_json::from_str::<PageActivityInput>(json).is_err(),
            "{json}"
        );
    }
}

#[test]
fn qualification_observer_accepts_only_a_real_page() {
    let started = Arc::new(tokio::sync::Notify::new());
    assert!(
        QualificationPageActivities::with_page_started_notifier(0, Arc::clone(&started)).is_none()
    );
    assert!(
        QualificationPageActivities::with_page_started_notifier(301, Arc::clone(&started))
            .is_none()
    );
    assert!(QualificationPageActivities::with_page_started_notifier(51, started).is_some());
}
