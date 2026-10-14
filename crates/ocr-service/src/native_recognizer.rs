use crate::{
    PageRecognitionError, PageRecognitionFuture, PageRecognizer, PageSourceError,
    PageSourceResolver, ParserProcess, ParserProcessError,
};
use ocr_domain::{DocumentPage, JobId, PageTask, ProductId, TenantId};
use ocr_store::PgJobStore;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, future::Future, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Notify, OnceCell};

type CachedPages = Arc<Vec<DocumentPage>>;
type CacheCell = Arc<OnceCell<CachedPages>>;
type CacheKey = (ProductId, TenantId, JobId);

struct NativeDocumentCache {
    capacity: usize,
    entries: Mutex<VecDeque<(CacheKey, CacheCell)>>,
    available: Arc<Notify>,
}

impl NativeDocumentCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(VecDeque::new()),
            available: Arc::new(Notify::new()),
        }
    }

    async fn load<F, Fut>(
        &self,
        product: &ProductId,
        tenant: &TenantId,
        job: &JobId,
        extract: F,
    ) -> Result<CachedDocument, PageRecognitionError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<DocumentPage>, PageRecognitionError>>,
    {
        let key = (product.clone(), tenant.clone(), job.clone());
        let cell = loop {
            let notified = self.available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut entries = self.entries.lock().await;
            if let Some(index) = entries.iter().position(|(existing, _)| *existing == key) {
                let entry = entries
                    .remove(index)
                    .ok_or(PageRecognitionError::Retryable)?;
                let cell = entry.1.clone();
                entries.push_back(entry);
                break cell;
            }
            if entries.len() >= self.capacity {
                if let Some(index) = entries.iter().position(|(_, cell)| {
                    Arc::strong_count(cell) == 1
                        && cell.get().is_none_or(|pages| Arc::strong_count(pages) == 1)
                }) {
                    entries.remove(index);
                } else {
                    drop(entries);
                    notified.await;
                    continue;
                }
            }
            let cell = Arc::new(OnceCell::new());
            entries.push_back((key.clone(), cell.clone()));
            break cell;
        };
        let lease = CellLease {
            cell,
            _signal: AvailabilitySignal(self.available.clone()),
        };
        let cell = &lease.cell;
        let loaded = cell
            .get_or_try_init(|| async { extract().await.map(Arc::new) })
            .await;
        match loaded {
            Ok(pages) => Ok(CachedDocument {
                pages: pages.clone(),
                _signal: AvailabilitySignal(self.available.clone()),
            }),
            Err(error) => {
                let mut entries = self.entries.lock().await;
                if Arc::strong_count(cell) == 2 {
                    entries
                        .retain(|(existing, entry)| *existing != key || !Arc::ptr_eq(entry, cell));
                }
                Err(error)
            }
        }
    }
}

struct AvailabilitySignal(Arc<Notify>);
impl Drop for AvailabilitySignal {
    fn drop(&mut self) {
        self.0.notify_waiters();
    }
}
// Drop data references before notifying slot waiters.
struct CellLease {
    cell: CacheCell,
    _signal: AvailabilitySignal,
}
struct CachedDocument {
    pages: CachedPages,
    _signal: AvailabilitySignal,
}
impl std::ops::Deref for CachedDocument {
    type Target = Vec<DocumentPage>;
    fn deref(&self) -> &Self::Target {
        &self.pages
    }
}

pub struct NativePageRecognizer<S> {
    source_loader: Arc<S>,
    parser: ParserProcess,
    cache: NativeDocumentCache,
    jobs: PgJobStore,
}

impl<S> NativePageRecognizer<S> {
    pub fn new(source_loader: Arc<S>, parser: ParserProcess, jobs: PgJobStore) -> Self {
        Self {
            source_loader,
            parser,
            cache: NativeDocumentCache::new(2),
            jobs,
        }
    }
}

impl<S: PageSourceResolver> PageRecognizer for NativePageRecognizer<S> {
    fn recognize<'a>(
        &'a self,
        product: &'a ProductId,
        tenant: &'a TenantId,
        job: &'a JobId,
        task: &'a PageTask,
    ) -> PageRecognitionFuture<'a> {
        Box::pin(async move {
            if let Some(page) = self
                .jobs
                .load_native_page(tenant, product, job, "native-text-v1", task.page)
                .await
                .map_err(|_| PageRecognitionError::Retryable)?
            {
                return Ok(page);
            }
            let pages = tokio::time::timeout(
                Duration::from_secs(75),
                self.cache.load(product, tenant, job, || async {
                    if let Some(pages) = self
                        .jobs
                        .load_native_pages(tenant, product, job, "native-text-v1")
                        .await
                        .map_err(|_| PageRecognitionError::Retryable)?
                    {
                        return Ok(pages);
                    }
                    let source = self
                        .source_loader
                        .load(product, tenant, job, task)
                        .await
                        .map_err(|error| match error {
                            PageSourceError::Unavailable => PageRecognitionError::Retryable,
                            PageSourceError::NotFound | PageSourceError::Invalid => {
                                PageRecognitionError::Permanent
                            }
                        })?;
                    let pages = self
                        .parser
                        .extract_native(source.bytes(), source.content_type())
                        .await
                        .map_err(|error| match error {
                            ParserProcessError::InvalidDocument
                            | ParserProcessError::PasswordRequired
                            | ParserProcessError::LimitsExceeded => PageRecognitionError::Permanent,
                            _ => PageRecognitionError::Retryable,
                        })?;
                    let page = pages
                        .iter()
                        .find(|page| u32::from(page.page) == task.page)
                        .ok_or(PageRecognitionError::Permanent)?;
                    let geometry = source.geometry();
                    let matches = page.width == geometry.width && page.height == geometry.height;
                    // Older accepted PDFs recorded unrotated dimensions.
                    let rotated_legacy =
                        page.width == geometry.height && page.height == geometry.width;
                    if !matches && !rotated_legacy {
                        return Err(PageRecognitionError::Permanent);
                    }
                    let accepted = self
                        .jobs
                        .load_accepted_source(tenant, product, job)
                        .await
                        .map_err(|_| PageRecognitionError::Retryable)?
                        .ok_or(PageRecognitionError::Permanent)?;
                    let mut pages = pages;
                    if source.content_type() == "image/tiff"
                        && accepted.page_count == 1
                        && pages.len() > 1
                    {
                        // Old accepted TIFFs reported only their first frame; new uploads report all frames.
                        pages.truncate(1);
                    }
                    let digest = format!(
                        "sha256:{}",
                        Sha256::digest(source.bytes())
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                    );
                    self.jobs
                        .save_native_pages(tenant, product, job, "native-text-v1", &digest, &pages)
                        .await
                        .map_err(|_| PageRecognitionError::Retryable)
                }),
            )
            .await
            .map_err(|_| PageRecognitionError::Retryable)??;
            pages
                .iter()
                .find(|page| u32::from(page.page) == task.page)
                .cloned()
                .ok_or(PageRecognitionError::Permanent)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::NativeDocumentCache;
    use crate::PageRecognitionError;
    use ocr_domain::{DocumentPage, JobId, PageNumber, ProductId, TenantId};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pages(width: u32) -> Vec<DocumentPage> {
        vec![DocumentPage::new(PageNumber::new(1).unwrap(), width, 10, vec![]).unwrap()]
    }

    #[tokio::test]
    async fn coalesces_concurrent_document_extraction_and_reuses_completed_text() {
        let cache = NativeDocumentCache::new(2);
        let product = ProductId::new("kora").unwrap();
        let tenant = TenantId::new("ten_A").unwrap();
        let job = JobId::new("job_A").unwrap();
        let calls = AtomicUsize::new(0);
        let load = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(pages(10))
        };
        let (first, second) = tokio::join!(
            cache.load(&product, &tenant, &job, load),
            cache.load(&product, &tenant, &job, load)
        );
        assert_eq!(first.unwrap()[0].width, 10);
        assert_eq!(second.unwrap()[0].width, 10);
        assert_eq!(
            cache.load(&product, &tenant, &job, load).await.unwrap()[0].width,
            10
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn never_reuses_another_tenants_document_and_does_not_cache_failures() {
        let cache = NativeDocumentCache::new(2);
        let product = ProductId::new("kora").unwrap();
        let tenant = TenantId::new("ten_A").unwrap();
        let other = TenantId::new("ten_B").unwrap();
        let job = JobId::new("job_A").unwrap();
        assert!(cache
            .load(&product, &tenant, &job, || async {
                Err(PageRecognitionError::Retryable)
            })
            .await
            .is_err());
        let first = cache
            .load(&product, &tenant, &job, || async { Ok(pages(10)) })
            .await
            .unwrap();
        let second = cache
            .load(&product, &other, &job, || async { Ok(pages(20)) })
            .await
            .unwrap();
        assert_eq!(first[0].width, 10);
        assert_eq!(second[0].width, 20);
    }

    #[tokio::test]
    async fn idle_cancelled_extractions_do_not_exhaust_the_cache() {
        let cache = NativeDocumentCache::new(1);
        let product = ProductId::new("kora").unwrap();
        let tenant = TenantId::new("ten_A").unwrap();
        let first = JobId::new("job_A").unwrap();
        let next = JobId::new("job_B").unwrap();
        let cancelled = cache.load(&product, &tenant, &first, || async {
            std::future::pending().await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), cancelled)
                .await
                .is_err()
        );
        assert_eq!(
            cache
                .load(&product, &tenant, &next, || async { Ok(pages(20)) })
                .await
                .unwrap()[0]
                .width,
            20
        );
    }
    #[tokio::test]
    async fn cache_pressure_waits_for_a_slot_instead_of_failing_valid_documents() {
        let cache = NativeDocumentCache::new(1);
        let product = ProductId::new("kora").unwrap();
        let tenant = TenantId::new("ten_A").unwrap();
        let first = JobId::new("job_A").unwrap();
        let second = JobId::new("job_B").unwrap();
        let first_request = async {
            let document = cache
                .load(&product, &tenant, &first, || async {
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    Ok(pages(10))
                })
                .await?;
            assert_eq!(document[0].width, 10);
            drop(document);
            Ok::<_, PageRecognitionError>(())
        };
        let second_request = async {
            let document = cache
                .load(&product, &tenant, &second, || async { Ok(pages(20)) })
                .await?;
            assert_eq!(document[0].width, 20);
            Ok::<_, PageRecognitionError>(())
        };
        let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(first_request, second_request)
        })
        .await
        .unwrap();
        assert!(first.is_ok());
        assert!(second.is_ok());
    }
}
