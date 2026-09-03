use std::{future::Future, pin::Pin, sync::Arc};

use ocr_domain::{DocumentPage, JobId, PageTask, ProductId, TenantId};
use thiserror::Error;

use crate::{
    PageArtifactWriteError, PageArtifactWriter, PageProcessError, PageProcessFuture, PageProcessor,
};

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum PageRecognitionError {
    #[error("page recognition is temporarily unavailable")]
    Retryable,
    #[error("page cannot be recognized")]
    Permanent,
}

pub type PageRecognitionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DocumentPage, PageRecognitionError>> + Send + 'a>>;

pub trait PageRecognizer: Send + Sync {
    fn recognize<'a>(&'a self, task: &'a PageTask) -> PageRecognitionFuture<'a>;
}

pub struct ArtifactPageProcessor<R, W> {
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    recognizer: Arc<R>,
    writer: Arc<W>,
}

impl<R, W> ArtifactPageProcessor<R, W>
where
    R: PageRecognizer,
    W: PageArtifactWriter,
{
    pub fn new(
        product_id: ProductId,
        tenant_id: TenantId,
        job_id: JobId,
        recognizer: Arc<R>,
        writer: Arc<W>,
    ) -> Self {
        Self {
            product_id,
            tenant_id,
            job_id,
            recognizer,
            writer,
        }
    }
}

impl<R, W> PageProcessor for ArtifactPageProcessor<R, W>
where
    R: PageRecognizer,
    W: PageArtifactWriter,
{
    fn process<'a>(&'a self, task: PageTask) -> PageProcessFuture<'a> {
        Box::pin(async move {
            let expected_key = format!(
                "ocr-job-{}-page-{}-attempt-{}",
                self.job_id.as_str(),
                task.page,
                task.attempt
            );
            if task.activity_key != expected_key {
                return Err(PageProcessError::Permanent);
            }
            let page = self
                .recognizer
                .recognize(&task)
                .await
                .map_err(|error| match error {
                    PageRecognitionError::Retryable => PageProcessError::Retryable,
                    PageRecognitionError::Permanent => PageProcessError::Permanent,
                })?;
            if u32::from(page.page) != task.page {
                return Err(PageProcessError::Permanent);
            }
            let artifact = self
                .writer
                .write(
                    &self.product_id,
                    &self.tenant_id,
                    &self.job_id,
                    &task,
                    &page,
                )
                .await
                .map_err(|error| match error {
                    PageArtifactWriteError::Unavailable => PageProcessError::Retryable,
                    PageArtifactWriteError::Invalid | PageArtifactWriteError::Conflict => {
                        PageProcessError::Permanent
                    }
                })?;
            if artifact.page != task.page
                || artifact.attempt != task.attempt
                || artifact.activity_key != task.activity_key
            {
                return Err(PageProcessError::Permanent);
            }
            Ok(artifact)
        })
    }
}
