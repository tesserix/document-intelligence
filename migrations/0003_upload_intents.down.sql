set lock_timeout = '5s';
set statement_timeout = '30s';

drop table if exists ocr_uploads;
drop type if exists ocr_upload_status;
