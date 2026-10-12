use crate::{store::hash, App, Config, Store};
use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, State},
    http::{HeaderMap, Method, Request, StatusCode, Uri},
    response::IntoResponse,
    routing::any,
    Json, Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

#[derive(Default)]
struct Cloud {
    fail_delete: bool,
    objects: Vec<(String, String, String)>,
    rejected: bool,
    job_error: Option<StatusCode>,
    signed_calls: usize,
}

async fn fixture(
    State(state): State<Arc<Mutex<Cloud>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    use hmac::{Hmac, KeyInit, Mac};
    let url = url::Url::parse(&format!("http://fixture{uri}")).unwrap();
    let path = url.path();
    let mut cloud = state.lock().unwrap();
    if path.starts_with("/v1/") {
        let tenant = headers["x-ocr-tenant-id"].to_str().unwrap();
        let timestamp = headers["x-ocr-timestamp"].to_str().unwrap();
        assert_eq!(headers["x-ocr-key-id"], "fixture");
        let canonical = format!("fixture\n{tenant}\n{timestamp}\n{method}\n{uri}");
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&[17; 32]).unwrap();
        mac.update(canonical.as_bytes());
        let signature: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(headers["x-ocr-signature"], signature);
        assert!(tenant.starts_with("ten_SBX_"));
        cloud.signed_calls += 1;
        let suffix = tenant.strip_prefix("ten_SBX_").unwrap();
        let upload = format!("upl_{suffix}");
        let job = format!("job_{suffix}");
        if path == "/v1/ocr/uploads" {
            return (StatusCode::CREATED,Json(json!({"upload_id":upload,"upload_url":format!("https://storage.googleapis.com/ocr-sandbox-q/products/ocr-sandbox/tenants/{tenant}/quarantine/{upload}?fixture=signature"),"required_headers":{"content-type":"image/png"}}))).into_response();
        }
        if path.ends_with("/complete") {
            let prefix = format!("products/ocr-sandbox/tenants/{tenant}/");
            cloud.objects.push((
                "ocr-sandbox-s".into(),
                format!("{prefix}documents/source"),
                "2".into(),
            ));
            return Json(json!({"status":"uploaded"})).into_response();
        }
        if path.starts_with("/v1/ocr/uploads/") {
            return Json(json!({"status":if cloud.rejected{"rejected"}else{"accepted"}}))
                .into_response();
        }
        if path == "/v1/ocr/jobs" {
            if let Some(status) = cloud.job_error {
                return status.into_response();
            }
            assert!(!body.is_empty());
            return (
                StatusCode::ACCEPTED,
                Json(json!({"job_id":job,"status":"accepted"})),
            )
                .into_response();
        }
        if path.ends_with("/result") {
            return Json(json!({"text":"synthetic OCR result","fields":{},"warnings":[],"validation_failures":[]})).into_response();
        }
        return Json(json!({"status":"completed"})).into_response();
    }
    if method == Method::PUT {
        assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"));
        let (bucket, name) = path.trim_start_matches('/').split_once('/').unwrap();
        cloud.objects.push((bucket.into(), name.into(), "1".into()));
        return StatusCode::OK.into_response();
    }
    let parts: Vec<_> = url.path_segments().unwrap().collect();
    let bucket = parts[3];
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    if method == Method::DELETE {
        if cloud.fail_delete {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let generation = query.get("generation").unwrap();
        assert_eq!(Some(generation), query.get("ifGenerationMatch"));
        cloud
            .objects
            .retain(|(b, _, g)| b != bucket || g != generation);
        return StatusCode::NO_CONTENT.into_response();
    }
    if parts.len() == 4 {
        return Json(json!({"name":bucket})).into_response();
    }
    let prefix = query.get("prefix").unwrap();
    Json(json!({"items":cloud.objects.iter().filter(|(b,n,_)|b==bucket&&n.starts_with(prefix)).map(|(_,n,g)|json!({"name":n,"generation":g})).collect::<Vec<_>>()})).into_response()
}

async fn database() -> (PgPool, PgPool, String) {
    let url = std::env::var("TEST_DATABASE_ADMIN_URL").unwrap();
    let admin = sqlx::postgres::PgPoolOptions::new()
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("sandbox_lifecycle_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("create schema {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_sessions.sql"))
        .execute(&pool)
        .await
        .unwrap();
    (admin, pool, schema)
}

fn app(pool: PgPool, base: &url::Url) -> Arc<App> {
    let config = Config {
        origin: base.clone(),
        upload_api: base.clone(),
        job_api: base.clone(),
        database_url: String::new(),
        key_id: "fixture".into(),
        secret: vec![17; 32],
        buckets: [
            "ocr-sandbox-q".into(),
            "ocr-sandbox-s".into(),
            "ocr-sandbox-p".into(),
            "ocr-sandbox-r".into(),
        ],
        daily_uploads: 100,
        admission_enabled: true,
    };
    let mut app = App::new(config, pool).unwrap();
    let inner = Arc::get_mut(&mut app).unwrap();
    inner.parser = ocr_service::ParserProcess::new(
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ocr-parser-sandbox"),
        std::time::Duration::from_secs(5),
    )
    .unwrap()
    .with_document_limits(
        crate::config::MAX_PAGES,
        crate::config::MAX_PIXELS,
        crate::config::MAX_PIXELS * u64::from(crate::config::MAX_PAGES),
    );
    inner.upstream.fixture(base.clone());
    inner.storage.fixture(base.join("/storage/v1/").unwrap());
    app
}

async fn call(
    router: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder().method(&method).uri(path).header(
        "content-type",
        if method == Method::PUT {
            "image/png"
        } else {
            "application/json"
        },
    );
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let mut request = request.body(Body::from(body)).unwrap();
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        4321,
    )));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_ADMIN_URL"]
async fn real_http_adapter_accepts_large_uploads_isolates_sessions_and_recovers_cleanup_after_restart(
) {
    let (admin, pool, schema) = database().await;
    let cloud = Arc::new(Mutex::new(Cloud::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let upstream = Router::new()
        .fallback(any(fixture))
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::config::MAX_BYTES,
        ))
        .with_state(cloud.clone());
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let app = app(pool.clone(), &base);
    let router = crate::router(app.clone());
    let rejected = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("origin", "https://untrusted.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        rejected
            .headers()
            .get("cache-control")
            .map(|v| v.to_str().unwrap()),
        Some("no-store")
    );

    let (status, _, session) = call(
        &router,
        Method::POST,
        "/v1/sandbox/sessions",
        None,
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let token = session["token"].as_str().unwrap();
    let (_, _, other) = call(
        &router,
        Method::POST,
        "/v1/sandbox/sessions",
        None,
        b"{}".to_vec(),
    )
    .await;
    let other = other["token"].as_str().unwrap();
    let mut image = include_bytes!("../../../benchmarks/v1/images/no_text.png").to_vec();
    image.resize(5 * 1024 * 1024, 0);
    let payload = json!({"content_type":"image/png","content_length":image.len(),"sha256":format!("sha256:{}",hash(&image))});
    let (status, _, upload) = call(
        &router,
        Method::POST,
        "/v1/ocr/uploads",
        Some(token),
        serde_json::to_vec(&payload).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(upload["upload_url"]
        .as_str()
        .unwrap()
        .starts_with(base.as_str()));
    assert!(!upload.to_string().contains("signature"));
    let id = upload["upload_id"].as_str().unwrap();
    let (status, _, _) = call(
        &router,
        Method::GET,
        &format!("/v1/ocr/uploads/{id}"),
        Some(other),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let content = format!("/v1/ocr/uploads/{id}/content");
    for (length, expected) in [
        (crate::config::MAX_BYTES + 1, StatusCode::PAYLOAD_TOO_LARGE),
        (image.len() + 1, StatusCode::BAD_REQUEST),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(&content)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "image/png")
                    .header("content-length", length)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let (notice, buffering) = tokio::sync::oneshot::channel();
    let waiting_body = Body::from_stream(futures_util::stream::once(async move {
        notice.send(()).unwrap();
        std::future::pending::<Result<axum::body::Bytes, std::io::Error>>().await
    }));
    let pending = tokio::spawn(
        router.clone().oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(&content)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "image/png")
                .body(waiting_body)
                .unwrap(),
        ),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), buffering)
        .await
        .unwrap()
        .unwrap();
    let (busy, _, _) = call(&router, Method::PUT, &content, Some(token), vec![]).await;
    assert_eq!(busy, StatusCode::TOO_MANY_REQUESTS);
    let (unauthorized, _, _) = call(&router, Method::PUT, &content, None, vec![]).await;
    assert_eq!(unauthorized, StatusCode::UNAUTHORIZED);
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    let (status, _, _) = call(&router, Method::PUT, &content, Some(token), image.clone()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let count = cloud.lock().unwrap().objects.len();
    let (status, _, _) = call(&router, Method::PUT, &content, Some(token), image).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(cloud.lock().unwrap().objects.len(), count);
    let (status, _, _) = call(
        &router,
        Method::POST,
        &format!("/v1/ocr/uploads/{id}/complete"),
        Some(token),
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let job = json!({"source":{"upload_id":id},"document_type":"general","output":{"text":true},"processing_class":"interactive"});
    let (status, _, created) = call(
        &router,
        Method::POST,
        "/v1/ocr/jobs",
        Some(token),
        serde_json::to_vec(&job).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job_id = created["job_id"].as_str().unwrap();
    let result_path = format!("/v1/ocr/jobs/{job_id}/result");
    assert_eq!(
        call(&router, Method::GET, &result_path, Some(other), vec![])
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&router, Method::GET, &result_path, None, vec![])
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    cloud.lock().unwrap().fail_delete = true;
    let (status, headers, result) =
        call(&router, Method::GET, &result_path, Some(token), vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-sandbox-images-deleted"], "false");
    assert_eq!(result["text"], "synthetic OCR result");
    assert!(!app.store.authorize(token).await.unwrap().images_deleted);
    drop(router);
    drop(app);
    cloud.lock().unwrap().fail_delete = false;
    let foreign = (
        "ocr-sandbox-s".to_owned(),
        "products/ocr-sandbox/tenants/ten_SBX_00000000000000000000000000000000/documents/source"
            .to_owned(),
        "999".to_owned(),
    );
    cloud.lock().unwrap().objects.push(foreign.clone());
    let restarted = app_for_restart(pool.clone(), &base);
    crate::cleanup::pass(restarted.clone()).await.unwrap();
    assert!(
        restarted
            .store
            .authorize(token)
            .await
            .unwrap()
            .images_deleted
    );
    assert_eq!(cloud.lock().unwrap().objects, vec![foreign.clone()]);
    let router = crate::router(restarted.clone());
    let (status, headers, _) = call(&router, Method::GET, &result_path, Some(token), vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-sandbox-images-deleted"], "true");
    assert_eq!(headers["cache-control"], "no-store");
    assert!(headers["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    assert!(cloud.lock().unwrap().signed_calls > 0);
    sqlx::query("update ocr_sandbox_sessions set expires_at=now()-interval '3 minutes',cleanup_after=now() where token_hash=$1").bind(hash(token.as_bytes())).execute(&pool).await.unwrap();
    assert!(matches!(
        restarted.store.authorize(token).await,
        Err(crate::Error::Unauthorized)
    ));
    crate::cleanup::pass(restarted).await.unwrap();
    let purged: bool = sqlx::query_scalar(
        "select purged_at is not null from ocr_sandbox_sessions where token_hash=$1",
    )
    .bind(hash(token.as_bytes()))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(purged);
    assert_eq!(cloud.lock().unwrap().objects, vec![foreign]);
    server.abort();
    drop(router);
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("drop schema {schema} cascade")))
        .execute(&admin)
        .await
        .unwrap();
}

fn app_for_restart(pool: PgPool, base: &url::Url) -> Arc<App> {
    app(pool, base)
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_ADMIN_URL"]
async fn rejected_images_are_cleaned_without_waiting_for_session_expiry() {
    let (admin, pool, schema) = database().await;
    let cloud = Arc::new(Mutex::new(Cloud {
        rejected: true,
        ..Cloud::default()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server_cloud = cloud.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .fallback(any(fixture))
                .with_state(server_cloud),
        )
        .await
        .unwrap()
    });
    let app = app(pool.clone(), &base);
    let (token, session) = app.store.create_session("network").await.unwrap();
    app.store.reserve_upload(&session.token_hash,&json!({"content_type":"image/png","content_length":100,"sha256":format!("sha256:{}","a".repeat(64))})).await.unwrap();
    app.store
        .record_id(&session.token_hash, "upl_REJECTED", false)
        .await
        .unwrap();
    cloud.lock().unwrap().objects.push((
        "ocr-sandbox-q".into(),
        format!(
            "products/ocr-sandbox/tenants/{}/quarantine/upl_REJECTED",
            session.tenant_id
        ),
        "1".into(),
    ));
    crate::cleanup::pass(app.clone()).await.unwrap();
    assert!(app.store.authorize(&token).await.unwrap().images_deleted);
    assert!(cloud.lock().unwrap().objects.is_empty());
    server.abort();
    drop(app);
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("drop schema {schema} cascade")))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_ADMIN_URL"]
async fn quotas_and_session_expiry_fail_closed() {
    let (admin, pool, schema) = database().await;
    let store = Store::new(pool.clone(), 100);
    let (token, _) = store.create_session("network").await.unwrap();
    for _ in 0..60 {
        store.authorize(&token).await.unwrap();
    }
    assert!(matches!(
        store.authorize(&token).await,
        Err(crate::Error::Limited)
    ));
    for _ in 0..9 {
        store.create_session("network").await.unwrap();
    }
    assert!(matches!(
        store.create_session("network").await,
        Err(crate::Error::Limited)
    ));
    sqlx::query("update ocr_sandbox_sessions set expires_at=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.authorize(&token).await,
        Err(crate::Error::Unauthorized)
    ));
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("drop schema {schema} cascade")))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_ADMIN_URL"]
async fn expired_rejected_job_requests_are_purged_but_ambiguous_failures_are_retained() {
    let (admin, pool, schema) = database().await;
    let cloud = Arc::new(Mutex::new(Cloud::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let upstream = Router::new()
        .fallback(any(fixture))
        .with_state(cloud.clone());
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let app = app(pool.clone(), &base);
    let router = crate::router(app.clone());
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::UNPROCESSABLE_ENTITY,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        cloud.lock().unwrap().job_error = Some(status);
        let (token, session) = app.store.create_session("network").await.unwrap();
        app.store.reserve_upload(&session.token_hash, &json!({"content_type":"image/png","content_length":100,"sha256":format!("sha256:{}","a".repeat(64))})).await.unwrap();
        app.store
            .record_id(&session.token_hash, "upl_FIXTURE", false)
            .await
            .unwrap();
        app.store
            .content_finished(&session.token_hash, true)
            .await
            .unwrap();
        cloud.lock().unwrap().objects.push((
            "ocr-sandbox-q".into(),
            format!(
                "products/ocr-sandbox/tenants/{}/quarantine/upl_FIXTURE",
                session.tenant_id
            ),
            status.as_u16().to_string(),
        ));
        let body = json!({"source":{"upload_id":"upl_FIXTURE"},"document_type":"general","processing_class":"interactive","output":{"text":true,"markdown":false,"layout":true,"evidence":true}});
        assert_eq!(
            call(
                &router,
                Method::POST,
                "/v1/ocr/jobs",
                Some(&token),
                serde_json::to_vec(&body).unwrap()
            )
            .await
            .0,
            status
        );
        sqlx::query("update ocr_sandbox_sessions set expires_at=now()-interval '3 minutes',cleanup_after=now() where token_hash=$1").bind(&session.token_hash).execute(&pool).await.unwrap();
        crate::cleanup::pass(app.clone()).await.unwrap();
        let purged: bool = sqlx::query_scalar(
            "select purged_at is not null from ocr_sandbox_sessions where token_hash=$1",
        )
        .bind(&session.token_hash)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(purged, status.is_client_error(), "status {status}");
        assert_eq!(
            cloud.lock().unwrap().objects.is_empty(),
            status.is_client_error(),
            "status {status}"
        );
    }
    server.abort();
    drop(router);
    drop(app);
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("drop schema {schema} cascade")))
        .execute(&admin)
        .await
        .unwrap();
}
