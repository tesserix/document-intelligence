use std::{collections::HashMap, env, str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use ocr_domain::ProductId;
use ocr_service::{
    router, router_with_dependencies, router_with_result_reader, router_with_upload_issuer,
    GcsResultReader, GcsUploadIssuer,
};
use ocr_store::PgJobStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::{net::TcpListener, signal};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

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
    let application = match (result_buckets, upload_buckets) {
        (Some(result_buckets), Some(upload_buckets)) => {
            let reader = GcsResultReader::new(&result_buckets)
                .context("RESULT_BUCKETS contains invalid configuration")?;
            let issuer =
                GcsUploadIssuer::new(&upload_buckets.values().cloned().collect::<Vec<_>>())
                    .context("QUARANTINE_BUCKETS contains invalid configuration")?;
            router_with_dependencies(
                PgJobStore::new(pool),
                Arc::new(reader),
                upload_buckets,
                Arc::new(issuer),
            )
        }
        (Some(result_buckets), None) => {
            let reader = GcsResultReader::new(&result_buckets)
                .context("RESULT_BUCKETS contains invalid configuration")?;
            router_with_result_reader(PgJobStore::new(pool), Arc::new(reader))
        }
        (None, Some(upload_buckets)) => {
            let issuer =
                GcsUploadIssuer::new(&upload_buckets.values().cloned().collect::<Vec<_>>())
                    .context("QUARANTINE_BUCKETS contains invalid configuration")?;
            router_with_upload_issuer(PgJobStore::new(pool), upload_buckets, Arc::new(issuer))
        }
        (None, None) => router(PgJobStore::new(pool)),
    };
    let listener = TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("cannot bind to {bind_address}"))?;

    info!(bind_address, "OCR service listening");
    axum::serve(listener, application)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server stopped unexpectedly")
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
    use super::parse_product_buckets;

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
}
