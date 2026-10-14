"""Exercise the packaged native parser without network or writable storage."""

import json
import subprocess
import sys


def source_pdf() -> bytes:
    text = b"BT /F1 16 Tf 20 180 Td (Native offline test) Tj ET"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 240] "
        b"/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Length "
        + str(len(text)).encode()
        + b" >>\nstream\n"
        + text
        + b"\nendstream",
    ]
    output = bytearray(b"%PDF-1.4\n")
    offsets = [0]
    for number, value in enumerate(objects, 1):
        offsets.append(len(output))
        output.extend(f"{number} 0 obj\n".encode() + value + b"\nendobj\n")
    xref = len(output)
    output.extend(f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode())
    for offset in offsets[1:]:
        output.extend(f"{offset:010} 00000 n \n".encode())
    output.extend(
        f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    )
    return bytes(output)


def extract(image: str) -> bytes:
    return subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "-i",
            "--network=none",
            "--read-only",
            "--memory=512m",
            "--pids-limit=64",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            image,
            "extract-native",
            "--content-type",
            "application/pdf",
            "--max-pages",
            "1000",
            "--max-page-pixels",
            "100000000",
            "--max-total-pixels",
            "100000000000",
        ],
        input=source_pdf(),
        capture_output=True,
        check=True,
        timeout=60,
    ).stdout


if __name__ == "__main__":
    first = extract(sys.argv[1])
    assert first == extract(sys.argv[1]), "native output changed on identical source"
    pages = json.loads(first)
    assert len(pages) == 1 and pages[0]["page"] == 1
    line = pages[0]["observations"][0]
    assert line["text"] == "Native offline test"
    assert line["observation_id"] == "obs_native_p1_line0"
    points = line["polygon"]["points"]
    assert len(points) == 4
    assert all(0 <= p[axis] <= 1 for p in points for axis in ("x", "y"))
    assert points[0]["x"] < points[1]["x"] and points[0]["y"] < points[2]["y"]
    print("Passed: offline, read-only native PDF text, citations and repeatable output")
