use std::sync::Arc;

use ocr_domain::{ProductId, TenantId, UploadId};
use ocr_service::{
    DocumentParseError, DocumentParser, DocumentReadError, ImportOutcome, Importer,
    MalwareInspector, ParserInspectionReport, PromotedSource, SourcePromoter, SourcePromotionError,
    UploadDocumentReader, UploadInspectionError,
};
use ocr_store::{PgJobStore, StoredUpload};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

struct CleanScanner;

impl MalwareInspector for CleanScanner {
    async fn inspect<'a>(&'a self, _upload: &'a StoredUpload) -> Result<(), UploadInspectionError> {
        Ok(())
    }
}

struct FixtureReader(Vec<u8>);

impl UploadDocumentReader for FixtureReader {
    async fn read<'a>(&'a self, _upload: &'a StoredUpload) -> Result<Vec<u8>, DocumentReadError> {
        Ok(self.0.clone())
    }
}

struct FixtureParser;

impl DocumentParser for FixtureParser {
    async fn inspect<'a>(
        &'a self,
        _encoded: &'a [u8],
        _content_type: &'a str,
    ) -> Result<ParserInspectionReport, DocumentParseError> {
        Ok(ParserInspectionReport {
            page_count: 2,
            maximum_page_pixels: 8_500_000,
            total_page_pixels: 16_000_000,
        })
    }
}

struct PasswordParser;

impl DocumentParser for PasswordParser {
    async fn inspect<'a>(
        &'a self,
        _encoded: &'a [u8],
        _content_type: &'a str,
    ) -> Result<ParserInspectionReport, DocumentParseError> {
        Err(DocumentParseError::PasswordRequired)
    }
}

struct FixturePromoter;

impl SourcePromoter for FixturePromoter {
    async fn promote<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        upload: &'a StoredUpload,
    ) -> Result<PromotedSource, SourcePromotionError> {
        Ok(PromotedSource {
            bucket: format!("dev-{}-ocr-source", product_id.as_str()),
            object_name: format!(
                "products/{}/tenants/{}/documents/sha256/source",
                product_id.as_str(),
                tenant_id.as_str()
            ),
            generation: 73,
            digest: upload.expected_digest.clone(),
            content_length: upload.expected_content_length,
        })
    }
}

async fn pools() -> (PgPool, PgPool) {
    let application = PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let admin = PgPool::connect(&std::env::var("TEST_DATABASE_ADMIN_URL").unwrap())
        .await
        .unwrap();
    (application, admin)
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn importer_accepts_once_and_denies_foreign_scope() {
    let (application, admin) = pools().await;
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_IMPORT'")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_IMPORT'")
        .execute(&admin)
        .await
        .unwrap();
    let bytes = b"%PDF-1.7 fixture".to_vec();
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at) values \
         ('upl_IMPORT', 'ten_IMPORT', 'kora', 'import-1', $1, 'dev-kora-ocr-quarantine', \
          'products/kora/tenants/ten_IMPORT/quarantine/upl_IMPORT', 'application/pdf', $2, $3, \
          'uploaded', now() + interval '10 minutes', 42, 'application/pdf', $2, $3, now())",
    )
    .bind(format!("sha256:{}", "a".repeat(64)))
    .bind(i64::try_from(bytes.len()).unwrap())
    .bind(&digest)
    .execute(&admin)
    .await
    .unwrap();
    let importer = Importer::new(
        PgJobStore::new(application),
        Arc::new(CleanScanner),
        Arc::new(FixtureReader(bytes)),
        Arc::new(FixtureParser),
        Arc::new(FixturePromoter),
    );
    let tenant = TenantId::new("ten_IMPORT").unwrap();
    let product = ProductId::new("kora").unwrap();
    let upload = UploadId::new("upl_IMPORT").unwrap();

    assert_eq!(
        importer
            .process(&tenant, &product, &upload, "importer-01")
            .await
            .unwrap(),
        ImportOutcome::Accepted
    );
    assert_eq!(
        importer
            .process(&tenant, &product, &upload, "importer-01")
            .await
            .unwrap(),
        ImportOutcome::Existing
    );
    assert_eq!(
        importer
            .process(
                &TenantId::new("ten_OTHER").unwrap(),
                &product,
                &upload,
                "importer-01",
            )
            .await
            .unwrap(),
        ImportOutcome::NotFound
    );

    let row: (String, i32, i64, i64, String, String) = sqlx::query_as(
        "select status::text, parser_page_count, parser_maximum_page_pixels, \
         parser_total_page_pixels, parser_profile, parser_version from ocr_uploads \
         where upload_id = 'upl_IMPORT'",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(row.0, "accepted");
    assert_eq!((row.1, row.2, row.3), (2, 8_500_000, 16_000_000));
    assert_eq!(row.4, "intake-v1");
    assert_eq!(row.5, env!("CARGO_PKG_VERSION"));
    let events: i64 = sqlx::query_scalar(
        "select count(*) from ocr_upload_outbox where upload_id = 'upl_IMPORT' \
         and event_type = 'ocr.upload.accepted.v1'",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn importer_rejects_password_required_without_retrying() {
    let (application, admin) = pools().await;
    sqlx::query("delete from ocr_upload_outbox where upload_id = 'upl_PASSWORD'")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id = 'upl_PASSWORD'")
        .execute(&admin)
        .await
        .unwrap();
    let bytes = b"%PDF-1.7 encrypted fixture".to_vec();
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    sqlx::query(
        "insert into ocr_uploads \
         (upload_id, tenant_id, product_id, idempotency_key, request_digest, object_bucket, \
          object_name, expected_content_type, expected_content_length, expected_digest, status, \
          expires_at, object_generation, verified_content_type, verified_content_length, \
          verified_digest, uploaded_at) values \
         ('upl_PASSWORD', 'ten_PASSWORD', 'kora', 'password-1', $1, \
          'dev-kora-ocr-quarantine', \
          'products/kora/tenants/ten_PASSWORD/quarantine/upl_PASSWORD', 'application/pdf', $2, \
          $3, 'uploaded', now() + interval '10 minutes', 44, 'application/pdf', $2, $3, now())",
    )
    .bind(format!("sha256:{}", "b".repeat(64)))
    .bind(i64::try_from(bytes.len()).unwrap())
    .bind(&digest)
    .execute(&admin)
    .await
    .unwrap();
    let importer = Importer::new(
        PgJobStore::new(application),
        Arc::new(CleanScanner),
        Arc::new(FixtureReader(bytes)),
        Arc::new(PasswordParser),
        Arc::new(FixturePromoter),
    );

    assert_eq!(
        importer
            .process(
                &TenantId::new("ten_PASSWORD").unwrap(),
                &ProductId::new("kora").unwrap(),
                &UploadId::new("upl_PASSWORD").unwrap(),
                "importer-password",
            )
            .await
            .unwrap(),
        ImportOutcome::Rejected
    );
    let reason: String = sqlx::query_scalar(
        "select rejection_reason from ocr_uploads where upload_id = 'upl_PASSWORD'",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(reason, "password_required");
}
