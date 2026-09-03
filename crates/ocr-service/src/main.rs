use std::{collections::HashMap, env, str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use ocr_domain::ProductId;
use ocr_service::{
    router_with_runtime_dependencies, CachePolicy, GcsResultReader, GcsUploadArtifactReader,
    GcsUploadIssuer, JobStatusCache, ResultArtifactReader, TelemetryConfig, TelemetryRuntime,
    UploadArtifactReader, UploadIntentIssuer, ValkeyJobStatusCache,
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
    let result_buckets = optional_csv_env("RESULT_BUCKETS")?;
    let upload_buckets = optional_product_buckets_env("QUARANTINE_BUCKETS")?;
    let results: Option<Arc<dyn ResultArtifactReader>> = result_buckets
        .map(|buckets| {
            GcsResultReader::new(&buckets)
                .map(|reader| Arc::new(reader) as Arc<dyn ResultArtifactReader>)
        })
        .transpose()
        .context("RESULT_BUCKETS contains invalid configuration")?;
    let (upload_buckets, uploads, upload_artifacts) = match upload_buckets {
        Some(upload_buckets) => {
            let buckets = upload_buckets.values().cloned().collect::<Vec<_>>();
            let issuer = GcsUploadIssuer::new(&buckets)
                .context("QUARANTINE_BUCKETS contains invalid configuration")?;
            let artifact_reader = GcsUploadArtifactReader::new(&buckets)
                .context("QUARANTINE_BUCKETS contains invalid configuration")?;
            (
                upload_buckets,
                Some(Arc::new(issuer) as Arc<dyn UploadIntentIssuer>),
                Some(Arc::new(artifact_reader) as Arc<dyn UploadArtifactReader>),
            )
        }
        None => (HashMap::new(), None, None),
    };
    let job_status_cache = optional_job_status_cache().await?;
    let application = router_with_runtime_dependencies(
        PgJobStore::new(pool),
        results,
        upload_buckets,
        uploads,
        upload_artifacts,
        job_status_cache,
    );
    let listener = TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("cannot bind to {bind_address}"))?;

    info!(bind_address, "OCR service listening");
    let server = axum::serve(listener, application)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server stopped unexpectedly");
    telemetry.shutdown().context("telemetry shutdown failed")?;
    server
}

async fn optional_job_status_cache() -> Result<Option<(Arc<dyn JobStatusCache>, CachePolicy)>> {
    let url = match env::var("VALKEY_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) => anyhow::bail!("VALKEY_URL must not be empty"),
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("VALKEY_URL is not valid UTF-8"),
    };
    let policy = parse_job_status_cache_policy(
        env::var("JOB_STATUS_CACHE_ACTIVE_TTL_SECONDS")
            .ok()
            .as_deref(),
        env::var("JOB_STATUS_CACHE_TERMINAL_TTL_SECONDS")
            .ok()
            .as_deref(),
        env::var("JOB_STATUS_CACHE_TIMEOUT_MILLISECONDS")
            .ok()
            .as_deref(),
        env::var("JOB_STATUS_CACHE_MAXIMUM_RECORD_BYTES")
            .ok()
            .as_deref(),
    )?;
    let cache = ValkeyJobStatusCache::connect(&url, policy.clone())
        .await
        .context("Valkey cache configuration is invalid")?;
    Ok(Some((Arc::new(cache), policy)))
}

fn parse_job_status_cache_policy(
    active_ttl_seconds: Option<&str>,
    terminal_ttl_seconds: Option<&str>,
    timeout_milliseconds: Option<&str>,
    maximum_record_bytes: Option<&str>,
) -> Result<CachePolicy> {
    let active_ttl = parse_bounded_number(active_ttl_seconds, 10, "active TTL")?;
    let terminal_ttl = parse_bounded_number(terminal_ttl_seconds, 300, "terminal TTL")?;
    let timeout = parse_bounded_number(timeout_milliseconds, 25, "timeout")?;
    let maximum_record_bytes = parse_bounded_number(maximum_record_bytes, 512, "record limit")?;
    CachePolicy::new(
        std::time::Duration::from_secs(active_ttl),
        std::time::Duration::from_secs(terminal_ttl),
        std::time::Duration::from_millis(timeout),
        maximum_record_bytes,
    )
    .context("job status cache policy is invalid")
}

fn parse_bounded_number<T>(value: Option<&str>, default: T, name: &str) -> Result<T>
where
    T: std::str::FromStr,
{
    value
        .map(str::parse)
        .transpose()
        .map_err(|_| anyhow::anyhow!("job status cache {name} is invalid"))
        .map(|value| value.unwrap_or(default))
}

fn optional_csv_env(name: &str) -> Result<Option<Vec<String>>> {
    match env::var(name) {
        Ok(value) => {
            let values = value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if values.is_empty() {
                anyhow::bail!("{name} must contain at least one value");
            }
            Ok(Some(values))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("{name} is not valid UTF-8"),
    }
}

fn optional_product_buckets_env(name: &str) -> Result<Option<HashMap<String, String>>> {
    let Some(values) = optional_csv_env(name)? else {
        return Ok(None);
    };
    parse_product_buckets(values).map(Some)
}

fn parse_product_buckets(values: Vec<String>) -> Result<HashMap<String, String>> {
    let mut buckets = HashMap::with_capacity(values.len());
    for value in values {
        let (product, bucket) = value
            .split_once('=')
            .context("QUARANTINE_BUCKETS entries must use product=bucket")?;
        ProductId::new(product).context("QUARANTINE_BUCKETS contains an invalid product")?;
        if bucket.is_empty()
            || buckets
                .insert(product.to_owned(), bucket.to_owned())
                .is_some()
        {
            anyhow::bail!("QUARANTINE_BUCKETS contains an invalid or duplicate product");
        }
    }
    Ok(buckets)
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

#[cfg(test)]
mod tests {
    use super::{parse_job_status_cache_policy, parse_product_buckets};
    use std::time::Duration;

    #[test]
    fn product_bucket_configuration_is_explicit_and_unique() {
        let buckets = parse_product_buckets(vec![
            "kora=dev-kora-ocr-quarantine".to_owned(),
            "other-product=dev-other-ocr-quarantine".to_owned(),
        ])
        .unwrap();
        assert_eq!(buckets["kora"], "dev-kora-ocr-quarantine");

        assert!(parse_product_buckets(vec!["Kora=bucket".to_owned()]).is_err());
        assert!(parse_product_buckets(vec!["kora=".to_owned()]).is_err());
        assert!(
            parse_product_buckets(vec!["kora=first".to_owned(), "kora=second".to_owned(),])
                .is_err()
        );
    }

    #[test]
    fn job_status_cache_policy_rejects_unbounded_configuration() {
        let policy =
            parse_job_status_cache_policy(Some("10"), Some("300"), Some("25"), Some("512"))
                .unwrap();
        assert_eq!(
            policy.ttl(ocr_domain::JobState::Processing),
            Duration::from_secs(10)
        );
        assert_eq!(
            policy.ttl(ocr_domain::JobState::Completed),
            Duration::from_secs(300)
        );

        assert!(parse_job_status_cache_policy(Some("0"), None, None, None).is_err());
        assert!(parse_job_status_cache_policy(None, None, Some("0"), None).is_err());
        assert!(parse_job_status_cache_policy(None, None, None, Some("999999")).is_err());
    }
}
