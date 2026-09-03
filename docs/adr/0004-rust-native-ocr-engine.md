# ADR-0004: Rust-native, evidence-first OCR engine

**Status:** proposed

## Context

The platform needs OCR that is fast, private, auditable and safe for AI agents. Existing OCR projects are useful references and benchmark competitors, but adopting one as the runtime would make its pipeline, output model, release cycle and deployment constraints part of our product boundary.

The valuable product is not raw text alone. It is a reproducible document observation graph: original-page geometry, reading order, calibrated confidence, transform provenance, structured layout, validation and review decisions. That is where Tesserix can build differentiated, reusable capability.

## Decision

Build a Tesserix-owned OCR engine and production runtime in Rust. External OCR implementations are benchmark material only; the service will not adopt or copy another project's runtime, pipeline, source or public contract.

The production engine owns:

- bounded document/page decoding and preprocessing;
- image-quality measurement and transform provenance;
- text-region detection, orientation and recognition inference;
- layout, table and reading-order reconstruction;
- confidence calibration and evidence coordinates;
- provider/model-independent observations;
- batch scheduling, streaming and hardware execution profiles;
- normalization, deterministic validation and review routing.

Model training and experimentation may use Python because the training ecosystem is materially stronger. Exported, signed and benchmarked model artefacts cross a strict model contract into Rust production inference. Production serving does not embed Python.

Start with ONNX Runtime through a pinned Rust integration because it provides mature CPU, CUDA and TensorRT execution providers. Keep inference behind an internal engine trait so a model can move to Candle, tract or a custom runtime only after a measured reason exists. The trait abstracts Tesserix model stages, not arbitrary third-party providers.

## Product niche

The engine is optimized for agent-consumable business documents rather than generic scene text:

- scanned and mobile-captured documents;
- forms, receipts, invoices, statements, IDs and contracts;
- evidence for every extracted claim;
- explicit unusable, unknown and partial outcomes;
- multilingual and mixed-script expansion driven by measured tenant demand;
- untrusted-content marking and prompt-injection signals;
- deterministic financial, date and identity validation;
- human correction as versioned provenance, not silent training data.

## Model ownership progression

1. Use legally approved open model weights as replaceable bootstrap artefacts where they meet the model contract.
2. Establish Tesserix golden datasets, calibration and deployment benchmarks before fine-tuning.
3. Fine-tune detector, recognizer and layout models for the chosen document and language cohorts.
4. Train or distill models only where owned data and measurements show a defensible quality, latency, privacy or cost advantage.

“Our OCR” means we own the pipeline, contracts, training/evaluation policy, deployed artefacts and release gates. It does not require inventing a foundation architecture before the first useful result.

## Runtime structure

Use a Cargo workspace with narrow crates: `ocr-domain`, `ocr-image`, `ocr-detect`, `ocr-recognize`, `ocr-layout`, `ocr-runtime`, `ocr-eval`, and the `document-service` binary. Crates communicate through typed observations and immutable page artefacts, not JSON internally.

CPU work runs outside Tokio executor threads in bounded worker pools. Model sessions are loaded once, warmed, pooled per device, model and version, and scheduled by pixel budget rather than document count. Inputs are grouped into bounded shape buckets for batching. Large documents stream page observations and never require the entire result in memory.

Native PDF/image codecs and ONNX Runtime remain unsafe/native dependencies even behind Rust. They execute in a sandboxed worker with no network, read-only filesystem, strict CPU, memory, time and output bounds, and disposable process isolation.

## AI boundary

A vision-language model is not the default OCR engine. It may interpret a bounded low-confidence region after deterministic OCR, with the original region and prompt-injection controls preserved. Semantic extraction and agent reasoning remain downstream stages and cannot modify OCR evidence.

## Temporal qualification

Temporal's Rust SDK is Public Preview at pre-release `v0.8.0` as of 2026-09-02. Development may spike it, but production selection requires replay, versioning, cancellation, heartbeat, child-workflow, 300-page history and crash/upgrade soak evidence. If it fails, use a narrow stable Go Temporal workflow runner calling the Rust service through a versioned internal protocol. Do not build a custom durable state machine for language purity.

## Alternatives

- Adopt an external open-source OCR runtime: rejected because its pipeline, compatibility and release constraints would become part of our product boundary.
- Pure Python service: rejected for the production runtime because predictable concurrency, memory bounds and native service safety are core requirements.
- Train every model from scratch immediately: rejected because it delays evaluation and product learning without proving an advantage.
- Vision/VLM for every page: rejected because cost, latency, reproducibility and prompt-injection surface are unacceptable.

## Consequences

Tesserix owns a coherent OCR product and can optimize it for agents. It also owns model compatibility, preprocessing correctness, calibration, multilingual expansion and hardware qualification. The first release must therefore be deliberately narrow and benchmarked rather than claiming universal OCR quality.
