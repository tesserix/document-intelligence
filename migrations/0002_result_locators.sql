set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_jobs
    add constraint ocr_jobs_scope_identity_unique unique (job_id, product_id, tenant_id);

create table ocr_results (
    job_id text primary key,
    product_id text not null,
    tenant_id text not null,
    document_id text not null check (document_id ~ '^doc_[A-Za-z0-9_]{1,64}$'),
    document_version text not null check (document_version ~ '^sha256:[a-f0-9]{64}$'),
    object_bucket text not null check (length(object_bucket) between 3 and 222),
    object_name text not null check (length(object_name) between 1 and 1024),
    object_generation bigint not null check (object_generation > 0),
    object_digest text not null check (object_digest ~ '^sha256:[a-f0-9]{64}$'),
    content_type text not null default 'application/json'
        check (content_type = 'application/json'),
    content_length bigint not null check (content_length between 1 and 16777216),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint ocr_results_job_scope_fk
        foreign key (job_id, product_id, tenant_id)
        references ocr_jobs (job_id, product_id, tenant_id)
        on delete restrict
);

create unique index ocr_results_document_version_idx
    on ocr_results (product_id, tenant_id, document_id, document_version);

alter table ocr_results enable row level security;
alter table ocr_results force row level security;

create policy ocr_results_scope on ocr_results
    using (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    )
    with check (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    );
