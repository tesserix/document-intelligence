set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    drop constraint ocr_uploads_rejection_reason_shape,
    drop column rejection_reason;
