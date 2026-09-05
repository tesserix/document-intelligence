use std::{collections::HashMap, env, sync::Arc, time::Duration};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use hmac::{Hmac, Mac};
use ocr_domain::{DocumentResult, JobId, ProductId, TenantId};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;

use ocr_store::{PgJobStore, ResultLookup};

use crate::{ResultArtifactReader, MAXIMUM_RESULT_BYTES};

const ACCESS_GRANT_KEY_ID_HEADER: &str = "x-ocr-key-id";
const ACCESS_GRANT_TENANT_HEADER: &str = "x-ocr-tenant-id";
const ACCESS_GRANT_SUBJECT_HEADER: &str = "x-ocr-subject";
const ACCESS_GRANT_TIMESTAMP_HEADER: &str = "x-ocr-timestamp";
const ACCESS_GRANT_SIGNATURE_HEADER: &str = "x-ocr-grant-signature";
const MCP_KEY_HEADER: &str = "x-mcp-key";
const MINIMUM_KEY_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct McpState {
    _jobs: PgJobStore,
    results: Option<Arc<dyn ResultArtifactReader>>,
    upstream_keys: McpUpstreamKeyVerifier,
    access_grants: McpAccessGrantVerifier,
}

#[derive(Debug, serde::Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

pub fn mcp_router(
    jobs: PgJobStore,
    results: Option<Arc<dyn ResultArtifactReader>>,
    upstream_keys: McpUpstreamKeyVerifier,
    access_grants: McpAccessGrantVerifier,
) -> Router {
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(|| async { StatusCode::OK }))
        .route("/mcp", post(handle_rpc))
        .with_state(McpState {
            _jobs: jobs,
            results,
            upstream_keys,
            access_grants,
        })
}

async fn handle_rpc(
    State(state): State<McpState>,
    headers: HeaderMap,
    Json(request): Json<RpcRequest>,
) -> Response {
    if headers.contains_key("mcp-session-id") {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            request.id,
            "invalid_request",
            "MCP sessions are not supported",
        );
    }
    let Some(upstream_product) = state.upstream_keys.verify(&headers) else {
        return rpc_error(
            StatusCode::UNAUTHORIZED,
            request.id,
            "authentication_required",
            "gateway authentication is required",
        );
    };
    if request.jsonrpc != "2.0" {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            request.id,
            "invalid_request",
            "JSON-RPC version is invalid",
        );
    }
    match request.method.as_str() {
        "initialize" => Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {
                "protocolVersion": "2026-07-28",
                "serverInfo": {"name": "document-intelligence-mcp", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"tools": {}}
            }
        }))
        .into_response(),
        "tools/list" => Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {"tools": tool_descriptors()}
        }))
        .into_response(),
        "tools/call" => handle_tool_call(&state, &headers, upstream_product, request).await,
        _ => rpc_error(
            StatusCode::BAD_REQUEST,
            request.id,
            "method_not_found",
            "MCP method is not supported",
        ),
    }
}

async fn handle_tool_call(
    state: &McpState,
    headers: &HeaderMap,
    upstream_product: ProductId,
    request: RpcRequest,
) -> Response {
    let Some(tool) = request.params.get("name").and_then(Value::as_str) else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            request.id,
            "invalid_params",
            "tool name is required",
        );
    };
    let arguments = request
        .params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Null);
    let Some(grant) =
        state
            .access_grants
            .verify(headers, tool, &arguments, OffsetDateTime::now_utc())
    else {
        return rpc_error(
            StatusCode::UNAUTHORIZED,
            request.id,
            "authentication_required",
            "OCR access grant is required",
        );
    };
    if grant.product_id() != &upstream_product {
        return rpc_error(
            StatusCode::FORBIDDEN,
            request.id,
            "authorization_denied",
            "OCR access grant is not authorized for this route",
        );
    }
    if !matches!(tool, "get_document_status" | "get_document_result") {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            request.id,
            "tool_not_found",
            "tool is not supported",
        );
    }
    let Some(job_id) = arguments.get("job_id").and_then(Value::as_str) else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            request.id,
            "invalid_params",
            "job ID is required",
        );
    };
    let Ok(job_id) = JobId::new(job_id) else {
        return rpc_error(
            StatusCode::NOT_FOUND,
            request.id,
            "not_found",
            "document job was not found",
        );
    };
    if tool == "get_document_result" {
        return document_result(state, grant, job_id, request.id).await;
    }
    let job = match state
        ._jobs
        .find(grant.tenant_id(), grant.product_id(), &job_id)
        .await
    {
        Ok(Some(job)) => job,
        Ok(None) => {
            return rpc_error(
                StatusCode::NOT_FOUND,
                request.id,
                "not_found",
                "document job was not found",
            )
        }
        Err(_) => {
            return rpc_error(
                StatusCode::SERVICE_UNAVAILABLE,
                request.id,
                "tool_unavailable",
                "OCR tool is temporarily unavailable",
            )
        }
    };
    Json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": request.id,
        "result": {
            "content": [{"type": "text", "text": format!("OCR job status: {:?}", job.state)}],
            "structuredContent": {"job_id": job.job_id, "status": job.state, "created_at": job.created_at}
        }
    }))
    .into_response()
}

async fn document_result(
    state: &McpState,
    grant: OcrAccessGrant,
    job_id: JobId,
    request_id: Value,
) -> Response {
    let Some(reader) = &state.results else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR result reader is not configured",
        );
    };
    let locator = match state
        ._jobs
        .find_result(grant.tenant_id(), grant.product_id(), &job_id)
        .await
    {
        Ok(ResultLookup::Ready(locator)) => locator,
        Ok(ResultLookup::NotFound) => {
            return rpc_error(
                StatusCode::NOT_FOUND,
                request_id,
                "not_found",
                "document job was not found",
            )
        }
        Ok(ResultLookup::NotReady(_) | ResultLookup::Unavailable(_)) => {
            return rpc_error(
                StatusCode::CONFLICT,
                request_id,
                "result_unavailable",
                "document result is not available",
            )
        }
        Err(_) => {
            return rpc_error(
                StatusCode::SERVICE_UNAVAILABLE,
                request_id,
                "tool_unavailable",
                "OCR tool is temporarily unavailable",
            )
        }
    };
    let Ok(expected_length) = usize::try_from(locator.content_length) else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR tool is temporarily unavailable",
        );
    };
    if expected_length > MAXIMUM_RESULT_BYTES {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR tool is temporarily unavailable",
        );
    }
    let Ok(bytes) = reader.read(&locator, MAXIMUM_RESULT_BYTES).await else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR tool is temporarily unavailable",
        );
    };
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    if bytes.len() != expected_length || digest != locator.object_digest {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR tool is temporarily unavailable",
        );
    }
    let Ok(result) = serde_json::from_slice::<DocumentResult>(&bytes) else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR tool is temporarily unavailable",
        );
    };
    if result.document_id != locator.document_id
        || result.document_version != locator.document_version
    {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR tool is temporarily unavailable",
        );
    }
    let Ok(structured_content) = serde_json::to_value(result) else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "tool_unavailable",
            "OCR tool is temporarily unavailable",
        );
    };
    Json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "content": [{"type": "text", "text": "OCR result returned as untrusted data."}],
            "structuredContent": structured_content
        }
    }))
    .into_response()
}

fn tool_descriptors() -> Value {
    serde_json::json!([
        {
            "name": "get_document_status",
            "description": "Read the status of one authorized OCR job.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["job_id"],
                "properties": {"job_id": {"type": "string", "minLength": 5, "maxLength": 96}}
            }
        },
        {
            "name": "get_document_result",
            "description": "Read the normalized, untrusted result of one authorized OCR job.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["job_id"],
                "properties": {"job_id": {"type": "string", "minLength": 5, "maxLength": 96}}
            }
        }
    ])
}

fn rpc_error(status: StatusCode, id: Value, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message}
        })),
    )
        .into_response()
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum McpAuthenticationConfigurationError {
    #[error("MCP key set is empty")]
    EmptyKeySet,
    #[error("MCP key identifier is invalid")]
    InvalidKeyId,
    #[error("MCP product is invalid")]
    InvalidProduct,
    #[error("MCP key is too short")]
    KeyTooShort,
    #[error("MCP key identifier is duplicated")]
    DuplicateKeyId,
    #[error("MCP clock skew is invalid")]
    InvalidClockSkew,
}

#[derive(Clone)]
struct SigningKey {
    product_id: ProductId,
    secret: Vec<u8>,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SigningKey")
            .field("product_id", &self.product_id)
            .field("secret", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct OcrAccessGrant {
    product_id: ProductId,
    tenant_id: TenantId,
    subject: String,
}

impl OcrAccessGrant {
    pub fn product_id(&self) -> &ProductId {
        &self.product_id
    }

    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }
}

#[derive(Debug, Clone)]
pub struct McpAccessGrantVerifier {
    keys: HashMap<String, SigningKey>,
    maximum_clock_skew: Duration,
}

impl McpAccessGrantVerifier {
    pub fn from_process_environment() -> Result<Self, McpAuthenticationConfigurationError> {
        let encoded = env::var("OCR_MCP_ACCESS_GRANT_KEYS")
            .map_err(|_| McpAuthenticationConfigurationError::EmptyKeySet)?;
        let skew = env::var("OCR_MCP_ACCESS_GRANT_CLOCK_SKEW_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(60));
        let keys = parse_key_set(&encoded)?;
        Self::new(
            keys.iter().map(|(key_id, product_id, secret)| {
                (key_id.as_str(), product_id.as_str(), secret.as_slice())
            }),
            skew,
        )
    }
    pub fn new<'a>(
        keys: impl IntoIterator<Item = (&'a str, &'a str, &'a [u8])>,
        maximum_clock_skew: Duration,
    ) -> Result<Self, McpAuthenticationConfigurationError> {
        if maximum_clock_skew.is_zero() {
            return Err(McpAuthenticationConfigurationError::InvalidClockSkew);
        }

        let mut configured_keys = HashMap::new();
        for (key_id, product_id, secret) in keys {
            if !valid_key_id(key_id) {
                return Err(McpAuthenticationConfigurationError::InvalidKeyId);
            }
            let product_id = ProductId::new(product_id)
                .map_err(|_| McpAuthenticationConfigurationError::InvalidProduct)?;
            if secret.len() < MINIMUM_KEY_BYTES {
                return Err(McpAuthenticationConfigurationError::KeyTooShort);
            }
            if configured_keys
                .insert(
                    key_id.to_owned(),
                    SigningKey {
                        product_id,
                        secret: secret.to_vec(),
                    },
                )
                .is_some()
            {
                return Err(McpAuthenticationConfigurationError::DuplicateKeyId);
            }
        }
        if configured_keys.is_empty() {
            return Err(McpAuthenticationConfigurationError::EmptyKeySet);
        }
        Ok(Self {
            keys: configured_keys,
            maximum_clock_skew,
        })
    }

    pub fn canonical_message(
        key_id: &str,
        tenant_id: &str,
        subject: &str,
        timestamp: i64,
        tool: &str,
        arguments: &Value,
    ) -> Result<String, McpAuthenticationConfigurationError> {
        if !valid_key_id(key_id) || !valid_subject(subject) || !valid_tool_name(tool) {
            return Err(McpAuthenticationConfigurationError::InvalidKeyId);
        }
        TenantId::new(tenant_id).map_err(|_| McpAuthenticationConfigurationError::InvalidKeyId)?;
        Ok(format!(
            "{key_id}\n{tenant_id}\n{subject}\n{timestamp}\n{tool}\nsha256:{:x}",
            Sha256::digest(canonical_json(arguments).as_bytes())
        ))
    }

    pub fn verify(
        &self,
        headers: &HeaderMap,
        tool: &str,
        arguments: &Value,
        now: OffsetDateTime,
    ) -> Option<OcrAccessGrant> {
        let key_id = header_value(headers, ACCESS_GRANT_KEY_ID_HEADER)?;
        let key = self.keys.get(key_id)?;
        let tenant_id = TenantId::new(header_value(headers, ACCESS_GRANT_TENANT_HEADER)?).ok()?;
        let subject = header_value(headers, ACCESS_GRANT_SUBJECT_HEADER)?;
        if !valid_subject(subject) || !valid_tool_name(tool) {
            return None;
        }
        let timestamp = header_value(headers, ACCESS_GRANT_TIMESTAMP_HEADER)?
            .parse::<i64>()
            .ok()
            .and_then(|value| OffsetDateTime::from_unix_timestamp(value).ok())?;
        if (now - timestamp).unsigned_abs() > self.maximum_clock_skew {
            return None;
        }
        let supplied_signature = decode_hex(header_value(headers, ACCESS_GRANT_SIGNATURE_HEADER)?)?;
        let message = Self::canonical_message(
            key_id,
            tenant_id.as_str(),
            subject,
            timestamp.unix_timestamp(),
            tool,
            arguments,
        )
        .ok()?;
        let mut mac = HmacSha256::new_from_slice(&key.secret).ok()?;
        mac.update(message.as_bytes());
        mac.verify_slice(&supplied_signature).ok()?;
        Some(OcrAccessGrant {
            product_id: key.product_id.clone(),
            tenant_id,
            subject: subject.to_owned(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct McpUpstreamKeyVerifier {
    keys: Vec<SigningKey>,
}

impl McpUpstreamKeyVerifier {
    pub fn from_process_environment() -> Result<Self, McpAuthenticationConfigurationError> {
        let encoded = env::var("OCR_MCP_UPSTREAM_KEYS")
            .map_err(|_| McpAuthenticationConfigurationError::EmptyKeySet)?;
        let keys = encoded
            .split(',')
            .map(|entry| {
                let (product_id, secret) = entry
                    .split_once('=')
                    .ok_or(McpAuthenticationConfigurationError::InvalidKeyId)?;
                let secret = decode_hex_value(secret)
                    .ok_or(McpAuthenticationConfigurationError::InvalidKeyId)?;
                Ok((product_id.to_owned(), secret))
            })
            .collect::<Result<Vec<_>, McpAuthenticationConfigurationError>>()?;
        Self::new(
            keys.iter()
                .map(|(product_id, secret)| (product_id.as_str(), secret.as_slice())),
        )
    }
    pub fn new<'a>(
        keys: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Result<Self, McpAuthenticationConfigurationError> {
        let mut configured_keys = Vec::new();
        for (product_id, secret) in keys {
            let product_id = ProductId::new(product_id)
                .map_err(|_| McpAuthenticationConfigurationError::InvalidProduct)?;
            if secret.len() < MINIMUM_KEY_BYTES {
                return Err(McpAuthenticationConfigurationError::KeyTooShort);
            }
            if configured_keys
                .iter()
                .any(|key: &SigningKey| key.product_id == product_id)
            {
                return Err(McpAuthenticationConfigurationError::DuplicateKeyId);
            }
            configured_keys.push(SigningKey {
                product_id,
                secret: secret.to_vec(),
            });
        }
        if configured_keys.is_empty() {
            return Err(McpAuthenticationConfigurationError::EmptyKeySet);
        }
        Ok(Self {
            keys: configured_keys,
        })
    }

    pub fn verify(&self, headers: &HeaderMap) -> Option<ProductId> {
        let supplied_key = headers.get(MCP_KEY_HEADER)?.as_bytes();
        self.keys
            .iter()
            .find(|key| constant_time_eq(supplied_key, &key.secret))
            .map(|key| key.product_id.clone())
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).expect("JSON values are serializable")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("JSON object keys are serializable"),
                        canonical_json(&values[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_subject(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'0'..=b'9' | b'_'))
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64 {
        return None;
    }
    decode_hex_value(value)
}

fn decode_hex_value(value: &str) -> Option<Vec<u8>> {
    if value.is_empty()
        || !value.len().is_multiple_of(2)
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
}

fn parse_key_set(
    encoded: &str,
) -> Result<Vec<(String, String, Vec<u8>)>, McpAuthenticationConfigurationError> {
    encoded
        .split(',')
        .map(|entry| {
            let (key_id, product_and_secret) = entry
                .split_once('=')
                .ok_or(McpAuthenticationConfigurationError::InvalidKeyId)?;
            let (product_id, secret) = product_and_secret
                .split_once(':')
                .ok_or(McpAuthenticationConfigurationError::InvalidKeyId)?;
            let secret = decode_hex_value(secret)
                .ok_or(McpAuthenticationConfigurationError::InvalidKeyId)?;
            Ok((key_id.to_owned(), product_id.to_owned(), secret))
        })
        .collect()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
