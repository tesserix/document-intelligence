use std::sync::Arc;

use ocr_domain::{JobId, PageTask, ProductId, TenantId};

use crate::{PageRecognitionError, PageRecognitionFuture, PageRecognizer};

/// Reads text layers natively and sends only pages without one, such as photos, to Document AI (ADR-0017).
pub struct HybridPageRecognizer<N, M> {
    native: Arc<N>,
    managed: Arc<M>,
}

impl<N, M> HybridPageRecognizer<N, M> {
    pub fn new(native: Arc<N>, managed: Arc<M>) -> Self {
        Self { native, managed }
    }
}

impl<N: PageRecognizer, M: PageRecognizer> PageRecognizer for HybridPageRecognizer<N, M> {
    fn recognize<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        job_id: &'a JobId,
        task: &'a PageTask,
    ) -> PageRecognitionFuture<'a> {
        Box::pin(async move {
            let page = self
                .native
                .recognize(product_id, tenant_id, job_id, task)
                .await?;
            if !page.observations.is_empty() {
                return Ok(page);
            }
            match self
                .managed
                .recognize(product_id, tenant_id, job_id, task)
                .await
            {
                // Pages Document AI cannot take stay text-free and are flagged for manual review.
                Err(PageRecognitionError::Permanent) => Ok(page),
                read => read,
            }
        })
    }
}
