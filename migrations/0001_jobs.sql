set lock_timeout = '5s';
set statement_timeout = '30s';

create type ocr_job_status as enum (
    'accepted', 'inspecting', 'processing', 'validating', 'cancelling',
    'cancelled', 'rejected', 'partial', 'review_required', 'completed'
);

create table ocr_jobs (
    job_id text primary key check (job_id ~ '^job_[A-Za-z0-9_]{1,64}$'),
    tenant_id text not null check (tenant_id ~ '^ten_[A-Za-z0-9_]{1,64}$'),
    product_id text not null check (product_id ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'),
    idempotency_key text not null check (length(idempotency_key) between 1 and 128),
    request_digest text not null check (request_digest ~ '^sha256:[a-f0-9]{64}$'),
    status ocr_job_status not null default 'accepted',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (product_id, tenant_id, idempotency_key)
);

create index ocr_jobs_scope_created_idx
    on ocr_jobs (product_id, tenant_id, created_at desc, job_id desc);

create table ocr_outbox (
    event_id bigint generated always as identity primary key,
    product_id text not null,
    tenant_id text not null,
    job_id text not null references ocr_jobs(job_id) on delete restrict,
    event_type text not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    published_at timestamptz
);

create index ocr_outbox_unpublished_idx
    on ocr_outbox (event_id) where published_at is null;

alter table ocr_jobs enable row level security;
alter table ocr_jobs force row level security;
alter table ocr_outbox enable row level security;
alter table ocr_outbox force row level security;

create policy ocr_jobs_scope on ocr_jobs
    using (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    )
    with check (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    );

create policy ocr_outbox_scope on ocr_outbox
    using (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    )
    with check (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    );
