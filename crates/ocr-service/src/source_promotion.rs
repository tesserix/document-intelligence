use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use object_store::{path::Path, GetOptions, PutMode};
use ocr_domain::{ProductId, TenantId};
use ocr_store::{StoredUpload, UploadState};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::timeout;
use url::Url;

use crate::{digest::sha256_digest, result_artifacts::is_bucket_name, MAXIMUM_UPLOAD_BYTES};

const MAXIMUM_REWRITE_CALLS: usize = 32;
const MAXIMUM_REWRITE_RESPONSE_BYTES: usize = 64 * 1024;
const PROMOTION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedSource {
    pub bucket: String,
    pub object_name: String,
    pub generation: String,
    pub digest: String,
    pub content_length: i64,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum SourcePromotionError {
    #[error("source promotion configuration is invalid")]
    InvalidConfiguration,
    #[error("source object is invalid")]
    InvalidSource,
    #[error("source destination conflicts with verified content")]
    DestinationConflict,
    #[error("source promotion is unavailable")]
    Unavailable,
}

struct PromotionRoute {
    destination_bucket: String,
    source: crate::storage_client::StorageClient,
    destination: crate::storage_client::StorageClient,
}

pub struct GcsSourcePromoter {
    routes: HashMap<String, PromotionRoute>,
    client: Arc<reqwest::Client>,
    rewrite_base_url: Url,
}

impl fmt::Debug for GcsSourcePromoter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GcsSourcePromoter")
            .field("route_count", &self.routes.len())
            .field(
                "rewrite_origin",
                &self.rewrite_base_url.origin().ascii_serialization(),
            )
            .finish()
    }
}

impl GcsSourcePromoter {
    pub fn new(routes: HashMap<String, String>) -> Result<Self, SourcePromotionError> {
        if routes.is_empty() {
            return Err(SourcePromotionError::InvalidConfiguration);
        }
        let mut configured = HashMap::with_capacity(routes.len());
        let mut destinations = HashSet::with_capacity(routes.len());
        for (source_bucket, destination_bucket) in routes {
            if !is_bucket_name(&source_bucket)
                || !is_bucket_name(&destination_bucket)
                || !crate::storage_client::promotion_compatible(&source_bucket, &destination_bucket)
                || configured.contains_key(&source_bucket)
                || !destinations.insert(destination_bucket.clone())
            {
                return Err(SourcePromotionError::InvalidConfiguration);
            }
            let source = crate::storage_client::StorageClient::new(&source_bucket)
                .map_err(|_| SourcePromotionError::InvalidConfiguration)?;
            let destination = crate::storage_client::StorageClient::new(&destination_bucket)
                .map_err(|_| SourcePromotionError::InvalidConfiguration)?;
            configured.insert(
                source_bucket,
                PromotionRoute {
                    destination_bucket,
                    source,
                    destination,
                },
            );
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| SourcePromotionError::InvalidConfiguration)?;
        let rewrite_base_url = Url::parse("https://storage.googleapis.com/storage/v1")
            .map_err(|_| SourcePromotionError::InvalidConfiguration)?;
        Ok(Self::with_parts(
            configured,
            Arc::new(client),
            rewrite_base_url,
        ))
    }

    fn with_parts(
        routes: HashMap<String, PromotionRoute>,
        client: Arc<reqwest::Client>,
        rewrite_base_url: Url,
    ) -> Self {
        Self {
            routes,
            client,
            rewrite_base_url,
        }
    }

    pub async fn promote(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        upload: &StoredUpload,
    ) -> Result<PromotedSource, SourcePromotionError> {
        timeout(
            PROMOTION_TIMEOUT,
            self.promote_inner(product_id, tenant_id, upload),
        )
        .await
        .map_err(|_| SourcePromotionError::Unavailable)?
    }

    async fn promote_inner(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        upload: &StoredUpload,
    ) -> Result<PromotedSource, SourcePromotionError> {
        let generation = upload
            .object_version
            .as_ref()
            .filter(|value| ocr_store::valid_object_version(value))
            .ok_or(SourcePromotionError::InvalidSource)?;
        let route = self
            .routes
            .get(&upload.object_bucket)
            .ok_or(SourcePromotionError::InvalidSource)?;
        let expected_source_name = format!(
            "products/{}/tenants/{}/quarantine/{}",
            product_id.as_str(),
            tenant_id.as_str(),
            upload.upload_id.as_str()
        );
        let digest = upload
            .expected_digest
            .strip_prefix("sha256:")
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(SourcePromotionError::InvalidSource)?;
        if upload.state != UploadState::Inspecting
            || upload.object_name != expected_source_name
            || !(1..=MAXIMUM_UPLOAD_BYTES).contains(&upload.expected_content_length)
        {
            return Err(SourcePromotionError::InvalidSource);
        }
        let destination_name = format!(
            "products/{}/tenants/{}/documents/sha256/{digest}",
            product_id.as_str(),
            tenant_id.as_str()
        );
        let crate::storage_client::StorageClient::Gcs(gcs) = &route.source else {
            return promote_stream(route, upload, &destination_name, generation).await;
        };
        if !matches!(
            route.destination,
            crate::storage_client::StorageClient::Gcs(_)
        ) {
            return Err(SourcePromotionError::InvalidConfiguration);
        }
        let credential = gcs
            .credentials()
            .get_credential()
            .await
            .map_err(|_| SourcePromotionError::Unavailable)?;
        let mut rewrite_token = None;
        for _ in 0..MAXIMUM_REWRITE_CALLS {
            let url = rewrite_url(
                &self.rewrite_base_url,
                upload
                    .object_bucket
                    .strip_prefix("gs://")
                    .unwrap_or(&upload.object_bucket),
                &upload.object_name,
                route
                    .destination_bucket
                    .strip_prefix("gs://")
                    .unwrap_or(&route.destination_bucket),
                &destination_name,
                generation
                    .parse::<i64>()
                    .map_err(|_| SourcePromotionError::InvalidSource)?,
                rewrite_token.as_deref(),
            )?;
            let mut request = self.client.post(url);
            if !credential.bearer.is_empty() {
                request = request.bearer_auth(&credential.bearer);
            }
            let response = request
                .send()
                .await
                .map_err(|_| SourcePromotionError::Unavailable)?;
            if response.status() == reqwest::StatusCode::PRECONDITION_FAILED {
                return verify_existing(
                    route,
                    &destination_name,
                    &upload.expected_digest,
                    upload.expected_content_length,
                )
                .await;
            }
            if response.status() == reqwest::StatusCode::NOT_FOUND
                || response.status() == reqwest::StatusCode::BAD_REQUEST
            {
                return Err(SourcePromotionError::InvalidSource);
            }
            if !response.status().is_success() {
                return Err(SourcePromotionError::Unavailable);
            }
            let body = bounded_response(response).await?;
            let result: RewriteResponse =
                serde_json::from_slice(&body).map_err(|_| SourcePromotionError::Unavailable)?;
            if result.done {
                let resource = result.resource.ok_or(SourcePromotionError::Unavailable)?;
                let promoted_generation = resource
                    .generation
                    .parse::<i64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or(SourcePromotionError::Unavailable)?;
                let promoted_length = resource
                    .size
                    .parse::<i64>()
                    .map_err(|_| SourcePromotionError::Unavailable)?;
                if resource.bucket
                    != route
                        .destination_bucket
                        .strip_prefix("gs://")
                        .unwrap_or(&route.destination_bucket)
                    || resource.name != destination_name
                    || promoted_length != upload.expected_content_length
                {
                    return Err(SourcePromotionError::DestinationConflict);
                }
                return Ok(PromotedSource {
                    bucket: route.destination_bucket.clone(),
                    object_name: resource.name,
                    generation: promoted_generation.to_string(),
                    digest: upload.expected_digest.clone(),
                    content_length: promoted_length,
                });
            }
            let token = result
                .rewrite_token
                .filter(|token| !token.is_empty() && token.len() <= 1024)
                .ok_or(SourcePromotionError::Unavailable)?;
            rewrite_token = Some(token);
        }
        Err(SourcePromotionError::Unavailable)
    }
}

async fn promote_stream(
    route: &PromotionRoute,
    upload: &StoredUpload,
    destination_name: &str,
    version: &str,
) -> Result<PromotedSource, SourcePromotionError> {
    if std::mem::discriminant(&route.source) != std::mem::discriminant(&route.destination) {
        return Err(SourcePromotionError::InvalidConfiguration);
    }
    let source_path =
        Path::parse(&upload.object_name).map_err(|_| SourcePromotionError::InvalidSource)?;
    let source = route
        .source
        .get_opts(
            &source_path,
            GetOptions::new().with_version(Some(version.to_owned())),
        )
        .await
        .map_err(|_| SourcePromotionError::Unavailable)?;
    if source.meta.version.as_deref() != Some(version) {
        return Err(SourcePromotionError::InvalidSource);
    }
    let bytes = crate::document_reader::collect_verified(
        upload.expected_content_length,
        &upload.expected_digest,
        source.meta.size,
        source.into_stream(),
    )
    .await
    .map_err(|error| match error {
        crate::importer::DocumentReadError::Invalid => SourcePromotionError::InvalidSource,
        crate::importer::DocumentReadError::Unavailable => SourcePromotionError::Unavailable,
    })?;
    let path = Path::parse(destination_name).map_err(|_| SourcePromotionError::InvalidSource)?;
    match route
        .destination
        .put_opts(&path, bytes.into(), PutMode::Create.into())
        .await
    {
        Ok(_)
        | Err(object_store::Error::AlreadyExists { .. })
        | Err(object_store::Error::Precondition { .. }) => {}
        Err(_) => return Err(SourcePromotionError::Unavailable),
    }
    verify_existing(
        route,
        destination_name,
        &upload.expected_digest,
        upload.expected_content_length,
    )
    .await
}

async fn bounded_response(response: reqwest::Response) -> Result<Vec<u8>, SourcePromotionError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| SourcePromotionError::Unavailable)?;
        if body.len().saturating_add(chunk.len()) > MAXIMUM_REWRITE_RESPONSE_BYTES {
            return Err(SourcePromotionError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn verify_existing(
    route: &PromotionRoute,
    destination_name: &str,
    expected_digest: &str,
    expected_length: i64,
) -> Result<PromotedSource, SourcePromotionError> {
    let path = Path::parse(destination_name).map_err(|_| SourcePromotionError::InvalidSource)?;
    let result = route
        .destination
        .get_opts(&path, GetOptions::default())
        .await
        .map_err(|_| SourcePromotionError::Unavailable)?;
    let generation = result
        .meta
        .version
        .as_deref()
        .filter(|value| ocr_store::valid_object_version(value))
        .map(str::to_owned)
        .ok_or(SourcePromotionError::DestinationConflict)?;
    let expected_length_u64 =
        u64::try_from(expected_length).map_err(|_| SourcePromotionError::InvalidSource)?;
    if result.meta.size != expected_length_u64 {
        return Err(SourcePromotionError::DestinationConflict);
    }
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    let mut stream = result.into_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| SourcePromotionError::Unavailable)?;
        length = length
            .checked_add(u64::try_from(chunk.len()).map_err(|_| SourcePromotionError::Unavailable)?)
            .ok_or(SourcePromotionError::DestinationConflict)?;
        if length > expected_length_u64 {
            return Err(SourcePromotionError::DestinationConflict);
        }
        hasher.update(&chunk);
    }
    let actual_digest = sha256_digest(hasher.finalize());
    if length != expected_length_u64 || actual_digest != expected_digest {
        return Err(SourcePromotionError::DestinationConflict);
    }
    Ok(PromotedSource {
        bucket: route.destination_bucket.clone(),
        object_name: destination_name.to_owned(),
        generation,
        digest: expected_digest.to_owned(),
        content_length: expected_length,
    })
}

fn rewrite_url(
    base: &Url,
    source_bucket: &str,
    source_name: &str,
    destination_bucket: &str,
    destination_name: &str,
    source_generation: i64,
    rewrite_token: Option<&str>,
) -> Result<Url, SourcePromotionError> {
    let mut url = base.clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| SourcePromotionError::InvalidConfiguration)?;
        segments.pop_if_empty();
        segments.extend([
            "b",
            source_bucket,
            "o",
            source_name,
            "rewriteTo",
            "b",
            destination_bucket,
            "o",
            destination_name,
        ]);
    }
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("sourceGeneration", &source_generation.to_string());
        query.append_pair("ifGenerationMatch", "0");
        if let Some(token) = rewrite_token {
            query.append_pair("rewriteToken", token);
        }
    }
    Ok(url)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RewriteResponse {
    #[serde(default)]
    done: bool,
    rewrite_token: Option<String>,
    resource: Option<RewriteResource>,
}

#[derive(Debug, Deserialize)]
struct RewriteResource {
    bucket: String,
    name: String,
    generation: String,
    size: String,
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use object_store::gcp::GoogleCloudStorageBuilder;
    use ocr_domain::{ProductId, TenantId, UploadId};
    use ocr_store::{StoredUpload, UploadState};
    use time::OffsetDateTime;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use url::Url;

    use super::{GcsSourcePromoter, PromotionRoute};

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let length = socket.read(&mut chunk).await.unwrap();
            assert!(length > 0, "connection closed before request headers");
            request.extend_from_slice(&chunk[..length]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return String::from_utf8(request).unwrap();
            }
            assert!(request.len() <= 8192, "request headers exceeded test bound");
        }
    }

    #[tokio::test]
    async fn promotion_pins_source_generation_and_creates_a_content_addressed_destination() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            let request_line = request.lines().next().unwrap();
            assert!(request_line.starts_with("POST /storage/v1/b/dev-kora-ocr-quarantine/o/"));
            assert!(request_line.contains("sourceGeneration=42"));
            assert!(request_line.contains("ifGenerationMatch=0"));
            assert!(
                request_line
                    .contains("products%2Fkora%2Ftenants%2Ften_PROMOTE%2Fquarantine%2Fupl_PROMOTE"),
                "unexpected request line: {request_line}"
            );
            assert!(request_line.contains("/rewriteTo/b/dev-kora-ocr-source/o/"));
            assert!(request_line.contains(
                "products%2Fkora%2Ftenants%2Ften_PROMOTE%2Fdocuments%2Fsha256%2Faaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ));
            let body = r#"{"done":true,"resource":{"bucket":"dev-kora-ocr-source","name":"products/kora/tenants/ten_PROMOTE/documents/sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","generation":"73","size":"8"}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(), body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let source = GoogleCloudStorageBuilder::new()
            .with_bucket_name("dev-kora-ocr-quarantine")
            .with_bearer_token("")
            .with_skip_signature(true)
            .build()
            .unwrap();
        let destination = GoogleCloudStorageBuilder::new()
            .with_bucket_name("dev-kora-ocr-source")
            .with_bearer_token("")
            .with_skip_signature(true)
            .build()
            .unwrap();
        let routes = HashMap::from([(
            "dev-kora-ocr-quarantine".to_owned(),
            PromotionRoute {
                destination_bucket: "dev-kora-ocr-source".to_owned(),
                source: source.into(),
                destination: destination.into(),
            },
        )]);
        let promoter = GcsSourcePromoter::with_parts(
            routes,
            Arc::new(reqwest::Client::new()),
            Url::parse(&format!("http://{address}/storage/v1")).unwrap(),
        );
        let digest = format!("sha256:{}", "a".repeat(64));
        let upload = StoredUpload {
            upload_id: UploadId::new("upl_PROMOTE").unwrap(),
            state: UploadState::Inspecting,
            object_bucket: "dev-kora-ocr-quarantine".to_owned(),
            object_name: "products/kora/tenants/ten_PROMOTE/quarantine/upl_PROMOTE".to_owned(),
            expected_content_type: "application/pdf".to_owned(),
            expected_content_length: 8,
            expected_digest: digest.clone(),
            expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(10),
            created_at: OffsetDateTime::now_utc(),
            object_version: Some("42".to_owned()),
            uploaded_at: Some(OffsetDateTime::now_utc()),
        };

        let promoted = promoter
            .promote(
                &ProductId::new("kora").unwrap(),
                &TenantId::new("ten_PROMOTE").unwrap(),
                &upload,
            )
            .await
            .unwrap();

        assert_eq!(promoted.bucket, "dev-kora-ocr-source");
        assert_eq!(promoted.generation, "73");
        assert_eq!(promoted.content_length, 8);
        assert_eq!(promoted.digest, digest);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn promotion_replay_rejects_an_existing_destination_with_different_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut rewrite_socket, _) = listener.accept().await.unwrap();
            let rewrite_request = read_request(&mut rewrite_socket).await;
            assert!(rewrite_request.starts_with("POST /storage/v1/"));
            rewrite_socket
                .write_all(b"HTTP/1.1 412 Precondition Failed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await
                .unwrap();

            let (mut get_socket, _) = listener.accept().await.unwrap();
            let get_request = read_request(&mut get_socket).await;
            assert!(
                get_request.starts_with("GET /dev%2Dkora%2Docr%2Dsource/"),
                "unexpected request: {get_request}"
            );
            get_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\netag: \"fixture\"\r\nlast-modified: Thu, 03 Sep 2026 00:00:00 GMT\r\nx-goog-generation: 73\r\nconnection: close\r\n\r\nDIFFEREN",
                )
                .await
                .unwrap();
        });
        let source = GoogleCloudStorageBuilder::new()
            .with_bucket_name("dev-kora-ocr-quarantine")
            .with_bearer_token("")
            .with_skip_signature(true)
            .build()
            .unwrap();
        let destination = GoogleCloudStorageBuilder::new()
            .with_bucket_name("dev-kora-ocr-source")
            .with_base_url(&format!("http://{address}"))
            .with_bearer_token("")
            .with_skip_signature(true)
            .build()
            .unwrap();
        let routes = HashMap::from([(
            "dev-kora-ocr-quarantine".to_owned(),
            PromotionRoute {
                destination_bucket: "dev-kora-ocr-source".to_owned(),
                source: source.into(),
                destination: destination.into(),
            },
        )]);
        let promoter = GcsSourcePromoter::with_parts(
            routes,
            Arc::new(reqwest::Client::new()),
            Url::parse(&format!("http://{address}/storage/v1")).unwrap(),
        );
        let upload = StoredUpload {
            upload_id: UploadId::new("upl_PROMOTE").unwrap(),
            state: UploadState::Inspecting,
            object_bucket: "dev-kora-ocr-quarantine".to_owned(),
            object_name: "products/kora/tenants/ten_PROMOTE/quarantine/upl_PROMOTE".to_owned(),
            expected_content_type: "application/pdf".to_owned(),
            expected_content_length: 8,
            expected_digest: format!("sha256:{}", "a".repeat(64)),
            expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(10),
            created_at: OffsetDateTime::now_utc(),
            object_version: Some("42".to_owned()),
            uploaded_at: Some(OffsetDateTime::now_utc()),
        };

        let error = promoter
            .promote(
                &ProductId::new("kora").unwrap(),
                &TenantId::new("ten_PROMOTE").unwrap(),
                &upload,
            )
            .await
            .unwrap_err();

        assert_eq!(error, super::SourcePromotionError::DestinationConflict);
        server.await.unwrap();
    }
}

#[cfg(test)]
mod cloud_protocol_tests {
    use super::*;
    use crate::storage_client::StorageClient;
    use object_store::{aws::AmazonS3Builder, azure::MicrosoftAzureBuilder};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn cloud_promotion_pins_sources_and_verifies_replayed_destinations() {
        for azure in [false, true] {
            for conflict in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}", listener.local_addr().unwrap());
                let server = tokio::spawn(async move {
                    for step in 0..3 {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        let mut buffer = Vec::new();
                        while !buffer.windows(4).any(|b| b == b"\r\n\r\n") {
                            let mut chunk = [0; 4096];
                            let read = socket.read(&mut chunk).await.unwrap();
                            assert!(read > 0 && buffer.len() < 32768);
                            buffer.extend_from_slice(&chunk[..read]);
                        }
                        let request = String::from_utf8_lossy(&buffer);
                        let target = request
                            .lines()
                            .next()
                            .unwrap()
                            .split_whitespace()
                            .nth(1)
                            .unwrap();
                        let url = Url::parse(&format!("http://fixture{target}")).unwrap();
                        if step == 1 {
                            assert!(request.starts_with("PUT "));
                            assert!(request
                                .to_ascii_lowercase()
                                .contains("if-none-match: *\r\n"));
                            let body = "<Error><Code>PreconditionFailed</Code></Error>";
                            socket.write_all(format!("HTTP/1.1 412 Precondition Failed\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                        } else {
                            assert!(request.starts_with("GET "));
                            if step == 0 {
                                let key = if azure { "versionid" } else { "versionId" };
                                assert!(url
                                    .query_pairs()
                                    .any(|(k, v)| k == key && v == "opaque+source/version"));
                            }
                            let header = if azure {
                                "x-ms-version-id"
                            } else {
                                "x-amz-version-id"
                            };
                            let version = if step == 0 {
                                "opaque+source/version"
                            } else {
                                "opaque-destination"
                            };
                            let body = if step == 2 && conflict {
                                "EVIL"
                            } else {
                                "DATA"
                            };
                            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-length: 4\r\netag: \"fixture\"\r\nlast-modified: Mon, 12 Oct 2026 00:00:00 GMT\r\n{header}: {version}\r\nconnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
                        }
                    }
                });
                let client = |bucket: &str| {
                    if azure {
                        StorageClient::Azure(
                            MicrosoftAzureBuilder::new()
                                .with_account("fixtureaccount")
                                .with_container_name(bucket)
                                .with_bearer_token_authorization("synthetic-test-token")
                                .with_endpoint(endpoint.clone())
                                .with_allow_http(true)
                                .build()
                                .unwrap(),
                        )
                    } else {
                        StorageClient::S3(
                            AmazonS3Builder::new()
                                .with_bucket_name(bucket)
                                .with_region("us-east-1")
                                .with_access_key_id("fixture-access")
                                .with_secret_access_key("synthetic-test-credential")
                                .with_endpoint(endpoint.clone())
                                .with_allow_http(true)
                                .build()
                                .unwrap(),
                        )
                    }
                };
                let source_bucket = if azure {
                    "az://fixtureaccount/quarantine"
                } else {
                    "s3://fixture-quarantine"
                };
                let destination_bucket = if azure {
                    "az://fixtureaccount/sources"
                } else {
                    "s3://fixture-sources"
                };
                let promoter = GcsSourcePromoter::with_parts(
                    HashMap::from([(
                        source_bucket.to_owned(),
                        PromotionRoute {
                            destination_bucket: destination_bucket.to_owned(),
                            source: client("quarantine"),
                            destination: client("sources"),
                        },
                    )]),
                    Arc::new(reqwest::Client::new()),
                    Url::parse("https://unused.example").unwrap(),
                );
                let upload = StoredUpload {
                    upload_id: ocr_domain::UploadId::new("upl_CLOUDPROMOTION").unwrap(),
                    state: UploadState::Inspecting,
                    object_bucket: source_bucket.to_owned(),
                    object_name:
                        "products/example/tenants/ten_CLOUDPROMOTION/quarantine/upl_CLOUDPROMOTION"
                            .into(),
                    expected_content_type: "image/png".into(),
                    expected_content_length: 4,
                    expected_digest: sha256_digest(Sha256::digest(b"DATA")),
                    expires_at: time::OffsetDateTime::UNIX_EPOCH,
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                    object_version: Some("opaque+source/version".into()),
                    uploaded_at: Some(time::OffsetDateTime::UNIX_EPOCH),
                };
                let outcome = promoter
                    .promote(
                        &ProductId::new("example").unwrap(),
                        &TenantId::new("ten_CLOUDPROMOTION").unwrap(),
                        &upload,
                    )
                    .await;
                if conflict {
                    assert_eq!(outcome, Err(SourcePromotionError::DestinationConflict));
                } else {
                    let promoted = outcome.unwrap();
                    assert_eq!(promoted.generation, "opaque-destination");
                    assert_eq!(promoted.bucket, destination_bucket);
                    assert_eq!(promoted.digest, upload.expected_digest);
                }
                server.await.unwrap();
            }
        }
    }
}
