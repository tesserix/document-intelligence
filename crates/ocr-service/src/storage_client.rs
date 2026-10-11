use std::{collections::BTreeMap, ops::Deref, time::Duration};

use axum::http::Method;
use object_store::{
    aws::{AmazonS3, AmazonS3Builder},
    azure::{MicrosoftAzure, MicrosoftAzureBuilder},
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    signer::Signer,
    ObjectStore,
};

pub(crate) enum StorageClient {
    Gcs(GoogleCloudStorage),
    S3(AmazonS3),
    Azure(MicrosoftAzure),
}

impl StorageClient {
    pub(crate) fn new(resource: &str) -> Result<Self, ()> {
        match location(resource)? {
            Location::Gcs(bucket) => GoogleCloudStorageBuilder::new()
                .with_bucket_name(bucket)
                .build()
                .map(Self::Gcs)
                .map_err(|_| ()),
            Location::S3(bucket) => AmazonS3Builder::from_env()
                .with_bucket_name(bucket)
                .with_region(
                    std::env::var("AWS_REGION")
                        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                        .unwrap_or_else(|_| "us-east-1".into()),
                )
                .with_allow_http(false)
                .build()
                .map(Self::S3)
                .map_err(|_| ()),
            Location::Azure(account, container) => MicrosoftAzureBuilder::from_env()
                .with_account(account)
                .with_container_name(container)
                .with_allow_http(false)
                .build()
                .map(Self::Azure)
                .map_err(|_| ()),
        }
    }

    pub(crate) async fn signed_upload(
        &self,
        path: &Path,
        expires: Duration,
        content_type: &str,
    ) -> Result<(String, BTreeMap<String, String>), ()> {
        let headers = upload_headers(content_type, matches!(self, Self::Azure(_)));
        let url = match self {
            Self::Gcs(store) => store.signed_url(Method::PUT, path, expires).await,
            Self::S3(store) => store.signed_url(Method::PUT, path, expires).await,
            Self::Azure(store) => store.signed_url(Method::PUT, path, expires).await,
        }
        .map_err(|_| ())?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(());
        }
        Ok((url.to_string(), headers))
    }
}

impl Deref for StorageClient {
    type Target = dyn ObjectStore;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Gcs(s) => s,
            Self::S3(s) => s,
            Self::Azure(s) => s,
        }
    }
}

impl From<GoogleCloudStorage> for StorageClient {
    fn from(store: GoogleCloudStorage) -> Self {
        Self::Gcs(store)
    }
}

enum Location<'a> {
    Gcs(&'a str),
    S3(&'a str),
    Azure(&'a str, &'a str),
}

pub(crate) fn is_storage_location(value: &str) -> bool {
    location(value).is_ok()
}

fn location(value: &str) -> Result<Location<'_>, ()> {
    if let Some(bucket) = value.strip_prefix("s3://") {
        return (dns_name(bucket)
            && !bucket.contains('_')
            && bucket.parse::<std::net::Ipv4Addr>().is_err())
        .then_some(Location::S3(bucket))
        .ok_or(());
    }
    if let Some(azure) = value.strip_prefix("az://") {
        let (account, container) = azure.split_once('/').ok_or(())?;
        return ((3..=24).contains(&account.len())
            && account
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && dns_name(container)
            && !container.contains(['.', '_'])
            && !container.contains("--"))
        .then_some(Location::Azure(account, container))
        .ok_or(());
    }
    let bucket = value.strip_prefix("gs://").unwrap_or(value);
    (dns_name(bucket) && !bucket.starts_with("goog") && !bucket.contains("google"))
        .then_some(Location::Gcs(bucket))
        .ok_or(())
}

fn dns_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !value.contains("..")
}

pub(crate) fn upload_headers(content_type: &str, azure: bool) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::from([("content-type".into(), content_type.into())]);
    if azure {
        headers.insert("x-ms-blob-type".into(), "BlockBlob".into());
    }
    headers
}

pub(crate) fn promotion_compatible(source: &str, destination: &str) -> bool {
    match (location(source), location(destination)) {
        (Ok(Location::Gcs(a)), Ok(Location::Gcs(b)))
        | (Ok(Location::S3(a)), Ok(Location::S3(b))) => a != b,
        (Ok(Location::Azure(a, c)), Ok(Location::Azure(b, d))) => (a, c) != (b, d),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::{GetOptions, PutMode};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn fixture(
        provider: &str,
        version: &str,
    ) -> (StorageClient, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let version_header = if provider == "s3" {
            "x-amz-version-id"
        } else {
            "x-ms-version-id"
        };
        let version = version.to_owned();
        let query_key = if provider == "s3" {
            "versionId"
        } else {
            "versionid"
        };
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut chunk = [0; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0 && request.len() < 32768);
                request.extend_from_slice(&chunk[..count]);
            }
            let request = String::from_utf8(request).unwrap();
            let target = request
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let url = url::Url::parse(&format!("http://fixture{target}")).unwrap();
            assert!(url
                .query_pairs()
                .any(|(key, value)| key == query_key && value == version));
            let response = format!("HTTP/1.1 200 OK\r\ncontent-length: 4\r\netag: \"fixture\"\r\nlast-modified: Mon, 12 Oct 2026 00:00:00 GMT\r\n{version_header}: {version}\r\nconnection: close\r\n\r\nDATA");
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client = if provider == "s3" {
            StorageClient::S3(
                AmazonS3Builder::new()
                    .with_bucket_name("fixture-bucket")
                    .with_region("us-east-1")
                    .with_access_key_id("fixture-access")
                    .with_secret_access_key("synthetic-test-credential")
                    .with_endpoint(endpoint)
                    .with_allow_http(true)
                    .build()
                    .unwrap(),
            )
        } else {
            StorageClient::Azure(
                MicrosoftAzureBuilder::new()
                    .with_account("fixtureaccount")
                    .with_container_name("fixture-container")
                    .with_bearer_token_authorization("synthetic-test-token")
                    .with_endpoint(endpoint)
                    .with_allow_http(true)
                    .build()
                    .unwrap(),
            )
        };
        (client, task)
    }

    #[tokio::test]
    async fn cloud_reads_pin_and_preserve_opaque_versions() {
        for (provider, version) in [
            ("s3", "opaque+version/with=characters"),
            ("azure", "2026-10-12T01:02:03.1234567Z"),
        ] {
            let (client, server) = fixture(provider, version).await;
            let response = client
                .get_opts(
                    &Path::from("document"),
                    GetOptions::new().with_version(Some(version.to_owned())),
                )
                .await
                .unwrap();
            assert_eq!(response.meta.version.as_deref(), Some(version));
            assert_eq!(response.bytes().await.unwrap().as_ref(), b"DATA");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn s3_create_only_writes_send_a_precondition_and_preserve_conflicts() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
            assert!(request.starts_with("put "));
            assert!(request.contains("if-none-match: *\r\n"));
            let body = "<Error><Code>PreconditionFailed</Code><Message>exists</Message></Error>";
            stream.write_all(format!("HTTP/1.1 412 Precondition Failed\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let client = AmazonS3Builder::new()
            .with_bucket_name("fixture-bucket")
            .with_region("us-east-1")
            .with_access_key_id("fixture-access")
            .with_secret_access_key("synthetic-test-credential")
            .with_endpoint(endpoint)
            .with_allow_http(true)
            .build()
            .unwrap();
        let outcome = client
            .put_opts(
                &Path::from("document"),
                bytes::Bytes::from_static(b"DATA").into(),
                PutMode::Create.into(),
            )
            .await;
        assert!(matches!(
            outcome,
            Err(object_store::Error::AlreadyExists { .. })
                | Err(object_store::Error::Precondition { .. })
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn signed_uploads_are_object_scoped_and_azure_returns_blob_headers() {
        let clients = [
            StorageClient::S3(
                AmazonS3Builder::new()
                    .with_bucket_name("fixture-bucket")
                    .with_region("us-east-1")
                    .with_access_key_id("fixture-access")
                    .with_secret_access_key("synthetic-test-credential")
                    .build()
                    .unwrap(),
            ),
            StorageClient::Azure(
                MicrosoftAzureBuilder::new()
                    .with_account("fixtureaccount")
                    .with_container_name("quarantine")
                    .with_access_key(base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        b"synthetic-test-key",
                    ))
                    .build()
                    .unwrap(),
            ),
        ];
        for client in clients {
            let (url, headers) = client
                .signed_upload(
                    &Path::from("products/example/tenants/ten_TEST/quarantine/upl_TEST"),
                    Duration::from_secs(60),
                    "image/png",
                )
                .await
                .unwrap();
            let url = url::Url::parse(&url).unwrap();
            assert_eq!(url.scheme(), "https");
            assert!(url
                .path()
                .ends_with("/products/example/tenants/ten_TEST/quarantine/upl_TEST"));
            assert_eq!(
                headers.get("content-type").map(String::as_str),
                Some("image/png")
            );
            if matches!(client, StorageClient::Azure(_)) {
                assert_eq!(
                    headers.get("x-ms-blob-type").map(String::as_str),
                    Some("BlockBlob")
                );
                assert!(url
                    .query_pairs()
                    .any(|(key, value)| key == "sr" && value == "b"));
            } else {
                assert!(url
                    .query_pairs()
                    .any(|(key, value)| key == "X-Amz-Expires" && value == "60"));
            }
        }
    }
}
