use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid sandbox configuration")]
    Configuration,
    #[error("sandbox dependency unavailable")]
    Unavailable,
    #[error("invalid request")]
    Invalid,
    #[error("session expired or invalid")]
    Unauthorized,
    #[error("object not found")]
    NotFound,
    #[error("sandbox quota exceeded")]
    Limited,
    #[error("request conflicts with session state")]
    Conflict,
    #[error("upstream request rejected")]
    Upstream(StatusCode),
}

impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid => (
                StatusCode::BAD_REQUEST,
                "InvalidRequest",
                "Check the file and request fields.",
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "SessionExpired",
                "Start a new sandbox session.",
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "NotFound",
                "This object is not available in your session.",
            ),
            Self::Limited => (
                StatusCode::TOO_MANY_REQUESTS,
                "SandboxLimit",
                "The sandbox limit has been reached. Try again later.",
            ),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "SessionConflict",
                "This session already has an image or a request in progress.",
            ),
            Self::Upstream(status) if status.is_client_error() => (
                status,
                "OcrRequestRejected",
                "The OCR service could not accept this request.",
            ),
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                "DependencyUnavailable",
                "The sandbox is temporarily unavailable. Please retry later.",
            ),
        };
        let mut response = (
            status,
            Json(json!({"error":{"code":code,"message":message}})),
        )
            .into_response();
        if status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert("retry-after", axum::http::HeaderValue::from_static("60"));
        }
        response
    }
}
