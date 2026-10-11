insert into ocr_uploads (
    upload_id, tenant_id, product_id, idempotency_key, request_digest,
    object_bucket, object_name, expected_content_type, expected_content_length,
    expected_digest, status, expires_at, object_generation, verified_content_type,
    verified_content_length, verified_digest, uploaded_at
) values (
    'upl_MIGRATIONLEGACY', 'ten_MIGRATION', 'example', 'migration-legacy',
    'sha256:' || repeat('a', 64), 'example-quarantine', 'legacy-object',
    'image/png', 128, 'sha256:' || repeat('a', 64), 'uploaded', now() + interval '1 hour',
    42, 'image/png', 128, 'sha256:' || repeat('a', 64), now()
);
