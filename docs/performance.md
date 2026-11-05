# Performance validation

Performance is workload-dependent. The server uses shared message buffers, bounded queues, asynchronous subscriptions, a small SQLite page cache and batched durable transactions. A language rewrite alone is not evidence of a speedup.

## Version 0.2.0 with authentication database — 2026-11-05

Three alternating runs of the final Rust image (`e32cee6`) and upstream Go 2.37.2 on native **linux/arm64** Docker Desktop containers, each limited to 2 CPUs and 512 MiB. Both have `auth-file` and `enable-login` enabled. The workload uses anonymous read-write access, exercising database ACL checks and live subscription permission checks, not password hashing or many distinct authenticated identities. SMTP, Web Push and iOS forwarding are disabled during this benchmark.

| Metric (median of three runs) | Rust 0.2.0 | Upstream Go 2.37.2 |
| --- | ---: | ---: |
| Idle process RSS | 9.99 MiB | 45.59 MiB |
| RSS with 1,000 subscriptions | 36.85 MiB | 81.42 MiB |
| RSS after publishing | 39.54 MiB | 125.61 MiB |
| Matrix requests / second | 4,928.0 | 2,322.7 |
| Publishing p95 latency | 12.47 ms | 27.72 ms |
| HTTP errors, each run | 0 | 0 |
| Received / expected deliveries, each run | 300,000 / 300,000 | 300,000 / 300,000 |

For this workload, median throughput was **2.12×** and after-load process RSS was **68.5% lower**. Rust throughput ranged from 4,044.7–5,272.0 requests/s; Go ranged from 1,575.8–2,336.4. Both were slower in the third round; all samples are retained. These short samples are not a sustained capacity claim, an amd64 benchmark or evidence of complete feature parity.

The six raw samples, exact image digest, command and environment are in [benchmark-results-v0.2.json](benchmark-results-v0.2.json). Rust shares identical in-flight authentication reads and reuses prepared statements; completed permission results are not retained in a timed cache. This avoids repeating the same SQL query for every subscriber receiving the same event while preserving subsequent revocation checks.

## Historical version 0.1 results — 2026-11-05

Three alternating runs on Docker Desktop for macOS using native **linux/arm64** containers. Docker had 10 CPUs and approximately 11.7 GiB available; each server was limited to 2 CPUs and 512 MiB. The values below are medians of the three samples for the workload described below.

| Metric | Rust server | Upstream Go 2.37.2 |
| --- | ---: | ---: |
| Idle process RSS | 5.52 MiB | 44.58 MiB |
| RSS with 1,000 subscriptions | 31.90 MiB | 78.19 MiB |
| RSS after publishing | 34.46 MiB | 128.95 MiB |
| Matrix requests / second | 5,257.4 | 2,467.2 |
| Publishing p95 latency | 12.24 ms | 21.33 ms |
| HTTP errors, each run | 0 | 0 |
| Received / expected deliveries, each run | 300,000 / 300,000 | 300,000 / 300,000 |

For this workload, Rust's median throughput was **2.13×**, and its after-load process RSS was **73.3% lower**. Throughput varied between 3,649.0–5,805.5 requests/s for Rust and 2,415.8–2,639.3 for Go. These short samples do not establish sustained production capacity or performance on amd64; the Docker runtime, Python client, CPU throttling and filesystem can influence the results. Both architecture images passed protocol smoke tests, but performance was measured only on arm64.

The full six samples and environment metadata are in [benchmark-results.json](benchmark-results.json). Both implementations use fresh anonymous Docker volumes for SQLite, avoiding a volume-versus-overlay-filesystem comparison. The Rust server code measured here is commit `4543695`.

## Reproducing the comparison

Build a local image and run the supplied benchmark:

```sh
docker buildx build --platform linux/arm64 -t ntfy-rs:verify --load .
docker pull binwiederhier/ntfy:v2.37.2
AUTH_DATABASE=1 RESULTS_DIR=bench-results/v0.2-auth bash scripts/bench-docker.sh
```

On amd64, change the build platform to `linux/amd64`. The script starts fresh containers for each sample, runs three rounds per implementation, and stores the raw JSON in `bench-results/`. It removes only its named benchmark containers when finished. Run on an otherwise idle machine; ports 18080 and 18081 must be free.

Omit `AUTH_DATABASE=1` to measure without an authentication database. `ROUNDS` controls the number of alternating rounds and `RESULTS_DIR` selects a separate output directory. If a Docker registry mirror is unavailable, pull the same official tag from `registry-1.docker.io/binwiederhier/ntfy:v2.37.2` and tag it locally as `binwiederhier/ntfy:v2.37.2`; verify its manifest digest matches the one below.

The primary workload is Matrix pushes: 1,000 live NDJSON subscribers across 100 topics, 30,000 unique event notifications, 32 concurrent persistent HTTP publishing connections. Every message must reach all 10 subscribers of its topic (300,000 total deliveries), with zero HTTP errors. Both servers have two worker CPUs (`GOMAXPROCS=2` / two Tokio workers), a 512 MiB container cap, equivalent elevated visitor limits, and persistent SQLite WAL with `synchronous=FULL` on an anonymous Docker volume. The Go server uses synchronous writes (default), while Rust groups simultaneous writes into a durable transaction and acknowledges only after commit. Set `REQUESTS=300000` when invoking the script to extend each sample; very long runs may require increasing the benchmark client's timeout.

`scripts/benchmark.py` measures end-to-end client-observed throughput and latency through Docker Desktop's port forwarding. It samples Linux process RSS from `/proc/1/status` before subscriptions, with all subscriptions connected, and after publishing. RSS includes resident code and process allocations but not the kernel's socket buffers or every cgroup-accounted page. This is not a maximum-capacity benchmark, and the Python load generator and Docker networking can become bottlenecks.

The upstream comparison image is ntfy 2.37.2, manifest digest `sha256:1875ddcf815c8a3c981d6d78c67a93452d0abba79b1ab9520b1f5f5238902bd7`. Upstream has additional capabilities not implemented here; results cannot be interpreted as a full feature-parity comparison.

For uncached or ordinary persisted publishing, run `scripts/benchmark.py` without `--matrix`, optionally adding `--cache`. These modes measure different durability semantics and should not be mixed in a single headline number.

## Functional validation

The 48 Rust tests cover account persistence, concurrent account connections, CLI/API restrictions, credential revocation, database migration, Web Push cryptography, SMTP and recovery links, as well as Matrix, UnifiedPush, streamed formats, replay, attachments, scheduling and resource limits. Final amd64 and arm64 containers also passed HTTP smoke tests with standard YAML on port 80, a custom cache path, account CLI/login and the web UI. External delivery tests use mocks; physical Android/iOS delivery and public-domain TLS still require verification in the deployed environment.
