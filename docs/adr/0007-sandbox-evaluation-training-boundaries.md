# ADR-0007: Separate sandbox, training and held-out evaluation boundaries

**Status:** proposed

## Context

The reusable Document Intelligence service needs repeatable OCR and AI-agent
experimentation. Sharing one bucket or identity between sandbox, training and
evaluation would expose sensitive data, allow benchmark leakage and make a
reported improvement impossible to trust. Ordinary CI and contributor pull
requests are an especially weak trust boundary.

## Decision

Use separate logical GCS storage classes and Workload Identities for ephemeral
sandbox data, quarantined dataset candidates, frozen train/calibration versions,
development evaluation versions, evaluation outputs and staged model bundles.
Keep the held-out golden test corpus in its existing protected bucket and deny it
to developers, sandbox runners, training jobs and ordinary CI.

Global CNPG owns manifests, lineage, grants, budgets, experiments, promotion and
audit state. GCS owns bytes. DevAI orchestrates opaque evaluation requests and
receives redacted metrics/trace references. Langfuse is a non-authoritative safe
projection with separate non-production/protected credentials; no consuming
product's production credentials are reused.

Promote only a signed compatibility manifest binding OCR code/models,
preprocessing, calibration, extraction schemas, routing, agent, Australis tool
contract, result schema, runtime and hardware requirements. Dataset versions are
immutable. Training, calibration, development evaluation and held-out test have
separate purposes and access paths.

## Alternatives

- One non-production bucket: rejected because one IAM/lifecycle boundary permits
  accidental training on evaluation data and broadens sandbox access.
- Copy the production golden corpus into DevAI: rejected because it duplicates
  sensitive data and gives CI a direct corpus path.
- Store datasets in Langfuse: rejected because Langfuse is experiment telemetry,
  not the canonical sensitive-data or governance store.
- Let training jobs evaluate the held-out test set: rejected because repeated
  visibility turns the test set into training feedback.
- Online learning from production corrections: rejected because consent,
  poisoning, label quality and rollback cannot be guaranteed.

## Consequences

Experiments are reproducible and test leakage is mechanically constrained, at
the cost of more buckets, identities, manifests and lifecycle policies. Protected
evaluation can be unavailable without affecting production OCR. Physical names,
projects, regions, identities, quotas and retention remain review gates owned by
GitOps rather than defaults embedded in application code.
