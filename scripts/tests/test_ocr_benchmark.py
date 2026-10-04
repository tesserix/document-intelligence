import hashlib
import json

import pytest

from ocr_benchmark import validate_endpoint, load_cases, sign_headers


def test_endpoints_allow_local_tunnels_or_tls_but_never_credentials():
    assert validate_endpoint("http://127.0.0.1:18783") == "http://127.0.0.1:18783"
    assert validate_endpoint("https://ocr.example.test") == "https://ocr.example.test"
    for value in [
        "http://remote.test",
        "https://user:secret@host",
        "https://host?secret=1",
        "file:///tmp/x",
    ]:
        with pytest.raises(ValueError):
            validate_endpoint(value)


def test_manifest_validates_hashes_and_refuses_path_escape(tmp_path):
    data = b"fixture"
    (tmp_path / "image.png").write_bytes(data)
    case = {
        "id": "case",
        "image": "image.png",
        "sha256": hashlib.sha256(data).hexdigest(),
        "expected_fields": {},
        "reference_text": "known",
    }
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"cases": [case]}))
    assert len(load_cases(manifest)) == 1
    (tmp_path / "image.png").write_bytes(b"changed")
    with pytest.raises(ValueError):
        load_cases(manifest)
    case["image"] = "../escape.png"
    manifest.write_text(json.dumps({"cases": [case]}))
    with pytest.raises(ValueError):
        load_cases(manifest)


def test_signature_is_bound_to_method_path_and_timestamp():
    headers = sign_headers(
        "key", b"test-secret", "ten_BENCH", "POST", "/v1/ocr/jobs", 123
    )
    assert headers["X-OCR-Timestamp"] == "123"
    assert len(headers["X-OCR-Signature"]) == 64
    assert headers != sign_headers(
        "key", b"test-secret", "ten_BENCH", "GET", "/v1/ocr/jobs", 123
    )
