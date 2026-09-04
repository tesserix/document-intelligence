#!/usr/bin/env bash
set -euo pipefail

admin_url="postgres://postgres:local@127.0.0.1:5432/postgres"
database_name="ocr_test"
application_role="ocr_test_app"

psql "$admin_url" -v ON_ERROR_STOP=1 <<SQL >/dev/null
create role ${application_role} login password 'local' nosuperuser nocreatedb nocreaterole;
create database ${database_name};
SQL

database_admin_url="postgres://postgres:local@127.0.0.1:5432/${database_name}"
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0001_jobs.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0002_result_locators.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0003_upload_intents.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0004_upload_reconciliation.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0005_job_upload_source.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0006_accepted_source.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0007_upload_inspection_lease.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0008_upload_rejection_reason.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0009_parser_inspection_metadata.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0010_import_rejection_reasons.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0011_job_outbox_delivery_lease.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0012_page_workflow_checkpoints.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0013_page_artifacts.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0014_webhook_subscription.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0015_work_scope_leases.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0015_work_scope_leases.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0014_webhook_subscription.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0013_page_artifacts.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0012_page_workflow_checkpoints.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0011_job_outbox_delivery_lease.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0010_import_rejection_reasons.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0009_parser_inspection_metadata.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0008_upload_rejection_reason.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0007_upload_inspection_lease.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0006_accepted_source.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0005_job_upload_source.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0004_upload_reconciliation.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0003_upload_intents.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0002_result_locators.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0001_jobs.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0001_jobs.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0002_result_locators.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0003_upload_intents.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0004_upload_reconciliation.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0005_job_upload_source.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0006_accepted_source.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0007_upload_inspection_lease.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0008_upload_rejection_reason.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0009_parser_inspection_metadata.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0010_import_rejection_reasons.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0011_job_outbox_delivery_lease.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0012_page_workflow_checkpoints.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0013_page_artifacts.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0014_webhook_subscription.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0015_work_scope_leases.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 <<SQL >/dev/null
grant usage on schema public to ${application_role};
grant select, insert, update, delete on ocr_jobs, ocr_outbox to ${application_role};
grant select, insert on ocr_results to ${application_role};
grant select, insert, update, delete on ocr_uploads to ${application_role};
grant select, insert, update, delete on ocr_upload_outbox to ${application_role};
grant select, insert, update, delete on ocr_page_workflows to ${application_role};
grant select, insert on ocr_page_artifacts to ${application_role};
grant usage, select on sequence ocr_upload_outbox_event_id_seq to ${application_role};
grant usage, select on sequence ocr_outbox_event_id_seq to ${application_role};
grant execute on function ocr_claim_work_scopes(text, integer) to ${application_role};
grant execute on function ocr_release_work_scope(text, text, text) to ${application_role};
grant execute on function ocr_register_work_scope(text, text, text) to ${application_role};
grant execute on function ocr_set_work_scope_pending(text, text, text, boolean) to ${application_role};
SQL

echo "TEST_DATABASE_URL=postgres://${application_role}:local@127.0.0.1:5432/${database_name}"
echo "TEST_DATABASE_ADMIN_URL=${database_admin_url}"
