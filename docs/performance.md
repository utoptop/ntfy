# Performance validation

Performance is workload-dependent. The server uses shared message buffers, bounded queues, asynchronous subscriptions, a small SQLite page cache and batched durable transactions. A language rewrite alone is not evidence of a speedup.

## Measured results — 2026-11-05

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
bash scripts/bench-docker.sh
```

On amd64, change the build platform to `linux/amd64`. The script starts fresh containers for each sample, runs three rounds per implementation, and stores the raw JSON in `bench-results/`. It removes only its named benchmark containers when finished. Run on an otherwise idle machine; ports 18080 and 18081 must be free.

The primary workload is Matrix pushes: 1,000 live NDJSON subscribers across 100 topics, 30,000 unique event notifications, 32 concurrent persistent HTTP publishing connections. Every message must reach all 10 subscribers of its topic (300,000 total deliveries), with zero HTTP errors. Both servers have two worker CPUs (`GOMAXPROCS=2` / two Tokio workers), a 512 MiB container cap, equivalent elevated visitor limits, and persistent SQLite WAL with `synchronous=FULL` on an anonymous Docker volume. The Go server uses synchronous writes (default), while Rust groups simultaneous writes into a durable transaction and acknowledges only after commit. Set `REQUESTS=300000` when invoking the script to extend each sample; very long runs may require increasing the benchmark client's timeout.

`scripts/benchmark.py` measures end-to-end client-observed throughput and latency through Docker Desktop's port forwarding. It samples Linux process RSS from `/proc/1/status` before subscriptions, with all subscriptions connected, and after publishing. RSS includes resident code and process allocations but not the kernel's socket buffers or every cgroup-accounted page. This is not a maximum-capacity benchmark, and the Python load generator and Docker networking can become bottlenecks.

The upstream comparison image is ntfy 2.37.2, manifest digest `sha256:1875ddcf815c8a3c981d6d78c67a93452d0abba79b1ab9520b1f5f5238902bd7`. Upstream has additional capabilities not implemented here; results cannot be interpreted as a full feature-parity comparison.

For uncached or ordinary persisted publishing, run `scripts/benchmark.py` without `--matrix`, optionally adding `--cache`. These modes measure different durability semantics and should not be mixed in a single headline number.

## Functional validation

The Rust integration suite verifies Matrix delivery and persistent deduplication, invalid pushkey rejection, binary UnifiedPush payloads, ACLs, restart recovery, streamed formats, WebSocket behavior, replay pagination and boundaries, slow-subscriber recovery, attachments, scheduling and resource limits. The Docker smoke test verifies the externally served HTTP protocol. Physical Android delivery and public-domain TLS require a final check on the deployed server and phone.
