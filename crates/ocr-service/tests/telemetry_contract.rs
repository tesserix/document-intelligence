use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ocr_service::{router, TelemetryConfig, TelemetryConfigError, TrustedIdentity};
use ocr_store::PgJobStore;
use opentelemetry::{
    global,
    trace::{TraceId, TracerProvider as _},
};
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{InMemorySpanExporter, SdkTracerProvider},
};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use tracing::Dispatch;
use tracing_subscriber::{
    fmt::{format::FmtSpan, MakeWriter},
    layer::SubscriberExt,
};

#[derive(Clone, Default)]
struct CapturedOutput(Arc<Mutex<Vec<u8>>>);

struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedOutput {
    type Writer = CapturedWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedWriter(Arc::clone(&self.0))
    }
}

impl CapturedOutput {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn http_trace_emits_only_allowlisted_operational_fields() {
    let output = CapturedOutput::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_writer(output.clone())
        .finish();
    let dispatch = Dispatch::new(subscriber);
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    let application = router(PgJobStore::new(pool));

    let response = application
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz?filename=passport-SENSITIVE-CONTENT.pdf")
                .header("authorization", "Bearer SENSITIVE-CREDENTIAL")
                .header("x-original-uri", "gs://private-bucket/SENSITIVE-OBJECT")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let mut scoped_request = Request::builder()
        .uri("/v1/ocr/jobs/not-a-valid-job")
        .body(Body::empty())
        .unwrap();
    scoped_request
        .extensions_mut()
        .insert(TrustedIdentity::new("kora", "ten_TELEMETRY_SCOPE").unwrap());
    let scoped_response = application.oneshot(scoped_request).await.unwrap();
    assert_eq!(scoped_response.status(), StatusCode::NOT_FOUND);
    drop(response);
    drop(scoped_response);
    let trace = output.text();
    for prohibited in [
        "passport-SENSITIVE-CONTENT.pdf",
        "SENSITIVE-CREDENTIAL",
        "SENSITIVE-OBJECT",
        "authorization",
        "x-original-uri",
    ] {
        assert!(
            !trace.contains(prohibited),
            "trace contained prohibited input: {prohibited}"
        );
    }
    assert!(trace.contains("document.api.request"));
    assert!(trace.contains("GET"));
    assert!(trace.contains("kora"), "{trace}");
    assert!(trace.contains("ten_TELEMETRY_SCOPE"), "{trace}");
}

#[test]
fn otlp_configuration_allows_only_credential_free_internal_gateways() {
    let config = TelemetryConfig::new(
        Some("http://otel-gateway.observability.svc.cluster.local:4317"),
        "development",
    )
    .unwrap();
    assert_eq!(
        config.endpoint(),
        Some("http://otel-gateway.observability.svc.cluster.local:4317/")
    );
    assert_eq!(config.environment(), "development");

    assert_eq!(
        TelemetryConfig::new(Some("https://langfuse.tesserix.app"), "development",).unwrap_err(),
        TelemetryConfigError::UntrustedEndpoint
    );
    assert_eq!(
        TelemetryConfig::new(
            Some("http://otel-gateway.observability.svc.cluster.local:4317/path?token=secret"),
            "development",
        )
        .unwrap_err(),
        TelemetryConfigError::UntrustedEndpoint
    );
    assert_eq!(
        TelemetryConfig::new(None, "Development Kora").unwrap_err(),
        TelemetryConfigError::InvalidEnvironment
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_trace_continues_a_valid_w3c_parent_without_exporting_headers() {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry().with(
        tracing_opentelemetry::layer().with_tracer(provider.tracer("telemetry-contract-test")),
    );
    let dispatch = Dispatch::new(subscriber);
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    let application = router(PgJobStore::new(pool));

    let response = application
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header(
                    "traceparent",
                    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                )
                .header("tracestate", "vendor=safe-correlation-only")
                .header("authorization", "Bearer MUST-NOT-BE-EXPORTED")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let request = spans
        .iter()
        .find(|span| span.name == "document.api.request")
        .unwrap();
    assert_eq!(
        request.span_context.trace_id(),
        TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap()
    );
    assert!(request.attributes.iter().any(|attribute| {
        attribute.key.as_str() == "http.response.status_code"
            && attribute.value.to_string() == "200"
    }));
    assert!(request
        .attributes
        .iter()
        .any(|attribute| attribute.key.as_str() == "duration_ms"));
    let attributes = format!("{:?}", request.attributes);
    assert!(!attributes.contains("MUST-NOT-BE-EXPORTED"));
    assert!(!attributes.contains("vendor=safe-correlation-only"));
}
