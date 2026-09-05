use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    GetOptions, ObjectStore,
};
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
    pub generation: i64,
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
    source: GoogleCloudStorage,
    destination: GoogleCloudStorage,
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
                || source_bucket == destination_bucket
                || configured.contains_key(&source_bucket)
                || !destinations.insert(destination_bucket.clone())
            {
                return Err(SourcePromotionError::InvalidConfiguration);
            }
            let source = GoogleCloudStorageBuilder::new()
                .with_bucket_name(&source_bucket)
                .build()
                .map_err(|_| SourcePromotionError::InvalidConfiguration)?;
            let destination = GoogleCloudStorageBuilder::new()
                .with_bucket_name(&destination_bucket)
                .build()
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
            .object_generation
            .filter(|value| *value > 0)
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
        let credential = route
            .source
            .credentials()
            .get_credential()
            .await
            .map_err(|_| SourcePromotionError::Unavailable)?;
        let mut rewrite_token = None;
        for _ in 0..MAXIMUM_REWRITE_CALLS {
            let url = rewrite_url(
                &self.rewrite_base_url,
                &upload.object_bucket,
                &upload.object_name,
                &route.destination_bucket,
                &destination_name,
                generation,
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
                if resource.bucket != route.destination_bucket
                    || resource.name != destination_name
                    || promoted_length != upload.expected_content_length
                {
                    return Err(SourcePromotionError::DestinationConflict);
                }
                return Ok(PromotedSource {
                    bucket: resource.bucket,
                    object_name: resource.name,
                    generation: promoted_generation,
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
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
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
                source,
                destination,
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
            object_generation: Some(42),
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
        assert_eq!(promoted.generation, 73);
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
                source,
                destination,
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
            object_generation: Some(42),
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
