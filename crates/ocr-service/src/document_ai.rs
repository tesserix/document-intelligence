use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use gcp_auth::TokenProvider;
use ocr_domain::{
    Confidence, DocumentPage, ObservationId, ObservationLevel, PageNumber, PageTask, Polygon,
    TextObservation,
};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::timeout;

use crate::{
    AcceptedPageSource, PageRecognitionError, PageRecognitionFuture, PageRecognizer,
    PageSourceError, PageSourceResolver,
};

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const DOCUMENT_AI_TIMEOUT: Duration = Duration::from_secs(30);
const MAXIMUM_MANAGED_INPUT_BYTES: usize = 20 * 1024 * 1024;
const MAXIMUM_MANAGED_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum DocumentAiConfigurationError {
    #[error("Document AI processor configuration is invalid")]
    Invalid,
}

#[derive(Clone)]
pub struct DocumentAiConfiguration {
    project_id: String,
    location: String,
    processor_id: String,
}

impl DocumentAiConfiguration {
    pub fn new(
        project_id: impl Into<String>,
        location: impl Into<String>,
        processor_id: impl Into<String>,
    ) -> Result<Self, DocumentAiConfigurationError> {
        let project_id = project_id.into();
        let location = location.into();
        let processor_id = processor_id.into();
        if !is_component(&project_id) || !is_component(&location) || !is_component(&processor_id) {
            return Err(DocumentAiConfigurationError::Invalid);
        }
        Ok(Self {
            project_id,
            location,
            processor_id,
        })
    }

    fn endpoint(&self) -> String {
        format!(
            "https://{}-documentai.googleapis.com/v1/projects/{}/locations/{}/processors/{}:process",
            self.location, self.project_id, self.location, self.processor_id
        )
    }
}

fn is_component(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum DocumentAiTransportError {
    #[error("Document AI rejected the page")]
    Invalid,
    #[error("Document AI is unavailable")]
    Unavailable,
}

pub struct DocumentAiRequest {
    page: u32,
    content_type: String,
    bytes: Vec<u8>,
}

impl DocumentAiRequest {
    fn new(page: u32, source: AcceptedPageSource) -> Self {
        let (bytes, content_type) = source.into_bytes_and_content_type();
        Self {
            page,
            content_type,
            bytes,
        }
    }

    pub fn page(&self) -> u32 {
        self.page
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

pub type DocumentAiTransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, DocumentAiTransportError>> + Send + 'a>>;

pub trait DocumentAiTransport: Send + Sync {
    fn process<'a>(&'a self, request: DocumentAiRequest) -> DocumentAiTransportFuture<'a>;
}

pub struct MetadataDocumentAiTransport {
    configuration: DocumentAiConfiguration,
    identity: gcp_auth::MetadataServiceAccount,
    client: Client,
}

impl MetadataDocumentAiTransport {
    pub async fn new(
        configuration: DocumentAiConfiguration,
    ) -> Result<Self, DocumentAiTransportError> {
        let identity = gcp_auth::MetadataServiceAccount::new()
            .await
            .map_err(|_| DocumentAiTransportError::Unavailable)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| DocumentAiTransportError::Unavailable)?;
        Ok(Self {
            configuration,
            identity,
            client,
        })
    }

    async fn process_inner(
        &self,
        request: DocumentAiRequest,
    ) -> Result<Vec<u8>, DocumentAiTransportError> {
        if request.bytes.is_empty()
            || request.bytes.len() > MAXIMUM_MANAGED_INPUT_BYTES
            || !is_supported_content_type(&request.content_type)
        {
            return Err(DocumentAiTransportError::Invalid);
        }
        let token = self
            .identity
            .token(&[CLOUD_PLATFORM_SCOPE])
            .await
            .map_err(|_| DocumentAiTransportError::Unavailable)?;
        let body = ProcessRequest {
            raw_document: RawDocument {
                content: STANDARD.encode(request.bytes),
                mime_type: request.content_type,
            },
            skip_human_review: true,
            process_options: ProcessOptions {
                individual_page_selector: IndividualPageSelector {
                    pages: vec![request.page],
                },
            },
        };
        let response = timeout(
            DOCUMENT_AI_TIMEOUT,
            self.client
                .post(self.configuration.endpoint())
                .bearer_auth(token.as_str())
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| DocumentAiTransportError::Unavailable)?
        .map_err(|_| DocumentAiTransportError::Unavailable)?;
        if !response.status().is_success() {
            return Err(map_http_status(response.status()));
        }
        read_bounded_response(response).await
    }
}

impl DocumentAiTransport for MetadataDocumentAiTransport {
    fn process<'a>(&'a self, request: DocumentAiRequest) -> DocumentAiTransportFuture<'a> {
        Box::pin(async move { self.process_inner(request).await })
    }
}

fn is_supported_content_type(value: &str) -> bool {
    matches!(
        value,
        "application/pdf" | "image/jpeg" | "image/png" | "image/tiff" | "image/webp"
    )
}

fn map_http_status(status: StatusCode) -> DocumentAiTransportError {
    if status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        DocumentAiTransportError::Unavailable
    } else {
        DocumentAiTransportError::Invalid
    }
}

async fn read_bounded_response(
    response: reqwest::Response,
) -> Result<Vec<u8>, DocumentAiTransportError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAXIMUM_MANAGED_RESPONSE_BYTES as u64)
    {
        return Err(DocumentAiTransportError::Invalid);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| DocumentAiTransportError::Unavailable)?;
        if body.len().saturating_add(chunk.len()) > MAXIMUM_MANAGED_RESPONSE_BYTES {
            return Err(DocumentAiTransportError::Invalid);
        }
        body.extend_from_slice(&chunk);
    }
    if body.is_empty() {
        return Err(DocumentAiTransportError::Invalid);
    }
    Ok(body)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessRequest {
    raw_document: RawDocument,
    skip_human_review: bool,
    process_options: ProcessOptions,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RawDocument {
    content: String,
    mime_type: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessOptions {
    individual_page_selector: IndividualPageSelector,
}

#[derive(Serialize)]
struct IndividualPageSelector {
    pages: Vec<u32>,
}

pub struct DocumentAiPageRecognizer {
    source: Arc<dyn PageSourceResolver>,
    transport: Arc<dyn DocumentAiTransport>,
}

impl DocumentAiPageRecognizer {
    pub fn new(
        source: Arc<dyn PageSourceResolver>,
        transport: Arc<dyn DocumentAiTransport>,
    ) -> Self {
        Self { source, transport }
    }

    async fn recognize_inner(
        &self,
        product_id: &ocr_domain::ProductId,
        tenant_id: &ocr_domain::TenantId,
        job_id: &ocr_domain::JobId,
        task: &PageTask,
    ) -> Result<DocumentPage, PageRecognitionError> {
        let source = self
            .source
            .load(product_id, tenant_id, job_id, task)
            .await
            .map_err(map_source_error)?;
        let geometry = source.geometry();
        let output = self
            .transport
            .process(DocumentAiRequest::new(task.page, source))
            .await
            .map_err(|error| match error {
                DocumentAiTransportError::Invalid => PageRecognitionError::Permanent,
                DocumentAiTransportError::Unavailable => PageRecognitionError::Retryable,
            })?;
        parse_document(&output, task.page, geometry.width, geometry.height)
    }
}

impl PageRecognizer for DocumentAiPageRecognizer {
    fn recognize<'a>(
        &'a self,
        product_id: &'a ocr_domain::ProductId,
        tenant_id: &'a ocr_domain::TenantId,
        job_id: &'a ocr_domain::JobId,
        task: &'a PageTask,
    ) -> PageRecognitionFuture<'a> {
        Box::pin(async move {
            self.recognize_inner(product_id, tenant_id, job_id, task)
                .await
        })
    }
}

fn map_source_error(error: PageSourceError) -> PageRecognitionError {
    match error {
        PageSourceError::Unavailable => PageRecognitionError::Retryable,
        PageSourceError::NotFound | PageSourceError::Invalid => PageRecognitionError::Permanent,
    }
}

fn parse_document(
    encoded: &[u8],
    expected_page: u32,
    width: u32,
    height: u32,
) -> Result<DocumentPage, PageRecognitionError> {
    let response: ProcessResponse =
        serde_json::from_slice(encoded).map_err(|_| PageRecognitionError::Permanent)?;
    let document = response.document.ok_or(PageRecognitionError::Permanent)?;
    if document.text.len() > MAXIMUM_MANAGED_RESPONSE_BYTES || document.pages.len() != 1 {
        return Err(PageRecognitionError::Permanent);
    }
    let page = document
        .pages
        .into_iter()
        .next()
        .ok_or(PageRecognitionError::Permanent)?;
    if page.page_number != expected_page || page.lines.len() > 100_000 {
        return Err(PageRecognitionError::Permanent);
    }
    let page_number =
        PageNumber::new(expected_page).map_err(|_| PageRecognitionError::Permanent)?;
    let observations = page
        .lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| observation(&document.text, page_number, index, line))
        .collect::<Result<Vec<_>, _>>()?;
    DocumentPage::new(page_number, width, height, observations)
        .map_err(|_| PageRecognitionError::Permanent)
}

fn observation(
    document_text: &str,
    page: PageNumber,
    index: usize,
    line: WireLine,
) -> Result<TextObservation, PageRecognitionError> {
    let reading_order = u32::try_from(index).map_err(|_| PageRecognitionError::Permanent)?;
    let text = anchored_text(document_text, &line.layout.text_anchor)?;
    let confidence = Confidence::new(
        line.layout
            .confidence
            .ok_or(PageRecognitionError::Permanent)?,
    )
    .map_err(|_| PageRecognitionError::Permanent)?;
    let points = line
        .layout
        .bounding_poly
        .normalized_vertices
        .into_iter()
        .map(|vertex| {
            ocr_domain::NormalizedPoint::new(vertex.x.unwrap_or(0.0), vertex.y.unwrap_or(0.0))
                .map_err(|_| PageRecognitionError::Permanent)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let polygon = Polygon::new(points).map_err(|_| PageRecognitionError::Permanent)?;
    let observation_id =
        ObservationId::try_from(format!("obs_gdoc_{}_{}", u32::from(page), index + 1))
            .map_err(|_| PageRecognitionError::Permanent)?;
    TextObservation::new(
        observation_id,
        ObservationLevel::Line,
        text,
        confidence,
        polygon,
        reading_order,
        None,
    )
    .map_err(|_| PageRecognitionError::Permanent)
}

fn anchored_text(
    document_text: &str,
    anchor: &WireTextAnchor,
) -> Result<String, PageRecognitionError> {
    if anchor.text_segments.is_empty() {
        return Err(PageRecognitionError::Permanent);
    }
    let mut value = String::new();
    for segment in &anchor.text_segments {
        let start = segment
            .start_index
            .as_ref()
            .map_or(Ok(0), WireOffset::value)?;
        let end = segment
            .end_index
            .as_ref()
            .ok_or(PageRecognitionError::Permanent)
            .and_then(WireOffset::value)?;
        if start > end
            || end > document_text.len()
            || !document_text.is_char_boundary(start)
            || !document_text.is_char_boundary(end)
        {
            return Err(PageRecognitionError::Permanent);
        }
        value.push_str(&document_text[start..end]);
    }
    if value.trim().is_empty() || value.len() > 65_536 {
        return Err(PageRecognitionError::Permanent);
    }
    Ok(value)
}

#[derive(Deserialize)]
struct ProcessResponse {
    document: Option<WireDocument>,
}

#[derive(Deserialize)]
struct WireDocument {
    text: String,
    pages: Vec<WirePage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePage {
    page_number: u32,
    lines: Vec<WireLine>,
}

#[derive(Deserialize)]
struct WireLine {
    layout: WireLayout,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireLayout {
    text_anchor: WireTextAnchor,
    confidence: Option<f64>,
    bounding_poly: WireBoundingPoly,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTextAnchor {
    text_segments: Vec<WireTextSegment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTextSegment {
    start_index: Option<WireOffset>,
    end_index: Option<WireOffset>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireOffset {
    Number(u64),
    String(String),
}

impl WireOffset {
    fn value(&self) -> Result<usize, PageRecognitionError> {
        match self {
            Self::Number(value) => {
                usize::try_from(*value).map_err(|_| PageRecognitionError::Permanent)
            }
            Self::String(value) => value
                .parse::<usize>()
                .map_err(|_| PageRecognitionError::Permanent),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireBoundingPoly {
    normalized_vertices: Vec<WireVertex>,
}

#[derive(Deserialize)]
struct WireVertex {
    x: Option<f64>,
    y: Option<f64>,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ocr_domain::{PageGeometry, PageNumber};

    use super::*;

    struct StaticSource {
        source: Mutex<Option<Result<AcceptedPageSource, PageSourceError>>>,
    }

    impl PageSourceResolver for StaticSource {
        fn load<'a>(
            &'a self,
            _product_id: &'a ocr_domain::ProductId,
            _tenant_id: &'a ocr_domain::TenantId,
            _job_id: &'a ocr_domain::JobId,
            _task: &'a PageTask,
        ) -> crate::PageSourceFuture<'a> {
            Box::pin(async move {
                self.source
                    .lock()
                    .map_err(|_| PageSourceError::Unavailable)?
                    .take()
                    .ok_or(PageSourceError::Unavailable)?
            })
        }
    }

    struct StaticTransport {
        response: Mutex<Option<Result<Vec<u8>, DocumentAiTransportError>>>,
        request: Mutex<Option<(u32, String, usize)>>,
    }

    impl DocumentAiTransport for StaticTransport {
        fn process<'a>(&'a self, request: DocumentAiRequest) -> DocumentAiTransportFuture<'a> {
            Box::pin(async move {
                self.request.lock().unwrap().replace((
                    request.page(),
                    request.content_type().to_owned(),
                    request.byte_len(),
                ));
                self.response.lock().unwrap().take().unwrap()
            })
        }
    }

    fn task(page: u32) -> PageTask {
        PageTask {
            page,
            attempt: 1,
            activity_key: format!("ocr-job-job_MANAGED-page-{page}-attempt-1"),
        }
    }

    fn scope() -> (
        ocr_domain::ProductId,
        ocr_domain::TenantId,
        ocr_domain::JobId,
    ) {
        (
            ocr_domain::ProductId::new("kora").unwrap(),
            ocr_domain::TenantId::new("ten_MANAGED").unwrap(),
            ocr_domain::JobId::new("job_MANAGED").unwrap(),
        )
    }

    fn source() -> AcceptedPageSource {
        AcceptedPageSource::from_verified(
            vec![7; 16],
            "application/pdf".to_owned(),
            PageGeometry::new(PageNumber::new(2).unwrap(), 1_000, 1_400).unwrap(),
        )
    }

    fn recognizer(
        source: Result<AcceptedPageSource, PageSourceError>,
        response: Result<Vec<u8>, DocumentAiTransportError>,
    ) -> (DocumentAiPageRecognizer, Arc<StaticTransport>) {
        let transport = Arc::new(StaticTransport {
            response: Mutex::new(Some(response)),
            request: Mutex::new(None),
        });
        (
            DocumentAiPageRecognizer::new(
                Arc::new(StaticSource {
                    source: Mutex::new(Some(source)),
                }),
                transport.clone(),
            ),
            transport,
        )
    }

    #[tokio::test]
    async fn valid_provider_response_becomes_bounded_line_evidence() {
        let response = br#"{
            "document": {
                "text": "Example\n",
                "pages": [{
                    "pageNumber": 2,
                    "lines": [{
                        "layout": {
                            "textAnchor": {"textSegments": [{"endIndex": "7"}]},
                            "confidence": 0.98,
                            "boundingPoly": {"normalizedVertices": [{}, {"x": 1}, {"x": 1, "y": 1}, {"y": 1}]}
                        }
                    }]
                }]
            }
        }"#
        .to_vec();
        let (recognizer, transport) = recognizer(Ok(source()), Ok(response));

        let (product_id, tenant_id, job_id) = scope();
        let page = recognizer
            .recognize(&product_id, &tenant_id, &job_id, &task(2))
            .await
            .unwrap();

        assert_eq!(page.width, 1_000);
        assert_eq!(page.height, 1_400);
        assert_eq!(page.observations.len(), 1);
        assert_eq!(page.observations[0].text, "Example");
        assert_eq!(
            transport.request.lock().unwrap().as_ref(),
            Some(&(2, "application/pdf".to_owned(), 16))
        );
    }

    #[tokio::test]
    async fn malformed_or_wrong_page_provider_output_is_permanent() {
        let wrong_page =
            br#"{"document":{"text":"Example","pages":[{"pageNumber":1,"lines":[]}]}}"#.to_vec();
        let (wrong_page_recognizer, _) = recognizer(Ok(source()), Ok(wrong_page));
        let (product_id, tenant_id, job_id) = scope();
        assert_eq!(
            wrong_page_recognizer
                .recognize(&product_id, &tenant_id, &job_id, &task(2))
                .await,
            Err(PageRecognitionError::Permanent)
        );

        let malformed = br#"{"document":{"text":"Example","pages":[{"pageNumber":2,"lines":[{"layout":{"textAnchor":{"textSegments":[{"endIndex":"7"}]},"confidence":0.98,"boundingPoly":{"normalizedVertices":[]}}}]}]}}"#.to_vec();
        let (malformed_recognizer, _) = recognizer(Ok(source()), Ok(malformed));
        assert_eq!(
            malformed_recognizer
                .recognize(&product_id, &tenant_id, &job_id, &task(2))
                .await,
            Err(PageRecognitionError::Permanent)
        );
    }

    #[tokio::test]
    async fn unavailable_source_or_provider_is_retryable() {
        let (unavailable_source, _) = recognizer(Err(PageSourceError::Unavailable), Ok(Vec::new()));
        let (product_id, tenant_id, job_id) = scope();
        assert_eq!(
            unavailable_source
                .recognize(&product_id, &tenant_id, &job_id, &task(2))
                .await,
            Err(PageRecognitionError::Retryable)
        );

        let (unavailable_provider, _) =
            recognizer(Ok(source()), Err(DocumentAiTransportError::Unavailable));
        assert_eq!(
            unavailable_provider
                .recognize(&product_id, &tenant_id, &job_id, &task(2))
                .await,
            Err(PageRecognitionError::Retryable)
        );
    }

    #[test]
    fn configuration_cannot_override_the_fixed_google_endpoint_shape() {
        assert!(
            DocumentAiConfiguration::new("tesseracthub-480811", "asia-south1", "abc_123").is_ok()
        );
        assert!(DocumentAiConfiguration::new("project/path", "asia-south1", "processor").is_err());
        assert!(DocumentAiConfiguration::new("project", "asia-south1", "processor:other").is_err());
    }
}
