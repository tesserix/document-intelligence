begin;
set local lock_timeout = '5s';
set local statement_timeout = '30s';
do $$ begin
    if exists (select 1 from ocr_native_documents) then
        raise exception 'cannot remove native page cache while documents remain';
    end if;
end $$;
drop table ocr_native_pages;
drop table ocr_native_documents;
commit;
