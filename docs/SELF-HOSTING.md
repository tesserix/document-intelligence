# Self-hosting

The source is Apache-2.0. You can build it without Tesserix registry credentials.
The complete OCR pipeline currently requires PostgreSQL, Temporal, ClamAV,
versioned object storage and Google Document AI. GCS remains the default;
S3/Azure adapters require migration 0018 and real-cloud qualification. See
[bring your own storage](BRING-YOUR-OWN-STORAGE.md) for routes, IAM and secrets. It is not an offline OCR appliance.
Use your own accounts, buckets, processor and credentials; provider charges are
separate. Valkey and OTLP telemetry are optional.

## Build and local startup check

Install Docker with BuildKit, Git, and (for source tests) Rust 1.94, a C/C++
compiler, pkg-config and protoc. Clone this repository and select a reviewed
release tag or commit. Build both images:

```sh
docker build --pull -t ocr-service:local .
docker build --pull -f Dockerfile.parser -t ocr-parser-sandbox:local .
docker run --rm --read-only --cap-drop=ALL \
  --security-opt=no-new-privileges --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --env-file ci/ocr-service-smoke.env \
  -p 127.0.0.1:18080:8080 --name ocr-local ocr-service:local
```

In another terminal, `curl --fail http://127.0.0.1:18080/healthz` should succeed.
`curl -i http://127.0.0.1:18080/readyz` should return 503: this deliberately tests
startup without cloud or database dependencies. It is **not** an OCR installation.
Stop it with `docker stop ocr-local`. For automated checks of both images, run
`bash scripts/smoke-images.sh ocr-service:local ocr-parser-sandbox:local`
from the repository root (requires Python 3). The smoke identity is public test data;
never use it in a deployed environment.

## Provision an installation

1. Provision PostgreSQL and a dedicated database. Apply `migrations/*.sql` in
   lexical order, excluding every `*.down.sql`, through your migration runner.
   Track applied versions; do not rerun them blindly. Run as a migration owner.
   Application connections must be NOSUPERUSER and NOBYPASSRLS. Grant required
   table/sequence permissions and the work-scope functions from migration 0015.
   `scripts/setup-test-database.sh` is only for disposable test databases: it
   creates known passwords and exercises destructive down migrations.
2. Provision a Temporal server and namespace `document-intelligence-<environment>`
   (for example `document-intelligence-staging`). Configure a shared task queue
   for dispatch and execution workers. Keep Temporal behind private networking.
3. Provision separate private GCS buckets for quarantine, accepted sources,
   derived page artifacts and canonical results. Enable uniform bucket-level
   access, public-access prevention and lifecycle rules appropriate to your
   retention obligations. Create a Document AI OCR processor in your chosen
   residency region and enable the relevant Google APIs and billing.
4. Use workload identities for each service, with only the required bucket and
   processor permissions. Upload issuance needs permission to issue its upload
   capability. The execution worker defaults to Google metadata-server credentials (for example
   GKE Workload Identity). Outside GCP, explicitly configure the documented
   `application-default` mode and your own credential federation. Validate it
   from the actual worker environment before accepting traffic. Do not distribute
   service-account keys, reuse Tesserix identities, or put credentials in images.
5. Run an updated ClamAV daemon on the dispatch worker's loopback network
   (`127.0.0.1:3310`); the adapter intentionally rejects remote scanner addresses.
   In Kubernetes use a sidecar in the same pod. Give untrusted parsing bounded
   CPU, memory, process count and execution time, no unnecessary privileges,
   and a read-only filesystem. The parser is a local child process; its filename
   alone does not provide an operating-system sandbox.
6. Configure and run the API, dispatch worker and execution worker below. Use
   your secret manager to inject database credentials and independent random
   workload-signing keys. Put authenticated HTTPS ingress and rate limits in
   front of the API; never publish database, Temporal or scanner ports.

## Process configuration

All binaries below are included in the service image under `/app`.

| Process | Required configuration |
|---|---|
| `ocr-service` | `DATABASE_URL`, `OCR_API_CAPABILITY=full` (or separate `uploads`/`jobs` deployments), `OCR_WORKLOAD_IDENTITY_KEYS`, `QUARANTINE_BUCKETS`, `RESULT_BUCKETS` |
| `ocr-dispatch-worker` | `DATABASE_URL`, `WORK_SCOPE_DATABASE_URL`, `WORKER_ID`, `TEMPORAL_TARGET`, `TEMPORAL_NAMESPACE`, `TEMPORAL_TASK_QUEUE`, `QUARANTINE_BUCKETS`, `SOURCE_BUCKETS`, `CLAMD_ADDRESS=127.0.0.1:3310`, `PARSER_EXECUTABLE=/app/ocr-parser-sandbox` |
| `ocr-execution-worker` | `DATABASE_URL`, `WORKER_ID`, the same Temporal settings, `SOURCE_BUCKETS`, `PAGE_BUCKETS`, `RESULT_BUCKETS`, `DOCUMENT_AI_PROJECT_ID`, `DOCUMENT_AI_LOCATION`, `DOCUMENT_AI_PROCESSOR_ID` |

`WORKER_ID` must be unique per worker instance. `TEMPORAL_TARGET` is an HTTP(S)
URL to your server. Consult your network and server configuration for transport
security; do not expose an unauthenticated Temporal server to the internet.

Bucket maps use `product=bucket,other-product=other-bucket`. **Exception:** the
API's `RESULT_BUCKETS` is a plain comma-separated bucket allowlist; the execution
worker's `RESULT_BUCKETS` is a product-to-bucket map. Source, page and result
maps on the execution worker must cover the same products.

`OCR_WORKLOAD_IDENTITY_KEYS` uses `key-id=product:hex-key` entries, with at least
32 random bytes per key. Register a distinct key for each trusted product
backend. End users and mobile apps must not receive these keys. Backends must
verify their users and derive tenant scope before signing requests. See
[the signing contract](adr/0009-signed-workload-identity-envelope.md).

Run workers by overriding the image command, e.g.
`ocr-service:local /app/ocr-execution-worker`. Disable the API-specific Docker
health check on worker containers (`--no-healthcheck`); monitor worker activity
and queue progress instead. Kubernetes should configure its own readiness and
liveness probes. `healthz` alone is not proof of dependency health.

## Start the processes on a dedicated integration VM

For a first complete installation, use a dedicated Google Compute Engine Linux
VM with a metadata-server service account authorized for your own test buckets
and processor. Restrict access to the VM. This single-node example is for
integration qualification, not an HA production topology. Production should
separate per-process workload identities and place replicas across failure
domains. It requires the provisioned PostgreSQL and Temporal services above.

Create private `api.env`, `dispatch.env` and `execution.env` files outside the
checkout through your secret manager, using the process table above. Set
`OCR_API_CAPABILITY=full` in `api.env`. Use unique `WORKER_ID` values and
`PARSER_EXECUTABLE=/app/ocr-parser-sandbox` in the dispatch configuration. Keep
these files mode 0600 and never attach them to issues. Configure the API result
bucket **allowlist** separately from the execution worker result bucket **map**.

Use the published image digest from your selected release. After exporting
`OCR_IMAGE` to that digest-qualified image reference and `CLAMAV_IMAGE` to a
reviewed, digest-pinned official ClamAV image, run:

```sh
test -n "$OCR_IMAGE" && test -n "$CLAMAV_IMAGE"
docker network create ocr-integration

docker run -d --name ocr-api --network ocr-integration \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --memory 512m --cpus 1 --pids-limit 256 \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --env-file /secure/ocr/api.env -p 127.0.0.1:18080:8080 \
  "$OCR_IMAGE"

docker run -d --name ocr-dispatch --network ocr-integration \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --memory 1g --cpus 2 --pids-limit 256 --no-healthcheck \
  --tmpfs /tmp:rw,noexec,nosuid,size=128m \
  --env-file /secure/ocr/dispatch.env \
  "$OCR_IMAGE" /app/ocr-dispatch-worker

docker run -d --name ocr-clamav --network container:ocr-dispatch \
  --memory 2g --cpus 1 --pids-limit 256 \
  --mount type=volume,source=ocr-clamav-definitions,target=/var/lib/clamav \
  "$CLAMAV_IMAGE"

docker run -d --name ocr-execution --network ocr-integration \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --memory 1g --cpus 2 --pids-limit 256 --no-healthcheck \
  --tmpfs /tmp:rw,noexec,nosuid,size=128m \
  --env-file /secure/ocr/execution.env \
  "$OCR_IMAGE" /app/ocr-execution-worker
```

Wait for ClamAV to load current definitions before uploading. Worker progress
and result retrieval must be monitored separately from API health. Loopback
binding means port 18080 is accessible only on the VM; use an approved private
tunnel for administration. Do not open it publicly to make a test work. For
HTTPS product traffic, place a correctly configured authenticated ingress in
front and preserve the exact signed URI. The resource limits above are starting
values for test traffic, not measured throughput guarantees.

Check `docker logs` for startup failures without collecting or sharing secret
values, and check `curl --fail http://127.0.0.1:18080/readyz`. A missing signing
key prevents startup. A missing adapter keeps readiness at 503. A ready API
alone does not prove the dispatch/execution workers are processing jobs.

For Kubernetes, translate these processes into separate API and worker
Deployments, with ClamAV alongside dispatch in the same Pod, private ClusterIP
Services, non-root/read-only security contexts for the OCR containers, workload
identities, network policies and per-process resource limits. Use API readiness
and liveness probes, worker queue-progress monitoring, at least two API and
execution replicas, disruption budgets and topology spreading. Store the desired
configuration in your own GitOps repository. Do not depend on Tesserix's private
infrastructure repository.

## HTTP endpoints

The [OpenAPI specification](../contracts/v1/openapi.json) is the contract source
of truth. All `/v1/ocr/*` endpoints require server-side workload signatures.
Health/readiness are unauthenticated and should still remain private.

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/healthz` | Process liveness |
| GET | `/readyz` | API dependency/configuration readiness |
| POST | `/v1/ocr/uploads` | Reserve upload; requires `Idempotency-Key` |
| GET | `/v1/ocr/uploads/{upload_id}` | Poll inspection/acceptance |
| POST | `/v1/ocr/uploads/{upload_id}/complete` | Verify uploaded content and queue inspection |
| POST | `/v1/ocr/jobs` | Create a job for an accepted upload; requires `Idempotency-Key` |
| GET | `/v1/ocr/jobs/{job_id}` | Poll job status |
| GET | `/v1/ocr/jobs/{job_id}/result` | Retrieve the canonical result |
| POST | `/v1/ocr/jobs/{job_id}/cancel` | Request cancellation |

Upload bytes with PUT to the temporary `upload_url` returned by the reservation,
using its returned `required_headers`. This is a storage capability, not an OCR
API route. Do not log it, alter it or forward OCR identity headers to it.

## Signing and a complete test request

The signing input is the following UTF-8 string, **without a trailing newline**:

```text
key-id
tenant-id
unix-timestamp
HTTP-METHOD
/path?query
```

Join those five fields with literal LF newline bytes, not the two characters
backslash and n. Compute lowercase hexadecimal HMAC-SHA256 with the registered product's secret
bytes. Send `X-OCR-Key-Id`, `X-OCR-Tenant-Id`, `X-OCR-Timestamp` and
`X-OCR-Signature`. Synchronize clocks; the default permitted skew is 60 seconds.
Only the trusted product backend derives tenant identity after authenticating
its user. An idempotent retry reuses its idempotency key but gets a fresh timestamp
and signature. A different method/path needs a different signature.

The repository contains the tested Python signing implementation and full
upload-to-result client in `scripts/ocr_benchmark.py`. From the repository root,
load one test product identity into `OCR_BENCHMARK_IDENTITY` through your secret
manager (format `key-id=product:hex-key`), and use the same API for both routes
when running the `full` capability:

```sh
python3 scripts/ocr_benchmark.py \
  --manifest benchmarks/v1/manifest.json \
  --upload-url http://127.0.0.1:18080 \
  --job-url http://127.0.0.1:18080 \
  --tenant ten_selfhost_verification \
  --output /tmp/ocr-first-run.json
cargo run -p ocr-eval -- \
  benchmarks/v1/manifest.json /tmp/ocr-first-run.json
```

Every run creates 21 synthetic uploads/jobs and incurs provider usage. Choose a
new output filename on repeat runs; existing reports are not overwritten. The
runner fails if a case fails or is cancelled, and attempts cancellation if
polling times out. Retain all failures when comparing results; never count a
missing result as zero cost or successful OCR.

The client performs: reserve → upload bytes → complete → wait for `accepted` →
create job → wait for `completed`, `partial` or `review_required` → retrieve
result. A partial/review result is retrievable but is not automatically accurate.
Preserve warnings, validation failures and evidence when using its fields.

| Symptom | Check |
|---|---|
| 401 | Registered key ID/product, signature bytes, clock skew, exact method/URI |
| 404 | Authenticated product/tenant ownership and the correct upload/job ID |
| 409 result not ready | Continue bounded polling; do not create duplicate jobs |
| Upload remains uploaded | Dispatch worker, scanner definitions, parser, source promotion and work-scope DB privileges |
| Job does not finish | Temporal namespace/task queue, execution worker, provider IAM/quota and storage access |
| Result artifact unavailable | Result-bucket mapping, object-read permission and worker publication logs |
| No text detected | Valid text-free image; retain this outcome and use the consumer's appropriate fallback |
| Malformed provider response | Inspect content-free diagnostic categories; do not log provider payloads |

## Verify the complete journey

Follow [the API guide](design/OCR-API-USAGE.md): reserve an upload, PUT the exact
bytes using returned headers, complete it, wait for acceptance, create a job,
poll it, and retrieve the result. Verify cross-tenant access returns 404 and
that unavailable dependencies fail closed. Check a text-free photograph, a
clear document, a rotated document, an unreadable image and a multi-page PDF.

Run the [21-case benchmark](../benchmarks/README.md) against your installation,
using a dedicated test tenant and your own signing key from a secret manager.
Retain the report, image version, configuration and costs. Test recovery from
worker termination and provider/storage outages before onboarding customers.

## Operations

Back up PostgreSQL and required artifacts; exercise restoration before launch.
Define retention and account-deletion behavior for originals, derived pages,
results and backups. Set provider quotas, bounded worker concurrency, resource
limits and cost alerts. Monitor successful result retrieval, queue age,
provider failures and p95 latency. Keep the previous image digest available for
rollback; review database compatibility separately. Never log document payloads,
signed URLs or identity headers. Report vulnerabilities through SECURITY.md.
