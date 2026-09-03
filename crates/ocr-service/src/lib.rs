//! HTTP boundary for the document intelligence service.

use std::time::Duration;

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use ocr_domain::{JobId, JobState, ProductId, TenantId};
use ocr_store::PgJobStore;
use serde::Serialize;
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
        .route("/v1/ocr/jobs/{job_id}", get(get_job))
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
