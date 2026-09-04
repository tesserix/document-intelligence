use std::sync::Mutex;

use ocr_domain::{JobId, PageGeometry, PageNumber, PageTask, ProductId, TenantId};
use ocr_service::{
    AcceptedPageSourceLoader, AcceptedSourceBytesReader, AcceptedSourceBytesReaderFuture,
    AcceptedSourceReadError, AcceptedSourceRepository, AcceptedSourceRepositoryFuture,
    PageSourceError, PageSourceResolver,
};
use ocr_store::StoredAcceptedSource;

struct RecordingRepository {
    source: Option<StoredAcceptedSource>,
    calls: Mutex<Vec<(String, String, String)>>,
}

impl AcceptedSourceRepository for RecordingRepository {
    fn load_accepted_source<'a>(
        &'a self,
        tenant_id: &'a TenantId,
        product_id: &'a ProductId,
        job_id: &'a JobId,
    ) -> AcceptedSourceRepositoryFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((
                tenant_id.as_str().to_owned(),
                product_id.as_str().to_owned(),
                job_id.as_str().to_owned(),
            ));
            Ok(self.source.clone())
        })
    }
}

struct StaticReader {
    result: Result<Vec<u8>, AcceptedSourceReadError>,
}

impl AcceptedSourceBytesReader for StaticReader {
    fn read<'a>(
        &'a self,
        _product_id: &'a ProductId,
        _tenant_id: &'a TenantId,
        _source: &'a StoredAcceptedSource,
    ) -> AcceptedSourceBytesReaderFuture<'a> {
        Box::pin(async move { self.result.clone() })
    }
}

fn source() -> StoredAcceptedSource {
    StoredAcceptedSource {
        bucket: "dev-kora-ocr-source".to_owned(),
        object_name: format!(
            "products/kora/tenants/ten_SOURCE/documents/sha256/{}",
            "a".repeat(64)
        ),
        generation: 7,
        digest: format!("sha256:{}", "a".repeat(64)),
        content_length: 16,
        content_type: "application/pdf".to_owned(),
        page_count: 2,
        maximum_page_pixels: 1_000_000,
        total_page_pixels: 2_000_000,
        page_geometries: vec![
            PageGeometry::new(PageNumber::new(1).unwrap(), 1_000, 1_000).unwrap(),
            PageGeometry::new(PageNumber::new(2).unwrap(), 1_000, 1_000).unwrap(),
        ],
        parser_profile: "strict-v1".to_owned(),
        parser_version: "1.0.0".to_owned(),
    }
}

fn task(page: u32) -> PageTask {
    PageTask {
        page,
        attempt: 1,
        activity_key: format!("ocr-job-job_SOURCE-page-{page}-attempt-1"),
    }
}

fn loader(
    source: Option<StoredAcceptedSource>,
    result: Result<Vec<u8>, AcceptedSourceReadError>,
) -> (
    AcceptedPageSourceLoader<RecordingRepository, StaticReader>,
    std::sync::Arc<RecordingRepository>,
) {
    let repository = std::sync::Arc::new(RecordingRepository {
        source,
        calls: Mutex::new(Vec::new()),
    });
    let loader = AcceptedPageSourceLoader::new(
        repository.clone(),
        std::sync::Arc::new(StaticReader { result }),
    );
    (loader, repository)
}

fn scope() -> (ProductId, TenantId, JobId) {
    (
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_SOURCE").unwrap(),
        JobId::new("job_SOURCE").unwrap(),
    )
}

#[tokio::test]
async fn loads_only_the_scoped_accepted_source_and_selected_page_geometry() {
    let (loader, repository) = loader(Some(source()), Ok(vec![7; 16]));

    let (product_id, tenant_id, job_id) = scope();
    let page = loader
        .load(&product_id, &tenant_id, &job_id, &task(2))
        .await
        .unwrap();

    assert_eq!(page.content_type(), "application/pdf");
    assert_eq!(page.bytes(), &[7; 16]);
    assert_eq!(u32::from(page.geometry().page), 2);
    assert_eq!(
        repository.calls.lock().unwrap().as_slice(),
        [(
            "ten_SOURCE".to_owned(),
            "kora".to_owned(),
            "job_SOURCE".to_owned(),
        )]
    );
}

#[tokio::test]
async fn rejects_missing_or_nonexistent_page_before_recognition() {
    let (missing, _) = loader(None, Ok(vec![7; 16]));
    let (product_id, tenant_id, job_id) = scope();
    assert!(matches!(
        missing
            .load(&product_id, &tenant_id, &job_id, &task(1))
            .await,
        Err(PageSourceError::NotFound)
    ));

    let (out_of_range, _) = loader(Some(source()), Ok(vec![7; 16]));
    assert!(matches!(
        out_of_range
            .load(&product_id, &tenant_id, &job_id, &task(3))
            .await,
        Err(PageSourceError::Invalid)
    ));
}

#[tokio::test]
async fn treats_storage_unavailability_as_retryable_but_invalid_sources_as_permanent() {
    let (product_id, tenant_id, job_id) = scope();
    let (unavailable, _) = loader(Some(source()), Err(AcceptedSourceReadError::Unavailable));
    assert!(matches!(
        unavailable
            .load(&product_id, &tenant_id, &job_id, &task(1))
            .await,
        Err(PageSourceError::Unavailable)
    ));

    let (invalid, _) = loader(Some(source()), Err(AcceptedSourceReadError::Invalid));
    assert!(matches!(
        invalid
            .load(&product_id, &tenant_id, &job_id, &task(1))
            .await,
        Err(PageSourceError::Invalid)
    ));
}
