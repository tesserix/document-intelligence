create table ocr_sandbox_sessions (
    token_hash text primary key check (token_hash ~ '^[a-f0-9]{64}$'),
    tenant_id text not null unique check (tenant_id ~ '^ten_SBX_[a-f0-9]{32}$'),
    network_hash text not null,
    created_at timestamptz not null default now(),
    expires_at timestamptz not null default now() + interval '15 minutes',
    request_window bigint not null default 0,
    request_count integer not null default 0,
    admitted_at timestamptz,
    upload_request text,
    upload_id text,
    content_started_at timestamptz,
    content_uploaded boolean not null default false,
    job_request text,
    job_id text,
    images_deleted_at timestamptz,
    purged_at timestamptz,
    cleanup_after timestamptz not null default now(),
    cleanup_lease_until timestamptz,
    cleanup_failures integer not null default 0
);
create index ocr_sandbox_network_created on ocr_sandbox_sessions(network_hash, created_at);
create index ocr_sandbox_admitted on ocr_sandbox_sessions(admitted_at) where admitted_at is not null;
create index ocr_sandbox_cleanup on ocr_sandbox_sessions(cleanup_after) where purged_at is null;
