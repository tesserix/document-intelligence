use std::{future::Future, pin::Pin, sync::Arc};

use ocr_domain::{ProductId, TenantId, UploadId};
use ocr_service::{
    ImportError, ImportOutcome, Importer, JobOutboxRelay, RelayError, RelayOutcome, WorkflowStarter,
};
use ocr_store::{ClaimWorkScopes, PgJobStore, PgWorkScopeDirectory};
use thiserror::Error;

pub type ReconcileFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ImportOutcome, ImportError>> + Send + 'a>>;

pub trait UploadReconciler: Send + Sync {
    fn reconcile<'a>(
        &'a self,
        tenant_id: &'a TenantId,
        product_id: &'a ProductId,
        upload_id: &'a UploadId,
        lease_owner: &'a str,
    ) -> ReconcileFuture<'a>;
}

impl<M, R, P, S> UploadReconciler for Importer<M, R, P, S>
where
    M: ocr_service::MalwareInspector,
    R: ocr_service::UploadDocumentReader,
    P: ocr_service::DocumentParser,
    S: ocr_service::SourcePromoter,
{
    fn reconcile<'a>(
        &'a self,
        tenant_id: &'a TenantId,
        product_id: &'a ProductId,
        upload_id: &'a UploadId,
        lease_owner: &'a str,
    ) -> ReconcileFuture<'a> {
        Box::pin(self.process(tenant_id, product_id, upload_id, lease_owner))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct WorkScopeDispatchOutcome {
    pub scopes: usize,
    pub uploads: usize,
    pub workflows: usize,
    pub lease_lost: usize,
}

#[derive(Debug, Error)]
pub enum WorkScopeDispatchError {
    #[error(transparent)]
    Store(#[from] ocr_store::Error),
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error(transparent)]
    Relay(#[from] RelayError),
    #[error("invalid work scope dispatcher configuration")]
    InvalidConfiguration,
}

pub struct WorkScopeDispatcher<R, W> {
    scopes: PgWorkScopeDirectory,
    jobs: PgJobStore,
    importer: Arc<R>,
    relay: Arc<JobOutboxRelay<W>>,
    lease_owner: String,
    scope_limit: i64,
    upload_limit: i64,
}

impl<R, W> WorkScopeDispatcher<R, W>
where
    R: UploadReconciler,
    W: WorkflowStarter,
{
    pub fn new(
        scopes: PgWorkScopeDirectory,
        jobs: PgJobStore,
        importer: Arc<R>,
        relay: Arc<JobOutboxRelay<W>>,
        lease_owner: &str,
        scope_limit: i64,
        upload_limit: i64,
    ) -> Result<Self, WorkScopeDispatchError> {
        if !valid_lease_owner(lease_owner)
            || !(1..=100).contains(&scope_limit)
            || !(1..=100).contains(&upload_limit)
        {
            return Err(WorkScopeDispatchError::InvalidConfiguration);
        }
        Ok(Self {
            scopes,
            jobs,
            importer,
            relay,
            lease_owner: lease_owner.to_owned(),
            scope_limit,
            upload_limit,
        })
    }

    pub async fn dispatch_once(&self) -> Result<WorkScopeDispatchOutcome, WorkScopeDispatchError> {
        let claimed = self
            .scopes
            .claim(ClaimWorkScopes {
                lease_owner: self.lease_owner.clone(),
                limit: self.scope_limit,
            })
            .await?;
        let mut outcome = WorkScopeDispatchOutcome {
            scopes: claimed.len(),
            uploads: 0,
            workflows: 0,
            lease_lost: 0,
        };
        for scope in claimed {
            let dispatched = self
                .dispatch_scope(&scope.tenant_id, &scope.product_id)
                .await;
            let released = self.scopes.release(&scope, &self.lease_owner).await?;
            if !released {
                outcome.lease_lost += 1;
            }
            let dispatched = dispatched?;
            outcome.uploads += dispatched.uploads;
            outcome.workflows += dispatched.workflows;
            outcome.lease_lost += dispatched.lease_lost;
        }
        Ok(outcome)
    }

    async fn dispatch_scope(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
    ) -> Result<WorkScopeDispatchOutcome, WorkScopeDispatchError> {
        let uploads = self
            .jobs
            .list_reconcilable_uploads(tenant_id, product_id, self.upload_limit)
            .await?;
        let mut outcome = WorkScopeDispatchOutcome {
            scopes: 0,
            uploads: 0,
            workflows: 0,
            lease_lost: 0,
        };
        for upload_id in uploads {
            let _ = self
                .importer
                .reconcile(tenant_id, product_id, &upload_id, &self.lease_owner)
                .await?;
            outcome.uploads += 1;
        }
        match self
            .relay
            .relay_scope(tenant_id, product_id, &self.lease_owner, self.upload_limit)
            .await?
        {
            RelayOutcome::Idle => {}
            RelayOutcome::Published(workflows) => outcome.workflows += workflows,
            RelayOutcome::Retryable { published } => outcome.workflows += published,
            RelayOutcome::LeaseLost { published } => {
                outcome.workflows += published;
                outcome.lease_lost += 1;
            }
        }
        Ok(outcome)
    }
}

fn valid_lease_owner(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b':' | b'-'))
        })
}

#[cfg(test)]
mod tests {
    use super::valid_lease_owner;

    #[test]
    fn worker_lease_owner_is_bounded_and_safe_for_database_claims() {
        assert!(valid_lease_owner("ocr-worker-01"));
        assert!(!valid_lease_owner(""));
        assert!(!valid_lease_owner("-starts-with-separator"));
        assert!(!valid_lease_owner("contains space"));
        assert!(!valid_lease_owner(&"a".repeat(129)));
    }
}
