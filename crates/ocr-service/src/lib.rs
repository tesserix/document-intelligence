//! HTTP boundary for the document intelligence service.

mod document_finalizer;
mod document_reader;
mod importer;
mod job_status_cache;
mod malware;
mod outbox_relay;
mod page_artifacts;
mod page_processor;
mod page_runner;
mod parser_process;
mod result_artifacts;
mod result_assembly;
mod result_publisher;
mod source_promotion;
mod telemetry;
mod upload_artifacts;
mod upload_intents;
mod webhook;
mod webhook_relay;

pub use job_status_cache::{
    CacheConfigurationError, CacheOperationError, CachePolicy, CacheReadFuture, CacheRecord,
    CacheRecordError, CacheScope, CacheWriteFuture, JobStatusCache, UnavailableJobStatusCache,
    ValkeyCacheConfigurationError, ValkeyJobStatusCache,
};

pub use document_finalizer::{DocumentFinalizeError, DocumentFinalizer};
pub use page_artifacts::{
    GcsPageArtifactReader, GcsPageArtifactWriter, PageArtifactConfigurationError,
    PageArtifactReadError, PageArtifactReadFuture, PageArtifactReader, PageArtifactWriteError,
    PageArtifactWriteFuture, PageArtifactWriter,
};
pub use page_processor::{
    ArtifactPageProcessor, PageRecognitionError, PageRecognitionFuture, PageRecognizer,
};
pub use page_runner::{
    CheckpointedPageRunner, PageProcessError, PageProcessFuture, PageProcessor, PageRunnerError,
    PageRunnerOutcome,
};
pub use parser_process::{
    ParserInspectionReport, ParserProcess, ParserProcessError, PARSER_PROFILE, PARSER_VERSION,
};
pub use result_artifacts::{
    GcsResultReader, GcsResultWriter, ResultArtifactConfigurationError, ResultArtifactWriteError,
    ResultArtifactWriteFuture, ResultArtifactWriter,
};
pub use result_assembly::{assemble_document_result, ResultAssemblyError};
pub use result_publisher::{PublishResultError, ResultPublisher};
pub use telemetry::{TelemetryConfig, TelemetryConfigError, TelemetryInitError, TelemetryRuntime};
pub use upload_artifacts::{GcsUploadArtifactReader, UploadArtifactConfigurationError};
pub use upload_intents::{GcsUploadIssuer, UploadIntentConfigurationError};
pub use webhook::{
    SignedWebhook, TerminalWebhookEvent, WebhookSignError, WebhookSigner, WebhookSigningSecret,
};
pub use webhook_relay::{
    WebhookOutboxRelay, WebhookPublishError, WebhookPublishFuture, WebhookPublishOutcome,
    WebhookPublisher, WebhookRelayError, WebhookRelayOutcome,
};

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::{rejection::JsonRejection, FromRequestParts, Path, State},
    http::{request::Parts, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ocr_domain::{
    DocumentResult, IdempotencyKey, JobId, JobState, ProductId, RequestDigest, TenantId, UploadId,
    WebhookSubscriptionId,
};
pub use ocr_store::StoredUpload;
use ocr_store::{
    CancelOutcome, CreateJob, CreateOutcome, CreateUpload, CreateUploadOutcome, PgJobStore,
    RecordUpload, RecordUploadOutcome, ResultLookup, StoredResultLocator, UploadState,
};
use opentelemetry::{global, propagation::Extractor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_http::{
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{field, info_span, warn, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

const REQUEST_ID_HEADER: &str = "x-request-id";
const MAXIMUM_RESULT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAXIMUM_UPLOAD_BYTES: i64 = 100 * 1024 * 1024;
const UPLOAD_INTENT_TTL_MINUTES: i64 = 10;

struct RequestHeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for RequestHeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}

#[derive(Debug)]
pub struct ResultArtifactError;

pub type ResultReadFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<Vec<u8>, ResultArtifactError>> + Send + 'a>>;

pub trait ResultArtifactReader: Send + Sync {
    fn read<'a>(
        &'a self,
        locator: &'a StoredResultLocator,
        maximum_bytes: usize,
    ) -> ResultReadFuture<'a>;
}

#[derive(Debug)]
pub struct UploadIssueError;

#[derive(Debug, Serialize)]
pub struct IssuedUpload {
    pub upload_url: String,
    pub required_headers: BTreeMap<String, String>,
}

pub type UploadIssueFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<IssuedUpload, UploadIssueError>> + Send + 'a>>;

pub trait UploadIntentIssuer: Send + Sync {
    fn issue<'a>(&'a self, upload: &'a StoredUpload) -> UploadIssueFuture<'a>;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum UploadArtifactError {
    NotFound,
    Invalid,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedUploadArtifact {
    pub object_generation: i64,
    pub content_type: String,
    pub content_length: i64,
    pub digest: String,
}

pub type UploadArtifactReadFuture<'a> = Pin<
    Box<
        dyn Future<Output = std::result::Result<VerifiedUploadArtifact, UploadArtifactError>>
            + Send
            + 'a,
    >,
>;

pub trait UploadArtifactReader: Send + Sync {
    fn verify<'a>(&'a self, upload: &'a StoredUpload) -> UploadArtifactReadFuture<'a>;
}

struct UnavailableUploadArtifactReader;

impl UploadArtifactReader for UnavailableUploadArtifactReader {
    fn verify<'a>(&'a self, _upload: &'a StoredUpload) -> UploadArtifactReadFuture<'a> {
        Box::pin(async { Err(UploadArtifactError::Unavailable) })
    }
}

struct UnavailableUploadIssuer;

impl UploadIntentIssuer for UnavailableUploadIssuer {
    fn issue<'a>(&'a self, _upload: &'a StoredUpload) -> UploadIssueFuture<'a> {
        Box::pin(async { Err(UploadIssueError) })
    }
}

struct UnavailableResultReader;

impl ResultArtifactReader for UnavailableResultReader {
    fn read<'a>(
        &'a self,
        _locator: &'a StoredResultLocator,
        _maximum_bytes: usize,
    ) -> ResultReadFuture<'a> {
        Box::pin(async { Err(ResultArtifactError) })
    }
}

#[derive(Clone)]
struct AppState {
    jobs: PgJobStore,
    job_status_cache: Arc<dyn JobStatusCache>,
    job_status_cache_policy: CachePolicy,
    results: Arc<dyn ResultArtifactReader>,
    result_artifacts_configured: bool,
    uploads: Arc<dyn UploadIntentIssuer>,
    upload_buckets: HashMap<String, String>,
    upload_intents_configured: bool,
    upload_artifacts: Arc<dyn UploadArtifactReader>,
    upload_artifacts_configured: bool,
}

#[derive(Debug, Clone)]
pub struct TrustedIdentity {
    product_id: ProductId,
    tenant_id: TenantId,
}

impl TrustedIdentity {
    pub fn new(product_id: &str, tenant_id: &str) -> ocr_domain::Result<Self> {
        Ok(Self {
            product_id: ProductId::new(product_id)?,
            tenant_id: TenantId::new(tenant_id)?,
        })
    }
}

struct VerifiedIdentity(TrustedIdentity);

impl<S> FromRequestParts<S> for VerifiedIdentity
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let identity = parts
            .extensions
            .get::<TrustedIdentity>()
            .cloned()
            .map(Self)
            .ok_or_else(|| ApiError::authentication_required(request_id(parts)))?;
        Span::current().record("product.id", identity.0.product_id.as_str());
        Span::current().record("tenant.id", identity.0.tenant_id.as_str());
        Ok(identity)
    }
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    fn authentication_required(request_id: String) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: ErrorBody {
                code: "authentication_required",
                message: "authentication is required",
                request_id,
            },
        }
    }

    fn not_found(request_id: String) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: ErrorBody {
                code: "job_not_found",
                message: "job was not found",
                request_id,
            },
        }
    }

    fn unavailable(request_id: String) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: ErrorBody {
                code: "service_unavailable",
                message: "service is temporarily unavailable",
                request_id,
            },
        }
    }

    fn bad_request(code: &'static str, message: &'static str, request_id: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: ErrorBody {
                code,
                message,
                request_id,
            },
        }
    }

    fn conflict(request_id: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: ErrorBody {
                code: "idempotency_conflict",
                message: "idempotency key was reused with different input",
                request_id,
            },
        }
    }

    fn not_cancellable(request_id: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: ErrorBody {
                code: "job_not_cancellable",
                message: "job cannot be cancelled in its current state",
                request_id,
            },
        }
    }

    fn result_not_ready(request_id: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: ErrorBody {
                code: "result_not_ready",
                message: "job result is not ready",
                request_id,
            },
        }
    }

    fn result_unavailable(request_id: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: ErrorBody {
                code: "result_unavailable",
                message: "job has no result",
                request_id,
            },
        }
    }

    fn upload_not_found(request_id: String) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: ErrorBody {
                code: "upload_not_found",
                message: "upload was not found",
                request_id,
            },
        }
    }

    fn upload_conflict(code: &'static str, message: &'static str, request_id: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: ErrorBody {
                code,
                message,
                request_id,
            },
        }
    }

    fn upload_verification_failed(request_id: String) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: ErrorBody {
                code: "upload_verification_failed",
                message: "uploaded object did not match its reservation",
                request_id,
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

#[derive(Debug, Serialize)]
struct JobResponse {
    job_id: JobId,
    status: JobState,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateJobRequest {
    source: Source,
    document_type: DocumentType,
    #[serde(default)]
    output: Option<OutputOptions>,
    #[serde(default)]
    extraction: Option<Extraction>,
    #[serde(default)]
    language_hints: Vec<String>,
    #[serde(default)]
    processing_class: Option<ProcessingClass>,
    #[serde(default)]
    webhook_subscription_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUploadRequest {
    content_type: String,
    content_length: i64,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct UploadIntentResponse {
    upload_id: UploadId,
    method: &'static str,
    upload_url: String,
    required_headers: BTreeMap<String, String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: time::OffsetDateTime,
}

#[derive(Debug, Serialize)]
struct UploadCompletionResponse {
    upload_id: UploadId,
    status: &'static str,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    upload_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DocumentType {
    Auto,
    General,
    Invoice,
    Receipt,
    PurchaseOrder,
    IdentityDocument,
    Contract,
    BankStatement,
    MedicalForm,
    ApplicationForm,
    Resume,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputOptions {
    text: bool,
    markdown: bool,
    layout: bool,
    evidence: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Extraction {
    schema_id: String,
    schema_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessingClass {
    Interactive,
    Priority,
    Batch,
}

#[derive(Debug, Clone)]
struct RequestIdFactory;

impl MakeRequestId for RequestIdFactory {
    fn make_request_id<B>(&mut self, _request: &axum::http::Request<B>) -> Option<RequestId> {
        HeaderValue::from_str(&Uuid::new_v4().to_string())
            .ok()
            .map(RequestId::new)
    }
}

pub fn router(store: PgJobStore) -> Router {
    build_router(
        store,
        RouterDependencies {
            job_status_cache: Arc::new(UnavailableJobStatusCache),
            job_status_cache_policy: default_job_status_cache_policy(),
            results: Arc::new(UnavailableResultReader),
            result_artifacts_configured: false,
            uploads: Arc::new(UnavailableUploadIssuer),
            upload_buckets: HashMap::new(),
            upload_intents_configured: false,
            upload_artifacts: Arc::new(UnavailableUploadArtifactReader),
            upload_artifacts_configured: false,
        },
    )
}

pub fn router_with_result_reader(
    store: PgJobStore,
    results: Arc<dyn ResultArtifactReader>,
) -> Router {
    build_router(
        store,
        RouterDependencies {
            job_status_cache: Arc::new(UnavailableJobStatusCache),
            job_status_cache_policy: default_job_status_cache_policy(),
            results,
            result_artifacts_configured: true,
            uploads: Arc::new(UnavailableUploadIssuer),
            upload_buckets: HashMap::new(),
            upload_intents_configured: false,
            upload_artifacts: Arc::new(UnavailableUploadArtifactReader),
            upload_artifacts_configured: false,
        },
    )
}

pub fn router_with_upload_issuer(
    store: PgJobStore,
    upload_buckets: HashMap<String, String>,
    uploads: Arc<dyn UploadIntentIssuer>,
) -> Router {
    build_router(
        store,
        RouterDependencies {
            job_status_cache: Arc::new(UnavailableJobStatusCache),
            job_status_cache_policy: default_job_status_cache_policy(),
            results: Arc::new(UnavailableResultReader),
            result_artifacts_configured: false,
            uploads,
            upload_buckets,
            upload_intents_configured: true,
            upload_artifacts: Arc::new(UnavailableUploadArtifactReader),
            upload_artifacts_configured: false,
        },
    )
}

pub fn router_with_upload_services(
    store: PgJobStore,
    upload_buckets: HashMap<String, String>,
    uploads: Arc<dyn UploadIntentIssuer>,
    upload_artifacts: Arc<dyn UploadArtifactReader>,
) -> Router {
    build_router(
        store,
        RouterDependencies {
            job_status_cache: Arc::new(UnavailableJobStatusCache),
            job_status_cache_policy: default_job_status_cache_policy(),
            results: Arc::new(UnavailableResultReader),
            result_artifacts_configured: false,
            uploads,
            upload_buckets,
            upload_intents_configured: true,
            upload_artifacts,
            upload_artifacts_configured: true,
        },
    )
}

pub fn router_with_dependencies(
    store: PgJobStore,
    results: Arc<dyn ResultArtifactReader>,
    upload_buckets: HashMap<String, String>,
    uploads: Arc<dyn UploadIntentIssuer>,
    upload_artifacts: Arc<dyn UploadArtifactReader>,
) -> Router {
    build_router(
        store,
        RouterDependencies {
            job_status_cache: Arc::new(UnavailableJobStatusCache),
            job_status_cache_policy: default_job_status_cache_policy(),
            results,
            result_artifacts_configured: true,
            uploads,
            upload_buckets,
            upload_intents_configured: true,
            upload_artifacts,
            upload_artifacts_configured: true,
        },
    )
}

pub fn router_with_job_status_cache(
    store: PgJobStore,
    job_status_cache: Arc<dyn JobStatusCache>,
    job_status_cache_policy: CachePolicy,
) -> Router {
    build_router(
        store,
        RouterDependencies {
            job_status_cache,
            job_status_cache_policy,
            results: Arc::new(UnavailableResultReader),
            result_artifacts_configured: false,
            uploads: Arc::new(UnavailableUploadIssuer),
            upload_buckets: HashMap::new(),
            upload_intents_configured: false,
            upload_artifacts: Arc::new(UnavailableUploadArtifactReader),
            upload_artifacts_configured: false,
        },
    )
}

pub fn router_with_runtime_dependencies(
    store: PgJobStore,
    results: Option<Arc<dyn ResultArtifactReader>>,
    upload_buckets: HashMap<String, String>,
    uploads: Option<Arc<dyn UploadIntentIssuer>>,
    upload_artifacts: Option<Arc<dyn UploadArtifactReader>>,
    job_status_cache: Option<(Arc<dyn JobStatusCache>, CachePolicy)>,
) -> Router {
    let result_artifacts_configured = results.is_some();
    let upload_intents_configured = uploads.is_some();
    let upload_artifacts_configured = upload_artifacts.is_some();
    let (job_status_cache, job_status_cache_policy) = job_status_cache.unwrap_or_else(|| {
        (
            Arc::new(UnavailableJobStatusCache),
            default_job_status_cache_policy(),
        )
    });
    build_router(
        store,
        RouterDependencies {
            job_status_cache,
            job_status_cache_policy,
            results: results.unwrap_or_else(|| Arc::new(UnavailableResultReader)),
            result_artifacts_configured,
            uploads: uploads.unwrap_or_else(|| Arc::new(UnavailableUploadIssuer)),
            upload_buckets,
            upload_intents_configured,
            upload_artifacts: upload_artifacts
                .unwrap_or_else(|| Arc::new(UnavailableUploadArtifactReader)),
            upload_artifacts_configured,
        },
    )
}

fn default_job_status_cache_policy() -> CachePolicy {
    CachePolicy::new(
        Duration::from_secs(10),
        Duration::from_secs(300),
        Duration::from_millis(25),
        512,
    )
    .expect("default job status cache policy is valid")
}

struct RouterDependencies {
    job_status_cache: Arc<dyn JobStatusCache>,
    job_status_cache_policy: CachePolicy,
    results: Arc<dyn ResultArtifactReader>,
    result_artifacts_configured: bool,
    uploads: Arc<dyn UploadIntentIssuer>,
    upload_buckets: HashMap<String, String>,
    upload_intents_configured: bool,
    upload_artifacts: Arc<dyn UploadArtifactReader>,
    upload_artifacts_configured: bool,
}

fn build_router(store: PgJobStore, dependencies: RouterDependencies) -> Router {
    let request_id_header = HeaderName::from_static(REQUEST_ID_HEADER);
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(readiness))
        .route("/v1/ocr/uploads", post(create_upload))
        .route(
            "/v1/ocr/uploads/{upload_id}/complete",
            post(complete_upload),
        )
        .route("/v1/ocr/jobs", post(create_job))
        .route("/v1/ocr/jobs/{job_id}", get(get_job))
        .route("/v1/ocr/jobs/{job_id}/result", get(get_result))
        .route("/v1/ocr/jobs/{job_id}/cancel", post(cancel_job))
        .with_state(AppState {
            jobs: store,
            job_status_cache: dependencies.job_status_cache,
            job_status_cache_policy: dependencies.job_status_cache_policy,
            results: dependencies.results,
            result_artifacts_configured: dependencies.result_artifacts_configured,
            uploads: dependencies.uploads,
            upload_buckets: dependencies.upload_buckets,
            upload_intents_configured: dependencies.upload_intents_configured,
            upload_artifacts: dependencies.upload_artifacts,
            upload_artifacts_configured: dependencies.upload_artifacts_configured,
        })
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::extract::Request| {
                    let request_id = request
                        .headers()
                        .get(REQUEST_ID_HEADER)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("missing");
                    let span = info_span!(
                        "document.api.request",
                        http.request.method = %request.method(),
                        http.response.status_code = field::Empty,
                        duration_ms = field::Empty,
                        request.id = request_id,
                        product.id = field::Empty,
                        tenant.id = field::Empty,
                    );
                    let parent = global::get_text_map_propagator(|propagator| {
                        propagator.extract(&RequestHeaderExtractor(request.headers()))
                    });
                    let _parent_attached = span.set_parent(parent);
                    span
                })
                .on_response(
                    |response: &axum::response::Response, latency: Duration, span: &Span| {
                        span.record("http.response.status_code", response.status().as_u16());
                        span.record("duration_ms", latency.as_millis() as u64);
                        tracing::info!(
                            parent: span,
                            http.response.status_code = response.status().as_u16(),
                            duration_ms = latency.as_millis() as u64,
                            "request completed"
                        );
                    },
                ),
        )
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(10),
        ))
        .layer(RequestBodyLimitLayer::new(64 * 1024))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, RequestIdFactory))
}

async fn readiness(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Result<StatusCode, ApiError> {
    let request_id = request_id_from_extensions(request.extensions());
    if !state.result_artifacts_configured
        || !state.upload_intents_configured
        || !state.upload_artifacts_configured
    {
        return Err(ApiError::unavailable(request_id));
    }
    state
        .jobs
        .ready()
        .await
        .map_err(|_| ApiError::unavailable(request_id))?;
    Ok(StatusCode::OK)
}

async fn complete_upload(
    State(state): State<AppState>,
    identity: VerifiedIdentity,
    Path(upload_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<UploadCompletionResponse>, ApiError> {
    let request_id = request_id_from_extensions(request.extensions());
    let upload_id =
        UploadId::new(&upload_id).map_err(|_| ApiError::upload_not_found(request_id.clone()))?;
    let upload = state
        .jobs
        .find_upload(&identity.0.tenant_id, &identity.0.product_id, &upload_id)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?
        .ok_or_else(|| ApiError::upload_not_found(request_id.clone()))?;
    match upload.state {
        UploadState::Uploaded => {
            return Ok(Json(UploadCompletionResponse {
                upload_id,
                status: "uploaded",
            }));
        }
        UploadState::Reserved if upload.expires_at <= time::OffsetDateTime::now_utc() => {
            return Err(ApiError::upload_conflict(
                "upload_expired",
                "upload reservation has expired",
                request_id,
            ));
        }
        UploadState::Reserved => {}
        _ => {
            return Err(ApiError::upload_conflict(
                "upload_not_completable",
                "upload cannot be completed in its current state",
                request_id,
            ));
        }
    }
    let artifact = state
        .upload_artifacts
        .verify(&upload)
        .await
        .map_err(|error| match error {
            UploadArtifactError::NotFound => ApiError::upload_conflict(
                "upload_not_ready",
                "uploaded object is not available",
                request_id.clone(),
            ),
            UploadArtifactError::Invalid => {
                ApiError::upload_verification_failed(request_id.clone())
            }
            UploadArtifactError::Unavailable => ApiError::unavailable(request_id.clone()),
        })?;
    let outcome = state
        .jobs
        .record_uploaded(
            &identity.0.tenant_id,
            &identity.0.product_id,
            &upload_id,
            RecordUpload {
                object_generation: artifact.object_generation,
                verified_content_type: artifact.content_type,
                verified_content_length: artifact.content_length,
                verified_digest: artifact.digest,
            },
        )
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    match outcome {
        RecordUploadOutcome::Recorded(_) | RecordUploadOutcome::Existing(_) => {
            Ok(Json(UploadCompletionResponse {
                upload_id,
                status: "uploaded",
            }))
        }
        RecordUploadOutcome::Expired => Err(ApiError::upload_conflict(
            "upload_expired",
            "upload reservation has expired",
            request_id,
        )),
        RecordUploadOutcome::VerificationMismatch => {
            Err(ApiError::upload_verification_failed(request_id))
        }
        RecordUploadOutcome::NotRecordable => Err(ApiError::upload_conflict(
            "upload_not_completable",
            "upload cannot be completed in its current state",
            request_id,
        )),
        RecordUploadOutcome::NotFound => Err(ApiError::upload_not_found(request_id)),
    }
}

async fn create_upload(
    State(state): State<AppState>,
    identity: VerifiedIdentity,
    headers: HeaderMap,
    payload: Result<Json<CreateUploadRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<UploadIntentResponse>), ApiError> {
    let request_id = request_id_from_headers(&headers);
    let idempotency_value = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "idempotency_key_required",
                "Idempotency-Key is required",
                request_id.clone(),
            )
        })?;
    let idempotency_key = IdempotencyKey::new(idempotency_value).map_err(|_| {
        ApiError::bad_request(
            "invalid_idempotency_key",
            "Idempotency-Key is invalid",
            request_id.clone(),
        )
    })?;
    let command = payload.map_err(|_| invalid_upload_request(&request_id))?.0;
    if !is_supported_content_type(&command.content_type)
        || !(1..=MAXIMUM_UPLOAD_BYTES).contains(&command.content_length)
        || RequestDigest::new(&command.sha256).is_err()
    {
        return Err(invalid_upload_request(&request_id));
    }
    let bucket = state
        .upload_buckets
        .get(identity.0.product_id.as_str())
        .ok_or_else(|| ApiError::unavailable(request_id.clone()))?
        .clone();
    let canonical =
        serde_json::to_vec(&command).map_err(|_| invalid_upload_request(&request_id))?;
    let request_digest = RequestDigest::new(&format!("sha256:{:x}", Sha256::digest(canonical)))
        .map_err(|_| invalid_upload_request(&request_id))?;
    let upload_id = UploadId::new(&format!("upl_{}", Uuid::new_v4().simple()))
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    let object_name = format!(
        "products/{}/tenants/{}/quarantine/{}",
        identity.0.product_id.as_str(),
        identity.0.tenant_id.as_str(),
        upload_id.as_str()
    );
    let expires_at =
        time::OffsetDateTime::now_utc() + time::Duration::minutes(UPLOAD_INTENT_TTL_MINUTES);
    let outcome = state
        .jobs
        .create_upload(CreateUpload {
            upload_id,
            tenant_id: identity.0.tenant_id,
            product_id: identity.0.product_id,
            idempotency_key,
            request_digest,
            object_bucket: bucket,
            object_name,
            expected_content_type: command.content_type,
            expected_content_length: command.content_length,
            expected_digest: command.sha256,
            expires_at,
        })
        .await
        .map_err(|error| match error {
            ocr_store::Error::IdempotencyConflict => ApiError::conflict(request_id.clone()),
            _ => ApiError::unavailable(request_id.clone()),
        })?;
    let (status, upload) = match outcome {
        CreateUploadOutcome::Created(upload) => (StatusCode::CREATED, upload),
        CreateUploadOutcome::Existing(upload) => (StatusCode::OK, upload),
    };
    let issued = state
        .uploads
        .issue(&upload)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    validate_issued_upload(&issued, &upload)
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    Ok((
        status,
        Json(UploadIntentResponse {
            upload_id: upload.upload_id,
            method: "PUT",
            upload_url: issued.upload_url,
            required_headers: issued.required_headers,
            expires_at: upload.expires_at,
        }),
    ))
}

fn invalid_upload_request(request_id: &str) -> ApiError {
    ApiError::bad_request(
        "invalid_upload_request",
        "upload request is invalid",
        request_id.to_owned(),
    )
}

fn is_supported_content_type(value: &str) -> bool {
    matches!(
        value,
        "application/pdf" | "image/jpeg" | "image/png" | "image/tiff" | "image/webp"
    )
}

fn validate_issued_upload(
    issued: &IssuedUpload,
    upload: &StoredUpload,
) -> std::result::Result<(), UploadIssueError> {
    let uri: Uri = issued.upload_url.parse().map_err(|_| UploadIssueError)?;
    let valid_url = uri.scheme_str() == Some("https") && uri.authority().is_some();
    let expected_headers = BTreeMap::from([
        (
            "content-type".to_owned(),
            upload.expected_content_type.clone(),
        ),
        ("x-goog-if-generation-match".to_owned(), "0".to_owned()),
    ]);
    if !valid_url || issued.required_headers != expected_headers {
        return Err(UploadIssueError);
    }
    Ok(())
}

async fn create_job(
    State(state): State<AppState>,
    identity: VerifiedIdentity,
    headers: HeaderMap,
    payload: Result<Json<CreateJobRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<JobResponse>), ApiError> {
    let request_id = request_id_from_headers(&headers);
    let idempotency_value = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "idempotency_key_required",
                "Idempotency-Key is required",
                request_id.clone(),
            )
        })?;
    let idempotency_key = IdempotencyKey::new(idempotency_value).map_err(|_| {
        ApiError::bad_request(
            "invalid_idempotency_key",
            "Idempotency-Key is invalid",
            request_id.clone(),
        )
    })?;
    let command = payload.map_err(|_| {
        ApiError::bad_request(
            "invalid_request",
            "request body is invalid",
            request_id.clone(),
        )
    })?;
    let upload_id = UploadId::new(&command.source.upload_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_request",
            "request body is invalid",
            request_id.clone(),
        )
    })?;
    let webhook_subscription_id = command
        .webhook_subscription_id
        .as_deref()
        .map(WebhookSubscriptionId::new)
        .transpose()
        .map_err(|_| {
            ApiError::bad_request(
                "invalid_webhook_subscription_id",
                "webhook subscription id is invalid",
                request_id.clone(),
            )
        })?;

    let canonical = serde_json::to_vec(&command.0).map_err(|_| {
        ApiError::bad_request(
            "invalid_request",
            "request body is invalid",
            request_id.clone(),
        )
    })?;
    let request_digest = RequestDigest::new(&format!("sha256:{:x}", Sha256::digest(canonical)))
        .map_err(|_| {
            ApiError::bad_request(
                "invalid_request",
                "request body is invalid",
                request_id.clone(),
            )
        })?;
    let generated_id = JobId::new(&format!("job_{}", Uuid::new_v4().simple()))
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    let outcome = state
        .jobs
        .create(CreateJob {
            job_id: generated_id.clone(),
            tenant_id: identity.0.tenant_id.clone(),
            product_id: identity.0.product_id.clone(),
            idempotency_key,
            request_digest,
            upload_id,
            webhook_subscription_id,
        })
        .await
        .map_err(|error| match error {
            ocr_store::Error::IdempotencyConflict => ApiError::conflict(request_id.clone()),
            ocr_store::Error::UploadSourceUnavailable => {
                ApiError::upload_not_found(request_id.clone())
            }
            _ => ApiError::unavailable(request_id.clone()),
        })?;
    let job = match outcome {
        CreateOutcome::Created(job) | CreateOutcome::Existing(job) => job,
    };
    let status_url = format!("/v1/ocr/jobs/{}", job.job_id.as_str());
    let result_url = format!("{status_url}/result");
    cache_job_status(
        &state,
        CacheScope::new(
            identity.0.product_id,
            identity.0.tenant_id,
            job.job_id.clone(),
        ),
        &job,
    )
    .await;

    Ok((
        StatusCode::ACCEPTED,
        Json(JobResponse {
            job_id: job.job_id,
            status: job.state,
            created_at: job.created_at,
            status_url: Some(status_url),
            result_url: Some(result_url),
        }),
    ))
}

async fn get_job(
    State(state): State<AppState>,
    identity: VerifiedIdentity,
    Path(job_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<JobResponse>, ApiError> {
    let request_id = request_id_from_extensions(request.extensions());
    let job_id = JobId::new(&job_id).map_err(|_| ApiError::not_found(request_id.clone()))?;
    let scope = CacheScope::new(
        identity.0.product_id.clone(),
        identity.0.tenant_id.clone(),
        job_id.clone(),
    );
    if let Some(record) = read_cached_job_status(&state, &scope).await {
        return Ok(Json(JobResponse {
            job_id,
            status: record.status(),
            created_at: record.created_at(),
            status_url: None,
            result_url: None,
        }));
    }
    let job = state
        .jobs
        .find(&identity.0.tenant_id, &identity.0.product_id, &job_id)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?
        .ok_or_else(|| ApiError::not_found(request_id))?;
    cache_job_status(&state, scope, &job).await;
    Ok(Json(JobResponse {
        job_id: job.job_id,
        status: job.state,
        created_at: job.created_at,
        status_url: None,
        result_url: None,
    }))
}

async fn get_result(
    State(state): State<AppState>,
    identity: VerifiedIdentity,
    Path(job_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<DocumentResult>, ApiError> {
    let request_id = request_id_from_extensions(request.extensions());
    let job_id = JobId::new(&job_id).map_err(|_| ApiError::not_found(request_id.clone()))?;
    let locator = match state
        .jobs
        .find_result(&identity.0.tenant_id, &identity.0.product_id, &job_id)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?
    {
        ResultLookup::Ready(locator) => locator,
        ResultLookup::NotReady(_) => return Err(ApiError::result_not_ready(request_id)),
        ResultLookup::Unavailable(_) => return Err(ApiError::result_unavailable(request_id)),
        ResultLookup::NotFound => return Err(ApiError::not_found(request_id)),
    };
    let expected_length = usize::try_from(locator.content_length)
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    if expected_length > MAXIMUM_RESULT_BYTES {
        return Err(ApiError::unavailable(request_id));
    }
    let bytes = state
        .results
        .read(&locator, MAXIMUM_RESULT_BYTES)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    if bytes.len() != expected_length || digest != locator.object_digest {
        return Err(ApiError::unavailable(request_id));
    }
    let result: DocumentResult =
        serde_json::from_slice(&bytes).map_err(|_| ApiError::unavailable(request_id.clone()))?;
    if result.document_id != locator.document_id
        || result.document_version != locator.document_version
    {
        return Err(ApiError::unavailable(request_id));
    }
    Ok(Json(result))
}

async fn cancel_job(
    State(state): State<AppState>,
    identity: VerifiedIdentity,
    Path(job_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<JobResponse>, ApiError> {
    let request_id = request_id_from_extensions(request.extensions());
    let job_id = JobId::new(&job_id).map_err(|_| ApiError::not_found(request_id.clone()))?;
    let outcome = state
        .jobs
        .cancel(&identity.0.tenant_id, &identity.0.product_id, &job_id)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    let job = match outcome {
        CancelOutcome::Requested(job) | CancelOutcome::Existing(job) => job,
        CancelOutcome::NotCancellable(_) => return Err(ApiError::not_cancellable(request_id)),
        CancelOutcome::NotFound => return Err(ApiError::not_found(request_id)),
    };
    cache_job_status(
        &state,
        CacheScope::new(
            identity.0.product_id,
            identity.0.tenant_id,
            job.job_id.clone(),
        ),
        &job,
    )
    .await;
    Ok(Json(JobResponse {
        job_id: job.job_id,
        status: job.state,
        created_at: job.created_at,
        status_url: None,
        result_url: None,
    }))
}

async fn cache_job_status(state: &AppState, scope: CacheScope, job: &ocr_store::StoredJob) {
    let record = CacheRecord::new(&scope, job.state, job.created_at);
    let ttl = state.job_status_cache_policy.ttl(job.state);
    match tokio::time::timeout(
        state.job_status_cache_policy.timeout(),
        state.job_status_cache.put(&scope, &record, ttl),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => warn!(cache.operation = "write", "job status cache unavailable"),
        Err(_) => warn!(cache.operation = "write", "job status cache timed out"),
    }
}

async fn read_cached_job_status(state: &AppState, scope: &CacheScope) -> Option<CacheRecord> {
    match tokio::time::timeout(
        state.job_status_cache_policy.timeout(),
        state.job_status_cache.get(scope),
    )
    .await
    {
        Ok(Ok(Some(record))) if record.matches_scope(scope) => Some(record),
        Ok(Ok(Some(_))) => {
            warn!(cache.operation = "read", "job status cache scope mismatch");
            None
        }
        Ok(Ok(None)) => None,
        Ok(Err(_)) => {
            warn!(cache.operation = "read", "job status cache unavailable");
            None
        }
        Err(_) => {
            warn!(cache.operation = "read", "job status cache timed out");
            None
        }
    }
}

fn request_id(parts: &Parts) -> String {
    request_id_from_extensions(&parts.extensions)
}

fn request_id_from_extensions(extensions: &axum::http::Extensions) -> String {
    extensions
        .get::<RequestId>()
        .and_then(|request_id| request_id.header_value().to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn request_id_from_headers(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}
pub use document_reader::{GcsDocumentReaderConfigurationError, GcsUploadDocumentReader};
pub use importer::{
    DocumentParseError, DocumentParser, DocumentReadError, ImportError, ImportOutcome, Importer,
    MalwareInspector, SourcePromoter, UploadDocumentReader,
};
pub use malware::{
    ClamdScanner, GcsUploadMalwareInspector, MalwareScanError, MalwareScanOutcome,
    UploadInspectionError, UploadInspectorConfigurationError,
};
pub use outbox_relay::{
    DurableWorkflowConfigurationError, DurableWorkflowStarter, JobOutboxRelay, RelayError,
    RelayOutcome, WorkflowAction, WorkflowDispatch, WorkflowDispatchError, WorkflowDispatchOutcome,
    WorkflowStarter,
};
pub use source_promotion::{GcsSourcePromoter, PromotedSource, SourcePromotionError};
