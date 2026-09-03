use ocr_service::{GcsPageArtifactReader, PageArtifactReadError, PageArtifactReader};
use ocr_store::StoredPageArtifact;

fn artifact(bucket: &str) -> StoredPageArtifact {
    StoredPageArtifact {
        page: 1,
        attempt: 1,
        activity_key: "ocr-job-job_PAGE_READ-page-1-attempt-1".to_owned(),
        object_bucket: bucket.to_owned(),
        object_name: "products/kora/tenants/ten_PAGE_READ/pages/job_PAGE_READ/1.json".to_owned(),
        object_generation: 7,
        object_digest: format!("sha256:{}", "a".repeat(64)),
        content_length: 128,
    }
}

#[test]
fn page_reader_requires_an_explicit_valid_bucket_allowlist() {
    assert!(GcsPageArtifactReader::new(&[]).is_err());
    assert!(GcsPageArtifactReader::new(&["invalid/bucket".to_owned()]).is_err());
    assert!(GcsPageArtifactReader::new(&["dev-kora-ocr-pages".to_owned()]).is_ok());
}

#[tokio::test]
async fn page_reader_rejects_a_locator_outside_its_allowlist_without_network_io() {
    let reader = GcsPageArtifactReader::new(&["dev-kora-ocr-pages".to_owned()]).unwrap();

    assert_eq!(
        reader.read(&artifact("other-product-pages"), 1024).await,
        Err(PageArtifactReadError::Invalid)
    );
}
