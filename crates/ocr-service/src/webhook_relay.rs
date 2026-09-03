use std::{future::Future, pin::Pin, sync::Arc};

use ocr_domain::{JobState, ProductId, TenantId, WebhookSubscriptionId};
use ocr_store::{ClaimJobOutbox, PgJobStore, PublishJobOutboxOutcome, WebhookOutboxEventType};
use thiserror::Error;

use crate::{TerminalWebhookEvent, WebhookSignError};

pub type WebhookPublishFuture<'a> =
    Pin<Box<dyn Future<Output = Result<WebhookPublishOutcome, WebhookPublishError>> + Send + 'a>>;

pub trait WebhookPublisher: Send + Sync {
    fn publish<'a>(
        &'a self,
        subscription_id: &'a WebhookSubscriptionId,
        event: &'a TerminalWebhookEvent,
    ) -> WebhookPublishFuture<'a>;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WebhookPublishOutcome {
    Delivered,
    Existing,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum WebhookPublishError {
    #[error("webhook publisher is unavailable")]
    Unavailable,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WebhookRelayOutcome {
    Idle,
    Published(usize),
    Retryable { published: usize },
    LeaseLost { published: usize },
}

#[derive(Debug, Error)]
pub enum WebhookRelayError {
    #[error(transparent)]
    Store(#[from] ocr_store::Error),
    #[error(transparent)]
    InvalidEvent(#[from] WebhookSignError),
}

pub struct WebhookOutboxRelay<P> {
    jobs: PgJobStore,
    publisher: Arc<P>,
}

impl<P> WebhookOutboxRelay<P>
where
    P: WebhookPublisher,
{
    pub fn new(jobs: PgJobStore, publisher: Arc<P>) -> Self {
        Self { jobs, publisher }
    }

    pub async fn relay_scope(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        lease_owner: &str,
        limit: i64,
    ) -> Result<WebhookRelayOutcome, WebhookRelayError> {
        let events = self
            .jobs
            .claim_webhook_outbox(
                tenant_id,
                product_id,
                ClaimJobOutbox {
                    lease_owner: lease_owner.to_owned(),
                    limit,
                },
            )
            .await?;
        if events.is_empty() {
            return Ok(WebhookRelayOutcome::Idle);
        }
        let mut published = 0_usize;
        for stored in events {
            let status = match stored.event_type {
                WebhookOutboxEventType::Completed => JobState::Completed,
                WebhookOutboxEventType::Partial => JobState::Partial,
                WebhookOutboxEventType::ReviewRequired => JobState::ReviewRequired,
            };
            let event = TerminalWebhookEvent::new(
                stored.event_id,
                stored.occurred_at,
                product_id.clone(),
                tenant_id.clone(),
                stored.job_id,
                stored.document_version,
                status,
            )?;
            if self
                .publisher
                .publish(&stored.webhook_subscription_id, &event)
                .await
                .is_err()
            {
                return Ok(WebhookRelayOutcome::Retryable { published });
            }
            match self
                .jobs
                .publish_job_outbox(tenant_id, product_id, stored.event_id, lease_owner)
                .await?
            {
                PublishJobOutboxOutcome::Published | PublishJobOutboxOutcome::Existing => {
                    published += 1;
                }
                PublishJobOutboxOutcome::LeaseLost | PublishJobOutboxOutcome::NotFound => {
                    return Ok(WebhookRelayOutcome::LeaseLost { published });
                }
            }
        }
        Ok(WebhookRelayOutcome::Published(published))
    }
}
