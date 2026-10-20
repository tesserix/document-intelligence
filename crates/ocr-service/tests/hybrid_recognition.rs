use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ocr_domain::{
    Confidence, DocumentPage, JobId, NormalizedPoint, ObservationLevel, PageNumber, PageTask,
    Polygon, ProductId, TenantId, TextObservation,
};
use ocr_service::{
    HybridPageRecognizer, PageRecognitionError, PageRecognitionFuture, PageRecognizer,
};

struct Recognizer {
    result: Result<Vec<&'static str>, PageRecognitionError>,
    calls: AtomicUsize,
}

impl Recognizer {
    fn reading(lines: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            result: Ok(lines.to_vec()),
            calls: AtomicUsize::new(0),
        })
    }

    fn failing(error: PageRecognitionError) -> Arc<Self> {
        Arc::new(Self {
            result: Err(error),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PageRecognizer for Recognizer {
    fn recognize<'a>(
        &'a self,
        _product_id: &'a ProductId,
        _tenant_id: &'a TenantId,
        _job_id: &'a JobId,
        task: &'a PageTask,
    ) -> PageRecognitionFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let lines = self.result.clone()?;
            let page = PageNumber::new(task.page).unwrap();
            let observations = lines
                .iter()
                .enumerate()
                .map(|(order, text)| {
                    TextObservation::new(
                        format!("obs_test_p{}_line{order}", task.page)
                            .try_into()
                            .unwrap(),
                        ObservationLevel::Line,
                        *text,
                        Confidence::new(0.95).unwrap(),
                        Polygon::new(vec![
                            NormalizedPoint::new(0.1, 0.1).unwrap(),
                            NormalizedPoint::new(0.9, 0.1).unwrap(),
                            NormalizedPoint::new(0.9, 0.2).unwrap(),
                        ])
                        .unwrap(),
                        u32::try_from(order).unwrap(),
                        None,
                    )
                    .unwrap()
                })
                .collect();
            Ok(DocumentPage::new(page, 1920, 1440, observations).unwrap())
        })
    }
}

async fn read(
    native: &Arc<Recognizer>,
    managed: &Arc<Recognizer>,
) -> Result<Vec<String>, PageRecognitionError> {
    let page = HybridPageRecognizer::new(native.clone(), managed.clone())
        .recognize(
            &ProductId::new("roamie").unwrap(),
            &TenantId::new("ten_HYBRID").unwrap(),
            &JobId::new("job_HYBRID").unwrap(),
            &PageTask {
                page: 1,
                attempt: 1,
                activity_key: "ocr-job-job_HYBRID-page-1-attempt-1".to_owned(),
            },
        )
        .await?;
    assert_eq!((page.width, page.height), (1920, 1440));
    Ok(page
        .observations
        .into_iter()
        .map(|line| line.text)
        .collect())
}

#[tokio::test]
async fn text_layer_pages_never_reach_document_ai() {
    let native = Recognizer::reading(&["Invoice 1048"]);
    let managed = Recognizer::reading(&["not used"]);
    assert_eq!(
        read(&native, &managed).await,
        Ok(vec!["Invoice 1048".to_owned()])
    );
    assert_eq!(managed.calls(), 0);
}

#[tokio::test]
async fn photos_without_a_text_layer_are_read_by_document_ai() {
    let native = Recognizer::reading(&[]);
    let managed = Recognizer::reading(&["出口", "Exit"]);
    assert_eq!(
        read(&native, &managed).await,
        Ok(vec!["出口".to_owned(), "Exit".to_owned()])
    );
    assert_eq!((native.calls(), managed.calls()), (1, 1));
}

#[tokio::test]
async fn a_page_document_ai_rejects_stays_for_manual_review() {
    let native = Recognizer::reading(&[]);
    let managed = Recognizer::failing(PageRecognitionError::Permanent);
    assert_eq!(read(&native, &managed).await, Ok(Vec::new()));
}

#[tokio::test]
async fn document_ai_outages_and_quota_retry_the_page() {
    for error in [
        PageRecognitionError::Retryable,
        PageRecognitionError::RateLimited,
    ] {
        let native = Recognizer::reading(&[]);
        let managed = Recognizer::failing(error);
        assert_eq!(read(&native, &managed).await, Err(error), "{error:?}");
    }
}

#[tokio::test]
async fn native_failures_never_fall_through_to_document_ai() {
    for error in [
        PageRecognitionError::Retryable,
        PageRecognitionError::Permanent,
    ] {
        let native = Recognizer::failing(error);
        let managed = Recognizer::reading(&["not used"]);
        assert_eq!(read(&native, &managed).await, Err(error), "{error:?}");
        assert_eq!(managed.calls(), 0, "{error:?}");
    }
}
