use std::{
    collections::{BTreeSet, HashMap},
    env,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use ocr_service::{
    AcceptedPageSourceLoader, ArtifactPageProcessor, CloudAcceptedSourceReader,
    CloudPageArtifactReader, CloudPageArtifactWriter, CloudResultWriter, DocumentAiConfiguration,
    DocumentAiPageRecognizer, DocumentFinalizer, HybridPageRecognizer, MetadataDocumentAiTransport,
    NativePageRecognizer, PageRecognizer, ParserProcess, ResultPublisher, TelemetryConfig,
    TelemetryRuntime, DOCUMENT_AI_OCR_VERSION,
};
use ocr_store::PgJobStore;
use ocr_temporal::{
    CheckpointedPageExecutor, DocumentFinalizerExecutor, DurableDocumentWorkflow,
    DurableFinalizationActivities, DurablePageActivities,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};
use temporalio_sdk::{Runtime, Worker, WorkerOptions};
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main]
async fn main() -> Result<()> {
    let telemetry = TelemetryRuntime::install(
        &TelemetryConfig::from_process_environment().context("telemetry configuration invalid")?,
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    )
    .context("telemetry initialization failed")?;

    let result = run().await;
    telemetry.shutdown().context("telemetry shutdown failed")?;
    result
}

async fn run() -> Result<()> {
    let database_url = required("DATABASE_URL")?;
    let options = PgConnectOptions::from_str(&database_url)
        .context("DATABASE_URL is invalid")?
        .statement_cache_capacity(0);
    let store = PgJobStore::new(
        PgPoolOptions::new()
            .max_connections(10)
            .connect_lazy_with(options),
    );
    let source_buckets = product_buckets("SOURCE_BUCKETS")?;
    let page_buckets = product_buckets("PAGE_BUCKETS")?;
    let result_buckets = product_buckets("RESULT_BUCKETS")?;
    require_matching_products(
        &source_buckets,
        &page_buckets,
        "SOURCE_BUCKETS",
        "PAGE_BUCKETS",
    )?;
    require_matching_products(
        &source_buckets,
        &result_buckets,
        "SOURCE_BUCKETS",
        "RESULT_BUCKETS",
    )?;

    let source = Arc::new(AcceptedPageSourceLoader::new(
        Arc::new(store.clone()),
        Arc::new(CloudAcceptedSourceReader::new(
            &source_buckets
                .iter()
                .map(|(product, bucket)| (product.clone(), bucket.clone()))
                .collect::<Vec<_>>(),
        )?),
    ));
    let pages = Arc::new(CloudPageArtifactWriter::new(page_buckets.clone())?);
    let native = Arc::new(NativePageRecognizer::new(
        source.clone(),
        ParserProcess::new("/app/ocr-parser-sandbox".into(), Duration::from_secs(60))?,
        store.clone(),
    ));
    let recognizer: Arc<dyn PageRecognizer> = match document_ai(
        setting("DOCUMENT_AI_PROJECT_ID")?,
        setting("DOCUMENT_AI_LOCATION")?,
        setting("DOCUMENT_AI_PROCESSOR_ID")?,
    )? {
        Some(configuration) => {
            let transport = MetadataDocumentAiTransport::new_with_auth(
                configuration,
                &env::var("DOCUMENT_AI_AUTH_MODE").unwrap_or_else(|_| "metadata".to_owned()),
            )
            .await
            .map_err(|_| anyhow::anyhow!("Document AI workload identity is unavailable"))?;
            info!(
                version = DOCUMENT_AI_OCR_VERSION,
                "pages without native text use Document AI"
            );
            Arc::new(HybridPageRecognizer::new(
                native,
                Arc::new(
                    DocumentAiPageRecognizer::new(source, Arc::new(transport)).with_preprocessor(
                        ParserProcess::new(
                            "/app/ocr-parser-sandbox".into(),
                            Duration::from_secs(5),
                        )?,
                    ),
                ),
            ))
        }
        None => native,
    };
    let processor = ArtifactPageProcessor::new(recognizer, pages);
    let page_executor = CheckpointedPageExecutor::new(
        store.clone(),
        processor,
        bounded("PAGE_BATCH_SIZE", 8, 1, 64)?,
        bounded("PAGE_CONCURRENCY", 4, 1, 64)?,
    )?;
    let page_reader = Arc::new(CloudPageArtifactReader::new(
        &page_buckets.values().cloned().collect::<Vec<_>>(),
    )?);
    let result_writer = Arc::new(CloudResultWriter::new(result_buckets)?);
    let finalizer = DocumentFinalizer::new(
        store.clone(),
        page_reader,
        ResultPublisher::new(store, result_writer),
        bounded("FINALIZATION_CONCURRENCY", 4, 1, 64)?,
    )?
    .with_native_processing();

    let namespace_value = required("TEMPORAL_NAMESPACE")?;
    let namespace = temporal_namespace(&namespace_value)?;
    let task_queue = required("TEMPORAL_TASK_QUEUE")?;
    let worker_id = required("WORKER_ID")?;
    let client = temporal_client(&required("TEMPORAL_TARGET")?, namespace, &worker_id).await?;
    let runtime = Runtime::new_assume_tokio(Default::default())?;
    let worker_options = WorkerOptions::new(task_queue.clone())
        .register_workflow::<DurableDocumentWorkflow>()
        .context("durable OCR workflow registration failed")?
        .register_activities(DurablePageActivities::new(Arc::new(page_executor)))
        .register_activities(DurableFinalizationActivities::new(Arc::new(
            DocumentFinalizerExecutor::new(finalizer),
        )))
        .build();
    let mut worker = Worker::new(&runtime, client, worker_options)?;
    let shutdown = worker.shutdown_handle();
    let running = worker.run();
    tokio::pin!(running);
    info!(task_queue = %task_queue, "OCR execution worker started");
    tokio::select! {
        result = &mut running => result.context("OCR execution worker stopped unexpectedly"),
        _ = shutdown_signal() => {
            shutdown();
            running.await.context("OCR execution worker shutdown failed")
        }
    }
}

async fn temporal_client(target_value: &str, namespace: &str, worker_id: &str) -> Result<Client> {
    let target = Url::parse(target_value).context("TEMPORAL_TARGET is invalid")?;
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
    let connection = Connection::connect(
        ConnectionOptions::new(target)
            .identity(worker_id)
            .connect_timeout(Duration::from_secs(5))
            .build(),
    )
    .await
    .context("Temporal connection failed")?;
    Client::new(connection, ClientOptions::new(namespace).build())
        .context("Temporal client configuration invalid")
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
        anyhow::bail!("{name} must contain at least one product bucket");
    }
    Ok(buckets)
}

fn require_matching_products(
    left: &HashMap<String, String>,
    right: &HashMap<String, String>,
    left_name: &str,
    right_name: &str,
) -> Result<()> {
    let left_products = left.keys().collect::<BTreeSet<_>>();
    let right_products = right.keys().collect::<BTreeSet<_>>();
    if left_products != right_products {
        anyhow::bail!("{left_name} and {right_name} must cover the same products");
    }
    Ok(())
}

/// Unset processor settings keep the worker native-only (ADR-0017).
fn document_ai(
    project: Option<String>,
    location: Option<String>,
    processor: Option<String>,
) -> Result<Option<DocumentAiConfiguration>> {
    match (project, location, processor) {
        (None, None, None) => Ok(None),
        (Some(project), Some(location), Some(processor)) => Ok(Some(
            DocumentAiConfiguration::new(project, location, processor)
                .context("DOCUMENT_AI_* processor settings are invalid")?,
        )),
        _ => {
            anyhow::bail!("DOCUMENT_AI_PROJECT_ID, LOCATION and PROCESSOR_ID must be set together")
        }
    }
}

fn setting(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
        Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("{name} is not valid UTF-8"),
    }
}

fn bounded(name: &str, default: usize, minimum: usize, maximum: usize) -> Result<usize> {
    let value = env::var(name)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .with_context(|| format!("{name} is invalid"))?
        .unwrap_or(default);
    if !(minimum..=maximum).contains(&value) {
        anyhow::bail!("{name} is outside its permitted range");
    }
    Ok(value)
}

fn required(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => anyhow::bail!("{name} is required"),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("{name} is not valid UTF-8"),
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
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{document_ai, require_matching_products, temporal_namespace};

    #[test]
    fn requires_a_dedicated_temporal_namespace() {
        assert_eq!(
            temporal_namespace("document-intelligence-sandbox").unwrap(),
            "document-intelligence-sandbox"
        );
        assert!(temporal_namespace("default").is_err());
        assert!(temporal_namespace("document-intelligence-prod-").is_err());
    }

    #[test]
    fn document_ai_is_enabled_only_by_a_complete_processor_setting() {
        let set = |value: &str| Some(value.to_owned());
        assert!(document_ai(None, None, None).unwrap().is_none());
        assert!(document_ai(
            set("tesseracthub-480811"),
            set("asia-south1"),
            set("395d58b2596d5031")
        )
        .unwrap()
        .is_some());
        assert!(document_ai(set("tesseracthub-480811"), None, set("395d58b2596d5031")).is_err());
        assert!(document_ai(set("tesseracthub-480811"), set("asia-south1"), set("a/b")).is_err());
    }

    #[test]
    fn rejects_bucket_routes_that_could_bypass_product_isolation() {
        let sources = HashMap::from([("kora".to_owned(), "source".to_owned())]);
        let pages = HashMap::from([("other".to_owned(), "pages".to_owned())]);
        assert!(
            require_matching_products(&sources, &pages, "SOURCE_BUCKETS", "PAGE_BUCKETS").is_err()
        );
    }
}
