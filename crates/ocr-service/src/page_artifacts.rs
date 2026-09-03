use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};

use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    GetOptions, ObjectStore, ObjectStoreExt, PutMode,
};
use ocr_domain::{DocumentPage, JobId, PageTask, ProductId, TenantId};
use ocr_store::StoredPageArtifact;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::timeout;

use crate::{result_artifacts::is_bucket_name, MAXIMUM_RESULT_BYTES};

const PAGE_ARTIFACT_IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum PageArtifactReadError {
    #[error("page artifact is invalid")]
    Invalid,
    #[error("page artifact was not found")]
    NotFound,
    #[error("page artifact storage is unavailable")]
    Unavailable,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum PageArtifactConfigurationError {
    #[error("page artifact buckets are missing")]
    MissingBuckets,
    #[error("page artifact bucket is invalid")]
    InvalidBucket,
    #[error("page artifact product is invalid")]
    InvalidProduct,
    #[error("page artifact client configuration is invalid")]
    Client,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum PageArtifactWriteError {
    #[error("page artifact is invalid")]
    Invalid,
    #[error("page artifact conflicts with an existing object")]
    Conflict,
    #[error("page artifact storage is unavailable")]
    Unavailable,
}

pub type PageArtifactReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, PageArtifactReadError>> + Send + 'a>>;

pub trait PageArtifactReader: Send + Sync {
    fn read<'a>(
        &'a self,
        artifact: &'a StoredPageArtifact,
        maximum_bytes: usize,
    ) -> PageArtifactReadFuture<'a>;
}

pub struct GcsPageArtifactReader {
    buckets: HashMap<String, GoogleCloudStorage>,
}

struct PageWriteRoute {
    bucket: String,
    store: GoogleCloudStorage,
}

pub struct GcsPageArtifactWriter {
    routes: HashMap<String, PageWriteRoute>,
}

impl GcsPageArtifactWriter {
    pub fn new(
        product_buckets: HashMap<String, String>,
    ) -> Result<Self, PageArtifactConfigurationError> {
        if product_buckets.is_empty() {
            return Err(PageArtifactConfigurationError::MissingBuckets);
        }
        let mut routes = HashMap::with_capacity(product_buckets.len());
        let mut buckets = std::collections::HashSet::with_capacity(product_buckets.len());
        for (product, bucket) in product_buckets {
            ProductId::new(&product).map_err(|_| PageArtifactConfigurationError::InvalidProduct)?;
            if !is_bucket_name(&bucket) || !buckets.insert(bucket.clone()) {
                return Err(PageArtifactConfigurationError::InvalidBucket);
            }
            let store = GoogleCloudStorageBuilder::new()
                .with_bucket_name(&bucket)
                .build()
                .map_err(|_| PageArtifactConfigurationError::Client)?;
            routes.insert(product, PageWriteRoute { bucket, store });
        }
        Ok(Self { routes })
    }

    pub async fn write(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        job_id: &JobId,
        task: &PageTask,
        page: &DocumentPage,
    ) -> Result<StoredPageArtifact, PageArtifactWriteError> {
        timeout(
            PAGE_ARTIFACT_IO_TIMEOUT,
            self.write_inner(product_id, tenant_id, job_id, task, page),
        )
        .await
        .map_err(|_| PageArtifactWriteError::Unavailable)?
    }

    async fn write_inner(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        job_id: &JobId,
        task: &PageTask,
        page: &DocumentPage,
    ) -> Result<StoredPageArtifact, PageArtifactWriteError> {
        if u32::from(page.page) != task.page
            || task.activity_key
                != format!(
                    "ocr-job-{}-page-{}-attempt-{}",
                    job_id.as_str(),
                    task.page,
                    task.attempt
                )
        {
            return Err(PageArtifactWriteError::Invalid);
        }
        let route = self
            .routes
            .get(product_id.as_str())
            .ok_or(PageArtifactWriteError::Invalid)?;
        let bytes = serde_json::to_vec(page).map_err(|_| PageArtifactWriteError::Invalid)?;
        if bytes.is_empty() || bytes.len() > MAXIMUM_RESULT_BYTES {
            return Err(PageArtifactWriteError::Invalid);
        }
        let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        let object_name = format!(
            "products/{}/tenants/{}/pages/{}/{}/attempt-{}.json",
            product_id.as_str(),
            tenant_id.as_str(),
            job_id.as_str(),
            task.page,
            task.attempt
        );
        let path = Path::parse(&object_name).map_err(|_| PageArtifactWriteError::Invalid)?;
        match route
            .store
            .put_opts(&path, bytes.clone().into(), PutMode::Create.into())
            .await
        {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
            Err(_) => return Err(PageArtifactWriteError::Unavailable),
        }
        let stored = route
            .store
            .get(&path)
            .await
            .map_err(|_| PageArtifactWriteError::Unavailable)?;
        let generation = stored
            .meta
            .version
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or(PageArtifactWriteError::Unavailable)?;
        if stored.meta.size
            != u64::try_from(bytes.len()).map_err(|_| PageArtifactWriteError::Invalid)?
        {
            return Err(PageArtifactWriteError::Conflict);
        }
        let persisted = stored
            .bytes()
            .await
            .map_err(|_| PageArtifactWriteError::Unavailable)?;
        if persisted.as_ref() != bytes.as_slice()
            || format!("sha256:{:x}", Sha256::digest(&persisted)) != digest
        {
            return Err(PageArtifactWriteError::Conflict);
        }
        Ok(StoredPageArtifact {
            page: task.page,
            attempt: task.attempt,
            activity_key: task.activity_key.clone(),
            object_bucket: route.bucket.clone(),
            object_name,
            object_generation: generation,
            object_digest: digest,
            content_length: i64::try_from(bytes.len())
                .map_err(|_| PageArtifactWriteError::Invalid)?,
        })
    }
}

impl GcsPageArtifactReader {
    pub fn new(buckets: &[String]) -> Result<Self, PageArtifactConfigurationError> {
        if buckets.is_empty() {
            return Err(PageArtifactConfigurationError::MissingBuckets);
        }
        let mut stores = HashMap::with_capacity(buckets.len());
        for bucket in buckets {
            if !is_bucket_name(bucket) || stores.contains_key(bucket) {
                return Err(PageArtifactConfigurationError::InvalidBucket);
            }
            let store = GoogleCloudStorageBuilder::new()
                .with_bucket_name(bucket)
                .build()
                .map_err(|_| PageArtifactConfigurationError::Client)?;
            stores.insert(bucket.clone(), store);
        }
        Ok(Self { buckets: stores })
    }
}

impl PageArtifactReader for GcsPageArtifactReader {
    fn read<'a>(
        &'a self,
        artifact: &'a StoredPageArtifact,
        maximum_bytes: usize,
    ) -> PageArtifactReadFuture<'a> {
        Box::pin(async move {
            timeout(PAGE_ARTIFACT_IO_TIMEOUT, async {
                if maximum_bytes == 0 || maximum_bytes > MAXIMUM_RESULT_BYTES {
                    return Err(PageArtifactReadError::Invalid);
                }
                let store = self
                    .buckets
                    .get(&artifact.object_bucket)
                    .ok_or(PageArtifactReadError::Invalid)?;
                let path = Path::parse(&artifact.object_name)
                    .map_err(|_| PageArtifactReadError::Invalid)?;
                let generation = artifact.object_generation.to_string();
                let result = match store
                    .get_opts(
                        &path,
                        GetOptions::new().with_version(Some(generation.clone())),
                    )
                    .await
                {
                    Ok(result) => result,
                    Err(object_store::Error::NotFound { .. }) => {
                        return Err(PageArtifactReadError::NotFound);
                    }
                    Err(_) => return Err(PageArtifactReadError::Unavailable),
                };
                let expected_length = u64::try_from(artifact.content_length)
                    .map_err(|_| PageArtifactReadError::Invalid)?;
                let maximum_bytes =
                    u64::try_from(maximum_bytes).map_err(|_| PageArtifactReadError::Invalid)?;
                if result.meta.version.as_deref() != Some(generation.as_str())
                    || result.meta.size != expected_length
                    || result.meta.size > maximum_bytes
                {
                    return Err(PageArtifactReadError::Invalid);
                }
                let bytes = result
                    .bytes()
                    .await
                    .map_err(|_| PageArtifactReadError::Unavailable)?;
                if bytes.len()
                    != usize::try_from(expected_length)
                        .map_err(|_| PageArtifactReadError::Invalid)?
                    || format!("sha256:{:x}", Sha256::digest(&bytes)) != artifact.object_digest
                {
                    return Err(PageArtifactReadError::Invalid);
                }
                Ok(bytes.to_vec())
            })
            .await
            .map_err(|_| PageArtifactReadError::Unavailable)?
        })
    }
}
