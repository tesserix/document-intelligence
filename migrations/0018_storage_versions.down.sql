set lock_timeout = '5s';
set statement_timeout = '30s';
do $$
begin
    if exists (select 1 from ocr_uploads where
        (object_version is not null and object_generation is null)
        or (source_object_version is not null and source_object_generation is null)
        or length(object_bucket) > 63 or length(source_bucket) > 63 or object_bucket like '%://%' or source_bucket like '%://%')
        or exists (select 1 from ocr_results where object_generation is null or object_bucket like '%://%')
        or exists (select 1 from ocr_page_artifacts where object_generation is null or object_bucket like '%://%') then
        raise exception 'cannot downgrade while opaque storage versions exist';
    end if;
end;
$$;
drop trigger ocr_uploads_version_sync on ocr_uploads;
drop trigger ocr_uploads_source_version_sync on ocr_uploads;
drop trigger ocr_results_version_sync on ocr_results;
drop trigger ocr_page_artifacts_version_sync on ocr_page_artifacts;
drop function ocr_sync_object_version();
drop function ocr_sync_source_version();
alter table ocr_uploads drop constraint ocr_uploads_verification_complete;
alter table ocr_uploads
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
alter table ocr_uploads drop constraint ocr_uploads_accepted_source_complete;
alter table ocr_uploads
    add constraint ocr_uploads_accepted_source_complete check (
        (status = 'accepted'
            and source_bucket is not null
            and source_object_name is not null
            and source_object_generation is not null
            and source_digest = verified_digest
            and source_content_length = verified_content_length
            and accepted_at is not null)
        or
        (status <> 'accepted'
            and source_bucket is null
            and source_object_name is null
            and source_object_generation is null
            and source_digest is null
            and source_content_length is null
            and accepted_at is null)
    ) not valid;
alter table ocr_uploads drop constraint ocr_uploads_source_bucket_check;
alter table ocr_uploads add constraint ocr_uploads_source_bucket_check check (char_length(source_bucket) between 3 and 63);
alter table ocr_uploads drop column object_version, drop column source_object_version;
alter table ocr_results drop constraint ocr_results_version_present, drop column object_version, alter column object_generation set not null;
alter table ocr_page_artifacts drop constraint ocr_page_artifacts_version_present, drop column object_version, alter column object_generation set not null;
alter table ocr_uploads drop constraint ocr_uploads_object_bucket_check;
alter table ocr_uploads add constraint ocr_uploads_object_bucket_check
    check (char_length(object_bucket) between 3 and 63);
