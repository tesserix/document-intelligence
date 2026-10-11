# Release qualification

The initial open-source distribution is beta. Production use in one consumer
is not a certification for every document type, language, workload or host.

A release must have passing Rust, database, secret, license and container checks,
and both images must be pullable without Tesserix credentials. Version tags
publish service and parser images with SBOM and provenance. Do not bypass a
failed build to create a release. Record immutable image digests in release notes.

Before promoting an installation to production, record evidence for:

- A clean build and upload → acceptance → job → retrievable result journey.
- Authorization isolation, malformed/oversized inputs, cancellation and retries.
- Text-free documents reported as no-text rather than provider parse failure.
- Provider/storage outages, worker restart and database backup restoration.
- Representative, legally redistributable or consented held-out documents,
  with accuracy by language/document type/quality, abstentions and failure rate.
- Load-test throughput, p50/p95 latency, resource use and cost per completed job.
- Retention, deletion, regional residency, alert ownership and rollback.

The 21 included synthetic fixtures are deterministic regression evidence only.
Do not publish customer images or describe synthetic benchmark results as
real-world accuracy. External document datasets must have verified licenses and
provenance before redistribution. No universal accuracy, availability or food
portion measurement guarantee is made.
