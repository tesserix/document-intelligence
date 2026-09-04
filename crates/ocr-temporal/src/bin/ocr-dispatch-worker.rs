use std::{
    collections::HashMap, env, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use ocr_service::{
    ClamdScanner, GcsSourcePromoter, GcsUploadDocumentReader, GcsUploadMalwareInspector, Importer,
    JobOutboxRelay, ParserProcess, TelemetryConfig, TelemetryRuntime,
};
use ocr_store::{PgJobStore, PgWorkScopeDirectory};
use ocr_temporal::{OfficialTemporalGateway, TemporalStarter, WorkScopeDispatcher};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};
use tokio::{signal, time};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main]
async fn main() -> Result<()> {
    let telemetry = TelemetryRuntime::install(
        &TelemetryConfig::from_process_environment()
            .context("telemetry configuration is invalid")?,
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    )
    .context("telemetry initialization failed")?;
    let result = run().await;
    let shutdown = telemetry.shutdown().context("telemetry shutdown failed");
    result?;
    shutdown
}

async fn run() -> Result<()> {
    let application_pool = pool("DATABASE_URL", 10)?;
    let scope_pool = pool("WORK_SCOPE_DATABASE_URL", 4)?;
    let jobs = PgJobStore::new(application_pool);
    let quarantine = product_buckets("QUARANTINE_BUCKETS")?;
    let sources = product_buckets("SOURCE_BUCKETS")?;
    let routes = source_routes(&quarantine, &sources)?;
    let scanner = Arc::new(ClamdScanner::new(
        loopback_socket("CLAMD_ADDRESS")?,
        Duration::from_secs(30),
        100 * 1024 * 1024,
    )?);
    let parser = Arc::new(ParserProcess::new(
        absolute_path("PARSER_EXECUTABLE")?,
        Duration::from_secs(60),
    )?);
    let quarantine_buckets = quarantine.values().cloned().collect::<Vec<_>>();
    let importer = Arc::new(Importer::new(
        jobs.clone(),
        Arc::new(GcsUploadMalwareInspector::new(
            &quarantine_buckets,
            scanner,
        )?),
        Arc::new(GcsUploadDocumentReader::new(&quarantine_buckets)?),
        parser,
        Arc::new(GcsSourcePromoter::new(routes)?),
    ));
    let gateway = Arc::new(OfficialTemporalGateway::new(temporal_client().await?));
    let starter = Arc::new(TemporalStarter::new(
        gateway,
        &required("TEMPORAL_TASK_QUEUE")?,
    )?);
    let dispatcher = WorkScopeDispatcher::new(
        PgWorkScopeDirectory::new(scope_pool),
        jobs.clone(),
        importer,
        Arc::new(JobOutboxRelay::new(jobs, starter)),
        &required("WORKER_ID")?,
        bounded("WORK_SCOPE_BATCH_SIZE", 10)?,
        bounded("UPLOAD_BATCH_SIZE", 10)?,
    )?;

    let mut interval = time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            () = shutdown_signal() => break,
            _ = interval.tick() => match dispatcher.dispatch_once().await {
                Ok(outcome) if outcome.scopes > 0 => info!(
                    scopes = outcome.scopes,
                    uploads = outcome.uploads,
                    workflows = outcome.workflows,
                    lease_lost = outcome.lease_lost,
                    "OCR dispatch pass completed"
                ),
                Ok(_) => {},
                Err(error) => warn!(error = %error, "OCR dispatch pass failed"),
            },
        }
    }
    Ok(())
}

fn pool(name: &str, maximum_connections: u32) -> Result<sqlx::PgPool> {
    let options = PgConnectOptions::from_str(&required(name)?)?.statement_cache_capacity(0);
    Ok(PgPoolOptions::new()
        .max_connections(maximum_connections)
        .connect_lazy_with(options))
}

async fn temporal_client() -> Result<Client> {
    let target = Url::parse(&required("TEMPORAL_TARGET")?)?;
    if !matches!(target.scheme(), "http" | "https")
        || target.host_str().is_none()
        || !target.username().is_empty()
        || target.password().is_some()
        || target.query().is_some()
        || target.fragment().is_some()
        || !matches!(target.path(), "" | "/")
    {
        anyhow::bail!("TEMPORAL_TARGET is invalid");
    }
    let namespace_value = required("TEMPORAL_NAMESPACE")?;
    let namespace = temporal_namespace(&namespace_value)?;
    let connection = Connection::connect(
        ConnectionOptions::new(target)
            .identity(&required("WORKER_ID")?)
            .connect_timeout(Duration::from_secs(5))
            .build(),
    )
    .await
    .context("Temporal connection failed")?;
    Client::new(connection, ClientOptions::new(namespace).build())
        .context("Temporal client configuration is invalid")
}

fn temporal_namespace(value: &str) -> Result<&str> {
    let environment = value
        .strip_prefix("document-intelligence-")
        .context("TEMPORAL_NAMESPACE must be dedicated to document intelligence")?;
    if !(2..=32).contains(&environment.len())
        || !environment
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || environment.starts_with('-')
        || environment.ends_with('-')
    {
        anyhow::bail!("TEMPORAL_NAMESPACE must include a valid environment suffix");
    }
    Ok(value)
}

fn product_buckets(name: &str) -> Result<HashMap<String, String>> {
    let mut buckets = HashMap::new();
    for value in required(name)?.split(',') {
        let (product, bucket) = value
            .trim()
            .split_once('=')
            .context("bucket entries must use product=bucket")?;
        if product.is_empty()
            || bucket.is_empty()
            || buckets
                .insert(product.to_owned(), bucket.to_owned())
                .is_some()
        {
            anyhow::bail!("{name} contains an invalid or duplicate product");
        }
    }
    if buckets.is_empty() {
        anyhow::bail!("{name} is empty");
    }
    Ok(buckets)
}

fn source_routes(
    quarantine: &HashMap<String, String>,
    sources: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    if quarantine.len() != sources.len() {
        anyhow::bail!("QUARANTINE_BUCKETS and SOURCE_BUCKETS must cover the same products");
    }
    let mut routes = HashMap::with_capacity(quarantine.len());
    for (product, quarantine_bucket) in quarantine {
        let source_bucket = sources
            .get(product)
            .context("QUARANTINE_BUCKETS and SOURCE_BUCKETS must cover the same products")?;
        if routes
            .insert(quarantine_bucket.clone(), source_bucket.clone())
            .is_some()
        {
            anyhow::bail!("QUARANTINE_BUCKETS contains duplicate buckets");
        }
    }
    Ok(routes)
}

fn loopback_socket(name: &str) -> Result<SocketAddr> {
    let address = required(name)?.parse::<SocketAddr>()?;
    if !address.ip().is_loopback() {
        anyhow::bail!("{name} must be a loopback address");
    }
    Ok(address)
}

fn absolute_path(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(required(name)?);
    if !path.is_absolute() {
        anyhow::bail!("{name} must be absolute");
    }
    Ok(path)
}

fn bounded(name: &str, default: i64) -> Result<i64> {
    match env::var(name) {
        Ok(value) => value.parse().context("worker batch size is invalid"),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("{name} is not UTF-8"),
    }
}

fn required(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => anyhow::bail!("{name} is required"),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("{name} is not UTF-8"),
    }
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
    tokio::select! { () = interrupt => {}, () = terminate => {} }
}

#[cfg(test)]
mod tests {
    use super::{source_routes, temporal_namespace};
    use std::collections::HashMap;

    #[test]
    fn source_routes_require_one_source_bucket_for_every_product() {
        let quarantine = HashMap::from([("product-a".to_owned(), "quarantine-a".to_owned())]);
        let sources = HashMap::from([("product-b".to_owned(), "source-b".to_owned())]);
        assert!(source_routes(&quarantine, &sources).is_err());
    }

    #[test]
    fn temporal_namespace_requires_a_dedicated_document_intelligence_namespace() {
        assert_eq!(
            temporal_namespace("document-intelligence-sandbox").unwrap(),
            "document-intelligence-sandbox"
        );
        assert!(temporal_namespace("default").is_err());
        assert!(temporal_namespace("other-workload-prod").is_err());
        assert!(temporal_namespace("document-intelligence-").is_err());
    }
}
