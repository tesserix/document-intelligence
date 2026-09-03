use fred::prelude::{Builder, ClientLike, Config, Expiration, KeysInterface};
use ocr_domain::{JobId, JobState, ProductId, TenantId};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, time::Duration};
use thiserror::Error;
use time::OffsetDateTime;

const CACHE_NAMESPACE: &str = "ocr:job-status:v1";
const MAXIMUM_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAXIMUM_TIMEOUT: Duration = Duration::from_secs(1);
const MINIMUM_RECORD_BYTES: usize = 128;
const MAXIMUM_RECORD_BYTES: usize = 4 * 1024;

#[derive(Debug, Error)]
pub enum CacheConfigurationError {
    #[error("cache TTL is invalid")]
    InvalidTtl,
    #[error("cache timeout is invalid")]
    InvalidTimeout,
    #[error("cache record limit is invalid")]
    InvalidRecordLimit,
}

#[derive(Debug, Clone, Error)]
pub enum CacheRecordError {
    #[error("cache record exceeds its size limit")]
    TooLarge,
    #[error("cache record is invalid")]
    Invalid,
}

#[derive(Debug, Clone)]
pub struct CacheScope {
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
}

impl CacheScope {
    pub fn new(product_id: ProductId, tenant_id: TenantId, job_id: JobId) -> Self {
        Self {
            product_id,
            tenant_id,
            job_id,
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{CACHE_NAMESPACE}:{}:{}:{}",
            self.product_id.as_str(),
            self.tenant_id.as_str(),
            self.job_id.as_str()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheRecord {
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    status: JobState,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl CacheRecord {
    pub fn new(scope: &CacheScope, status: JobState, created_at: OffsetDateTime) -> Self {
        Self {
            product_id: scope.product_id.clone(),
            tenant_id: scope.tenant_id.clone(),
            job_id: scope.job_id.clone(),
            status,
            created_at,
        }
    }

    pub fn encode(&self, maximum_bytes: usize) -> Result<Vec<u8>, CacheRecordError> {
        let value = serde_json::to_vec(self).map_err(|_| CacheRecordError::Invalid)?;
        if value.len() > maximum_bytes {
            return Err(CacheRecordError::TooLarge);
        }
        Ok(value)
    }

    pub fn decode(value: &[u8], maximum_bytes: usize) -> Result<Self, CacheRecordError> {
        if value.len() > maximum_bytes {
            return Err(CacheRecordError::TooLarge);
        }
        serde_json::from_slice(value).map_err(|_| CacheRecordError::Invalid)
    }

    pub fn matches_scope(&self, scope: &CacheScope) -> bool {
        self.product_id == scope.product_id
            && self.tenant_id == scope.tenant_id
            && self.job_id == scope.job_id
    }

    pub fn status(&self) -> JobState {
        self.status
    }

    pub fn created_at(&self) -> OffsetDateTime {
        self.created_at
    }
}

#[derive(Debug, Clone)]
pub struct CachePolicy {
    active_ttl: Duration,
    terminal_ttl: Duration,
    timeout: Duration,
    maximum_record_bytes: usize,
}

impl CachePolicy {
    pub fn new(
        active_ttl: Duration,
        terminal_ttl: Duration,
        timeout: Duration,
        maximum_record_bytes: usize,
    ) -> Result<Self, CacheConfigurationError> {
        if active_ttl.is_zero()
            || terminal_ttl.is_zero()
            || active_ttl > MAXIMUM_TTL
            || terminal_ttl > MAXIMUM_TTL
        {
            return Err(CacheConfigurationError::InvalidTtl);
        }
        if timeout.is_zero() || timeout > MAXIMUM_TIMEOUT {
            return Err(CacheConfigurationError::InvalidTimeout);
        }
        if !(MINIMUM_RECORD_BYTES..=MAXIMUM_RECORD_BYTES).contains(&maximum_record_bytes) {
            return Err(CacheConfigurationError::InvalidRecordLimit);
        }
        Ok(Self {
            active_ttl,
            terminal_ttl,
            timeout,
            maximum_record_bytes,
        })
    }

    pub fn ttl(&self, status: JobState) -> Duration {
        if matches!(
            status,
            JobState::Cancelled
                | JobState::Rejected
                | JobState::Partial
                | JobState::ReviewRequired
                | JobState::Completed
        ) {
            self.terminal_ttl
        } else {
            self.active_ttl
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn maximum_record_bytes(&self) -> usize {
        self.maximum_record_bytes
    }
}

#[derive(Debug, Copy, Clone, Error)]
#[error("job status cache operation failed")]
pub struct CacheOperationError;

pub type CacheReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<CacheRecord>, CacheOperationError>> + Send + 'a>>;
pub type CacheWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), CacheOperationError>> + Send + 'a>>;

pub trait JobStatusCache: Send + Sync {
    fn get<'a>(&'a self, scope: &'a CacheScope) -> CacheReadFuture<'a>;
    fn put<'a>(
        &'a self,
        scope: &'a CacheScope,
        record: &'a CacheRecord,
        ttl: Duration,
    ) -> CacheWriteFuture<'a>;
}

pub struct UnavailableJobStatusCache;

impl JobStatusCache for UnavailableJobStatusCache {
    fn get<'a>(&'a self, _scope: &'a CacheScope) -> CacheReadFuture<'a> {
        Box::pin(async { Ok(None) })
    }

    fn put<'a>(
        &'a self,
        _scope: &'a CacheScope,
        _record: &'a CacheRecord,
        _ttl: Duration,
    ) -> CacheWriteFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Error)]
pub enum ValkeyCacheConfigurationError {
    #[error("Valkey URL is invalid")]
    InvalidUrl,
    #[error("Valkey client configuration is invalid")]
    InvalidClient,
    #[error("Valkey client could not start")]
    ClientStart,
}

pub struct ValkeyJobStatusCache {
    client: fred::prelude::Client,
    policy: CachePolicy,
}

impl ValkeyJobStatusCache {
    pub async fn connect(
        url: &str,
        policy: CachePolicy,
    ) -> Result<Self, ValkeyCacheConfigurationError> {
        let config =
            Config::from_url(url).map_err(|_| ValkeyCacheConfigurationError::InvalidUrl)?;
        let client = Builder::from_config(config)
            .with_connection_config(|config| {
                config.connection_timeout = policy.timeout();
            })
            .build()
            .map_err(|_| ValkeyCacheConfigurationError::InvalidClient)?;
        client
            .init()
            .await
            .map_err(|_| ValkeyCacheConfigurationError::ClientStart)?;
        Ok(Self { client, policy })
    }
}

impl JobStatusCache for ValkeyJobStatusCache {
    fn get<'a>(&'a self, scope: &'a CacheScope) -> CacheReadFuture<'a> {
        Box::pin(async move {
            let value = tokio::time::timeout(
                self.policy.timeout(),
                self.client.getrange::<Vec<u8>, _>(
                    scope.key(),
                    0,
                    self.policy.maximum_record_bytes(),
                ),
            )
            .await
            .map_err(|_| CacheOperationError)?
            .map_err(|_| CacheOperationError)?;
            if value.is_empty() {
                return Ok(None);
            }
            CacheRecord::decode(&value, self.policy.maximum_record_bytes())
                .map(Some)
                .map_err(|_| CacheOperationError)
        })
    }

    fn put<'a>(
        &'a self,
        scope: &'a CacheScope,
        record: &'a CacheRecord,
        ttl: Duration,
    ) -> CacheWriteFuture<'a> {
        Box::pin(async move {
            let value = record
                .encode(self.policy.maximum_record_bytes())
                .map_err(|_| CacheOperationError)?;
            let ttl_millis = i64::try_from(ttl.as_millis())
                .ok()
                .filter(|ttl| *ttl > 0)
                .ok_or(CacheOperationError)?;
            tokio::time::timeout(
                self.policy.timeout(),
                self.client.set::<(), _, _>(
                    scope.key(),
                    value,
                    Some(Expiration::PX(ttl_millis)),
                    None,
                    false,
                ),
            )
            .await
            .map_err(|_| CacheOperationError)?
            .map_err(|_| CacheOperationError)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CachePolicy, CacheRecord, CacheScope};
    use ocr_domain::{JobId, JobState, ProductId, TenantId};
    use std::time::Duration;
    use time::OffsetDateTime;

    fn scope() -> CacheScope {
        CacheScope::new(
            ProductId::new("kora").unwrap(),
            TenantId::new("ten_CACHE").unwrap(),
            JobId::new("job_CACHE").unwrap(),
        )
    }

    #[test]
    fn key_is_versioned_and_scoped_by_product_tenant_and_job() {
        assert_eq!(scope().key(), "ocr:job-status:v1:kora:ten_CACHE:job_CACHE");
    }

    #[test]
    fn record_round_trip_preserves_only_status_metadata_and_scope() {
        let scope = scope();
        let created_at = OffsetDateTime::from_unix_timestamp(1_725_000_000).unwrap();
        let encoded = CacheRecord::new(&scope, JobState::Processing, created_at)
            .encode(512)
            .unwrap();

        assert!(!encoded.windows(7).any(|window| window == b"content"));
        let decoded = CacheRecord::decode(&encoded, 512).unwrap();
        assert!(decoded.matches_scope(&scope));
        assert_eq!(decoded.status(), JobState::Processing);
        assert_eq!(decoded.created_at(), created_at);
    }

    #[test]
    fn malformed_oversized_and_unknown_cache_records_are_rejected() {
        assert!(CacheRecord::decode(b"not-json", 512).is_err());
        assert!(CacheRecord::decode(&vec![b'x'; 513], 512).is_err());
        assert!(CacheRecord::decode(
            br#"{"product_id":"kora","tenant_id":"ten_CACHE","job_id":"job_CACHE","status":"accepted","created_at":"2024-08-30T00:00:00Z","document_text":"untrusted"}"#,
            512,
        )
        .is_err());
    }

    #[test]
    fn policy_requires_bounded_nonzero_ttls_and_timeout() {
        assert!(CachePolicy::new(
            Duration::ZERO,
            Duration::from_secs(300),
            Duration::from_millis(25),
            512,
        )
        .is_err());
        assert!(CachePolicy::new(
            Duration::from_secs(10),
            Duration::from_secs(300),
            Duration::ZERO,
            512,
        )
        .is_err());

        let policy = CachePolicy::new(
            Duration::from_secs(10),
            Duration::from_secs(300),
            Duration::from_millis(25),
            512,
        )
        .unwrap();
        assert_eq!(policy.ttl(JobState::Processing), Duration::from_secs(10));
        assert_eq!(policy.ttl(JobState::Completed), Duration::from_secs(300));
    }
}
