use crate::{
    config::{MAX_BYTES, MAX_PIXELS},
    storage::Storage,
    store::{hash, Session},
    upstream::Upstream,
    Config, Error, Store,
};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::Semaphore;

pub struct App {
    pub config: Config,
    pub store: Store,
    pub(crate) storage: Storage,
    pub(crate) upstream: Upstream,
    permits: Semaphore,
    pdf_permits: Semaphore,
    parser: ocr_service::ParserProcess,
}

impl App {
    pub fn new(config: Config, pool: sqlx::PgPool) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self {
            store: Store::new(pool, config.daily_uploads),
            storage: Storage::new(&config)?,
            upstream: Upstream::new(&config)?,
            config,
            permits: Semaphore::new(8),
            pdf_permits: Semaphore::new(1),
            parser: ocr_service::ParserProcess::new(
                "/app/ocr-parser-sandbox".into(),
                std::time::Duration::from_secs(5),
            )
            .map_err(|_| Error::Configuration)?,
        }))
    }

    async fn session(&self, headers: &HeaderMap) -> Result<Session, Error> {
        let token = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or(Error::Unauthorized)?;
        self.store.authorize(token).await
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/readyz", get(ready))
        .route("/v1/sandbox/sessions", post(create_session))
        .route("/v1/sandbox/session", get(session_status))
        .route("/v1/ocr/uploads", post(reserve_upload))
        .route("/v1/ocr/uploads/{id}", get(upload_status))
        .route("/v1/ocr/uploads/{id}/content", put(upload_content))
        .route("/v1/ocr/uploads/{id}/complete", post(complete_upload))
        .route("/v1/ocr/jobs", post(create_job))
        .route("/v1/ocr/jobs/{id}", get(job_status))
        .route("/v1/ocr/jobs/{id}/result", get(job_result))
        .route("/v1/ocr/jobs/{id}/cancel", post(cancel_job))
        .merge(crate::web::router())
        .layer(axum::middleware::from_fn_with_state(app.clone(), boundary))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BYTES))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(90),
        ))
        .layer(axum::middleware::from_fn(security_headers))
        .with_state(app)
}

async fn boundary(
    State(app): State<Arc<App>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let permit = match app.permits.try_acquire() {
        Ok(p) => p,
        Err(_) => return Error::Limited.into_response(),
    };
    if request.headers().get("origin").is_some_and(|o| {
        o.to_str().ok() != Some(app.config.origin.origin().ascii_serialization().as_str())
    }) {
        return Error::Invalid.into_response();
    }
    let response = next.run(request).await;
    drop(permit);
    response
}

async fn security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    for (key,value) in [
        ("cache-control","no-store"),("x-content-type-options","nosniff"),("referrer-policy","no-referrer"),
        ("x-frame-options","DENY"),("x-robots-tag","noindex, nofollow"),
        ("permissions-policy","camera=(), microphone=(), geolocation=()"),
        ("content-security-policy","default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'")
    ] {response.headers_mut().insert(key,HeaderValue::from_static(value));}
    response
}

async fn ready(State(app): State<Arc<App>>) -> Result<Json<Value>, Error> {
    sqlx::query("select token_hash from ocr_sandbox_sessions limit 1")
        .fetch_optional(&app.store.pool)
        .await?;
    app.storage.ready().await?;
    Ok(Json(json!({"status":"ready"})))
}

async fn create_session(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<(StatusCode, Json<Value>), Error> {
    if !app.config.admission_enabled {
        return Err(Error::Unavailable);
    }
    let network = match peer.ip() {
        std::net::IpAddr::V4(ip) => ip.to_string(),
        std::net::IpAddr::V6(ip) => format!("{:x?}", &ip.octets()[..8]),
    };
    let mut material = app.config.secret.clone();
    material.extend_from_slice(
        format!(
            "sandbox-network:{}:{network}",
            time::OffsetDateTime::now_utc().date()
        )
        .as_bytes(),
    );
    let (token, session) = app.store.create_session(&hash(&material)).await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"token":token,"expires_at":session.expires,"limits":{"max_bytes":MAX_BYTES,"max_pixels":MAX_PIXELS,"images_per_session":1,"daily_uploads":app.config.daily_uploads}}),
        ),
    ))
}

async fn session_status(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Error> {
    let session = app.session(&headers).await?;
    Ok(Json(
        json!({"expires_at":session.expires,"images_deleted":session.images_deleted}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadInput {
    content_type: String,
    content_length: usize,
    sha256: String,
}

fn validate_upload(body: &Value) -> Result<UploadInput, Error> {
    let input: UploadInput = serde_json::from_value(body.clone()).map_err(|_| Error::Invalid)?;
    let digest = input.sha256.strip_prefix("sha256:").ok_or(Error::Invalid)?;
    if !matches!(
        input.content_type.as_str(),
        "application/pdf" | "image/jpeg" | "image/png"
    ) || !(1..=MAX_BYTES).contains(&input.content_length)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid);
    }
    Ok(input)
}

fn owned(session: &Session, id: &str, job: bool) -> Result<(), Error> {
    let expected = if job {
        session.job_id.as_deref()
    } else {
        session.upload_id.as_deref()
    };
    if expected != Some(id) {
        return Err(Error::NotFound);
    }
    Ok(())
}

fn response_id(value: &Value, job: bool) -> Result<&str, Error> {
    value
        .get(if job { "job_id" } else { "upload_id" })
        .and_then(Value::as_str)
        .filter(|s| {
            s.starts_with(if job { "job_" } else { "upl_" })
                && (5..=68).contains(&s.len())
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .ok_or(Error::Unavailable)
}

async fn reserve_upload(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), Error> {
    if !app.config.admission_enabled {
        return Err(Error::Unavailable);
    }
    let input = validate_upload(&body)?;
    let session = app.session(&headers).await?;
    let session = app.store.reserve_upload(&session.token_hash, &body).await?;
    app.storage.ready().await?;
    let mut result = app
        .upstream
        .call(
            &app.config,
            &session,
            Method::POST,
            "/v1/ocr/uploads",
            Some(&body),
            false,
        )
        .await?;
    let id = response_id(&result, false)?.to_owned();
    app.store.record_id(&session.token_hash, &id, false).await?;
    result["upload_url"] = json!(app
        .config
        .origin
        .join(&format!("/v1/ocr/uploads/{id}/content"))
        .map_err(|_| Error::Configuration)?
        .as_str());
    result["required_headers"] = json!({"content-type":input.content_type});
    Ok((StatusCode::CREATED, Json(result)))
}

fn validate_image(body: &[u8], input: &UploadInput) -> Result<(), Error> {
    if body.len() != input.content_length || format!("sha256:{}", hash(body)) != input.sha256 {
        return Err(Error::Invalid);
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(body))
        .with_guessed_format()
        .map_err(|_| Error::Invalid)?;
    let format = reader.format().ok_or(Error::Invalid)?;
    if !matches!(
        (format, input.content_type.as_str()),
        (image::ImageFormat::Png, "image/png") | (image::ImageFormat::Jpeg, "image/jpeg")
    ) {
        return Err(Error::Invalid);
    }
    let (width, height) = reader.into_dimensions().map_err(|_| Error::Invalid)?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::Invalid);
    }
    Ok(())
}

async fn validate_pdf(
    body: &[u8],
    input: &UploadInput,
    parser: &ocr_service::ParserProcess,
) -> Result<(), Error> {
    if body.len() != input.content_length
        || body.len() > MAX_BYTES
        || format!("sha256:{}", hash(body)) != input.sha256
        || !body.starts_with(b"%PDF-")
    {
        return Err(Error::Invalid);
    }
    let report = parser
        .inspect(body, "application/pdf")
        .await
        .map_err(|error| match error {
            ocr_service::ParserProcessError::PasswordRequired => Error::PasswordProtected,
            ocr_service::ParserProcessError::InvalidDocument
            | ocr_service::ParserProcessError::LimitsExceeded => Error::InvalidDocument,
            _ => Error::Unavailable,
        })?;
    if report.page_count != 1 || report.maximum_page_pixels > MAX_PIXELS as i64 {
        return Err(Error::InvalidDocument);
    }
    Ok(())
}

async fn upload_content(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Error> {
    let session = app.session(&headers).await?;
    owned(&session, &id, false)?;
    let input = validate_upload(session.upload_request.as_ref().ok_or(Error::Conflict)?)?;
    let bytes = body.clone();
    if input.content_type == "application/pdf" {
        let _permit = app.pdf_permits.try_acquire().map_err(|_| Error::Limited)?;
        validate_pdf(&bytes, &input, &app.parser).await?;
    } else {
        tokio::task::spawn_blocking(move || validate_image(&bytes, &input))
            .await
            .map_err(|_| Error::Unavailable)??;
    }
    if session.content_uploaded {
        return Ok(StatusCode::NO_CONTENT);
    }
    app.store.claim_content(&session.token_hash).await?;
    let result = async {
        let payload = session.upload_request.as_ref().ok_or(Error::Conflict)?;
        let reservation = app
            .upstream
            .call(
                &app.config,
                &session,
                Method::POST,
                "/v1/ocr/uploads",
                Some(payload),
                false,
            )
            .await?;
        if response_id(&reservation, false)? != id {
            return Err(Error::Unavailable);
        }
        let content_type = payload["content_type"].as_str().ok_or(Error::Invalid)?;
        app.upstream
            .put(&app.config, &session, &reservation, body, content_type)
            .await
    }
    .await;
    app.store
        .content_finished(&session.token_hash, result.is_ok())
        .await?;
    result?;
    Ok(StatusCode::NO_CONTENT)
}

async fn upload_status(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Error> {
    let session = app.session(&headers).await?;
    owned(&session, &id, false)?;
    Ok(Json(
        app.upstream
            .call(
                &app.config,
                &session,
                Method::GET,
                &format!("/v1/ocr/uploads/{id}"),
                None,
                false,
            )
            .await?,
    ))
}

async fn complete_upload(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Error> {
    let session = app.session(&headers).await?;
    owned(&session, &id, false)?;
    if !session.content_uploaded {
        return Err(Error::Conflict);
    }
    Ok(Json(
        app.upstream
            .call(
                &app.config,
                &session,
                Method::POST,
                &format!("/v1/ocr/uploads/{id}/complete"),
                Some(&json!({})),
                false,
            )
            .await?,
    ))
}

fn validate_output(output: &Value) -> Result<(), Error> {
    let object = output.as_object().ok_or(Error::Invalid)?;
    if object.iter().any(|(key, value)| {
        !matches!(key.as_str(), "text" | "markdown" | "layout" | "evidence") || !value.is_boolean()
    }) {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn validate_job(body: &Value, session: &Session) -> Result<(), Error> {
    let object = body.as_object().ok_or(Error::Invalid)?;
    if object.keys().any(|k| {
        !matches!(
            k.as_str(),
            "source"
                | "document_type"
                | "output"
                | "language_hints"
                | "processing_class"
                | "extraction"
        )
    }) || body.get("source") != Some(&json!({"upload_id":session.upload_id}))
        || body.get("document_type").and_then(Value::as_str) != Some("general")
        || body.get("processing_class").and_then(Value::as_str) != Some("interactive")
    {
        return Err(Error::Invalid);
    }
    if let Some(output) = body.get("output") {
        validate_output(output)?;
    }
    if let Some(hints) = body.get("language_hints") {
        let hints = hints
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 4)
            .ok_or(Error::Invalid)?;
        if hints
            .iter()
            .any(|h| !matches!(h.as_str(), Some("en" | "fr" | "es" | "hi")))
        {
            return Err(Error::Invalid);
        }
    }
    if body
        .get("extraction")
        .is_some_and(|v| v != &json!({"schema_id":"kora.nutrition_label","schema_version":"1"}))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

async fn create_job(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), Error> {
    let session = app.session(&headers).await?;
    if session.upload_id.is_none() || !session.content_uploaded {
        return Err(Error::Conflict);
    }
    validate_job(&body, &session)?;
    app.store.reserve_job(&session.token_hash, &body).await?;
    let result = app
        .upstream
        .call(
            &app.config,
            &session,
            Method::POST,
            "/v1/ocr/jobs",
            Some(&body),
            true,
        )
        .await?;
    app.store
        .record_id(&session.token_hash, response_id(&result, true)?, true)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

async fn job_status(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Error> {
    let session = app.session(&headers).await?;
    owned(&session, &id, true)?;
    Ok(Json(
        app.upstream
            .call(
                &app.config,
                &session,
                Method::GET,
                &format!("/v1/ocr/jobs/{id}"),
                None,
                true,
            )
            .await?,
    ))
}

async fn cancel_job(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Error> {
    let session = app.session(&headers).await?;
    owned(&session, &id, true)?;
    Ok(Json(
        app.upstream
            .call(
                &app.config,
                &session,
                Method::POST,
                &format!("/v1/ocr/jobs/{id}/cancel"),
                Some(&json!({})),
                true,
            )
            .await?,
    ))
}

async fn job_result(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let session = app.session(&headers).await?;
    owned(&session, &id, true)?;
    let result = app
        .upstream
        .call(
            &app.config,
            &session,
            Method::GET,
            &format!("/v1/ocr/jobs/{id}/result"),
            None,
            true,
        )
        .await?;
    let deleted = session.images_deleted || crate::cleanup::images(&app, &session).await.is_ok();
    let mut response = Json(result).into_response();
    response.headers_mut().insert(
        "x-sandbox-images-deleted",
        HeaderValue::from_static(if deleted { "true" } else { "false" }),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    #[tokio::test]
    async fn sandbox_pdf_validation_checks_bytes_and_one_page_budget() {
        let path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ocr-parser-sandbox");
        let parser =
            ocr_service::ParserProcess::new(path, std::time::Duration::from_secs(5)).unwrap();
        for (bytes, accepted) in [
            (
                include_bytes!("../tests/fixtures/ocr-upload-test.pdf").as_slice(),
                true,
            ),
            (
                include_bytes!("../tests/fixtures/two-page.pdf").as_slice(),
                false,
            ),
            (
                include_bytes!("../tests/fixtures/ocr-upload-test.png").as_slice(),
                false,
            ),
            (b"%PDF-1.7\nnot a document".as_slice(), false),
        ] {
            let input = super::UploadInput {
                content_type: "application/pdf".into(),
                content_length: bytes.len(),
                sha256: format!("sha256:{}", crate::store::hash(bytes)),
            };
            assert_eq!(
                super::validate_pdf(bytes, &input, &parser).await.is_ok(),
                accepted
            );
        }
    }
    #[test]
    fn sandbox_accepts_pdf_upload_reservations() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(super::validate_upload(
            &json!({"content_type":"application/pdf","content_length":100,"sha256":digest})
        )
        .is_ok());
    }

    #[test]
    fn sandbox_rejects_unknown_or_non_boolean_output_options() {
        for output in [
            json!({"text":true,"ai":true}),
            json!({"text":"true"}),
            json!([]),
            json!(null),
        ] {
            assert!(super::validate_output(&output).is_err());
        }
        assert!(super::validate_output(
            &json!({"text":true,"markdown":false,"layout":true,"evidence":true})
        )
        .is_ok());
    }

    #[test]
    fn sandbox_rejects_documents_outside_its_upload_budget() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(super::validate_upload(
            &json!({"content_type":"image/png","content_length":100,"sha256":digest})
        )
        .is_ok());
        for body in [
            json!({"content_type":"image/tiff","content_length":100,"sha256":digest}),
            json!({"content_type":"image/png","content_length":4194305,"sha256":digest}),
            json!({"content_type":"image/png","content_length":100,"sha256":digest,"tenant_id":"ten_OTHER"}),
        ] {
            assert!(super::validate_upload(&body).is_err());
        }
    }
}
