set lock_timeout = '5s';
set statement_timeout = '30s';

drop index concurrently if exists ocr_outbox_claimable_idx;

alter table ocr_outbox
    drop constraint ocr_outbox_delivery_shape,
    drop column dead_lettered_at,
    drop column delivery_lease_expires_at,
    drop column delivery_lease_owner,
    drop column delivery_attempts;
