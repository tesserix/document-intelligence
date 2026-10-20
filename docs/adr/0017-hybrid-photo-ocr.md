# ADR-0017: Hybrid OCR for photos and image-only pages

Status: Accepted. Supersedes ADR-0016's image-page policy and its native-only execution binary; every other ADR-0016 decision stands.

## Context

After ADR-0016 shipped, every photo uploaded by Kora and Roamie returned zero observations with `ocr_no_text_detected`. Products showed "no text in photo" for readable signs, receipts and statements. Native PDF extraction cannot read text pixels, and Document Intelligence is the OCR every product depends on, so a manual-review-only answer for photos is not acceptable.

The deployment never lost its Document AI path: the processor stayed enabled, each product worker kept `roles/documentai.apiUser`, the chart still passes `DOCUMENT_AI_*`, and egress already allowed Google APIs. Six real street-sign photos (Japanese, Korean, Vietnamese, Thai, Hindi/Odia, Chinese) read correctly on `pretrained-ocr-v2.1.1-2025-01-31` in 4–16 seconds each. The processor's default, `pretrained-ocr-v1.0-2020-09-23`, garbled the Thai and Odia text.

## Decision

The execution worker recognizes each page natively first. A page with native text is used as-is and never leaves the cluster. A page with no native text — any photo, or an image-only PDF page — is sent to Document AI.

- Requests name `pretrained-ocr-v2.1.1-2025-01-31` explicitly (`DOCUMENT_AI_OCR_VERSION`), so a provider default change cannot alter results silently. Moving to another version is a reviewed code change.
- Document AI observations use `obs_docai_p{page}_line{n}` IDs. Results containing them record provider `native-text-document-ai`, the pinned model version and profile `hybrid-ocr-v1`; only other non-native IDs are treated as legacy and sent to review.
- Document AI quota or outage errors retry the page through the existing durable checkpoints. A page Document AI rejects (unsupported type, over 20 MiB, invalid response) keeps its empty native page and is flagged for manual review rather than failing the document.
- Document AI is enabled only when `DOCUMENT_AI_PROJECT_ID`, `DOCUMENT_AI_LOCATION` and `DOCUMENT_AI_PROCESSOR_ID` are all set; a partial setting stops startup. The public sandbox (ADR-0015) leaves them unset and stays native-only, so anonymous uploads never reach a paid provider.

## Consequences

Photos and scanned pages are machine-read again with line evidence and confidence; text PDFs stay deterministic, free and in-cluster. Document AI charges apply per image page only. Page content for those pages is sent to Google Document AI in the configured region, as it was before ADR-0016. Each Document AI page adds roughly 4–16 seconds inside the existing 30-second provider and 120-second activity timeouts.

Rejected: a self-hosted OCR model (no multilingual model matched Document AI on the sample signs and it would add GPU/CPU capacity to qualify); per-product vision-model transcription (duplicates OCR in every consumer and loses the shared evidence, review and retention contract).
