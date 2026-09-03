use ocr_domain::{IdempotencyKey, JobId, ProductId, RequestDigest, TenantId, UploadId};

#[test]
fn trusted_scope_identifiers_accept_only_canonical_values() {
    assert!(TenantId::new("ten_01JTEST").is_ok());
    assert!(ProductId::new("kora").is_ok());
    assert!(ProductId::new("customer-support").is_ok());

    for invalid in ["", "tenant-one", "ten_a/b", "ten_"] {
        assert!(TenantId::new(invalid).is_err(), "accepted {invalid:?}");
    }
    for invalid in ["", "Kora", "-kora", "kora-", "prod/kora"] {
        assert!(ProductId::new(invalid).is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn job_and_idempotency_identifiers_are_bounded() {
    assert!(JobId::new("job_01JTEST").is_ok());
    assert!(IdempotencyKey::new("client-request-01JTEST").is_ok());

    assert!(JobId::new("invoice.pdf").is_err());
    assert!(IdempotencyKey::new("").is_err());
    assert!(IdempotencyKey::new(&"a".repeat(129)).is_err());
    assert!(IdempotencyKey::new("contains whitespace").is_err());
}

#[test]
fn upload_identifiers_are_opaque_and_bounded() {
    assert!(UploadId::new("upl_A1_b2").is_ok());
    assert!(UploadId::new("upload_A1").is_err());
    assert!(UploadId::new("upl_").is_err());
    assert!(UploadId::new(&format!("upl_{}", "a".repeat(65))).is_err());
    assert!(UploadId::new("upl_a/b").is_err());
}

#[test]
fn request_digest_accepts_only_canonical_sha256() {
    assert!(RequestDigest::new(&format!("sha256:{}", "a".repeat(64))).is_ok());
    assert!(RequestDigest::new(&format!("sha256:{}", "A".repeat(64))).is_err());
    assert!(RequestDigest::new("sha256:short").is_err());
}

#[test]
fn deserialization_cannot_bypass_identity_invariants() {
    assert!(serde_json::from_str::<TenantId>(r#""../../other-tenant""#).is_err());
    assert!(serde_json::from_str::<ProductId>(r#""prod/kora""#).is_err());
    assert!(serde_json::from_str::<IdempotencyKey>(r#""contains whitespace""#).is_err());
}
