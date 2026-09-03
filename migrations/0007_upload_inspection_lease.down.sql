set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    drop constraint ocr_uploads_inspection_lease_shape,
    drop column inspection_lease_expires_at,
    drop column inspection_lease_owner,
    drop column inspection_attempts;
