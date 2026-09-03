use ocr_domain::{DocumentVersion, JobId, JobState, ProductId, TenantId};
use ocr_service::{TerminalWebhookEvent, WebhookSigner, WebhookSigningSecret};
use time::OffsetDateTime;

fn event() -> TerminalWebhookEvent {
    TerminalWebhookEvent::new(
        42,
        OffsetDateTime::from_unix_timestamp(1_788_480_000).unwrap(),
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_WEBHOOK").unwrap(),
        JobId::new("job_WEBHOOK").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "a".repeat(64))).unwrap(),
        JobState::Completed,
    )
    .unwrap()
}

#[test]
fn terminal_webhook_is_deterministic_signed_and_content_free() {
    let secret = WebhookSigningSecret::new(b"a-32-byte-webhook-signing-secret!").unwrap();
    let signer = WebhookSigner::new(secret);
    let first = signer.sign(&event()).unwrap();
    let replay = signer.sign(&event()).unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.event_id, "evt_42");
    assert_eq!(first.timestamp, "1788480000");
    assert!(first.signature.starts_with("v1="));
    assert_eq!(first.signature.len(), 67);
    let body = std::str::from_utf8(&first.body).unwrap();
    assert!(body.contains(r#""event_type":"ocr.job.completed.v1""#));
    assert!(body.contains(r#""content_trust":"untrusted""#));
    for forbidden in [
        "text", "field", "object_", "bucket", "url", "secret", "prompt",
    ] {
        assert!(!body.contains(forbidden), "body contained {forbidden}");
    }
}

#[test]
fn signer_rejects_invalid_secrets_and_nonterminal_states() {
    assert!(WebhookSigningSecret::new(b"short").is_err());
    let secret = WebhookSigningSecret::new(b"another-32-byte-signing-secret!!").unwrap();
    assert_eq!(format!("{secret:?}"), "WebhookSigningSecret([REDACTED])");
    assert!(TerminalWebhookEvent::new(
        1,
        OffsetDateTime::UNIX_EPOCH,
        ProductId::new("kora").unwrap(),
        TenantId::new("ten_WEBHOOK").unwrap(),
        JobId::new("job_WEBHOOK").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "b".repeat(64))).unwrap(),
        JobState::Processing,
    )
    .is_err());
}
