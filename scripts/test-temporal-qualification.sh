#!/usr/bin/env bash
set -euo pipefail

version="1.5.0"
platform="$(uname -s | tr '[:upper:]' '[:lower:]')"
architecture="$(uname -m)"

case "${platform}-${architecture}" in
  darwin-x86_64) archive="temporal_cli_${version}_darwin_amd64.tar.gz"; checksum="5060e803598bb17b14fc8d40cfa1f50bdd6f2f3ad346e3b042f7c761dd7393ee" ;;
  darwin-arm64) archive="temporal_cli_${version}_darwin_arm64.tar.gz"; checksum="7437c43acc82416e8a612aeabe396bc1fb78efca0b648afb0253ad30c511b8e3" ;;
  linux-x86_64) archive="temporal_cli_${version}_linux_amd64.tar.gz"; checksum="0e847562a59ac7cbed38893bfd21944da4f2ff1213339963c79edf3685cc0c55" ;;
  linux-aarch64|linux-arm64) archive="temporal_cli_${version}_linux_arm64.tar.gz"; checksum="6533d3399f3620ebb5356514e2e85865785e7fd16294df516000e2e7c56cd8a6" ;;
  *) echo "unsupported Temporal qualification platform: ${platform}-${architecture}" >&2; exit 2 ;;
esac

qualification_dir="$(mktemp -d)"
trap 'rm -rf "${qualification_dir}"' EXIT
archive_path="${qualification_dir}/${archive}"
curl --fail --silent --show-error --location \
  "https://github.com/temporalio/cli/releases/download/v${version}/${archive}" \
  --output "${archive_path}"

if command -v sha256sum >/dev/null 2>&1; then
  printf '%s  %s\n' "${checksum}" "${archive_path}" | sha256sum --check --status
else
  actual_checksum="$(shasum -a 256 "${archive_path}" | awk '{print $1}')"
  test "${actual_checksum}" = "${checksum}"
fi

tar -xzf "${archive_path}" -C "${qualification_dir}" temporal
TEMPORAL_CLI_PATH="${qualification_dir}/temporal" \
  cargo test -p ocr-temporal --test live_temporal_qualification -- --ignored --nocapture --test-threads=1
