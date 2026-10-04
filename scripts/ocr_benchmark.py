"""Run frozen synthetic fixtures against an explicitly selected OCR deployment."""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import sys
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener
import uuid

Json = dict[str, Any]
TERMINAL = {"completed", "partial", "review_required", "failed", "cancelled"}


def validate_endpoint(value: str) -> str:
    url = urlsplit(value)
    if (
        not url.hostname
        or url.username
        or url.password
        or url.query
        or url.fragment
        or url.path not in {"", "/"}
        or not (
            url.scheme == "https"
            or (
                url.scheme == "http"
                and url.hostname in {"127.0.0.1", "::1", "localhost"}
            )
        )
    ):
        raise ValueError("API endpoint must be credential-free HTTPS or a local tunnel")
    return value.rstrip("/")


def load_cases(manifest: Path) -> list[Json]:
    if manifest.stat().st_size > 16 * 1024 * 1024:
        raise ValueError("manifest exceeds limit")
    data = json.loads(manifest.read_text())
    cases = data.get("cases", [])
    if not isinstance(cases, list) or not 1 <= len(cases) <= 1000:
        raise ValueError("invalid benchmark cases")
    seen: set[str] = set()
    root = manifest.parent.resolve()
    for case in cases:
        if (
            not isinstance(case, dict)
            or not re.fullmatch(r"[a-z0-9_]{1,80}", case.get("id", ""))
            or case["id"] in seen
        ):
            raise ValueError("invalid or duplicate case ID")
        seen.add(case["id"])
        image = (root / case["image"]).resolve()
        if (
            not image.is_relative_to(root)
            or not image.is_file()
            or image.stat().st_size > 20 * 1024 * 1024
        ):
            raise ValueError("fixture path or size is invalid")
        if hashlib.sha256(image.read_bytes()).hexdigest() != case["sha256"]:
            raise ValueError("fixture hash mismatch")
    return cases


def sign_headers(
    key_id: str, secret: bytes, tenant: str, method: str, path: str, timestamp: int
) -> dict[str, str]:
    ts = str(timestamp)
    message = "\n".join((key_id, tenant, ts, method, path)).encode()
    return {
        "X-OCR-Key-Id": key_id,
        "X-OCR-Tenant-Id": tenant,
        "X-OCR-Timestamp": ts,
        "X-OCR-Signature": hmac.new(secret, message, hashlib.sha256).hexdigest(),
    }


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(
        self, req: Any, fp: Any, code: int, msg: str, headers: Any, newurl: str
    ) -> None:
        return None


class Client:
    def __init__(self, identity: str, tenant: str):
        key_id, value = identity.split("=", 1)
        _product, secret = value.split(":", 1)
        self.key_id = key_id
        self.secret = bytes.fromhex(secret)
        if len(self.secret) < 32 or not re.fullmatch(r"ten_[A-Za-z0-9_]{1,64}", tenant):
            raise ValueError("invalid benchmark identity or tenant")
        self.tenant = tenant
        self.opener = build_opener(NoRedirect())

    def request(
        self, url: str, method: str, body: bytes | None, headers: dict[str, str]
    ) -> bytes:
        try:
            with self.opener.open(
                Request(url, data=body, headers=headers, method=method), timeout=30
            ) as response:
                payload: bytes = response.read(8 * 1024 * 1024 + 1)
                if len(payload) > 8 * 1024 * 1024:
                    raise ValueError("response exceeds limit")
                return payload
        except HTTPError as error:
            raise RuntimeError(f"HTTP {error.code}") from None
        except (URLError, TimeoutError):
            raise RuntimeError("transport unavailable") from None

    def call(
        self,
        base: str,
        method: str,
        path: str,
        body: Json | None = None,
        key: str | None = None,
    ) -> Json:
        headers = sign_headers(
            self.key_id, self.secret, self.tenant, method, path, int(time.time())
        )
        headers["Content-Type"] = "application/json"
        if key:
            headers["Idempotency-Key"] = key
        payload = self.request(
            base + path,
            method,
            json.dumps(body).encode() if body is not None else None,
            headers,
        )
        result: Json = json.loads(payload) if payload else {}
        return result

    def poll(self, base: str, path: str, states: set[str]) -> Json:
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            result = self.call(base, "GET", path)
            if result.get("status") in states:
                return result
            time.sleep(0.5)
        raise RuntimeError("polling deadline exceeded")


def run_case(
    client: Client, case: Json, root: Path, upload: str, jobs: str, run_id: str
) -> Json:
    start = time.monotonic()
    record: Json = {
        "id": case["id"],
        "sha256": case["sha256"],
        "status": "failed",
        "elapsed_ms": 0,
        "text": "",
        "fields": {},
    }
    job_id: str | None = None
    terminal = False
    try:
        image = root / case["image"]
        body = image.read_bytes()
        mime = "image/jpeg" if image.suffix == ".jpg" else "image/png"
        key = "benchmark-" + run_id + "-" + case["id"]
        reserved = client.call(
            upload,
            "POST",
            "/v1/ocr/uploads",
            {
                "content_type": mime,
                "content_length": len(body),
                "sha256": "sha256:" + case["sha256"],
            },
            key + "-u",
        )
        target = urlsplit(reserved["upload_url"])
        if (
            target.scheme != "https"
            or target.username
            or target.password
            or not (
                target.hostname == "storage.googleapis.com"
                or (target.hostname or "").endswith(".storage.googleapis.com")
            )
        ):
            raise ValueError("untrusted upload target")
        client.request(
            reserved["upload_url"], "PUT", body, reserved.get("required_headers", {})
        )
        path = "/v1/ocr/uploads/" + reserved["upload_id"]
        client.call(upload, "POST", path + "/complete", {})
        record["upload_ms"] = round((time.monotonic() - start) * 1000)
        inspection_start = time.monotonic()
        inspected = client.poll(upload, path, {"accepted", "rejected", "expired"})
        record["inspection_ms"] = round((time.monotonic() - inspection_start) * 1000)
        if inspected["status"] != "accepted":
            raise RuntimeError("upload not accepted")
        request: Json = {
            "source": {"upload_id": reserved["upload_id"]},
            "document_type": "general",
            "output": {
                "text": True,
                "markdown": False,
                "layout": True,
                "evidence": True,
            },
            "language_hints": case.get("language_hints", ["en"]),
            "processing_class": "interactive",
        }
        if case.get("extraction"):
            request["extraction"] = case["extraction"]
        job_start = time.monotonic()
        job = client.call(jobs, "POST", "/v1/ocr/jobs", request, key + "-j")
        job_id = job["job_id"]
        record["job_id"] = job_id
        path = "/v1/ocr/jobs/" + str(job_id)
        status = client.poll(jobs, path, TERMINAL)
        terminal = True
        record["status"] = status["status"]
        record["job_wait_ms"] = round((time.monotonic() - job_start) * 1000)
        if status["status"] in {"completed", "partial", "review_required"}:
            result_start = time.monotonic()
            result = client.call(jobs, "GET", path + "/result")
            record.update(
                {
                    key: result[key]
                    for key in ("text", "fields", "warnings", "validation_failures")
                }
            )
            record["result_read_ms"] = round((time.monotonic() - result_start) * 1000)
    except (RuntimeError, ValueError, KeyError, OSError):
        record["status"] = "failed"
        record["error"] = "benchmark_request_failed"
    finally:
        if job_id and not terminal:
            try:
                client.call(jobs, "POST", "/v1/ocr/jobs/" + job_id + "/cancel", {})
            except (RuntimeError, ValueError, OSError):
                record["cleanup_required"] = True
        record["elapsed_ms"] = round((time.monotonic() - start) * 1000)
    return record


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--upload-url", type=validate_endpoint, required=True)
    parser.add_argument("--job-url", type=validate_endpoint, required=True)
    parser.add_argument("--tenant", default="ten_ocr_benchmark")
    args = parser.parse_args()
    cases = load_cases(args.manifest)
    client = Client(os.environ["OCR_BENCHMARK_IDENTITY"], args.tenant)
    with args.output.open("x") as output:
        output.write("[]\n")
    run_id = uuid.uuid4().hex[:16]
    records = []
    for case in cases:
        record = run_case(
            client, case, args.manifest.parent, args.upload_url, args.job_url, run_id
        )
        records.append(record)
        args.output.write_text(json.dumps(records, indent=2) + "\n")
        print(record["id"], record["status"], record["elapsed_ms"], flush=True)
    return int(any(r["status"] in {"failed", "cancelled"} for r in records))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, ValueError, KeyError, OSError):
        print(
            "Benchmark could not run; check arguments, fixture hashes, and identity configuration",
            file=sys.stderr,
        )
        sys.exit(2)
