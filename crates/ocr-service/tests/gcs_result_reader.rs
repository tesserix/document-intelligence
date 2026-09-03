use ocr_domain::{DocumentId, DocumentVersion};
use std::collections::HashMap;

use ocr_service::{GcsResultReader, GcsResultWriter, ResultArtifactReader};
use ocr_store::StoredResultLocator;

fn locator(bucket: &str) -> StoredResultLocator {
    StoredResultLocator {
        document_id: DocumentId::new("doc_GCSREADER").unwrap(),
        document_version: DocumentVersion::new(&format!("sha256:{}", "a".repeat(64))).unwrap(),
        object_bucket: bucket.to_owned(),
        object_name: "products/kora/tenants/ten_TEST/results/job_TEST/v1.json".to_owned(),
        object_generation: 7,
        object_digest: format!("sha256:{}", "b".repeat(64)),
        content_length: 128,
    }
}

#[test]
fn reader_requires_an_explicit_nonempty_bucket_allowlist() {
    assert!(GcsResultReader::new(&[]).is_err());
    assert!(GcsResultReader::new(&["invalid/bucket".to_owned()]).is_err());
    assert!(GcsResultReader::new(&["ocr-dev-results-au".to_owned()]).is_ok());
}

#[test]
fn writer_requires_one_isolated_bucket_per_valid_product() {
    assert!(GcsResultWriter::new(HashMap::new()).is_err());
    assert!(GcsResultWriter::new(HashMap::from([(
        "Kora".to_owned(),
        "dev-kora-ocr-results".to_owned()
    )]))
    .is_err());
    assert!(GcsResultWriter::new(HashMap::from([(
        "kora".to_owned(),
        "invalid/bucket".to_owned()
    )]))
    .is_err());
    assert!(GcsResultWriter::new(HashMap::from([(
        "kora".to_owned(),
        "dev-kora-ocr-results".to_owned()
    )]))
    .is_ok());
}

#[tokio::test]
async fn reader_rejects_a_locator_outside_its_bucket_allowlist() {
    let reader = GcsResultReader::new(&["ocr-dev-results-au".to_owned()]).unwrap();

    assert!(reader
        .read(&locator("another-products-results"), 1024)
        .await
        .is_err());
}
