use ocr_domain::UploadId;
use ocr_service::{GcsUploadIssuer, StoredUpload, UploadIntentIssuer};
use time::OffsetDateTime;

#[test]
fn issuer_requires_an_explicit_nonempty_bucket_allowlist() {
    assert!(GcsUploadIssuer::new(&[]).is_err());
    assert!(GcsUploadIssuer::new(&["INVALID_BUCKET".to_owned()]).is_err());
    assert!(GcsUploadIssuer::new(&["dev-kora-ocr-quarantine".to_owned()]).is_ok());
}

#[tokio::test]
async fn issuer_rejects_an_upload_outside_its_bucket_allowlist() {
    let issuer = GcsUploadIssuer::new(&["dev-kora-ocr-quarantine".to_owned()]).unwrap();
    let upload = StoredUpload {
        upload_id: UploadId::new("upl_GCSISSUER").unwrap(),
        object_bucket: "dev-other-ocr-quarantine".to_owned(),
        object_name: "products/other/tenants/ten_OTHER/quarantine/upl_GCSISSUER".to_owned(),
        expected_content_type: "application/pdf".to_owned(),
        expected_content_length: 1024,
        expected_digest: format!("sha256:{}", "a".repeat(64)),
        expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(10),
        created_at: OffsetDateTime::now_utc(),
    };

    assert!(issuer.issue(&upload).await.is_err());
}
