# Sandbox deployment reference

`storage/` is an optional Terraform module for **new dedicated buckets**. It is
not wired into Tesserix's production state. Import/adapt through your owning
infrastructure repository; never apply it against existing product buckets.
Pass a project, region, globally unique bucket suffix and dedicated cleanup IAM
principal. No secrets are accepted or generated. The role ID is project-wide;
instantiate once per project or compose it centrally for multiple sandboxes.

The module explicitly disables recoverable object retention to match the public
sandbox's deletion contract. It prevents bucket destruction and includes a
one-day object lifecycle backstop. Core OCR read/write identities still need
separate bucket-scoped IAM. Keep the workload HMAC secret in OpenBao and inject
it into the adapter and private OCR APIs through your existing secret operator.

Deploy `Dockerfile.sandbox` through your normal GitOps pipeline. The [operator
guide](../../docs/OCR-SANDBOX.md#self-hosting) lists required environment,
PostgreSQL migration, isolation, probes, resource and network controls. Keep
`SANDBOX_ADMISSION_ENABLED=false` until real-cloud cleanup and quota tests pass.
Do not copy Kora's or Roamie's database, buckets or service credentials.

Security scan interpretation: Trivy flags disabled versioning (GCP-0078) and
Google-managed rather than customer-managed encryption (GCP-0066). Versioning is
intentionally off to honor verified deletion; buckets remain encrypted at rest
with Google's managed keys and block public access. These findings are retained,
not suppressed. A deployment needing CMEK can add it without enabling retention.
The distroless image exposes `/healthz` and `/readyz` for platform probes; it does
not contain curl or a shell for a Docker HEALTHCHECK (DS-0026).
