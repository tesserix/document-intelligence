use std::collections::HashMap;

use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    GetOptions, ObjectStore,
};
use thiserror::Error;

use crate::{ResultArtifactError, ResultArtifactReader, ResultReadFuture};
use ocr_store::StoredResultLocator;

#[derive(Debug, Error)]
pub enum ResultArtifactConfigurationError {
    #[error("at least one result bucket is required")]
    MissingBuckets,
    #[error("result bucket name is invalid")]
    InvalidBucket,
    #[error("result bucket client configuration is invalid")]
    Client,
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

fn is_bucket_name(value: &str) -> bool {
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
