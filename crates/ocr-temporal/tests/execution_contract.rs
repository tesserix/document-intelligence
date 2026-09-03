use std::time::Duration;

use ocr_temporal::{ActivityPolicy, WorkflowPlan};

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
