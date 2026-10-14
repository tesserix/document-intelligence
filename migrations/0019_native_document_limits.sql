begin;
set local lock_timeout = '5s';
set local statement_timeout = '30s';

alter table ocr_uploads
    add constraint ocr_uploads_native_page_count check (parser_page_count between 1 and 1000) not valid,
    add constraint ocr_uploads_native_total_pixels check (parser_total_page_pixels between 1 and 100000000000) not valid;
alter table ocr_page_artifacts
    add constraint ocr_page_artifacts_native_page_number check (page_number between 1 and 1000) not valid;

alter table ocr_uploads validate constraint ocr_uploads_native_page_count;
alter table ocr_uploads validate constraint ocr_uploads_native_total_pixels;
alter table ocr_page_artifacts validate constraint ocr_page_artifacts_native_page_number;

alter table ocr_uploads
    drop constraint ocr_uploads_parser_page_count_check,
    drop constraint ocr_uploads_parser_total_page_pixels_check;
alter table ocr_page_artifacts drop constraint ocr_page_artifacts_page_number_check;
commit;
