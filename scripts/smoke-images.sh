#!/usr/bin/env bash
set -euo pipefail

service_image="${1:-ocr-service:local}"
parser_image="${2:-ocr-parser-sandbox:local}"
container="ocr-smoke-$$"
cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT

for image in "$service_image" "$parser_image"; do
  test "$(docker image inspect --format '{{.Config.User}}' "$image")" = '10001:10001'
done

docker run --rm -i --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges "$parser_image" \
  --content-type image/png --max-pages 1 --max-page-pixels 100000000 \
  --max-total-pixels 100000000 < benchmarks/v1/images/receipt_dark.png \
  | python3 -c 'import json,sys; r=json.load(sys.stdin); assert r["page_count"] == 1; assert r["pages"][0]["width"] == 1100; assert r["pages"][0]["height"] == 850'

python3 ci/native-text-smoke.py "$parser_image"

set +e
printf 'invalid document' | docker run --rm -i --network none --read-only \
  --cap-drop ALL --security-opt no-new-privileges "$parser_image" \
  --content-type image/png --max-pages 1 --max-page-pixels 100000000 \
  --max-total-pixels 100000000
parser_status=$?
set -e
test "$parser_status" -eq 10

docker run -d --name "$container" --read-only --cap-drop ALL \
  --security-opt no-new-privileges --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  -p 127.0.0.1::8080 --env-file ci/ocr-service-smoke.env "$service_image" >/dev/null
port="$(docker port "$container" 8080/tcp | sed 's/.*://')"
for _attempt in $(seq 1 30); do
  if curl --fail --silent "http://127.0.0.1:${port}/healthz" >/dev/null; then
    break
  fi
  sleep 1
done
curl --fail --silent "http://127.0.0.1:${port}/healthz" >/dev/null
test "$(curl --silent --output /dev/null --write-out '%{http_code}' "http://127.0.0.1:${port}/readyz")" = 503
test "$(curl --silent --output /dev/null --write-out '%{http_code}' "http://127.0.0.1:${port}/v1/ocr/jobs/job_missing")" = 401
docker exec "$container" /app/ocr-healthcheck
printf 'Passed: non-root images, valid/invalid parser input, liveness, dependency readiness, unauthenticated denial\n'
