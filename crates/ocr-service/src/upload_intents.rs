use crate::storage_client::StorageClient;
use std::{collections::HashMap, time::Duration};

use object_store::path::Path;
use thiserror::Error;

use crate::{
    result_artifacts::is_bucket_name, IssuedUpload, StoredUpload, UploadIntentIssuer,
    UploadIssueError, UploadIssueFuture,
};

#[derive(Debug, Error)]
pub enum UploadIntentConfigurationError {
    #[error("at least one quarantine bucket is required")]
    MissingBuckets,
    #[error("quarantine bucket name is invalid")]
    InvalidBucket,
    #[error("quarantine bucket client configuration is invalid")]
    Client,
}

pub struct GcsUploadIssuer {
    stores: HashMap<String, StorageClient>,
}

impl GcsUploadIssuer {
    pub fn new(buckets: &[String]) -> Result<Self, UploadIntentConfigurationError> {
        if buckets.is_empty() {
            return Err(UploadIntentConfigurationError::MissingBuckets);
        }
        let mut stores = HashMap::with_capacity(buckets.len());
        for bucket in buckets {
            if !is_bucket_name(bucket) || stores.contains_key(bucket) {
                return Err(UploadIntentConfigurationError::InvalidBucket);
            }
            let store =
                StorageClient::new(bucket).map_err(|_| UploadIntentConfigurationError::Client)?;
            stores.insert(bucket.clone(), store);
        }
        Ok(Self { stores })
    }
}

impl UploadIntentIssuer for GcsUploadIssuer {
    fn issue<'a>(&'a self, upload: &'a StoredUpload) -> UploadIssueFuture<'a> {
        Box::pin(async move {
            let store = self
                .stores
                .get(&upload.object_bucket)
                .ok_or(UploadIssueError)?;
            let path = Path::parse(&upload.object_name).map_err(|_| UploadIssueError)?;
            let remaining = upload.expires_at - time::OffsetDateTime::now_utc();
            let seconds = u64::try_from(remaining.whole_seconds()).map_err(|_| UploadIssueError)?;
            if seconds == 0 {
                return Err(UploadIssueError);
            }
            let (upload_url, required_headers) = store
                .signed_upload(
                    &path,
                    Duration::from_secs(seconds),
                    &upload.expected_content_type,
                )
                .await
                .map_err(|_| UploadIssueError)?;
            Ok(IssuedUpload {
                upload_url,
                required_headers,
            })
        })
    }
}
