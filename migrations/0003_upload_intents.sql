set lock_timeout = '5s';
set statement_timeout = '30s';

create type ocr_upload_status as enum (
    'reserved', 'uploaded', 'inspecting', 'accepted', 'rejected', 'expired'
);

create table ocr_uploads (
    upload_id text primary key check (upload_id ~ '^upl_[A-Za-z0-9_]{1,64}$'),
    tenant_id text not null check (tenant_id ~ '^ten_[A-Za-z0-9_]{1,64}$'),
    product_id text not null check (product_id ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'),
    idempotency_key text not null check (length(idempotency_key) between 1 and 128),
    request_digest text not null check (request_digest ~ '^sha256:[a-f0-9]{64}$'),
    object_bucket text not null check (length(object_bucket) between 3 and 63),
    object_name text not null check (length(object_name) between 1 and 1024),
    expected_content_type text not null check (expected_content_type in (
        'application/pdf', 'image/jpeg', 'image/png', 'image/tiff', 'image/webp'
    )),
    expected_content_length bigint not null check (expected_content_length between 1 and 104857600),
    expected_digest text not null check (expected_digest ~ '^sha256:[a-f0-9]{64}$'),
    status ocr_upload_status not null default 'reserved',
    expires_at timestamptz not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (product_id, tenant_id, idempotency_key),
    unique (object_bucket, object_name)
);

create index ocr_uploads_scope_created_idx
    on ocr_uploads (product_id, tenant_id, created_at desc, upload_id desc);

alter table ocr_uploads enable row level security;
alter table ocr_uploads force row level security;

create policy ocr_uploads_scope on ocr_uploads
    using (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    )
    with check (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    );
