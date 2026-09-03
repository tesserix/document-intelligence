set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    add column source_bucket text check (char_length(source_bucket) between 3 and 63),
    add column source_object_name text check (char_length(source_object_name) between 1 and 1024),
    add column source_object_generation bigint check (source_object_generation > 0),
    add column source_digest text check (source_digest ~ '^sha256:[a-f0-9]{64}$'),
    add column source_content_length bigint check (source_content_length between 1 and 104857600),
    add column accepted_at timestamptz,
    add constraint ocr_uploads_accepted_source_complete check (
        (status = 'accepted'
            and source_bucket is not null
            and source_object_name is not null
            and source_object_generation is not null
            and source_digest = verified_digest
            and source_content_length = verified_content_length
            and accepted_at is not null)
        or
        (status <> 'accepted'
            and source_bucket is null
            and source_object_name is null
            and source_object_generation is null
            and source_digest is null
            and source_content_length is null
            and accepted_at is null)
    ) not valid;

alter table ocr_uploads validate constraint ocr_uploads_accepted_source_complete;
