set lock_timeout = '5s';
set statement_timeout = '30s';

create table ocr_page_workflows (
    job_id text primary key,
    product_id text not null,
    tenant_id text not null,
    workflow_schema_version smallint not null default 1
        check (workflow_schema_version = 1),
    revision bigint not null default 0 check (revision >= 0),
    checkpoint jsonb not null check (
        jsonb_typeof(checkpoint) = 'object'
        and octet_length(checkpoint::text) between 1 and 262144
    ),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint ocr_page_workflows_job_scope_fk
        foreign key (job_id, product_id, tenant_id)
        references ocr_jobs (job_id, product_id, tenant_id)
        on delete restrict
);

alter table ocr_page_workflows enable row level security;
alter table ocr_page_workflows force row level security;

create policy ocr_page_workflows_scope on ocr_page_workflows
    using (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    )
    with check (
        tenant_id = current_setting('app.tenant_id', true)
        and product_id = current_setting('app.product_id', true)
    );
