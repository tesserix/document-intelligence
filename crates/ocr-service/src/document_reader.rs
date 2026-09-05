use std::collections::HashMap;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    GetOptions, ObjectStore,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::task;

use crate::{
    digest::sha256_digest,
    importer::{DocumentReadError, UploadDocumentReader},
    result_artifacts::is_bucket_name,
    StoredUpload, MAXIMUM_UPLOAD_BYTES,
};
use ocr_store::UploadState;

#[derive(Debug, Error)]
pub enum GcsDocumentReaderConfigurationError {
    #[error("at least one quarantine bucket is required")]
    MissingBuckets,
    #[error("quarantine bucket name is invalid")]
    InvalidBucket,
    #[error("quarantine bucket client configuration is invalid")]
    Client,
}

pub struct GcsUploadDocumentReader {
    stores: HashMap<String, GoogleCloudStorage>,
}

impl GcsUploadDocumentReader {
    pub fn new(buckets: &[String]) -> Result<Self, GcsDocumentReaderConfigurationError> {
        if buckets.is_empty() {
            return Err(GcsDocumentReaderConfigurationError::MissingBuckets);
        }
        let mut stores = HashMap::with_capacity(buckets.len());
        for bucket in buckets {
            if !is_bucket_name(bucket) || stores.contains_key(bucket) {
                return Err(GcsDocumentReaderConfigurationError::InvalidBucket);
            }
            let store = GoogleCloudStorageBuilder::new()
                .with_bucket_name(bucket)
                .build()
                .map_err(|_| GcsDocumentReaderConfigurationError::Client)?;
            stores.insert(bucket.clone(), store);
        }
        Ok(Self { stores })
    }
}

impl UploadDocumentReader for GcsUploadDocumentReader {
    async fn read<'a>(&'a self, upload: &'a StoredUpload) -> Result<Vec<u8>, DocumentReadError> {
        if upload.state != UploadState::Inspecting {
            return Err(DocumentReadError::Invalid);
        }
        let generation = upload
            .object_generation
            .filter(|generation| *generation > 0)
            .ok_or(DocumentReadError::Invalid)?;
        let store = self
            .stores
            .get(&upload.object_bucket)
            .ok_or(DocumentReadError::Invalid)?;
        let path = Path::parse(&upload.object_name).map_err(|_| DocumentReadError::Invalid)?;
        let expected_generation = generation.to_string();
        let result = store
            .get_opts(
                &path,
                GetOptions {
                    version: Some(expected_generation.clone()),
                    ..GetOptions::default()
                },
            )
            .await
            .map_err(map_object_error)?;
        if result.meta.version.as_deref() != Some(expected_generation.as_str()) {
            return Err(DocumentReadError::Invalid);
        }
        collect_verified(
            upload.expected_content_length,
            &upload.expected_digest,
            result.meta.size,
            result.into_stream(),
        )
        .await
    }
}

pub(crate) async fn collect_verified<S>(
    expected_content_length: i64,
    expected_digest: &str,
    size: u64,
    mut stream: S,
) -> Result<Vec<u8>, DocumentReadError>
where
    S: Stream<Item = object_store::Result<Bytes>> + Unpin,
{
    let expected_length =
        u64::try_from(expected_content_length).map_err(|_| DocumentReadError::Invalid)?;
    let maximum_length =
        u64::try_from(MAXIMUM_UPLOAD_BYTES).map_err(|_| DocumentReadError::Invalid)?;
    if size != expected_length || size > maximum_length {
        return Err(DocumentReadError::Invalid);
    }
    let capacity = usize::try_from(size).map_err(|_| DocumentReadError::Invalid)?;
    let mut bytes = Vec::with_capacity(capacity);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_object_error)?;
        if bytes.len().saturating_add(chunk.len()) > capacity {
            return Err(DocumentReadError::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != capacity {
        return Err(DocumentReadError::Invalid);
    }
    let (bytes, digest) = task::spawn_blocking(move || {
        let digest = sha256_digest(Sha256::digest(&bytes));
        (bytes, digest)
    })
    .await
    .map_err(|_| DocumentReadError::Unavailable)?;
    if digest != expected_digest {
        return Err(DocumentReadError::Invalid);
    }
    Ok(bytes)
}

fn map_object_error(error: object_store::Error) -> DocumentReadError {
    match error {
        object_store::Error::NotFound { .. } | object_store::Error::Precondition { .. } => {
            DocumentReadError::Invalid
        }
        _ => DocumentReadError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_verified, sha256_digest};
    use bytes::Bytes;
    use futures_util::stream;
    use ocr_domain::UploadId;
    use ocr_store::{StoredUpload, UploadState};
    use sha2::{Digest, Sha256};
    use time::OffsetDateTime;

    #[tokio::test]
    async fn exact_generation_stream_is_bounded_and_digest_verified() {
        let bytes = b"%PDF-1.7 fixture";
        let upload = StoredUpload {
            upload_id: UploadId::new("upl_STREAM_READER").unwrap(),
            state: UploadState::Inspecting,
            object_bucket: "dev-kora-ocr-quarantine".to_owned(),
            object_name: "products/kora/tenants/ten_TEST/quarantine/upl_STREAM_READER".to_owned(),
            expected_content_type: "application/pdf".to_owned(),
            expected_content_length: i64::try_from(bytes.len()).unwrap(),
            expected_digest: sha256_digest(Sha256::digest(bytes)),
            expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(10),
            created_at: OffsetDateTime::now_utc(),
            object_generation: Some(42),
            uploaded_at: Some(OffsetDateTime::now_utc()),
        };
        let valid = stream::iter([
            Ok(Bytes::from_static(b"%PDF-1.7 ")),
            Ok(Bytes::from_static(b"fixture")),
        ]);
        assert_eq!(
            collect_verified(
                upload.expected_content_length,
                &upload.expected_digest,
                u64::try_from(bytes.len()).unwrap(),
                valid,
            )
            .await
            .unwrap(),
            bytes
        );
        let wrong = stream::iter([Ok(Bytes::from_static(b"%PDF-1.7 changed"))]);
        assert!(collect_verified(
            upload.expected_content_length,
            &upload.expected_digest,
            u64::try_from(bytes.len()).unwrap(),
            wrong,
        )
        .await
        .is_err());
    }
}
