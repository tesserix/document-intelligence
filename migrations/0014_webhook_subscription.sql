set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_jobs
    add column webhook_subscription_id text;

alter table ocr_jobs
    add constraint ocr_jobs_webhook_subscription_id_shape
    check (webhook_subscription_id ~ '^whs_[A-Za-z0-9_]{1,64}$') not valid;

alter table ocr_jobs validate constraint ocr_jobs_webhook_subscription_id_shape;
