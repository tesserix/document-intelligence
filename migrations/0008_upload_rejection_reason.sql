set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    add column rejection_reason text check (rejection_reason in (
        'inspection_attempts_exhausted', 'malware_detected', 'invalid_document',
        'parser_limits_exceeded'
    ));

update ocr_uploads
set rejection_reason = 'inspection_attempts_exhausted'
where status = 'rejected';

alter table ocr_uploads
    add constraint ocr_uploads_rejection_reason_shape check (
        (status = 'rejected' and rejection_reason is not null)
        or (status <> 'rejected' and rejection_reason is null)
    ) not valid;

alter table ocr_uploads validate constraint ocr_uploads_rejection_reason_shape;
