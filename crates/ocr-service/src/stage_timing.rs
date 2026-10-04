use ocr_domain::JobId;
use std::future::Future;
use tokio::time::Instant;

struct StageTiming<'a> {
    stage: &'static str,
    job_id: &'a JobId,
    started: Instant,
    outcome: &'static str,
}

impl Drop for StageTiming<'_> {
    fn drop(&mut self) {
        tracing::info!(
            stage = self.stage,
            job_id = self.job_id.as_str(),
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            outcome = self.outcome,
            "ocr stage finished"
        );
    }
}

pub(crate) async fn measure_stage<T, E>(
    stage: &'static str,
    job_id: &JobId,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let mut timing = StageTiming {
        stage,
        job_id,
        started: Instant::now(),
        outcome: "cancelled",
    };
    let result = future.await;
    timing.outcome = if result.is_ok() { "ok" } else { "error" };
    result
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct Output(Arc<Mutex<Vec<u8>>>);
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test(start_paused = true)]
    async fn reports_stage_duration_and_outcome_without_error_payloads() {
        let output = Output(Arc::new(Mutex::new(Vec::new())));
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        async {
            let result = super::measure_stage(
                "provider",
                &ocr_domain::JobId::new("job_TIMING").unwrap(),
                async {
                    tokio::time::sleep(Duration::from_millis(123)).await;
                    Err::<(), _>("private document text")
                },
            )
            .await;
            assert!(result.is_err());
        }
        .with_subscriber(subscriber)
        .await;
        let bytes = output.0.lock().unwrap();
        let event: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(event["fields"]["stage"], "provider");
        assert_eq!(event["fields"]["elapsed_ms"], 123);
        assert_eq!(event["fields"]["outcome"], "error");
        assert!(!String::from_utf8_lossy(&bytes).contains("private document text"));
    }
    #[tokio::test(start_paused = true)]
    async fn cancelled_stage_records_its_elapsed_time() {
        let output = Output(Arc::new(Mutex::new(Vec::new())));
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        async {
            let job = ocr_domain::JobId::new("job_CANCELLED").unwrap();
            let result = tokio::time::timeout(
                Duration::from_millis(25),
                super::measure_stage("provider", &job, std::future::pending::<Result<(), ()>>()),
            )
            .await;
            assert!(result.is_err());
        }
        .with_subscriber(subscriber)
        .await;
        let event: serde_json::Value = serde_json::from_slice(&output.0.lock().unwrap()).unwrap();
        assert_eq!(event["fields"]["elapsed_ms"], 25);
        assert_eq!(event["fields"]["outcome"], "cancelled");
    }
}
