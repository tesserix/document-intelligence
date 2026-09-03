set lock_timeout = '5s';
set statement_timeout = '30s';

drop table if exists ocr_upload_outbox;

alter table ocr_uploads
    drop constraint if exists ocr_uploads_verification_complete,
    drop column if exists uploaded_at,
    drop column if exists verified_digest,
    drop column if exists verified_content_length,
    drop column if exists verified_content_type,
    drop column if exists object_generation,
    drop constraint if exists ocr_uploads_scope_identity_unique;
