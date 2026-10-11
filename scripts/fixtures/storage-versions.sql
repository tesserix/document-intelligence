do $$
begin
    if not exists (select 1 from ocr_uploads where upload_id = 'upl_MIGRATIONLEGACY'
        and object_version is null and coalesce(object_version, object_generation::text) = '42') then
        raise exception 'legacy generation fallback failed';
    end if;
end;
$$;

insert into ocr_uploads (
    upload_id, tenant_id, product_id, idempotency_key, request_digest,
    object_bucket, object_name, expected_content_type, expected_content_length,
    expected_digest, status, expires_at, object_version, verified_content_type,
    verified_content_length, verified_digest, uploaded_at
) values (
    'upl_MIGRATIONOPAQUE', 'ten_MIGRATION', 'example', 'migration-opaque',
    'sha256:' || repeat('b', 64), 's3://example-quarantine', 'opaque-object',
    'image/png', 128, 'sha256:' || repeat('b', 64), 'uploaded', now() + interval '1 hour',
    'opaque+version/fixture', 'image/png', 128, 'sha256:' || repeat('b', 64), now()
);

update ocr_uploads set object_version = '42' where upload_id = 'upl_MIGRATIONLEGACY';
do $$
begin
    if not exists (select 1 from ocr_uploads where upload_id = 'upl_MIGRATIONOPAQUE'
        and object_generation is null and object_version = 'opaque+version/fixture') then
        raise exception 'opaque version persistence failed';
    end if;
    if not exists (select 1 from ocr_uploads where upload_id = 'upl_MIGRATIONLEGACY'
        and object_generation = 42 and object_version = '42') then
        raise exception 'legacy dual-write failed';
    end if;
    begin
        update ocr_uploads set object_version = '43' where upload_id = 'upl_MIGRATIONLEGACY';
        raise check_violation using message = 'conflicting versions were accepted';
    exception when raise_exception then
        if sqlerrm <> 'inconsistent object version' then raise; end if;
    end;
end;
$$;
