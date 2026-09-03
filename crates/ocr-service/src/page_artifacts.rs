use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};

use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    GetOptions, ObjectStore,
};
use ocr_store::StoredPageArtifact;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::timeout;

use crate::{result_artifacts::is_bucket_name, MAXIMUM_RESULT_BYTES};

const PAGE_ARTIFACT_READ_TIMEOUT: Duration = Duration::from_secs(30);

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
    #[error("page artifact client configuration is invalid")]
    Client,
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
            timeout(PAGE_ARTIFACT_READ_TIMEOUT, async {
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
