set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_jobs
    add column upload_id text check (upload_id ~ '^upl_[A-Za-z0-9_]{1,64}$'),
    add constraint ocr_jobs_upload_scope_fk
        foreign key (upload_id, product_id, tenant_id)
        references ocr_uploads (upload_id, product_id, tenant_id)
        on delete restrict
        not valid;

alter table ocr_jobs validate constraint ocr_jobs_upload_scope_fk;

create index concurrently ocr_jobs_upload_scope_idx
    on ocr_jobs (product_id, tenant_id, upload_id)
    where upload_id is not null;
