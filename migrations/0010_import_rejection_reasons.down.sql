set lock_timeout = '5s';
set statement_timeout = '30s';

update ocr_uploads
set rejection_reason = 'invalid_document'
where rejection_reason in ('password_required', 'source_conflict');

alter table ocr_uploads
    drop constraint ocr_uploads_rejection_reason_check,
    add constraint ocr_uploads_rejection_reason_check check (rejection_reason in (
        'inspection_attempts_exhausted', 'malware_detected', 'invalid_document',
        'parser_limits_exceeded'
    )) not valid;

alter table ocr_uploads validate constraint ocr_uploads_rejection_reason_check;
