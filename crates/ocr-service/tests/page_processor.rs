use std::sync::Arc;

use ocr_domain::{DocumentPage, JobId, PageNumber, PageTask, ProductId, TenantId};
use ocr_service::{
    ArtifactPageProcessor, PageArtifactWriteError, PageArtifactWriteFuture, PageArtifactWriter,
    PageProcessError, PageProcessor, PageRecognitionError, PageRecognitionFuture, PageRecognizer,
};
use ocr_store::StoredPageArtifact;

struct StubRecognizer {
    error: Option<PageRecognitionError>,
}

struct WrongPageRecognizer;

impl PageRecognizer for WrongPageRecognizer {
    fn recognize<'a>(
        &'a self,
        _product_id: &'a ProductId,
        _tenant_id: &'a TenantId,
        _job_id: &'a JobId,
        task: &'a PageTask,
    ) -> PageRecognitionFuture<'a> {
        Box::pin(async move {
            DocumentPage::new(
                PageNumber::new(task.page + 1).unwrap(),
                1000,
                1400,
                Vec::new(),
            )
            .map_err(|_| PageRecognitionError::Permanent)
        })
    }
}

impl PageRecognizer for StubRecognizer {
    fn recognize<'a>(
        &'a self,
        _product_id: &'a ProductId,
        _tenant_id: &'a TenantId,
        _job_id: &'a JobId,
        task: &'a PageTask,
    ) -> PageRecognitionFuture<'a> {
        Box::pin(async move {
            if let Some(error) = self.error {
                return Err(error);
            }
            DocumentPage::new(PageNumber::new(task.page).unwrap(), 1000, 1400, Vec::new())
                .map_err(|_| PageRecognitionError::Permanent)
        })
    }
}

struct StubWriter {
    error: Option<PageArtifactWriteError>,
}

impl PageArtifactWriter for StubWriter {
    fn write<'a>(
        &'a self,
        _product_id: &'a ProductId,
        _tenant_id: &'a TenantId,
        _job_id: &'a JobId,
        task: &'a PageTask,
        _page: &'a DocumentPage,
    ) -> PageArtifactWriteFuture<'a> {
        Box::pin(async move {
            if let Some(error) = self.error {
                return Err(error);
            }
            Ok(StoredPageArtifact {
                page: task.page,
                attempt: task.attempt,
                activity_key: task.activity_key.clone(),
                object_bucket: "dev-kora-ocr-pages".to_owned(),
                object_name: format!("pages/{}.json", task.activity_key),
                object_generation: 7,
                object_digest: format!("sha256:{}", "a".repeat(64)),
                content_length: 128,
            })
        })
    }
}

fn task() -> PageTask {
    PageTask {
        page: 2,
        attempt: 1,
        activity_key: "ocr-job-job_PROCESSOR-page-2-attempt-1".to_owned(),
    }
}

fn processor(
    recognition_error: Option<PageRecognitionError>,
    write_error: Option<PageArtifactWriteError>,
) -> ArtifactPageProcessor<StubRecognizer, StubWriter> {
    ArtifactPageProcessor::new(
        Arc::new(StubRecognizer {
            error: recognition_error,
        }),
        Arc::new(StubWriter { error: write_error }),
    )
}

fn scope() -> (ProductId, TenantId, JobId) {
    (
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_PROCESSOR").unwrap(),
        JobId::new("job_PROCESSOR").unwrap(),
    )
}

#[tokio::test]
async fn recognized_page_is_written_as_the_claimed_activity_artifact() {
    let (product_id, tenant_id, job_id) = scope();
    let artifact = processor(None, None)
        .process(&product_id, &tenant_id, &job_id, task())
        .await
        .unwrap();

    assert_eq!(artifact.page, 2);
    assert_eq!(artifact.attempt, 1);
    assert_eq!(
        artifact.activity_key,
        "ocr-job-job_PROCESSOR-page-2-attempt-1"
    );
}

#[tokio::test]
async fn transient_dependencies_retry_but_invalid_output_is_permanent() {
    let (product_id, tenant_id, job_id) = scope();
    assert_eq!(
        processor(Some(PageRecognitionError::Retryable), None)
            .process(&product_id, &tenant_id, &job_id, task())
            .await,
        Err(PageProcessError::Retryable)
    );
    assert_eq!(
        processor(None, Some(PageArtifactWriteError::Unavailable))
            .process(&product_id, &tenant_id, &job_id, task())
            .await,
        Err(PageProcessError::Retryable)
    );
    assert_eq!(
        processor(None, Some(PageArtifactWriteError::Conflict))
            .process(&product_id, &tenant_id, &job_id, task())
            .await,
        Err(PageProcessError::Permanent)
    );
    let wrong_page = ArtifactPageProcessor::new(
        Arc::new(WrongPageRecognizer),
        Arc::new(StubWriter { error: None }),
    );
    assert_eq!(
        wrong_page
            .process(&product_id, &tenant_id, &job_id, task())
            .await,
        Err(PageProcessError::Permanent)
    );
}
