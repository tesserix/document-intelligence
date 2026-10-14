begin;
set local lock_timeout = '5s';
set local statement_timeout = '30s';

create table ocr_native_documents (
    native_document_id bigint generated always as identity primary key,
    job_id text not null,
    product_id text not null,
    tenant_id text not null,
    processing_profile text not null check (processing_profile ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$'),
    source_digest text not null check (source_digest ~ '^sha256:[a-f0-9]{64}$'),
    page_count integer not null check (page_count between 1 and 1000),
    duration_ms bigint not null check (duration_ms>=0),
    created_at timestamptz not null default now(),
    unique (job_id, processing_profile, source_digest),
    unique (native_document_id, product_id, tenant_id),
    foreign key (job_id, product_id, tenant_id) references ocr_jobs (job_id, product_id, tenant_id) on delete cascade
);
create table ocr_native_pages (
    native_document_id bigint not null,
    product_id text not null,
    tenant_id text not null,
    page_number integer not null check (page_number between 1 and 1000),
    page_data jsonb not null check (
        jsonb_typeof(page_data) = 'object'
        and page_data->>'page' = page_number::text
        and octet_length(page_data::text) between 1 and 134217728
    ),
    primary key (native_document_id, page_number),
    foreign key (native_document_id, product_id, tenant_id)
        references ocr_native_documents (native_document_id, product_id, tenant_id) on delete cascade
);

alter table ocr_native_documents enable row level security;
alter table ocr_native_documents force row level security;
alter table ocr_native_pages enable row level security;
alter table ocr_native_pages force row level security;

create policy ocr_native_documents_scope on ocr_native_documents
    using (tenant_id=current_setting('app.tenant_id',true) and product_id=current_setting('app.product_id',true))
    with check (tenant_id=current_setting('app.tenant_id',true) and product_id=current_setting('app.product_id',true));
create policy ocr_native_pages_scope on ocr_native_pages
    using (tenant_id=current_setting('app.tenant_id',true) and product_id=current_setting('app.product_id',true))
    with check (tenant_id=current_setting('app.tenant_id',true) and product_id=current_setting('app.product_id',true));

do $$ declare application_role text;
begin
    for application_role in
        select grantee from information_schema.role_table_grants
        where table_schema=current_schema() and table_name='ocr_page_artifacts' and grantee<>'PUBLIC'
        group by grantee having bool_or(privilege_type='SELECT') and bool_or(privilege_type='INSERT')
    loop
        execute format('grant select, insert, delete on ocr_native_documents, ocr_native_pages to %I',application_role);
        execute format('grant usage, select on sequence ocr_native_documents_native_document_id_seq to %I',application_role);
    end loop;
end $$;
commit;
