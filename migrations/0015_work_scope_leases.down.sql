set lock_timeout = '5s';
set statement_timeout = '30s';

drop function if exists ocr_release_work_scope(text, text, text);
drop function if exists ocr_claim_work_scopes(text, integer);
drop function if exists ocr_set_work_scope_pending(text, text, text, boolean);
drop function if exists ocr_register_work_scope(text, text, text);
drop index if exists ocr_work_scopes_lease_idx;
drop table if exists ocr_work_scopes;
