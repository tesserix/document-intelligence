use std::{sync::Arc, time::Duration};

use ocr_domain::UploadId;
use ocr_service::{ClamdScanner, GcsUploadMalwareInspector, UploadInspectionError};
use ocr_store::{StoredUpload, UploadState};
use time::OffsetDateTime;

fn scanner() -> Arc<ClamdScanner> {
    Arc::new(
        ClamdScanner::new(
            "127.0.0.1:3310".parse().unwrap(),
            Duration::from_secs(30),
            1024,
        )
        .unwrap(),
    )
}

#[test]
fn inspector_requires_an_explicit_valid_bucket_allowlist() {
    assert!(GcsUploadMalwareInspector::new(&[], scanner()).is_err());
    assert!(GcsUploadMalwareInspector::new(&["INVALID".to_owned()], scanner()).is_err());
    assert!(
        GcsUploadMalwareInspector::new(&["dev-kora-ocr-quarantine".to_owned()], scanner(),).is_ok()
    );
}

#[tokio::test]
async fn inspector_rejects_an_upload_outside_its_bucket_allowlist_without_network_io() {
    let inspector =
        GcsUploadMalwareInspector::new(&["dev-kora-ocr-quarantine".to_owned()], scanner()).unwrap();
    let upload = StoredUpload {
        upload_id: UploadId::new("upl_INSPECT").unwrap(),
        state: UploadState::Inspecting,
        object_bucket: "dev-other-ocr-quarantine".to_owned(),
        object_name: "products/other/tenants/ten_TEST/quarantine/upl_INSPECT".to_owned(),
        expected_content_type: "application/pdf".to_owned(),
        expected_content_length: 8,
        expected_digest: format!("sha256:{}", "a".repeat(64)),
        expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(10),
        created_at: OffsetDateTime::now_utc(),
        object_generation: Some(42),
        uploaded_at: Some(OffsetDateTime::now_utc()),
    };

    assert_eq!(
        inspector.inspect(&upload).await.unwrap_err(),
        UploadInspectionError::Invalid
    );
}
