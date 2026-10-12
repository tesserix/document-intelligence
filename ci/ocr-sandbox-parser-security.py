import argparse
import json
import struct
import subprocess
import zlib


def chunk(kind: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", zlib.crc32(kind + data))
    )


def png(metadata: bytes = b"") -> bytes:
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
        + metadata
        + chunk(b"IDAT", zlib.compress(b"\0\xff\0\0"))
        + chunk(b"IEND", b"")
    )


def compressed_bomb() -> bytes:
    compressor = zlib.compressobj()
    block = bytes(1024 * 1024)
    profile = b"".join(compressor.compress(block) for _ in range(500))
    profile += compressor.flush()
    return profile


def check(image: str, name: str, data: bytes, expected_exit: int) -> None:
    result = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "-i",
            "--network",
            "none",
            "--read-only",
            "--memory",
            "256m",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "10001",
            "--entrypoint",
            "/app/ocr-parser-sandbox",
            image,
            "--content-type",
            "image/png",
            "--max-pages",
            "20",
            "--max-page-pixels",
            "12000000",
            "--max-total-pixels",
            "240000000",
        ],
        input=data,
        capture_output=True,
        timeout=30,
        check=False,
    )
    print(
        json.dumps(
            {
                "test": name,
                "encoded_bytes": len(data),
                "memory_cap": "256 MiB",
                "exit_code": result.returncode,
                "expected_exit": expected_exit,
                "passed": result.returncode == expected_exit,
            }
        ),
        flush=True,
    )
    if result.returncode != expected_exit:
        raise AssertionError(
            f"{name}: expected {expected_exit}, got {result.returncode}"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default="ocr-sandbox:ci")
    args = parser.parse_args()
    check(args.image, "valid PNG", png(), 0)
    check(
        args.image,
        "PNG with valid compressed metadata",
        png(chunk(b"iCCP", b"SyntheticProfile\0\0" + zlib.compress(bytes(128)))),
        0,
    )
    bomb = compressed_bomb()
    for kind, prefix in (
        (b"iCCP", b"SyntheticProfile\0\0"),
        (b"zTXt", b"Comment\0\0"),
        (b"iTXt", b"Comment\0\1\0\0\0"),
    ):
        check(
            args.image,
            f"PNG {kind.decode()} decompression bomb",
            png(chunk(kind, prefix + bomb)),
            10,
        )
    text = chunk(b"tEXt", b"Comment\0" + bytes(600 * 1024))
    check(args.image, "PNG cumulative metadata budget", png(text + text), 10)
    check(args.image, "PNG truncated chunk", png()[:-5], 10)


if __name__ == "__main__":
    main()
