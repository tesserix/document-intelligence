begin;
set local lock_timeout = '5s';
set local statement_timeout = '30s';
do $$ begin
    if exists (select 1 from ocr_results where content_length>16777216)
        or exists (select 1 from ocr_page_artifacts where content_length>16777216) then
        raise exception 'cannot downgrade output limits while larger artifacts remain';
    end if;
end $$;
alter table ocr_results add constraint ocr_results_content_length_check check (content_length between 1 and 16777216) not valid;
alter table ocr_page_artifacts add constraint ocr_page_artifacts_content_length_check check (content_length between 1 and 16777216) not valid;
alter table ocr_results validate constraint ocr_results_content_length_check;
alter table ocr_page_artifacts validate constraint ocr_page_artifacts_content_length_check;
alter table ocr_results drop constraint ocr_results_native_content_length;
alter table ocr_page_artifacts drop constraint ocr_page_artifacts_native_content_length;
commit;
