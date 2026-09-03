set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_jobs drop column webhook_subscription_id;
