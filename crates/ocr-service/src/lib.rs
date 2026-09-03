//! HTTP boundary for the document intelligence service.

use std::time::Duration;

use axum::{
    extract::{rejection::JsonRejection, FromRequestParts, Path, State},
    http::{request::Parts, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ocr_domain::{IdempotencyKey, JobId, JobState, ProductId, RequestDigest, TenantId};
use ocr_store::{CancelOutcome, CreateJob, CreateOutcome, PgJobStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_http::{
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use uuid::Uuid;

const REQUEST_ID_HEADER: &str = "x-request-id";

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
        parts
            .extensions
            .get::<TrustedIdentity>()
            .cloned()
            .map(Self)
            .ok_or_else(|| ApiError::authentication_required(request_id(parts)))
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
    let request_id_header = HeaderName::from_static(REQUEST_ID_HEADER);
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/v1/ocr/jobs", post(create_job))
        .route("/v1/ocr/jobs/{job_id}", get(get_job))
        .route("/v1/ocr/jobs/{job_id}/cancel", post(cancel_job))
        .with_state(store)
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(10),
        ))
        .layer(RequestBodyLimitLayer::new(64 * 1024))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, RequestIdFactory))
}

async fn create_job(
    State(store): State<PgJobStore>,
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
    if !is_upload_id(&command.source.upload_id) {
        return Err(ApiError::bad_request(
            "invalid_request",
            "request body is invalid",
            request_id,
        ));
    }

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
    let outcome = store
        .create(CreateJob {
            job_id: generated_id.clone(),
            tenant_id: identity.0.tenant_id,
            product_id: identity.0.product_id,
            idempotency_key,
            request_digest,
        })
        .await
        .map_err(|error| match error {
            ocr_store::Error::IdempotencyConflict => ApiError::conflict(request_id.clone()),
            _ => ApiError::unavailable(request_id.clone()),
        })?;
    let job = match outcome {
        CreateOutcome::Created(job) | CreateOutcome::Existing(job) => job,
    };
    let status_url = format!("/v1/ocr/jobs/{}", job.job_id.as_str());
    let result_url = format!("{status_url}/result");

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
    State(store): State<PgJobStore>,
    identity: VerifiedIdentity,
    Path(job_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<JobResponse>, ApiError> {
    let request_id = request_id_from_extensions(request.extensions());
    let job_id = JobId::new(&job_id).map_err(|_| ApiError::not_found(request_id.clone()))?;
    let job = store
        .find(&identity.0.tenant_id, &identity.0.product_id, &job_id)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?
        .ok_or_else(|| ApiError::not_found(request_id))?;
    Ok(Json(JobResponse {
        job_id: job.job_id,
        status: job.state,
        created_at: job.created_at,
        status_url: None,
        result_url: None,
    }))
}

async fn cancel_job(
    State(store): State<PgJobStore>,
    identity: VerifiedIdentity,
    Path(job_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<JobResponse>, ApiError> {
    let request_id = request_id_from_extensions(request.extensions());
    let job_id = JobId::new(&job_id).map_err(|_| ApiError::not_found(request_id.clone()))?;
    let outcome = store
        .cancel(&identity.0.tenant_id, &identity.0.product_id, &job_id)
        .await
        .map_err(|_| ApiError::unavailable(request_id.clone()))?;
    let job = match outcome {
        CancelOutcome::Requested(job) | CancelOutcome::Existing(job) => job,
        CancelOutcome::NotCancellable(_) => return Err(ApiError::not_cancellable(request_id)),
        CancelOutcome::NotFound => return Err(ApiError::not_found(request_id)),
    };
    Ok(Json(JobResponse {
        job_id: job.job_id,
        status: job.state,
        created_at: job.created_at,
        status_url: None,
        result_url: None,
    }))
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

fn is_upload_id(value: &str) -> bool {
    let suffix = value.strip_prefix("upl_").unwrap_or_default();
    !suffix.is_empty()
        && suffix.len() <= 64
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
