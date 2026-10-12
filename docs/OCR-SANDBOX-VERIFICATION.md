# Sandbox verification — 2026-10-12

Status: deployed to isolated GKE infrastructure at `ocr-test.tesserix.app`.
Application PR #106 and infrastructure PRs #1356–#1358 are merged. Public API
access remains closed while final browser qualification completes.

## Live qualification

Synthetic fixtures were uploaded through the deployed adapter and processed by
the dedicated OCR workers on 2026-10-12. No generative model was invoked.

| Check | Observed result |
| --- | --- |
| Nutrition label | Completed; 331 text characters, structured nutrition fields; 16.59 seconds end to end |
| Receipt | Completed; 124 text characters; 13.45 seconds end to end |
| Blank image | Review required; zero text characters; retake warning; 14.38 seconds |
| Source cleanup | All three results reported verified deletion; independent all-generation GCS listing confirmed their source images absent |
| Disconnected client | Receipt completed during a 45-second pause without client polling; source deletion verified on return |
| Cross-session access | Result lookup returned HTTP 404 |
| Session request budget | Bounded burst returned 59 HTTP 200 and 66 HTTP 429 after one authorized lookup |
| Browser rejected-job regression | Failed with two polls before fix; passed with one poll, no result retrieval, and controls restored |

The initial API example omitted required `output.markdown` and returned HTTP 400;
the example is corrected. Its abandoned upload remains subject to session-expiry
cleanup, independently of successful job cleanup. Timings are individual samples,
not throughput or latency guarantees.

## Earlier local qualification

| Check actually run | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed; affected crate rechecked after final test additions |
| `cargo build --workspace --locked` | Passed |
| `cargo test --workspace --locked` | Passed: 237 tests; 53 environment-dependent tests ignored by default |
| `cargo test -p ocr-sandbox -- --include-ignored` with local PostgreSQL | Passed: 9 tests, none ignored |
| `node --check crates/ocr-sandbox/web/app.mjs` | Passed |
| `node --test crates/ocr-sandbox/web/score.test.mjs` | Passed: 3 tests |
| `docker build -f Dockerfile.sandbox -t ocr-sandbox:local .` | Passed on local ARM64 host; hosted AMD64 CI not run yet |
| Local container HTTP smoke | Passed: read-only filesystem, no capabilities, health, UI, security headers, admission disabled |
| `actionlint .github/workflows/ci.yml .github/workflows/release-image.yml` | Passed |
| Terraform `init -backend=false`, `fmt -check`, `validate` | Passed; no plan or apply |
| `cargo deny check` | Passed: advisories, bans, licenses, sources |
| `gitleaks git --redact` and `gitleaks dir . --redact` | Passed: no detected secrets |
| `trivy fs --scanners vuln,secret,misconfig --exit-code 1 .` | Nonzero: low missing container HEALTHCHECK findings; no reported dependency/secret findings |
| `trivy config --exit-code 1 deploy/sandbox` | Nonzero: 4 medium versioning-disabled and 4 low Google-managed encryption findings, documented in deployment reference |
| `trivy image --scanners vuln --severity HIGH,CRITICAL --exit-code 1 ocr-sandbox:local` | Passed: no high/critical OS findings; Rust dependencies checked separately with cargo-deny |
| `git diff --check` | Passed |

Regression tests were observed failing before fixes for upload bounds, quota
races, cleanup failure, rejected-upload cleanup, output-option validation and
security headers on rejected requests. Lifecycle tests use real PostgreSQL with
local synthetic HTTP OCR/storage fixtures. They cover signing, cross-session
404s, duplicate requests, budgets, expiry, cleanup restart recovery and preserving
another tenant's object. They do not prove real provider OCR accuracy or cloud
IAM correctness.

Browser review covered desktop, sample selection, admission-disabled error, and
a 390px mobile viewport (390px page width, no horizontal overflow). A successful
real OCR browser journey remains pending cloud qualification. The page and
scoring tests do not manufacture results or arbitrary-upload accuracy scores.

Production rollout must provision isolated credentials, buckets, database and
workers through the owning GitOps repositories; configure edge limits and
platform probes; then run real fixture, cleanup, restart and quota tests before
enabling public admission. See [operator guide](OCR-SANDBOX.md) and
[storage reference](../deploy/sandbox/README.md).
