set lock_timeout = '5s';
set statement_timeout = '30s';

drop table if exists ocr_outbox;
drop table if exists ocr_jobs;
drop type if exists ocr_job_status;
