set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    add constraint ocr_uploads_scope_identity_unique
    unique (upload_id, product_id, tenant_id),
    add column object_generation bigint check (object_generation > 0),
    add column verified_content_type text check (verified_content_type in (
        'application/pdf', 'image/jpeg', 'image/png', 'image/tiff', 'image/webp'
    )),
    add column verified_content_length bigint check (
        verified_content_length between 1 and 104857600
    ),
    add column verified_digest text check (verified_digest ~ '^sha256:[a-f0-9]{64}$'),
    add column uploaded_at timestamptz,
    add constraint ocr_uploads_verification_complete check (
        (status in ('reserved', 'expired') and object_generation is null
            and verified_content_type is null and verified_content_length is null
            and verified_digest is null and uploaded_at is null)
        or
        (status in ('uploaded', 'inspecting', 'accepted', 'rejected')
            and object_generation is not null
            and verified_content_type is not null and verified_content_length is not null
            and verified_digest is not null and uploaded_at is not null
            and verified_content_type = expected_content_type
            and verified_content_length = expected_content_length
            and verified_digest = expected_digest)
    );

create table ocr_upload_outbox (
    event_id bigint generated always as identity primary key,
    product_id text not null,
    tenant_id text not null,
    upload_id text not null,
    event_type text not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    published_at timestamptz,
    constraint ocr_upload_outbox_upload_scope_fk
        foreign key (upload_id, product_id, tenant_id)
        references ocr_uploads (upload_id, product_id, tenant_id)
        on delete restrict
);

create index ocr_upload_outbox_unpublished_idx
    on ocr_upload_outbox (event_id) where published_at is null;

alter table ocr_upload_outbox enable row level security;
alter table ocr_upload_outbox force row level security;

create policy ocr_upload_outbox_scope on ocr_upload_outbox
    using (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    )
    with check (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    );
