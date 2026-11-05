#!/usr/bin/env bash
set -euo pipefail
# Run after building ntfy-rs:verify. Requires free local ports 18080/18081.
# Each sample starts a fresh container and a fresh anonymous database volume.
mkdir -p bench-results
requests=${REQUESTS:-30000}
cleanup() { docker rm -fv ntfy-rs-bench ntfy-go-bench >/dev/null 2>&1 || true; }
trap cleanup EXIT
for round in 1 2 3; do
  for engine in rust go; do
    cleanup
    if [ "$engine" = rust ]; then
      name=ntfy-rs-bench
      port=18080
      docker run -d --name "$name" --cpus=2 --memory=512m --ulimit nofile=65536:65536 \
        -p 127.0.0.1:18080:8080 -e NTFY_BASE_URL=https://ntfy.aosn.de \
        -e NTFY_VISITOR_REQUEST_LIMIT_BURST=1000000 -e NTFY_VISITOR_SUBSCRIPTION_LIMIT=10000 \
        ntfy-rs:verify >/dev/null
    else
      name=ntfy-go-bench
      port=18081
      docker run -d --name "$name" --cpus=2 --memory=512m --ulimit nofile=65536:65536 \
        -p 127.0.0.1:18081:80 -e GOMAXPROCS=2 -e NTFY_BASE_URL=https://ntfy.aosn.de \
        -v /var/lib/ntfy -e NTFY_CACHE_FILE=/var/lib/ntfy/cache.db \
        -e 'NTFY_CACHE_STARTUP_QUERIES=PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;' \
        -e NTFY_VISITOR_REQUEST_LIMIT_BURST=1000000 -e NTFY_VISITOR_SUBSCRIPTION_LIMIT=10000 \
        -e NTFY_VISITOR_MESSAGE_DAILY_LIMIT=1000000 \
        binwiederhier/ntfy:2.37.2 serve >/dev/null
    fi
    for attempt in $(seq 1 30); do
      if curl -fsS "http://127.0.0.1:$port/v1/health" >/dev/null 2>&1; then break; fi
      sleep 1
    done
    python3 scripts/benchmark.py --url "http://127.0.0.1:$port" --container "$name" \
      --matrix --connections 1000 --topics 100 --requests "$requests" --concurrency 32 \
      | tee "bench-results/$engine-matrix-$round.json"
  done
done
