use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use ocr_domain::{DocumentVersion, JobId, JobState, ProductId, TenantId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::digest::hex_encode;

type HmacSha256 = Hmac<Sha256>;

#[derive(Serialize)]
pub struct TerminalWebhookEvent {
    event_id: String,
    event_type: &'static str,
    #[serde(with = "time::serde::rfc3339")]
    occurred_at: OffsetDateTime,
    product_id: ProductId,
    tenant_id: TenantId,
    job_id: JobId,
    document_version: DocumentVersion,
    status: JobState,
    content_trust: &'static str,
}

impl TerminalWebhookEvent {
    pub fn new(
        event_id: i64,
        occurred_at: OffsetDateTime,
        product_id: ProductId,
        tenant_id: TenantId,
        job_id: JobId,
        document_version: DocumentVersion,
        status: JobState,
    ) -> Result<Self, WebhookSignError> {
        if event_id <= 0 {
            return Err(WebhookSignError::InvalidEvent);
        }
        let event_type = match status {
            JobState::Completed => "ocr.job.completed.v1",
            JobState::Partial => "ocr.job.partial.v1",
            JobState::ReviewRequired => "ocr.job.review_required.v1",
            _ => return Err(WebhookSignError::InvalidEvent),
        };
        Ok(Self {
            event_id: format!("evt_{event_id}"),
            event_type,
            occurred_at,
            product_id,
            tenant_id,
            job_id,
            document_version,
            status,
            content_trust: "untrusted",
        })
    }
}

pub struct WebhookSigningSecret(Zeroizing<Vec<u8>>);

impl WebhookSigningSecret {
    pub fn new(value: &[u8]) -> Result<Self, WebhookSignError> {
        if (32..=128).contains(&value.len()) {
            Ok(Self(Zeroizing::new(value.to_vec())))
        } else {
            Err(WebhookSignError::InvalidSecret)
        }
    }
}

impl fmt::Debug for WebhookSigningSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebhookSigningSecret([REDACTED])")
    }
}

pub struct WebhookSigner {
    secret: WebhookSigningSecret,
}

impl WebhookSigner {
    pub fn new(secret: WebhookSigningSecret) -> Self {
        Self { secret }
    }

    pub fn sign(&self, event: &TerminalWebhookEvent) -> Result<SignedWebhook, WebhookSignError> {
        let body = serde_json::to_vec(event).map_err(|_| WebhookSignError::InvalidEvent)?;
        let event_id = event.event_id.clone();
        let timestamp = event.occurred_at.unix_timestamp().to_string();
        let body_digest = hex_encode(Sha256::digest(&body));
        let signing_input = format!("{timestamp}.{event_id}.{body_digest}");
        let mut mac = HmacSha256::new_from_slice(&self.secret.0)
            .map_err(|_| WebhookSignError::InvalidSecret)?;
        mac.update(signing_input.as_bytes());
        let signature = format!("v1={}", hex_encode(mac.finalize().into_bytes()));
        Ok(SignedWebhook {
            event_id,
            timestamp,
            signature,
            body,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedWebhook {
    pub event_id: String,
    pub timestamp: String,
    pub signature: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum WebhookSignError {
    #[error("webhook event is invalid")]
    InvalidEvent,
    #[error("webhook signing secret is invalid")]
    InvalidSecret,
}
