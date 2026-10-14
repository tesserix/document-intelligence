begin;
set local lock_timeout = '5s';
set local statement_timeout = '30s';

do $$ begin
    if exists (select 1 from ocr_uploads where parser_page_count > 300 or parser_total_page_pixels > 1000000000)
        or exists (select 1 from ocr_page_artifacts where page_number > 300)
        or exists (select 1 from ocr_page_workflows where jsonb_array_length(checkpoint->'pages') > 300) then
        raise exception 'cannot downgrade native document limits while larger documents remain';
    end if;
end $$;

alter table ocr_uploads
    add constraint ocr_uploads_parser_page_count_check check (parser_page_count between 1 and 300) not valid,
    add constraint ocr_uploads_parser_total_page_pixels_check check (parser_total_page_pixels between 1 and 1000000000) not valid;
alter table ocr_page_artifacts
    add constraint ocr_page_artifacts_page_number_check check (page_number between 1 and 300) not valid;

alter table ocr_uploads validate constraint ocr_uploads_parser_page_count_check;
alter table ocr_uploads validate constraint ocr_uploads_parser_total_page_pixels_check;
alter table ocr_page_artifacts validate constraint ocr_page_artifacts_page_number_check;

alter table ocr_uploads drop constraint ocr_uploads_native_page_count, drop constraint ocr_uploads_native_total_pixels;
alter table ocr_page_artifacts drop constraint ocr_page_artifacts_native_page_number;
commit;
