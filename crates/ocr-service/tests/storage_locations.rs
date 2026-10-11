use ocr_service::{GcsResultReader, GcsUploadIssuer};

#[test]
fn configured_storage_routes_accept_all_three_clouds() {
    for resource in [
        "example-ocr-quarantine",
        "gs://example-ocr-quarantine",
        "s3://example-ocr-quarantine",
        "az://exampleaccount/quarantine",
    ] {
        assert!(
            GcsUploadIssuer::new(&[resource.to_owned()]).is_ok(),
            "{resource}"
        );
        assert!(
            GcsResultReader::new(&[resource.to_owned()]).is_ok(),
            "{resource}"
        );
    }
}

#[test]
fn storage_routes_reject_urls_credentials_and_ambiguous_paths() {
    for resource in [
        "https://storage.example/bucket",
        "s3://user:password@bucket",
        "s3://example-bucket/prefix",
        "s3://example-bucket?token=secret",
        "az://exampleaccount/container/extra",
        "az://127.0.0.1/container",
        "gs://example-bucket#fragment",
        "file:///tmp/data",
    ] {
        assert!(GcsUploadIssuer::new(&[resource.to_owned()]).is_err());
    }
}

#[test]
fn promotion_rejects_cross_cloud_and_aliased_same_bucket_routes() {
    for (source, destination) in [
        ("s3://example-quarantine", "az://exampleaccount/sources"),
        ("example-quarantine", "gs://example-quarantine"),
    ] {
        assert!(
            ocr_service::CloudSourcePromoter::new(std::collections::HashMap::from([(
                source.into(),
                destination.into()
            )]))
            .is_err()
        );
    }
}
