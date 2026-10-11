# Self-hosting

The source is Apache-2.0. You can build it without Tesserix registry credentials.
The complete OCR pipeline currently requires PostgreSQL, Temporal, ClamAV,
Google Cloud Storage and Google Document AI. It is not an offline OCR appliance.
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
   capability. The execution worker requires Google metadata-server credentials (for example
   GKE Workload Identity); a local ADC login alone is insufficient. Validate it
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
