set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_jobs
    add column extraction_schema_id text,
    add column extraction_schema_version text;

alter table ocr_jobs
    add constraint ocr_jobs_extraction_schema_shape check (
        (extraction_schema_id is null and extraction_schema_version is null)
        or (
            extraction_schema_id is not null
            and extraction_schema_version is not null
            and length(extraction_schema_id) between 1 and 128
            and length(extraction_schema_version) between 1 and 64
        )
    ) not valid;

alter table ocr_jobs validate constraint ocr_jobs_extraction_schema_shape;
