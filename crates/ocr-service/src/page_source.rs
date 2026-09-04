use std::{future::Future, pin::Pin, sync::Arc};

use ocr_domain::{JobId, PageGeometry, PageTask, ProductId, TenantId};
use ocr_store::{PgJobStore, StoredAcceptedSource};

use crate::{AcceptedSourceBytesReader, AcceptedSourceReadError};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PageSourceError {
    NotFound,
    Invalid,
    Unavailable,
}

pub type AcceptedSourceRepositoryFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Option<StoredAcceptedSource>, ocr_store::Error>> + Send + 'a>,
>;

pub trait AcceptedSourceRepository: Send + Sync {
    fn load_accepted_source<'a>(
        &'a self,
        tenant_id: &'a TenantId,
        product_id: &'a ProductId,
        job_id: &'a JobId,
    ) -> AcceptedSourceRepositoryFuture<'a>;
}

impl AcceptedSourceRepository for PgJobStore {
    fn load_accepted_source<'a>(
        &'a self,
        tenant_id: &'a TenantId,
        product_id: &'a ProductId,
        job_id: &'a JobId,
    ) -> AcceptedSourceRepositoryFuture<'a> {
        Box::pin(async move {
            PgJobStore::load_accepted_source(self, tenant_id, product_id, job_id).await
        })
    }
}

pub struct AcceptedPageSource {
    bytes: Vec<u8>,
    content_type: String,
    geometry: PageGeometry,
}

impl AcceptedPageSource {
    pub(crate) fn from_verified(
        bytes: Vec<u8>,
        content_type: String,
        geometry: PageGeometry,
    ) -> Self {
        Self {
            bytes,
            content_type,
            geometry,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn geometry(&self) -> PageGeometry {
        self.geometry
    }

    pub(crate) fn into_bytes_and_content_type(self) -> (Vec<u8>, String) {
        (self.bytes, self.content_type)
    }
}

pub type PageSourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AcceptedPageSource, PageSourceError>> + Send + 'a>>;

pub trait PageSourceResolver: Send + Sync {
    fn load<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        job_id: &'a JobId,
        task: &'a PageTask,
    ) -> PageSourceFuture<'a>;
}

pub struct AcceptedPageSourceLoader<R, B> {
    repository: Arc<R>,
    reader: Arc<B>,
}

impl<R, B> AcceptedPageSourceLoader<R, B>
where
    R: AcceptedSourceRepository,
    B: AcceptedSourceBytesReader,
{
    pub fn new(repository: Arc<R>, reader: Arc<B>) -> Self {
        Self { repository, reader }
    }

    async fn load_inner(
        &self,
        product_id: &ProductId,
        tenant_id: &TenantId,
        job_id: &JobId,
        task: &PageTask,
    ) -> Result<AcceptedPageSource, PageSourceError> {
        let source = self
            .repository
            .load_accepted_source(tenant_id, product_id, job_id)
            .await
            .map_err(map_store_error)?
            .ok_or(PageSourceError::NotFound)?;
        let geometry = page_geometry(&source, task.page)?;
        let bytes = self
            .reader
            .read(product_id, tenant_id, &source)
            .await
            .map_err(|error| match error {
                AcceptedSourceReadError::Invalid => PageSourceError::Invalid,
                AcceptedSourceReadError::Unavailable => PageSourceError::Unavailable,
            })?;
        if i64::try_from(bytes.len()).ok() != Some(source.content_length) {
            return Err(PageSourceError::Invalid);
        }
        Ok(AcceptedPageSource::from_verified(
            bytes,
            source.content_type,
            geometry,
        ))
    }
}

impl<R, B> PageSourceResolver for AcceptedPageSourceLoader<R, B>
where
    R: AcceptedSourceRepository,
    B: AcceptedSourceBytesReader,
{
    fn load<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        job_id: &'a JobId,
        task: &'a PageTask,
    ) -> PageSourceFuture<'a> {
        Box::pin(async move { self.load_inner(product_id, tenant_id, job_id, task).await })
    }
}

fn page_geometry(
    source: &StoredAcceptedSource,
    page: u32,
) -> Result<PageGeometry, PageSourceError> {
    if page == 0
        || source.page_count == 0
        || source.page_count != u32::try_from(source.page_geometries.len()).unwrap_or_default()
    {
        return Err(PageSourceError::Invalid);
    }
    source
        .page_geometries
        .get(usize::try_from(page - 1).map_err(|_| PageSourceError::Invalid)?)
        .copied()
        .filter(|geometry| u32::from(geometry.page) == page)
        .ok_or(PageSourceError::Invalid)
}

fn map_store_error(error: ocr_store::Error) -> PageSourceError {
    match error {
        ocr_store::Error::InvalidStoredJob
        | ocr_store::Error::InvalidStoredUpload
        | ocr_store::Error::UploadSourceUnavailable => PageSourceError::Invalid,
        ocr_store::Error::Database(_)
        | ocr_store::Error::IdempotencyConflict
        | ocr_store::Error::InvalidStoredResult
        | ocr_store::Error::InvalidOutboxEvent
        | ocr_store::Error::InvalidStoredPageWorkflow
        | ocr_store::Error::InvalidStoredPageArtifact
        | ocr_store::Error::InvalidWorkScope => PageSourceError::Unavailable,
    }
}
