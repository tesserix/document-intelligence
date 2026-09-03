set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    add column inspection_attempts integer not null default 0
        check (inspection_attempts between 0 and 10),
    add column inspection_lease_owner text
        check (char_length(inspection_lease_owner) between 1 and 128),
    add column inspection_lease_expires_at timestamptz,
    add constraint ocr_uploads_inspection_lease_shape check (
        (status = 'inspecting'
            and inspection_attempts > 0
            and inspection_lease_owner is not null
            and inspection_lease_expires_at is not null)
        or
        (status in ('accepted', 'rejected')
            and inspection_attempts > 0
            and inspection_lease_owner is null
            and inspection_lease_expires_at is null)
        or
        (status in ('reserved', 'uploaded', 'expired')
            and inspection_attempts = 0
            and inspection_lease_owner is null
            and inspection_lease_expires_at is null)
    ) not valid;

alter table ocr_uploads validate constraint ocr_uploads_inspection_lease_shape;
