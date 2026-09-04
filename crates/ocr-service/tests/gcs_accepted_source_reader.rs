use ocr_domain::{PageGeometry, PageNumber, ProductId, TenantId};
use ocr_service::{AcceptedSourceReadError, GcsAcceptedSourceReader};
use ocr_store::StoredAcceptedSource;

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
        page_count: 1,
        maximum_page_pixels: 1_000_000,
        total_page_pixels: 1_000_000,
        page_geometries: vec![PageGeometry::new(PageNumber::new(1).unwrap(), 1_000, 1_000).unwrap()],
        parser_profile: "strict-v1".to_owned(),
        parser_version: "1.0.0".to_owned(),
    }
}

#[tokio::test]
async fn reader_rejects_cross_product_or_tenant_source_before_storage_io() {
    let reader =
        GcsAcceptedSourceReader::new(&[("kora".to_owned(), "dev-kora-ocr-source".to_owned())])
            .unwrap();
    let product = ProductId::new("kora").unwrap();
    let tenant = TenantId::new("ten_SOURCE").unwrap();

    let mut wrong_bucket = source();
    wrong_bucket.bucket = "dev-other-ocr-source".to_owned();
    assert_eq!(
        reader.read(&product, &tenant, &wrong_bucket).await,
        Err(AcceptedSourceReadError::Invalid)
    );

    let mut wrong_tenant = source();
    wrong_tenant.object_name = format!(
        "products/kora/tenants/ten_OTHER/documents/sha256/{}",
        "a".repeat(64)
    );
    assert_eq!(
        reader.read(&product, &tenant, &wrong_tenant).await,
        Err(AcceptedSourceReadError::Invalid)
    );
}
