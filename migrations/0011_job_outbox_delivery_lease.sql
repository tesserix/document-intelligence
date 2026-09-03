set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_outbox
    add column delivery_attempts integer not null default 0
        check (delivery_attempts between 0 and 20),
    add column delivery_lease_owner text
        check (char_length(delivery_lease_owner) between 1 and 128),
    add column delivery_lease_expires_at timestamptz,
    add column dead_lettered_at timestamptz,
    add constraint ocr_outbox_delivery_shape check (
        (published_at is not null
            and delivery_attempts > 0
            and delivery_lease_owner is null
            and delivery_lease_expires_at is null
            and dead_lettered_at is null)
        or
        (published_at is null
            and dead_lettered_at is not null
            and delivery_attempts = 20
            and delivery_lease_owner is null
            and delivery_lease_expires_at is null)
        or
        (published_at is null
            and dead_lettered_at is null
            and ((delivery_attempts = 0
                    and delivery_lease_owner is null
                    and delivery_lease_expires_at is null)
                or
                (delivery_attempts > 0
                    and delivery_lease_owner is not null
                    and delivery_lease_expires_at is not null)))
    ) not valid;

alter table ocr_outbox validate constraint ocr_outbox_delivery_shape;

create index concurrently ocr_outbox_claimable_idx
    on ocr_outbox (product_id, tenant_id, event_id)
    where published_at is null and dead_lettered_at is null;
