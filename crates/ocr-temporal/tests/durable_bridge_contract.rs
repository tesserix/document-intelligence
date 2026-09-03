use std::time::Duration;

use ocr_domain::PageWorkflowStatus;
use ocr_service::{PageRunnerError, PageRunnerOutcome};
use ocr_temporal::{
    durable_activity_options, DurableActivityInput, DurableActivityOutput, DurableActivityStatus,
    DurableExecutionError, DurableExecutionErrorKind,
};
use temporalio_sdk::{activities::ActivityError, ActivityCancellationType, ActivityCloseTimeouts};

#[test]
fn durable_activity_payloads_are_identifiers_and_status_only() {
    let input = serde_json::from_str::<DurableActivityInput>(
        r#"{"schema_version":"1","product_id":"kora","tenant_id":"ten_BRIDGE","job_id":"job_BRIDGE"}"#,
    )
    .unwrap();
    let input_json = serde_json::to_value(input).unwrap();
    assert_eq!(
        input_json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["job_id", "product_id", "schema_version", "tenant_id"]
    );

    let output = DurableActivityOutput::new(DurableActivityStatus::Partial);
    let output_json = serde_json::to_value(output).unwrap();
    assert_eq!(output_json, serde_json::json!({"status": "partial"}));
}

#[test]
fn durable_activity_payload_rejects_content_and_unknown_versions() {
    for json in [
        r#"{"schema_version":"2","product_id":"kora","tenant_id":"ten_BRIDGE","job_id":"job_BRIDGE"}"#,
        r#"{"schema_version":"1","product_id":"kora","tenant_id":"ten_BRIDGE","job_id":"job_BRIDGE","document_text":"untrusted"}"#,
    ] {
        assert!(
            serde_json::from_str::<DurableActivityInput>(json).is_err(),
            "{json}"
        );
    }
}

#[test]
fn durable_execution_errors_separate_transport_retry_from_business_state() {
    for kind in [
        DurableExecutionErrorKind::DependencyUnavailable,
        DurableExecutionErrorKind::RevisionConflict,
    ] {
        assert!(DurableExecutionError::new(kind).is_retryable(), "{kind:?}");
    }

    for kind in [
        DurableExecutionErrorKind::InvalidInput,
        DurableExecutionErrorKind::ScopeNotFound,
    ] {
        assert!(!DurableExecutionError::new(kind).is_retryable(), "{kind:?}");
    }
}

#[test]
fn page_runner_state_maps_to_metadata_without_turning_page_failure_into_activity_failure() {
    for (runner, expected) in [
        (
            PageRunnerOutcome::Progressed(PageWorkflowStatus::Running),
            DurableActivityStatus::Running,
        ),
        (
            PageRunnerOutcome::Progressed(PageWorkflowStatus::Completed),
            DurableActivityStatus::Completed,
        ),
        (
            PageRunnerOutcome::Progressed(PageWorkflowStatus::Partial),
            DurableActivityStatus::Partial,
        ),
        (
            PageRunnerOutcome::Idle(PageWorkflowStatus::Cancelled),
            DurableActivityStatus::Cancelled,
        ),
    ] {
        assert_eq!(DurableActivityOutput::from(runner).status(), expected);
    }
}

#[test]
fn page_runner_control_errors_have_explicit_activity_retry_classification() {
    for (runner, expected) in [
        (
            PageRunnerError::RetryableConflict,
            DurableExecutionErrorKind::RevisionConflict,
        ),
        (
            PageRunnerError::NotFound,
            DurableExecutionErrorKind::ScopeNotFound,
        ),
        (
            PageRunnerError::InvalidConfiguration,
            DurableExecutionErrorKind::InvalidInput,
        ),
    ] {
        assert_eq!(DurableExecutionError::from(runner).kind(), expected);
    }
}

#[test]
fn temporal_activity_failure_preserves_retry_class_without_sensitive_details() {
    for (kind, retryable) in [
        (DurableExecutionErrorKind::DependencyUnavailable, true),
        (DurableExecutionErrorKind::ScopeNotFound, false),
    ] {
        let ActivityError::Application(error) =
            DurableExecutionError::new(kind).into_activity_error()
        else {
            panic!("execution error must map to an application failure");
        };
        assert_eq!(!error.is_non_retryable(), retryable);
        assert_eq!(error.to_string(), "durable activity execution failed");
    }
}

#[test]
fn durable_runner_activity_has_bounded_transport_recovery() {
    let options = durable_activity_options(7).unwrap();
    let retry = options.retry_policy.unwrap();

    assert_eq!(options.activity_id.as_deref(), Some("ocr-runner-0007"));
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
    assert!(durable_activity_options(0).is_none());
    assert!(durable_activity_options(901).is_none());
}
