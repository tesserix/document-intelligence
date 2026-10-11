set lock_timeout = '5s';
set statement_timeout = '30s';

-- Expand only: legacy readers/writers retain numeric GCS generations.
alter table ocr_uploads
    add column object_version text check (length(object_version) between 1 and 1024 and object_version ~ '^[!-~]+$' and object_version <> 'null'),
    add column source_object_version text check (length(source_object_version) between 1 and 1024 and source_object_version ~ '^[!-~]+$' and source_object_version <> 'null');
alter table ocr_results
    add column object_version text check (length(object_version) between 1 and 1024 and object_version ~ '^[!-~]+$' and object_version <> 'null'),
    alter column object_generation drop not null,
    add constraint ocr_results_version_present check (object_version is not null or object_generation is not null);
alter table ocr_page_artifacts
    add column object_version text check (length(object_version) between 1 and 1024 and object_version ~ '^[!-~]+$' and object_version <> 'null'),
    alter column object_generation drop not null,
    add constraint ocr_page_artifacts_version_present check (object_version is not null or object_generation is not null);

create function ocr_sync_object_version() returns trigger language plpgsql as $$
begin
    if new.object_version is null then
        new.object_version := new.object_generation::text;
    elsif new.object_version ~ '^[1-9][0-9]{0,18}$'
        and length(new.object_version) <= 19
        and new.object_version::numeric <= 9223372036854775807 then
        if new.object_generation is not null and new.object_generation::text <> new.object_version then
            raise exception 'inconsistent object version';
        end if;
        new.object_generation := new.object_version::bigint;
    elsif new.object_generation is not null then
        raise exception 'inconsistent object version';
    end if;
    return new;
end;
$$;

create function ocr_sync_source_version() returns trigger language plpgsql as $$
begin
    if new.source_object_version is null then
        new.source_object_version := new.source_object_generation::text;
    elsif new.source_object_version ~ '^[1-9][0-9]{0,18}$'
        and length(new.source_object_version) <= 19
        and new.source_object_version::numeric <= 9223372036854775807 then
        if new.source_object_generation is not null and new.source_object_generation::text <> new.source_object_version then
            raise exception 'inconsistent source version';
        end if;
        new.source_object_generation := new.source_object_version::bigint;
    elsif new.source_object_generation is not null then
        raise exception 'inconsistent source version';
    end if;
    return new;
end;
$$;

create trigger ocr_uploads_version_sync before insert or update on ocr_uploads
    for each row execute function ocr_sync_object_version();
create trigger ocr_uploads_source_version_sync before insert or update on ocr_uploads
    for each row execute function ocr_sync_source_version();
create trigger ocr_results_version_sync before insert or update on ocr_results
    for each row execute function ocr_sync_object_version();
create trigger ocr_page_artifacts_version_sync before insert or update on ocr_page_artifacts
    for each row execute function ocr_sync_object_version();

alter table ocr_uploads drop constraint ocr_uploads_verification_complete;
alter table ocr_uploads add constraint ocr_uploads_verification_complete check (
    (status in ('reserved', 'expired') and object_generation is null and object_version is null
        and verified_content_type is null and verified_content_length is null
        and verified_digest is null and uploaded_at is null)
    or
    (status in ('uploaded', 'inspecting', 'accepted', 'rejected')
        and coalesce(object_version, object_generation::text) is not null
        and verified_content_type is not null and verified_content_length is not null
        and verified_digest is not null and uploaded_at is not null
        and verified_content_type = expected_content_type
        and verified_content_length = expected_content_length
        and verified_digest = expected_digest)
) not valid;
alter table ocr_uploads validate constraint ocr_uploads_verification_complete;

alter table ocr_uploads drop constraint ocr_uploads_accepted_source_complete;
alter table ocr_uploads add constraint ocr_uploads_accepted_source_complete check (
    (status = 'accepted' and source_bucket is not null and source_object_name is not null
        and coalesce(source_object_version, source_object_generation::text) is not null
        and source_digest = verified_digest and source_content_length = verified_content_length
        and accepted_at is not null)
    or
    (status <> 'accepted' and source_bucket is null and source_object_name is null
        and source_object_generation is null and source_object_version is null
        and source_digest is null and source_content_length is null and accepted_at is null)
) not valid;
alter table ocr_uploads validate constraint ocr_uploads_accepted_source_complete;
alter table ocr_uploads drop constraint ocr_uploads_source_bucket_check;
alter table ocr_uploads add constraint ocr_uploads_source_bucket_check
    check (char_length(source_bucket) between 3 and 222) not valid;
alter table ocr_uploads validate constraint ocr_uploads_source_bucket_check;
alter table ocr_uploads drop constraint ocr_uploads_object_bucket_check;
alter table ocr_uploads add constraint ocr_uploads_object_bucket_check
    check (char_length(object_bucket) between 3 and 222) not valid;
alter table ocr_uploads validate constraint ocr_uploads_object_bucket_check;
