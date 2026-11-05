# Performance validation

Performance is workload-dependent. The server uses shared message buffers, bounded queues, asynchronous subscriptions, a small SQLite page cache and batched durable transactions. A language rewrite alone is not evidence of a speedup.

## Reproducing the comparison

Build a local image and run the supplied benchmark:

```sh
docker buildx build --platform linux/arm64 -t ntfy-rs:verify --load .
docker pull binwiederhier/ntfy:2.37.2
bash scripts/bench-docker.sh
```

On amd64, change the build platform to `linux/amd64`. The script starts fresh containers for each sample, runs three rounds per implementation, and stores the raw JSON in `bench-results/`. It removes only its named benchmark containers when finished. Run on an otherwise idle machine; ports 18080 and 18081 must be free.

The primary workload is Matrix pushes: 1,000 live NDJSON subscribers across 100 topics, 3,000 unique event notifications, 32 concurrent persistent HTTP publishing connections. Every message must reach all 10 subscribers of its topic (30,000 total deliveries), with zero HTTP errors. Both servers have two worker CPUs (`GOMAXPROCS=2` / two Tokio workers), a 512 MiB container cap, equivalent elevated visitor limits, and persistent SQLite WAL with `synchronous=FULL`. The Go server uses synchronous writes (default), while Rust groups simultaneous writes into a durable transaction and acknowledges only after commit.

`scripts/benchmark.py` measures end-to-end client-observed throughput and latency through Docker Desktop's port forwarding. It samples Linux process RSS from `/proc/1/status` before subscriptions, with all subscriptions connected, and after publishing. RSS includes resident code and process allocations but not the kernel's socket buffers or every cgroup-accounted page. This is not a maximum-capacity benchmark, and the Python load generator and Docker networking can become bottlenecks.

The upstream comparison image is ntfy 2.37.2, manifest digest `sha256:1875ddcf815c8a3c981d6d78c67a93452d0abba79b1ab9520b1f5f5238902bd7`. Upstream has additional capabilities not implemented here; results cannot be interpreted as a full feature-parity comparison.

For uncached or ordinary persisted publishing, run `scripts/benchmark.py` without `--matrix`, optionally adding `--cache`. These modes measure different durability semantics and should not be mixed in a single headline number.

## Functional validation

The Rust integration suite verifies Matrix delivery and persistent deduplication, invalid pushkey rejection, binary UnifiedPush payloads, ACLs, restart recovery, streamed formats, WebSocket behavior, replay pagination and boundaries, slow-subscriber recovery, attachments, scheduling and resource limits. The Docker smoke test verifies the externally served HTTP protocol. Physical Android delivery and public-domain TLS require a final check on the deployed server and phone.
