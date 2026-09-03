use ocr_domain::UploadId;
use ocr_service::{
    DocumentReadError, GcsDocumentReaderConfigurationError, GcsUploadDocumentReader,
    UploadDocumentReader,
};
use ocr_store::{StoredUpload, UploadState};
use time::OffsetDateTime;

fn upload() -> StoredUpload {
    StoredUpload {
        upload_id: UploadId::new("upl_READER").unwrap(),
        state: UploadState::Accepted,
        object_bucket: "dev-kora-ocr-quarantine".to_owned(),
        object_name: "products/kora/tenants/ten_TEST/quarantine/upl_READER".to_owned(),
        expected_content_type: "application/pdf".to_owned(),
        expected_content_length: 8,
        expected_digest: format!("sha256:{}", "a".repeat(64)),
        expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(10),
        created_at: OffsetDateTime::now_utc(),
        object_generation: Some(42),
        uploaded_at: Some(OffsetDateTime::now_utc()),
    }
}

#[test]
fn document_reader_requires_an_explicit_valid_bucket_allowlist() {
    assert!(matches!(
        GcsUploadDocumentReader::new(&[]),
        Err(GcsDocumentReaderConfigurationError::MissingBuckets)
    ));
    assert!(matches!(
        GcsUploadDocumentReader::new(&["INVALID_BUCKET".to_owned()]),
        Err(GcsDocumentReaderConfigurationError::InvalidBucket)
    ));
}

#[tokio::test]
async fn document_reader_rejects_non_inspecting_uploads_without_network_io() {
    let reader = GcsUploadDocumentReader::new(&["dev-kora-ocr-quarantine".to_owned()]).unwrap();
    let result = UploadDocumentReader::read(&reader, &upload()).await;
    assert_eq!(result.unwrap_err(), DocumentReadError::Invalid);
}
