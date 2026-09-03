set lock_timeout = '5s';
set statement_timeout = '30s';

create table ocr_page_artifacts (
    job_id text not null,
    product_id text not null,
    tenant_id text not null,
    page_number integer not null check (page_number between 1 and 300),
    attempt smallint not null check (attempt between 1 and 10),
    activity_key text not null check (char_length(activity_key) between 1 and 160),
    object_bucket text not null check (char_length(object_bucket) between 3 and 222),
    object_name text not null check (char_length(object_name) between 1 and 1024),
    object_generation bigint not null check (object_generation > 0),
    object_digest text not null check (object_digest ~ '^sha256:[a-f0-9]{64}$'),
    content_length bigint not null check (content_length between 1 and 16777216),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (job_id, page_number),
    unique (job_id, activity_key),
    constraint ocr_page_artifacts_job_scope_fk
        foreign key (job_id, product_id, tenant_id)
        references ocr_jobs (job_id, product_id, tenant_id)
        on delete restrict
);

alter table ocr_page_artifacts enable row level security;
alter table ocr_page_artifacts force row level security;

create policy ocr_page_artifacts_scope on ocr_page_artifacts
    using (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    )
    with check (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    );
