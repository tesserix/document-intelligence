#!/usr/bin/env bash
set -euo pipefail

container="ocr-valkey-contract-$$"
image="valkey/valkey@sha256:b21fd94099dcd4bc6b2b9230daef69b6558b887ad4a2a1afe56ff6e745a88cdb"

cleanup() {
  docker rm --force "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker run --detach --name "$container" --publish 127.0.0.1::6379 "$image" >/dev/null
port="$(docker port "$container" 6379/tcp | sed -n 's/.*://p')"
if [[ -z "$port" ]]; then
  echo "Valkey test port was not assigned" >&2
  exit 1
fi

for _ in {1..50}; do
  if docker exec "$container" valkey-cli ping >/dev/null 2>&1; then
    TEST_VALKEY_URL="redis://127.0.0.1:${port}" \
      cargo run --locked -p ocr-service --example valkey_cache_contract
    exit 0
  fi
  sleep 0.1
done

echo "Valkey test container did not become ready" >&2
exit 1
