# ADR-0011: Signed OCR model profile contract

**Status:** proposed

## Context

The first engine cohort targets printed English scans and mobile images. The service design point is 20 submitted jobs per second and 100 pages per second, with a 300-page or 100 MiB input ceiling. A model profile crosses a native-code trust boundary: compromised staging storage, an untrusted manifest, an oversized artifact, or an incompatible tensor declaration must not result in model loading or tenant-controlled execution.

The profile must be independently promotable and rollbackable. It must also prove what model, calibration, dataset, license, execution profile, and tensor contract produced an observation. Neither a tenant nor an API request may choose a key, URL, artifact path, device, runtime option, or profile.

## Decision

The engine accepts a bounded detached Ed25519 signature over the exact model-manifest bytes. A process-local, deployment-configured keyring resolves the signing key ID; unknown or duplicate keys fail closed. The verifier checks the signature before JSON parsing, rejects manifests over 64 KiB, and only accepts schema version 1 with bounded identifiers, canonical SHA-256 artifact identity, license/dataset/calibration provenance, model version, engine-runtime contract revision, execution profile, supported locales, rollback predecessor, and bounded named tensor contracts.

The verifier exposes a verified profile only after those checks. It separately hashes a caller-supplied bounded artifact and requires the signed SHA-256 value to match. It does not download an artifact, accept a URI or model path, load an ONNX session, choose a device, or permit a tenant-specific model choice.

For an artifact mismatch, malformed contract, untrusted key, invalid signature, or byte-limit breach, the engine rejects the candidate before inference. No manifest bytes, signature, artifact bytes, tensor values, or OCR content are logged.

## Failure behaviour

- Staging/object store unavailable: the worker leaves the job retryable; no profile is invented from cache.
- Manifest, signer, signature, or tensor contract invalid: the candidate is non-retryably rejected and is not executable.
- Artifact digest mismatch or artifact over limit: the candidate is non-retryably rejected before loading native code.
- Key rotation: deployment supplies both the active and predecessor public keys during overlap; revoking a key makes its candidates unselectable. The profile's rollback predecessor is metadata, not an automatic rollback action.

## Consequences

This adds no selected model, model artifact, ONNX dependency, provider, runtime worker, dataset access, or deployment. Model licensing, golden-dataset benchmark thresholds, key distribution, execution-provider qualification, and Temporal production approval remain gates in issue #9. The simplest alternative—loading a locally named model with a checksum—was rejected because it cannot prove signer authorization or make the complete tensor/profile contract reviewable.
