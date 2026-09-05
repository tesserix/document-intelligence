use std::collections::HashMap;

use bytes::Bytes;
use futures_util::StreamExt;
use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    ObjectStoreExt,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{sync::mpsc, task};

use crate::{
    digest::sha256_digest, result_artifacts::is_bucket_name, StoredUpload, UploadArtifactError,
    UploadArtifactReadFuture, UploadArtifactReader, VerifiedUploadArtifact, MAXIMUM_UPLOAD_BYTES,
};

#[derive(Debug, Error)]
pub enum UploadArtifactConfigurationError {
    #[error("at least one quarantine bucket is required")]
    MissingBuckets,
    #[error("quarantine bucket name is invalid")]
    InvalidBucket,
    #[error("quarantine bucket client configuration is invalid")]
    Client,
}

pub struct GcsUploadArtifactReader {
    stores: HashMap<String, GoogleCloudStorage>,
}

impl GcsUploadArtifactReader {
    pub fn new(buckets: &[String]) -> Result<Self, UploadArtifactConfigurationError> {
        if buckets.is_empty() {
            return Err(UploadArtifactConfigurationError::MissingBuckets);
        }
        let mut stores = HashMap::with_capacity(buckets.len());
        for bucket in buckets {
            if !is_bucket_name(bucket) || stores.contains_key(bucket) {
                return Err(UploadArtifactConfigurationError::InvalidBucket);
            }
            let store = GoogleCloudStorageBuilder::new()
                .with_bucket_name(bucket)
                .build()
                .map_err(|_| UploadArtifactConfigurationError::Client)?;
            stores.insert(bucket.clone(), store);
        }
        Ok(Self { stores })
    }
}

impl UploadArtifactReader for GcsUploadArtifactReader {
    fn verify<'a>(&'a self, upload: &'a StoredUpload) -> UploadArtifactReadFuture<'a> {
        Box::pin(async move {
            let store = self
                .stores
                .get(&upload.object_bucket)
                .ok_or(UploadArtifactError::Unavailable)?;
            let path =
                Path::parse(&upload.object_name).map_err(|_| UploadArtifactError::Invalid)?;
            let result = store.get(&path).await.map_err(map_object_error)?;
            let generation = result
                .meta
                .version
                .as_deref()
                .ok_or(UploadArtifactError::Invalid)?
                .parse::<i64>()
                .map_err(|_| UploadArtifactError::Invalid)?;
            if generation <= 0 {
                return Err(UploadArtifactError::Invalid);
            }
            let size = result.meta.size;
            let mut stream = result.into_stream();
            verify_stream(upload, generation, size, &mut stream).await
        })
    }
}

async fn verify_stream(
    upload: &StoredUpload,
    generation: i64,
    size: u64,
    stream: &mut (impl futures_util::Stream<Item = object_store::Result<Bytes>> + Unpin),
) -> Result<VerifiedUploadArtifact, UploadArtifactError> {
    let expected_length =
        u64::try_from(upload.expected_content_length).map_err(|_| UploadArtifactError::Invalid)?;
    let maximum_length =
        u64::try_from(MAXIMUM_UPLOAD_BYTES).map_err(|_| UploadArtifactError::Invalid)?;
    if size != expected_length || size > maximum_length {
        return Err(UploadArtifactError::Invalid);
    }
    let (sender, mut receiver) = mpsc::channel::<Bytes>(4);
    let hash_task = task::spawn_blocking(move || {
        let mut hasher = Sha256::new();
        while let Some(chunk) = receiver.blocking_recv() {
            hasher.update(&chunk);
        }
        sha256_digest(hasher.finalize())
    });
    let stream_result = async {
        let mut count = 0_u64;
        let mut prefix = Vec::with_capacity(12);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_object_error)?;
            count = count
                .checked_add(u64::try_from(chunk.len()).map_err(|_| UploadArtifactError::Invalid)?)
                .ok_or(UploadArtifactError::Invalid)?;
            if count > maximum_length || count > expected_length {
                return Err(UploadArtifactError::Invalid);
            }
            if prefix.len() < 12 {
                let remaining = 12 - prefix.len();
                prefix.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            sender
                .send(chunk)
                .await
                .map_err(|_| UploadArtifactError::Unavailable)?;
        }
        Ok::<_, UploadArtifactError>((count, prefix))
    }
    .await;
    drop(sender);
    let digest = hash_task
        .await
        .map_err(|_| UploadArtifactError::Unavailable)?;
    let (content_length, prefix) = stream_result?;
    let content_type = detect_content_type(&prefix).ok_or(UploadArtifactError::Invalid)?;
    if content_length != expected_length
        || content_type != upload.expected_content_type
        || digest != upload.expected_digest
    {
        return Err(UploadArtifactError::Invalid);
    }
    Ok(VerifiedUploadArtifact {
        object_generation: generation,
        content_type: content_type.to_owned(),
        content_length: i64::try_from(content_length).map_err(|_| UploadArtifactError::Invalid)?,
        digest,
    })
}

fn map_object_error(error: object_store::Error) -> UploadArtifactError {
    match error {
        object_store::Error::NotFound { .. } => UploadArtifactError::NotFound,
        _ => UploadArtifactError::Unavailable,
    }
}

fn detect_content_type(prefix: &[u8]) -> Option<&'static str> {
    if prefix.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if prefix.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if prefix.starts_with(b"II*\0") || prefix.starts_with(b"MM\0*") {
        Some("image/tiff")
    } else if prefix.len() >= 12 && &prefix[..4] == b"RIFF" && &prefix[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_content_type, sha256_digest, verify_stream};
    use bytes::Bytes;
    use futures_util::{stream, StreamExt};
    use ocr_domain::UploadId;
    use ocr_store::{StoredUpload, UploadState};
    use sha2::{Digest, Sha256};
    use time::OffsetDateTime;

    #[test]
    fn content_type_comes_from_magic_bytes() {
        let cases: &[(&[u8], Option<&str>)] = &[
            (b"%PDF-1.7", Some("application/pdf")),
            (b"\x89PNG\r\n\x1a\nrest", Some("image/png")),
            (b"\xff\xd8\xffrest", Some("image/jpeg")),
            (b"II*\0rest", Some("image/tiff")),
            (b"MM\0*rest", Some("image/tiff")),
            (b"RIFF1234WEBPrest", Some("image/webp")),
            (b"<html>", None),
        ];
        for (prefix, expected) in cases {
            assert_eq!(detect_content_type(prefix), *expected, "prefix {prefix:?}");
        }
    }

    #[tokio::test]
    async fn stream_verification_checks_size_digest_and_detected_type() {
        let bytes = b"%PDF-1.7\nsynthetic";
        let upload = StoredUpload {
            upload_id: UploadId::new("upl_STREAM").unwrap(),
            state: UploadState::Reserved,
            object_bucket: "dev-kora-ocr-quarantine".to_owned(),
            object_name: "products/kora/tenants/ten_TEST/quarantine/upl_STREAM".to_owned(),
            expected_content_type: "application/pdf".to_owned(),
            expected_content_length: i64::try_from(bytes.len()).unwrap(),
            expected_digest: sha256_digest(Sha256::digest(bytes)),
            expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(10),
            created_at: OffsetDateTime::now_utc(),
            object_generation: None,
            uploaded_at: None,
        };
        let mut valid = stream::iter([
            Ok(Bytes::from_static(b"%PD")),
            Ok(Bytes::from_static(b"F-1.7\nsynthetic")),
        ])
        .boxed();
        let verified = verify_stream(&upload, 71, u64::try_from(bytes.len()).unwrap(), &mut valid)
            .await
            .unwrap();
        assert_eq!(verified.object_generation, 71);
        assert_eq!(verified.digest, upload.expected_digest);
        assert_eq!(verified.content_type, "application/pdf");

        let mut wrong_type = stream::iter([Ok(Bytes::from_static(b"<html>not a pdf!!"))]).boxed();
        assert!(verify_stream(
            &upload,
            72,
            u64::try_from(bytes.len()).unwrap(),
            &mut wrong_type,
        )
        .await
        .is_err());
    }
}
