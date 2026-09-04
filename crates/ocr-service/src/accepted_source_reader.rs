use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};

use object_store::{
    gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder},
    path::Path,
    GetOptions, ObjectStore,
};
use ocr_domain::{ProductId, TenantId};
use ocr_store::StoredAcceptedSource;
use thiserror::Error;
use tokio::time::timeout;

use crate::{
    document_reader::collect_verified, importer::DocumentReadError,
    result_artifacts::is_bucket_name,
};

const SOURCE_READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum AcceptedSourceReadError {
    #[error("accepted source is invalid")]
    Invalid,
    #[error("accepted source storage is unavailable")]
    Unavailable,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum AcceptedSourceReaderConfigurationError {
    #[error("accepted source routes are missing")]
    MissingRoutes,
    #[error("accepted source route is invalid")]
    InvalidRoute,
    #[error("accepted source reader client configuration is invalid")]
    Client,
}

pub type AcceptedSourceBytesReaderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, AcceptedSourceReadError>> + Send + 'a>>;

pub trait AcceptedSourceBytesReader: Send + Sync {
    fn read<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        source: &'a StoredAcceptedSource,
    ) -> AcceptedSourceBytesReaderFuture<'a>;
}

struct SourceRoute {
    bucket: String,
    store: GoogleCloudStorage,
}

pub struct GcsAcceptedSourceReader {
    routes: HashMap<String, SourceRoute>,
}

impl GcsAcceptedSourceReader {
    pub fn new(
        routes: &[(String, String)],
    ) -> Result<Self, AcceptedSourceReaderConfigurationError> {
        if routes.is_empty() {
            return Err(AcceptedSourceReaderConfigurationError::MissingRoutes);
        }
        let mut configured = HashMap::with_capacity(routes.len());
        for (product, bucket) in routes {
            ProductId::new(product)
                .map_err(|_| AcceptedSourceReaderConfigurationError::InvalidRoute)?;
            if !is_bucket_name(bucket) || configured.contains_key(product) {
                return Err(AcceptedSourceReaderConfigurationError::InvalidRoute);
            }
            let store = GoogleCloudStorageBuilder::new()
                .with_bucket_name(bucket)
                .build()
                .map_err(|_| AcceptedSourceReaderConfigurationError::Client)?;
            configured.insert(
                product.clone(),
                SourceRoute {
                    bucket: bucket.clone(),
                    store,
                },
            );
        }
        Ok(Self { routes: configured })
    }

    pub async fn read(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        source: &StoredAcceptedSource,
    ) -> Result<Vec<u8>, AcceptedSourceReadError> {
        timeout(
            SOURCE_READ_TIMEOUT,
            self.read_inner(product_id, tenant_id, source),
        )
        .await
        .map_err(|_| AcceptedSourceReadError::Unavailable)?
    }

    async fn read_inner(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        source: &StoredAcceptedSource,
    ) -> Result<Vec<u8>, AcceptedSourceReadError> {
        let route = self
            .routes
            .get(product_id.as_str())
            .ok_or(AcceptedSourceReadError::Invalid)?;
        let digest = source
            .digest
            .strip_prefix("sha256:")
            .ok_or(AcceptedSourceReadError::Invalid)?;
        let expected_name = format!(
            "products/{}/tenants/{}/documents/sha256/{digest}",
            product_id.as_str(),
            tenant_id.as_str(),
        );
        if source.bucket != route.bucket || source.object_name != expected_name {
            return Err(AcceptedSourceReadError::Invalid);
        }
        let path =
            Path::parse(&source.object_name).map_err(|_| AcceptedSourceReadError::Invalid)?;
        let expected_generation = source.generation.to_string();
        let result = route
            .store
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
            return Err(AcceptedSourceReadError::Invalid);
        }
        collect_verified(
            source.content_length,
            &source.digest,
            result.meta.size,
            result.into_stream(),
        )
        .await
        .map_err(map_document_error)
    }
}

impl AcceptedSourceBytesReader for GcsAcceptedSourceReader {
    fn read<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        source: &'a StoredAcceptedSource,
    ) -> AcceptedSourceBytesReaderFuture<'a> {
        Box::pin(async move { self.read(product_id, tenant_id, source).await })
    }
}

fn map_object_error(error: object_store::Error) -> AcceptedSourceReadError {
    match error {
        object_store::Error::NotFound { .. } | object_store::Error::Precondition { .. } => {
            AcceptedSourceReadError::Invalid
        }
        _ => AcceptedSourceReadError::Unavailable,
    }
}

fn map_document_error(error: DocumentReadError) -> AcceptedSourceReadError {
    match error {
        DocumentReadError::Invalid => AcceptedSourceReadError::Invalid,
        DocumentReadError::Unavailable => AcceptedSourceReadError::Unavailable,
    }
}
