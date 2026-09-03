set lock_timeout = '5s';
set statement_timeout = '30s';

drop table if exists ocr_results;
alter table ocr_jobs drop constraint if exists ocr_jobs_scope_identity_unique;
