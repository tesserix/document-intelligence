# OCR benchmark v1

21 frozen synthetic images: the original 11 nutrition quality variants, four
receipt variants, four invoice variants, and French/Spanish clear documents.
No customer documents or credentials are included. Ground truth lives in the
manifest; image hashes prevent accidental substitution. Images were generated
with Pillow and the system Arial font; only rendered pixels are distributed.
The frozen bytes, rather than a machine-specific font renderer, define this
version. It is a regression corpus, not a representative real-world dataset.

## Quick test: three images

Use these Apache-2.0 synthetic images in your own API client, or run the
three-case manifest below. No customer photos are included.

| Image | Expected result |
| --- | --- |
| [Clear nutrition label](v1/images/clear.png) | Completed; 20 expected nutrition fields in the manifest. |
| [Clear receipt](v1/images/receipt_clear.png) | Completed; receipt text, including total 14.85. |
| [Text-free image](v1/images/no_text.png) | Review required; empty text and `ocr_no_text_detected`, not a parser failure. |

Open an image and choose **Download raw file** on GitHub, or use the files in
your clone. The [full image directory](v1/images) also contains difficult images
for testing review handling. All image bytes are hash-checked before uploading.
The original 21-case manifest is unchanged.

After deploying the service using the [self-hosting guide](../docs/SELF-HOSTING.md),
configure `OCR_BENCHMARK_IDENTITY` as described below. From the repository root,
with Python 3.11+ and your authorized API connections available, run:

```sh
python3 scripts/ocr_benchmark.py \
  --manifest benchmarks/v1/quick-manifest.json \
  --upload-url http://127.0.0.1:18783 \
  --job-url http://127.0.0.1:18784 \
  --tenant ten_ocr_quicktest \
  --output /tmp/ocr-quicktest-results.json
cargo run -p ocr-eval -- benchmarks/v1/quick-manifest.json /tmp/ocr-quicktest-results.json
```

Replace the URLs with your deployment's HTTPS origins or authorized local
tunnels. This creates three real uploads/jobs and incurs your provider/storage
charges; it does not connect to a public Tesserix demo. Use a new output filename
on subsequent runs. Python uses only the standard library; Rust is needed only
for the optional evaluator. Inspect each result's status, text and warnings:
the runner's successful exit means results were retrieved, not that every OCR
value matched. Provider outputs can vary; compare with the manifest references.
The text-free case should contain no invented text.

## Production or another explicitly selected deployment

Establish your authorized upload/job API connections. The runner requires
`OCR_BENCHMARK_IDENTITY` in its process environment, in the existing
`key-id=product:hex-secret` format. Retrieve that through your authorized secret
manager without printing it, saving it to a file, or passing it in command-line
arguments. The runner never writes credentials or signed upload URLs.

```sh
python3 scripts/ocr_benchmark.py \
  --manifest benchmarks/v1/manifest.json \
  --upload-url http://127.0.0.1:18783 \
  --job-url http://127.0.0.1:18784 \
  --tenant ten_ocr_benchmark \
  --output /tmp/ocr-benchmark-results.json
cargo run -p ocr-eval -- benchmarks/v1/manifest.json /tmp/ocr-benchmark-results.json
```

The run uploads 21 documents and creates 21 jobs. Every invocation uses fresh
idempotency keys. Inputs remain under normal upload retention; jobs that exceed
the polling deadline are cancelled when possible, with `cleanup_required` in the
record if cancellation fails. Failed cases remain in the output and cause a
nonzero exit. Existing output files are never overwritten. A rerun uses a new
output path. Signed identity headers are never forwarded across redirects.

Review per-case scores as well as the totals. Nutrient tables use exact known
fields rather than text-order-sensitive CER. Receipts/invoices and the language
fixtures use bounded NFC CER/WER. Review/partial is expected for unreadable or
cropped images. Low resolution may improve without becoming fully reliable.
Latency is client-observed, includes polling/network overhead, and is not a
load-tested SLO. Successful known references do not calibrate model confidence.

In worker logs, filter `message="ocr stage finished"` and the run's recorded job
IDs. Aggregate `elapsed_ms` by `stage` and `outcome`; do not compare failed or
cancelled attempts as successful latency. Count `provider` events to verify the
one-call path. Product backends must still be tested through their own network
and identity integration.

Preparation profile and limits: [ADR 0013](../docs/adr/0013-bounded-ocr-quality-benchmark.md).
