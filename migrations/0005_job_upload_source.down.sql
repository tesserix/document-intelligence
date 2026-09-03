set lock_timeout = '5s';
set statement_timeout = '30s';

drop index concurrently if exists ocr_jobs_upload_scope_idx;

alter table ocr_jobs
    drop constraint if exists ocr_jobs_upload_scope_fk,
    drop column if exists upload_id;
