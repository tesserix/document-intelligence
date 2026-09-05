use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Request, StatusCode},
};
use hmac::{Hmac, KeyInit, Mac};
use ocr_service::{mcp_router, McpAccessGrantVerifier, McpUpstreamKeyVerifier};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use tower::ServiceExt;

type HmacSha256 = Hmac<Sha256>;

const PRODUCT_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
const MCP_KEY: &[u8] = b"abcdef0123456789abcdef0123456789";

fn store_without_connection() -> ocr_store::PgJobStore {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    ocr_store::PgJobStore::new(pool)
}

fn signed_grant(
    key_id: &str,
    tenant_id: &str,
    subject: &str,
    timestamp: i64,
    tool: &str,
    arguments: &Value,
) -> HeaderMap {
    let mut mac = HmacSha256::new_from_slice(PRODUCT_KEY).unwrap();
    mac.update(
        McpAccessGrantVerifier::canonical_message(
            key_id, tenant_id, subject, timestamp, tool, arguments,
        )
        .unwrap()
        .as_bytes(),
    );
    let signature = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let mut headers = HeaderMap::new();
    headers.insert("x-ocr-key-id", key_id.parse().unwrap());
    headers.insert("x-ocr-tenant-id", tenant_id.parse().unwrap());
    headers.insert("x-ocr-subject", subject.parse().unwrap());
    headers.insert("x-ocr-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert("x-ocr-grant-signature", signature.parse().unwrap());
    headers
}

#[test]
fn grant_binds_product_tenant_subject_tool_and_canonical_arguments() {
    let verifier =
        McpAccessGrantVerifier::new([("kora-v1", "kora", PRODUCT_KEY)], Duration::from_secs(60))
            .unwrap();
    let arguments = json!({"job_id": "job_RESULT"});
    let headers = signed_grant(
        "kora-v1",
        "ten_KORA",
        "usr_123",
        1_700_000_000,
        "get_document_result",
        &arguments,
    );

    let grant = verifier
        .verify(
            &headers,
            "get_document_result",
            &arguments,
            time::OffsetDateTime::from_unix_timestamp(1_700_000_030).unwrap(),
        )
        .unwrap();

    assert_eq!(grant.product_id().as_str(), "kora");
    assert_eq!(grant.tenant_id().as_str(), "ten_KORA");
    assert_eq!(grant.subject(), "usr_123");
    assert!(verifier
        .verify(
            &headers,
            "get_document_result",
            &json!({"job_id": "job_OTHER"}),
            time::OffsetDateTime::from_unix_timestamp(1_700_000_030).unwrap(),
        )
        .is_none());
    assert!(verifier
        .verify(
            &headers,
            "get_document_status",
            &arguments,
            time::OffsetDateTime::from_unix_timestamp(1_700_000_030).unwrap(),
        )
        .is_none());
}

#[test]
fn gateway_key_must_match_the_grant_product() {
    let verifier = McpUpstreamKeyVerifier::new([
        ("kora", MCP_KEY),
        ("devai", b"0123456789abcdef0123456789abcdef"),
    ])
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("x-mcp-key", HeaderValue::from_bytes(MCP_KEY).unwrap());

    assert_eq!(verifier.verify(&headers).unwrap().as_str(), "kora");
    headers.insert("x-mcp-key", "invalid".parse().unwrap());
    assert!(verifier.verify(&headers).is_none());
}

#[tokio::test]
async fn tools_list_requires_the_gateway_key_and_exposes_only_read_tools() {
    let application = mcp_router(
        store_without_connection(),
        None,
        McpUpstreamKeyVerifier::new([("kora", MCP_KEY)]).unwrap(),
        McpAccessGrantVerifier::new([("kora-v1", "kora", PRODUCT_KEY)], Duration::from_secs(60))
            .unwrap(),
    );
    let request = Request::post("/mcp")
        .header("content-type", "application/json")
        .header("x-mcp-key", HeaderValue::from_bytes(MCP_KEY).unwrap())
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))
        .unwrap();

    let response = application.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let tools = body["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "get_document_status");
    assert_eq!(tools[1]["name"], "get_document_result");
}

#[tokio::test]
async fn tools_call_rejects_a_gateway_authenticated_request_without_an_access_grant() {
    let application = mcp_router(
        store_without_connection(),
        None,
        McpUpstreamKeyVerifier::new([("kora", MCP_KEY)]).unwrap(),
        McpAccessGrantVerifier::new([("kora-v1", "kora", PRODUCT_KEY)], Duration::from_secs(60))
            .unwrap(),
    );
    let request = Request::post("/mcp")
        .header("content-type", "application/json")
        .header("x-mcp-key", HeaderValue::from_bytes(MCP_KEY).unwrap())
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_document_status","arguments":{"job_id":"job_RESULT"}}}"#,
        ))
        .unwrap();

    let response = application.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tools_call_hides_an_invalid_job_id_after_grant_verification() {
    let application = mcp_router(
        store_without_connection(),
        None,
        McpUpstreamKeyVerifier::new([("kora", MCP_KEY)]).unwrap(),
        McpAccessGrantVerifier::new([("kora-v1", "kora", PRODUCT_KEY)], Duration::from_secs(60))
            .unwrap(),
    );
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let arguments = json!({"job_id": "not-a-job"});
    let headers = signed_grant(
        "kora-v1",
        "ten_KORA",
        "usr_123",
        timestamp,
        "get_document_status",
        &arguments,
    );
    let mut request = Request::post("/mcp")
        .header("content-type", "application/json")
        .header("x-mcp-key", HeaderValue::from_bytes(MCP_KEY).unwrap())
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_document_status","arguments":{"job_id":"not-a-job"}}}"#,
        ))
        .unwrap();
    *request.headers_mut() = headers;
    request
        .headers_mut()
        .insert("x-mcp-key", HeaderValue::from_bytes(MCP_KEY).unwrap());
    request
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));

    let response = application.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
