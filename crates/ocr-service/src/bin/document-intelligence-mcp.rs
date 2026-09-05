use std::{env, str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use ocr_service::{
    mcp_router, GcsResultReader, McpAccessGrantVerifier, McpUpstreamKeyVerifier,
    ResultArtifactReader, TelemetryConfig, TelemetryRuntime,
};
use ocr_store::PgJobStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::{net::TcpListener, signal};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let telemetry_config = TelemetryConfig::from_process_environment()
        .context("telemetry configuration is invalid")?;
    let telemetry = TelemetryRuntime::install(
        &telemetry_config,
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    )
    .context("telemetry initialization failed")?;
    let database_url = env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let bind_address = env::var("BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let options = PgConnectOptions::from_str(&database_url)
        .context("DATABASE_URL is invalid")?
        .statement_cache_capacity(0);
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_lazy_with(options);
    let result_buckets = env::var("RESULT_BUCKETS")
        .context("RESULT_BUCKETS is required")?
        .split(',')
        .map(str::trim)
        .filter(|bucket| !bucket.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let results = GcsResultReader::new(&result_buckets)
        .context("RESULT_BUCKETS contains invalid configuration")?;
    let application = mcp_router(
        PgJobStore::new(pool),
        Some(Arc::new(results) as Arc<dyn ResultArtifactReader>),
        McpUpstreamKeyVerifier::from_process_environment()
            .context("MCP upstream key configuration is invalid")?,
        McpAccessGrantVerifier::from_process_environment()
            .context("MCP access grant configuration is invalid")?,
    );
    let listener = TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("cannot bind to {bind_address}"))?;

    info!(bind_address, "document intelligence MCP listening");
    let server = axum::serve(listener, application)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server stopped unexpectedly");
    telemetry.shutdown().context("telemetry shutdown failed")?;
    server
}

async fn shutdown_signal() {
    let interrupt = async {
        signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}
