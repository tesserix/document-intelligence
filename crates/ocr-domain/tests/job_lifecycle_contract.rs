use ocr_domain::{JobLifecycle, JobState};

#[test]
fn job_follows_only_the_reviewed_lifecycle() {
    let mut job = JobLifecycle::new();

    for next in [
        JobState::Inspecting,
        JobState::Processing,
        JobState::Validating,
        JobState::Completed,
    ] {
        job.transition_to(next).unwrap();
    }

    assert_eq!(job.state(), JobState::Completed);
    assert!(job.transition_to(JobState::Processing).is_err());
}

#[test]
fn invalid_shortcuts_are_rejected() {
    let mut job = JobLifecycle::new();

    assert!(job.transition_to(JobState::Completed).is_err());
    assert_eq!(job.state(), JobState::Accepted);
}

#[test]
fn cancellation_is_durable_and_idempotent() {
    let mut job = JobLifecycle::new();
    job.transition_to(JobState::Inspecting).unwrap();
    job.transition_to(JobState::Processing).unwrap();

    assert_eq!(job.request_cancellation().unwrap(), JobState::Cancelling);
    assert_eq!(job.request_cancellation().unwrap(), JobState::Cancelling);
    job.transition_to(JobState::Cancelled).unwrap();
    assert_eq!(job.request_cancellation().unwrap(), JobState::Cancelled);
}

#[test]
fn completed_and_review_jobs_cannot_be_cancelled() {
    let mut completed = JobLifecycle::new();
    completed.transition_to(JobState::Inspecting).unwrap();
    completed.transition_to(JobState::Processing).unwrap();
    completed.transition_to(JobState::Validating).unwrap();
    completed.transition_to(JobState::Completed).unwrap();

    let mut review = JobLifecycle::new();
    review.transition_to(JobState::Inspecting).unwrap();
    review.transition_to(JobState::Processing).unwrap();
    review.transition_to(JobState::Partial).unwrap();
    review.transition_to(JobState::ReviewRequired).unwrap();

    assert!(completed.request_cancellation().is_err());
    assert!(review.request_cancellation().is_err());
}

#[test]
fn deserialization_cannot_construct_an_illegal_initial_state() {
    assert!(serde_json::from_str::<JobLifecycle>(r#"{"state":"processing"}"#).is_err());
}
