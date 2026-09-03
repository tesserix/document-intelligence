set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    drop constraint ocr_uploads_accepted_source_complete,
    drop column accepted_at,
    drop column source_content_length,
    drop column source_digest,
    drop column source_object_generation,
    drop column source_object_name,
    drop column source_bucket;
