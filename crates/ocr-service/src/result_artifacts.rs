use std::{collections::HashMap, time::Duration};

use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    GetOptions, ObjectStore, ObjectStoreExt, PutMode,
};
use ocr_domain::{DocumentResult, JobId, ProductId, TenantId};
use ocr_store::StoredResultLocator;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::timeout;

use crate::{ResultArtifactError, ResultArtifactReader, ResultReadFuture, MAXIMUM_RESULT_BYTES};

const RESULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum ResultArtifactConfigurationError {
    #[error("at least one result bucket is required")]
    MissingBuckets,
    #[error("result bucket name is invalid")]
    InvalidBucket,
    #[error("result product is invalid")]
    InvalidProduct,
    #[error("result bucket client configuration is invalid")]
    Client,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum ResultArtifactWriteError {
    #[error("result artifact is invalid")]
    Invalid,
    #[error("result artifact conflicts with an existing object")]
    Conflict,
    #[error("result artifact storage is unavailable")]
    Unavailable,
}

struct ResultWriteRoute {
    bucket: String,
    store: GoogleCloudStorage,
}

struct PreparedResultArtifact {
    bytes: Vec<u8>,
    digest: String,
    object_name: String,
}

pub struct GcsResultWriter {
    routes: HashMap<String, ResultWriteRoute>,
}

impl GcsResultWriter {
    pub fn new(
        product_buckets: HashMap<String, String>,
    ) -> Result<Self, ResultArtifactConfigurationError> {
        if product_buckets.is_empty() {
            return Err(ResultArtifactConfigurationError::MissingBuckets);
        }
        let mut routes = HashMap::with_capacity(product_buckets.len());
        let mut buckets = std::collections::HashSet::with_capacity(product_buckets.len());
        for (product, bucket) in product_buckets {
            ProductId::new(&product)
                .map_err(|_| ResultArtifactConfigurationError::InvalidProduct)?;
            if !is_bucket_name(&bucket) || !buckets.insert(bucket.clone()) {
                return Err(ResultArtifactConfigurationError::InvalidBucket);
            }
            let store = GoogleCloudStorageBuilder::new()
                .with_bucket_name(&bucket)
                .build()
                .map_err(|_| ResultArtifactConfigurationError::Client)?;
            routes.insert(product, ResultWriteRoute { bucket, store });
        }
        Ok(Self { routes })
    }

    pub async fn write(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        job_id: &JobId,
        result: &DocumentResult,
    ) -> Result<StoredResultLocator, ResultArtifactWriteError> {
        timeout(
            RESULT_WRITE_TIMEOUT,
            self.write_inner(product_id, tenant_id, job_id, result),
        )
        .await
        .map_err(|_| ResultArtifactWriteError::Unavailable)?
    }

    async fn write_inner(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        job_id: &JobId,
        result: &DocumentResult,
    ) -> Result<StoredResultLocator, ResultArtifactWriteError> {
        let route = self
            .routes
            .get(product_id.as_str())
            .ok_or(ResultArtifactWriteError::Invalid)?;
        let prepared = prepare_result_artifact(
            product_id.as_str(),
            tenant_id.as_str(),
            job_id.as_str(),
            result,
        )?;
        let path =
            Path::parse(&prepared.object_name).map_err(|_| ResultArtifactWriteError::Invalid)?;
        match route
            .store
            .put_opts(&path, prepared.bytes.clone().into(), PutMode::Create.into())
            .await
        {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
            Err(_) => return Err(ResultArtifactWriteError::Unavailable),
        }
        let stored = route
            .store
            .get(&path)
            .await
            .map_err(|_| ResultArtifactWriteError::Unavailable)?;
        let generation = stored
            .meta
            .version
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or(ResultArtifactWriteError::Unavailable)?;
        let expected_size =
            u64::try_from(prepared.bytes.len()).map_err(|_| ResultArtifactWriteError::Invalid)?;
        if stored.meta.size != expected_size {
            return Err(ResultArtifactWriteError::Conflict);
        }
        let persisted = stored
            .bytes()
            .await
            .map_err(|_| ResultArtifactWriteError::Unavailable)?;
        if persisted.as_ref() != prepared.bytes.as_slice()
            || format!("sha256:{:x}", Sha256::digest(&persisted)) != prepared.digest
        {
            return Err(ResultArtifactWriteError::Conflict);
        }
        Ok(StoredResultLocator {
            document_id: result.document_id.clone(),
            document_version: result.document_version.clone(),
            object_bucket: route.bucket.clone(),
            object_name: prepared.object_name,
            object_generation: generation,
            object_digest: prepared.digest,
            content_length: i64::try_from(prepared.bytes.len())
                .map_err(|_| ResultArtifactWriteError::Invalid)?,
        })
    }
}

fn prepare_result_artifact(
    product_id: &str,
    tenant_id: &str,
    job_id: &str,
    result: &DocumentResult,
) -> Result<PreparedResultArtifact, ResultArtifactWriteError> {
    ProductId::new(product_id).map_err(|_| ResultArtifactWriteError::Invalid)?;
    TenantId::new(tenant_id).map_err(|_| ResultArtifactWriteError::Invalid)?;
    JobId::new(job_id).map_err(|_| ResultArtifactWriteError::Invalid)?;
    let bytes = serde_json::to_vec(result).map_err(|_| ResultArtifactWriteError::Invalid)?;
    if bytes.is_empty() || bytes.len() > MAXIMUM_RESULT_BYTES {
        return Err(ResultArtifactWriteError::Invalid);
    }
    let version = String::from(result.document_version.clone());
    let version_digest = version
        .strip_prefix("sha256:")
        .ok_or(ResultArtifactWriteError::Invalid)?;
    Ok(PreparedResultArtifact {
        digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
        object_name: format!(
            "products/{product_id}/tenants/{tenant_id}/results/{job_id}/{version_digest}.json"
        ),
        bytes,
    })
}

pub struct GcsResultReader {
    stores: HashMap<String, GoogleCloudStorage>,
}

impl GcsResultReader {
    pub fn new(buckets: &[String]) -> Result<Self, ResultArtifactConfigurationError> {
        if buckets.is_empty() {
            return Err(ResultArtifactConfigurationError::MissingBuckets);
        }
        let mut stores = HashMap::with_capacity(buckets.len());
        for bucket in buckets {
            if !is_bucket_name(bucket) || stores.contains_key(bucket) {
                return Err(ResultArtifactConfigurationError::InvalidBucket);
            }
            let store = GoogleCloudStorageBuilder::new()
                .with_bucket_name(bucket)
                .build()
                .map_err(|_| ResultArtifactConfigurationError::Client)?;
            stores.insert(bucket.clone(), store);
        }
        Ok(Self { stores })
    }
}

impl ResultArtifactReader for GcsResultReader {
    fn read<'a>(
        &'a self,
        locator: &'a StoredResultLocator,
        maximum_bytes: usize,
    ) -> ResultReadFuture<'a> {
        Box::pin(async move {
            let store = self
                .stores
                .get(&locator.object_bucket)
                .ok_or(ResultArtifactError)?;
            let path = Path::parse(&locator.object_name).map_err(|_| ResultArtifactError)?;
            let generation = locator.object_generation.to_string();
            let result = store
                .get_opts(
                    &path,
                    GetOptions::new().with_version(Some(generation.clone())),
                )
                .await
                .map_err(|_| ResultArtifactError)?;
            let expected_length =
                u64::try_from(locator.content_length).map_err(|_| ResultArtifactError)?;
            let maximum_bytes = u64::try_from(maximum_bytes).map_err(|_| ResultArtifactError)?;
            if result.meta.version.as_deref() != Some(generation.as_str())
                || result.meta.size != expected_length
                || result.meta.size > maximum_bytes
            {
                return Err(ResultArtifactError);
            }
            let bytes = result.bytes().await.map_err(|_| ResultArtifactError)?;
            if bytes.len() > usize::try_from(maximum_bytes).map_err(|_| ResultArtifactError)? {
                return Err(ResultArtifactError);
            }
            Ok(bytes.to_vec())
        })
    }
}

pub(crate) fn is_bucket_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && !value.contains("..")
        && !value.starts_with("goog")
        && !value.contains("google")
}

#[cfg(test)]
mod tests {
    use ocr_domain::{DocumentId, DocumentResult, DocumentResultPayload, DocumentVersion, JobId};

    use super::prepare_result_artifact;

    #[test]
    fn prepared_result_uses_a_deterministic_non_user_controlled_path_and_digest() {
        let result = DocumentResult::new(
            DocumentId::new("doc_WRITER").unwrap(),
            DocumentVersion::new(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            DocumentResultPayload::default(),
        )
        .unwrap();

        let prepared = prepare_result_artifact(
            "kora",
            "ten_WRITER",
            JobId::new("job_WRITER").unwrap().as_str(),
            &result,
        )
        .unwrap();

        assert_eq!(
            prepared.object_name,
            format!(
                "products/kora/tenants/ten_WRITER/results/job_WRITER/{}.json",
                "a".repeat(64)
            )
        );
        assert_eq!(prepared.digest.len(), "sha256:".len() + 64);
        assert_eq!(
            serde_json::from_slice::<DocumentResult>(&prepared.bytes).unwrap(),
            result
        );
    }
}
