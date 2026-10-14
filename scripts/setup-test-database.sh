#!/usr/bin/env bash
set -euo pipefail

test_port="${OCR_TEST_PORT:-5432}"
[[ "$test_port" =~ ^[0-9]{1,5}$ ]] || exit 1
admin_url="postgres://postgres:local@127.0.0.1:${test_port}/postgres"
database_name="ocr_test"
application_role="ocr_test_app"

psql "$admin_url" -v ON_ERROR_STOP=1 <<SQL >/dev/null
create role ${application_role} login password 'local' nosuperuser nocreatedb nocreaterole;
create database ${database_name};
SQL

database_admin_url="${admin_url%/*}/${database_name}"
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
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0016_parser_page_geometry.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0017_job_extraction_schema.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f scripts/fixtures/storage-legacy.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0018_storage_versions.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f scripts/fixtures/storage-versions.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0019_native_document_limits.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0020_native_page_cache.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0021_document_output_limits.sql >/dev/null
rollback_log="$(mktemp)"
trap 'rm -f "$rollback_log"' EXIT
if psql "$database_admin_url" -1 -v ON_ERROR_STOP=1 -f migrations/0018_storage_versions.down.sql >"$rollback_log" 2>&1; then
    echo "storage downgrade unexpectedly accepted opaque references" >&2
    exit 1
fi
grep -q 'cannot downgrade while opaque storage versions exist' "$rollback_log"
psql "$database_admin_url" -v ON_ERROR_STOP=1 -c "delete from ocr_uploads where upload_id in ('upl_MIGRATIONLEGACY', 'upl_MIGRATIONOPAQUE')" >/dev/null

psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0021_document_output_limits.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0020_native_page_cache.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0019_native_document_limits.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0018_storage_versions.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0017_job_extraction_schema.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0016_parser_page_geometry.down.sql >/dev/null
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
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0016_parser_page_geometry.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0017_job_extraction_schema.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0018_storage_versions.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0018_storage_versions.down.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -c 'alter table ocr_uploads drop constraint ocr_uploads_verification_complete, drop constraint ocr_uploads_accepted_source_complete' >/dev/null
psql "$database_admin_url" -1 -v ON_ERROR_STOP=1 -f migrations/0018_storage_versions.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0019_native_document_limits.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0020_native_page_cache.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 -f migrations/0021_document_output_limits.sql >/dev/null
psql "$database_admin_url" -v ON_ERROR_STOP=1 <<'SQL' >/dev/null
do $$ begin
    if (select count(*) from pg_constraint where conrelid = 'ocr_uploads'::regclass
        and conname in ('ocr_uploads_verification_complete', 'ocr_uploads_accepted_source_complete')
        and convalidated) <> 2 then
        raise exception 'storage migration did not restore validated upload constraints';
    end if;
end $$;
SQL
psql "$database_admin_url" -v ON_ERROR_STOP=1 <<SQL >/dev/null
grant usage on schema public to ${application_role};
grant select, insert, update, delete on ocr_jobs, ocr_outbox to ${application_role};
grant select, insert on ocr_results to ${application_role};
grant select, insert, update, delete on ocr_uploads to ${application_role};
grant select, insert, update, delete on ocr_upload_outbox to ${application_role};
grant select, insert, update, delete on ocr_page_workflows to ${application_role};
grant select, insert on ocr_page_artifacts to ${application_role};
grant select, insert, delete on ocr_native_documents, ocr_native_pages to ${application_role};
grant usage, select on sequence ocr_native_documents_native_document_id_seq to ${application_role};
grant usage, select on sequence ocr_upload_outbox_event_id_seq to ${application_role};
grant usage, select on sequence ocr_outbox_event_id_seq to ${application_role};
grant execute on function ocr_claim_work_scopes(text, integer) to ${application_role};
grant execute on function ocr_release_work_scope(text, text, text) to ${application_role};
grant execute on function ocr_register_work_scope(text, text, text) to ${application_role};
grant execute on function ocr_set_work_scope_pending(text, text, text, boolean) to ${application_role};
SQL

echo "TEST_DATABASE_URL=postgres://${application_role}:local@127.0.0.1:${test_port}/${database_name}"
echo "TEST_DATABASE_ADMIN_URL=${database_admin_url}"

if ! command -v pdftotext >/dev/null; then
    if command -v apt-get >/dev/null; then
        sudo apt-get update >/dev/null
        sudo apt-get install --no-install-recommends -y poppler-utils >/dev/null
    else
        echo 'Install Poppler (pdftotext) before native-processing tests.' >&2
        exit 1
    fi
fi
cargo build --locked -p ocr-parser-sandbox --bin ocr-parser-sandbox >&2
