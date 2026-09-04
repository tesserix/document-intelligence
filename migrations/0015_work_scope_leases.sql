set lock_timeout = '5s';
set statement_timeout = '30s';

create table ocr_work_scopes (
    product_id text not null check (product_id ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'),
    tenant_id text not null check (tenant_id ~ '^ten_[A-Za-z0-9_]{1,64}$'),
    upload_pending boolean not null default false,
    dispatch_pending boolean not null default false,
    lease_owner text check (char_length(lease_owner) between 1 and 128),
    lease_expires_at timestamptz,
    updated_at timestamptz not null default now(),
    primary key (product_id, tenant_id),
    constraint ocr_work_scopes_lease_shape check (
        (lease_owner is null and lease_expires_at is null)
        or (lease_owner is not null and lease_expires_at is not null)
    )
);

create index ocr_work_scopes_lease_idx
    on ocr_work_scopes (lease_expires_at, updated_at, product_id, tenant_id);

create or replace function ocr_register_work_scope(
    scope_product_id text,
    scope_tenant_id text,
    work_kind text
)
returns void
language sql
security definer
set search_path = pg_catalog, public
as $$
    with requested as (
        select work_kind = 'upload' as upload_pending,
               work_kind = 'dispatch' as dispatch_pending
    )
    insert into public.ocr_work_scopes (product_id, tenant_id, upload_pending, dispatch_pending)
    select scope_product_id, scope_tenant_id, upload_pending, dispatch_pending from requested
    where work_kind in ('upload', 'dispatch')
    on conflict (product_id, tenant_id) do update
    set upload_pending = public.ocr_work_scopes.upload_pending or excluded.upload_pending,
        dispatch_pending = public.ocr_work_scopes.dispatch_pending or excluded.dispatch_pending,
        updated_at = now()
$$;

create or replace function ocr_set_work_scope_pending(
    scope_product_id text,
    scope_tenant_id text,
    work_kind text,
    is_pending boolean
)
returns void
language sql
security definer
set search_path = pg_catalog, public
as $$
    update public.ocr_work_scopes
    set upload_pending = case when work_kind = 'upload' then is_pending else upload_pending end,
        dispatch_pending = case when work_kind = 'dispatch' then is_pending else dispatch_pending end,
        updated_at = now()
    where product_id = scope_product_id
      and tenant_id = scope_tenant_id
      and work_kind in ('upload', 'dispatch')
$$;

create or replace function ocr_claim_work_scopes(
    claim_owner text,
    claim_limit integer
)
returns table (product_id text, tenant_id text)
language plpgsql
security definer
set search_path = pg_catalog, public
as $$
begin
    if claim_owner !~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'
        or claim_limit not between 1 and 100 then
        raise exception 'invalid work scope claim';
    end if;

    return query
    with candidates as (
        select scopes.product_id, scopes.tenant_id
        from public.ocr_work_scopes as scopes
        where (scopes.lease_owner = claim_owner
                or scopes.lease_expires_at is null
                or scopes.lease_expires_at <= now())
          and (scopes.upload_pending or scopes.dispatch_pending)
        order by scopes.updated_at, scopes.product_id, scopes.tenant_id
        for update skip locked
        limit claim_limit
    ), claimed as (
        update public.ocr_work_scopes as scopes
        set lease_owner = claim_owner,
            lease_expires_at = now() + interval '5 minutes',
            updated_at = now()
        from candidates
        where scopes.product_id = candidates.product_id
          and scopes.tenant_id = candidates.tenant_id
        returning scopes.product_id, scopes.tenant_id
    )
    select claimed.product_id, claimed.tenant_id
    from claimed;
end;
$$;

create or replace function ocr_release_work_scope(
    scope_product_id text,
    scope_tenant_id text,
    claim_owner text
)
returns boolean
language sql
security definer
set search_path = pg_catalog, public
as $$
    update public.ocr_work_scopes
    set lease_owner = null,
        lease_expires_at = null,
        updated_at = now()
    where product_id = scope_product_id
      and tenant_id = scope_tenant_id
      and lease_owner = claim_owner
      and lease_expires_at > now()
    returning true
$$;

revoke all on function ocr_claim_work_scopes(text, integer) from public;
revoke all on function ocr_release_work_scope(text, text, text) from public;
revoke all on function ocr_register_work_scope(text, text, text) from public;
revoke all on function ocr_set_work_scope_pending(text, text, text, boolean) from public;
