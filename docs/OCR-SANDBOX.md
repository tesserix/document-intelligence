# OCR sandbox

A small, anonymous capability test for this OCR service. The standalone
`ocr-sandbox` binary serves both the website and a session-authenticated API.
It uses the existing upload, inspection, OCR and rule-based extraction pipeline.
There are **no JEV, chat or generative-model calls**. OCR itself uses Google
Document AI and incurs provider charges.

The planned hosted address is `https://ocr-test.tesserix.app`. This document does
not imply that a public instance has been deployed. Use your deployed origin in
the examples below.

## What you can test

- Upload one JPEG or PNG (at most 4 MiB and 12 megapixels).
- Read extracted text, warnings, provider confidence, and optional nutrition fields.
- Download the result JSON.
- Compare the bundled nutrition label, receipt and blank-image samples against
  checked-in expectations. Scores are conditional on the exact sample hash.
  Character error rate uses normalized text; it is not general OCR accuracy.
- An arbitrary image has no known reference, so the UI shows **Not scored**.
  OCR cannot determine food weight or how much someone ate from a photograph.

## API walkthrough

Requires Bash, curl, jq, Python 3, and a non-sensitive PNG fixture. This example
uses the repository's blank-image sample and does not enable extraction.

```bash
set -euo pipefail
BASE=http://localhost:8080 # replace with your HTTPS sandbox origin
FILE=benchmarks/v1/images/no_text.png
SESSION=$(curl --fail-with-body -sS -X POST "$BASE/v1/sandbox/sessions")
TOKEN=$(printf '%s' "$SESSION" | jq -er .token)
SIZE=$(wc -c < "$FILE" | tr -d ' ')
SHA=$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$FILE")
UPLOAD=$(jq -n --arg sha "sha256:$SHA" --argjson size "$SIZE" \
  '{content_type:"image/png",content_length:$size,sha256:$sha}' |
  curl --fail-with-body -sS -X POST "$BASE/v1/ocr/uploads" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' --data-binary @-)
ID=$(printf '%s' "$UPLOAD" | jq -er .upload_id)
curl --fail-with-body -sS -X PUT "$BASE/v1/ocr/uploads/$ID/content" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: image/png' --data-binary "@$FILE"
curl --fail-with-body -sS -X POST "$BASE/v1/ocr/uploads/$ID/complete" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{}'
# Wait for inspection before creating a job.
for attempt in {1..60}; do
  STATUS=$(curl --fail-with-body -sS "$BASE/v1/ocr/uploads/$ID" \
    -H "Authorization: Bearer $TOKEN" | jq -er .status)
  case "$STATUS" in
    accepted) break;;
    rejected|expired) printf 'Upload: %s\n' "$STATUS"; exit 1;;
  esac
  sleep 2
done
[ "$STATUS" = accepted ] || exit 1
JOB=$(jq -n --arg id "$ID" \
  '{source:{upload_id:$id},document_type:"general",processing_class:"interactive",output:{text:true,layout:true,evidence:true}}' |
  curl --fail-with-body -sS -X POST "$BASE/v1/ocr/jobs" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' --data-binary @-)
JOB_ID=$(printf '%s' "$JOB" | jq -er .job_id)
for attempt in {1..120}; do
  STATUS=$(curl --fail-with-body -sS "$BASE/v1/ocr/jobs/$JOB_ID" \
    -H "Authorization: Bearer $TOKEN" | jq -er .status)
  case "$STATUS" in
    completed|partial|review_required) break;;
    failed|cancelled) printf 'Job: %s\n' "$STATUS"; exit 1;;
  esac
  sleep 2
done
case "$STATUS" in completed|partial|review_required) ;; *) exit 1;; esac
curl --fail-with-body -sS -D result-headers.txt \
  "$BASE/v1/ocr/jobs/$JOB_ID/result" -H "Authorization: Bearer $TOKEN" -o result.json
# x-sandbox-images-deleted is true only after storage absence is verified.
curl --fail-with-body -sS "$BASE/v1/sandbox/session" -H "Authorization: Bearer $TOKEN"
unset TOKEN SESSION
```

Do not enable shell tracing or put session tokens in URLs. Clients must not use
workload signing credentials. A session token permits access only to its single
upload and job; cross-session lookups return 404. To cancel, POST an empty JSON
object to `/v1/ocr/jobs/{job_id}/cancel` with the session bearer token.

| Limit | Value |
| --- | --- |
| Session lifetime | 15 minutes |
| Images/jobs per session | 1 |
| Requests per session | 60/minute |
| Sessions per source network | 10/hour |
| Shared session budget | 1,000/day |
| Shared upload budget | Configurable 1–100/day; default 100 |
| Adapter concurrent requests | 8 per process |

Counters are shared through PostgreSQL and fail closed. IPv6 addresses are grouped
by /64. Only the actual TCP peer is used; forwarded headers are not trusted.
Behind a reverse proxy, visitors may share its source-network limit. Configure
trusted edge rate limiting separately; do not weaken the adapter by accepting
arbitrary `X-Forwarded-For`. Anonymous access cannot guarantee a unique human.
A depleted budget returns 429; database/storage outages return 503. Retrying
identical admitted requests does not create a second upload or job.

## Privacy and cleanup

Use non-sensitive test images. A durable cleanup worker runs independently of the
browser. It removes quarantine and accepted source images after terminal OCR (or
rejection), retries failures, and verifies absence before reporting deletion.
The UI reports pending cleanup honestly. The local file and browser preview are
not deleted. Result and page artifacts are removed after session expiry plus a
two-minute in-flight buffer, once unfinished processing has terminated. Google
Document AI processing is subject to its own provider data policy.

Dedicated GCS buckets must have **soft delete and versioning disabled**, no
retention policy and no default event hold. The adapter checks this on readiness
and before purging, and deletes exact generations only under its session prefix.
A one-day bucket lifecycle rule is a delayed outage backstop, not the immediate
cleanup mechanism. The adapter never changes another product's bucket policy.
Session metadata is pruned after seven days only when purge is complete (or no
upload was admitted). Configure a separate retention job for the isolated core
OCR database, logs and backups; the adapter does not purge those core tables.
Never log image bytes, extracted content, session tokens or signed URLs.

## Self-hosting

Use GKE or another container host with PostgreSQL and Google application default
credentials. GitHub Pages alone cannot protect server credentials or run durable
cleanup. The image has no shell and runs as a non-root user.

```bash
cargo build --locked --release -p ocr-sandbox
# Apply to a dedicated database with an administrative migration identity:
psql "$SANDBOX_DATABASE_URL" -v ON_ERROR_STOP=1 \
  -f crates/ocr-sandbox/migrations/0001_sessions.sql
# Set the environment described below, then:
./target/release/ocr-sandbox
# Or build the standalone container:
docker build -f Dockerfile.sandbox -t ocr-sandbox:local .
```

| Environment | Purpose |
| --- | --- |
| `SANDBOX_ORIGIN` | Exact public HTTPS origin; HTTP accepted only for localhost |
| `SANDBOX_UPLOAD_API` | Private upload API base URL |
| `SANDBOX_JOB_API` | Private job API base URL |
| `SANDBOX_DATABASE_URL` | Dedicated adapter ledger PostgreSQL connection |
| `SANDBOX_WORKLOAD_IDENTITY` | `key-id=ocr-sandbox:<64 hex characters>`; from a secret manager |
| `SANDBOX_BUCKETS` | Four distinct bare GCS names, quarantine/source/pages/results, each starting `ocr-sandbox-` |
| `SANDBOX_DAILY_UPLOADS` | Optional 1–100, default 100 |
| `SANDBOX_ADMISSION_ENABLED` | Default false; explicitly true only after qualification |
| `SANDBOX_BIND_ADDRESS` | Optional, default `0.0.0.0:8080` |

Register the same workload identity in the private OCR APIs, scoped exclusively
to product `ocr-sandbox`. Generate a random 32-byte secret; store it in OpenBao or
your secret manager, never Terraform values/state, Git or browser JavaScript.
Use a dedicated core OCR database, queue, workers and four buckets. Do not point
this public adapter at Kora or Roamie storage or use their signing credentials.
The current public adapter cleanup supports GCS; the core service's S3/Azure
support does not imply this adapter's cleanup is qualified for those providers.

The runtime database role needs SELECT/INSERT/UPDATE/DELETE only on the adapter
ledger tables. Apply schema changes separately; the binary does not migrate on
startup. Bind a workload identity with bucket metadata read, object list and
object delete on exactly these four buckets. The upload content PUT uses the
private API's scoped signed URL, never exposes it to the browser, and validates
its bucket/path. Core OCR identities require their normal scoped read/write and
Document AI permissions, documented in the deployment guide.

Expose only port 8080 through HTTPS ingress with request-size, connection and
edge request-rate limits. `/healthz` is process liveness; `/readyz` checks the
ledger and deletion policy. Configure a read-only root filesystem, no privilege
escalation, dropped capabilities, seccomp RuntimeDefault, resource limits and
network policies allowing only DNS, dedicated PostgreSQL/private OCR APIs and
required Google endpoints. Preserve the cleanup worker during admission outages.
Disable admission before rollback and drain cleanup; do not drop its database
while objects remain. The down migration refuses outstanding cleanup.

See [architecture and rollout gates](adr/0015-public-ocr-sandbox.md). Before public
admission, run a real fixture through upload → inspection → OCR → results and
verify all image generations disappear; test browser closure, worker restart,
quota exhaustion, cancellation and a failed delete. Local mocked-provider tests
do not replace this cloud qualification.

## Development verification

```bash
cargo fmt --all -- --check
cargo clippy -p ocr-sandbox --all-targets -- -D warnings
# The tests create isolated schemas in a local disposable PostgreSQL database.
TEST_DATABASE_ADMIN_URL=postgres://postgres:local@localhost:5432/ocr_test \
  cargo test -p ocr-sandbox -- --include-ignored
node --check crates/ocr-sandbox/web/app.mjs
node --test crates/ocr-sandbox/web/score.test.mjs
```

Integration tests use a local HTTP OCR/storage fixture and a real PostgreSQL
ledger. They verify signing, authorization, quotas, idempotency, cleanup failure
and restart recovery. They do not call the real provider or spend cloud budget.

See the [local verification record](OCR-SANDBOX-VERIFICATION.md) for executed checks and remaining cloud qualification.
