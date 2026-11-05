#!/usr/bin/env python3
"""Dependency-free HTTP benchmark. Measures client-observed latency, not server CPU alone.

Run against an isolated benchmark instance with elevated rate/subscription limits.
Memory samples use /proc/1/status inside the named Docker container (Linux VmRSS).
"""
import argparse
import asyncio
import json
import math
import subprocess
import time
import uuid
from urllib.parse import urlsplit


async def headers(reader):
    status = await reader.readline()
    if not status:
        raise RuntimeError("server closed connection")
    result = {}
    while True:
        line = await reader.readline()
        if line in (b"\r\n", b"\n", b""):
            break
        key, value = line.decode().split(":", 1)
        result[key.lower()] = value.strip()
    return int(status.split()[1]), result


async def chunk(reader):
    line = await reader.readline()
    if not line:
        return b""
    size = int(line.split(b";", 1)[0], 16)
    if size == 0:
        await reader.readline()
        return b""
    data = await reader.readexactly(size)
    await reader.readexactly(2)
    return data


async def response(reader):
    status, h = await headers(reader)
    if "content-length" in h:
        data = await reader.readexactly(int(h["content-length"]))
    elif h.get("transfer-encoding") == "chunked":
        data = bytearray()
        while part := await chunk(reader):
            data.extend(part)
    else:
        data = b""
    return status, data


def rss(container):
    if not container:
        return None
    output = subprocess.check_output(["docker", "exec", container, "cat", "/proc/1/status"], text=True)
    for line in output.splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) * 1024


async def main(args):
    url = urlsplit(args.url)
    host, port = url.hostname, url.port or 80
    if url.scheme != "http":
        raise ValueError("Use a local HTTP benchmark endpoint to isolate server performance")
    prefix = "upBench" + uuid.uuid4().hex[:8]
    topics = [f"{prefix}{i}" for i in range(max(1, args.topics))]
    latency, failures, writers, consumers = [], [], [], []
    received = 0
    memory_idle = await asyncio.to_thread(rss, args.container)

    async def subscribe(i):
        reader, writer = await asyncio.open_connection(host, port)
        topic = topics[i % len(topics)]
        writer.write(f"GET /{topic}/json HTTP/1.1\r\nHost: {host}\r\n\r\n".encode())
        await writer.drain()
        status, h = await headers(reader)
        if status != 200:
            raise RuntimeError(f"subscription HTTP {status}")
        first = await chunk(reader)
        if json.loads(first)["event"] != "open":
            raise RuntimeError("expected open event")
        writers.append(writer)

        async def consume():
            nonlocal received
            pending = b""
            try:
                while data := await chunk(reader):
                    pending += data
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        if line and json.loads(line).get("event") == "message":
                            received += 1
            except (asyncio.IncompleteReadError, ConnectionError):
                pass
        consumers.append(asyncio.create_task(consume()))

    for offset in range(0, args.connections, 20):
        await asyncio.gather(*(subscribe(i) for i in range(offset, min(offset + 20, args.connections))))
    memory_connected = await asyncio.to_thread(rss, args.container)

    async def worker(worker_id):
        reader, writer = await asyncio.open_connection(host, port)
        try:
            for index in range(worker_id, args.requests, args.concurrency):
                topic = topics[index % len(topics)]
                if args.matrix:
                    path = "/_matrix/push/v1/notify"
                    data = json.dumps({"notification": {"event_id": f"${prefix}-{index}", "room_id": "!bench:example.org", "counts": {"unread": index}, "devices": [{"app_id": "io.element.android.x", "pushkey": f"{args.push_origin}/{topic}?up=1"}]}}).encode()
                else:
                    path = f"/{topic}?up=1&cache={'yes' if args.cache else 'no'}"
                    data = (str(index) + ":" + "x" * args.bytes).encode()
                request = f"POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {len(data)}\r\nContent-Type: application/json\r\n\r\n".encode() + data
                start = time.perf_counter()
                writer.write(request)
                await writer.drain()
                status, body = await response(reader)
                latency.append((time.perf_counter() - start) * 1000)
                if status != 200:
                    failures.append(status)
                elif args.matrix and json.loads(body).get("rejected"):
                    failures.append("rejected")
        finally:
            writer.close()
            await writer.wait_closed()

    start = time.perf_counter()
    await asyncio.gather(*(worker(i) for i in range(args.concurrency)))
    seconds = time.perf_counter() - start
    expected = sum(sum(1 for i in range(args.connections) if i % len(topics) == j % len(topics)) for j in range(args.requests))
    deadline = time.perf_counter() + 10
    while received < expected and time.perf_counter() < deadline:
        await asyncio.sleep(0.02)
    memory_loaded = await asyncio.to_thread(rss, args.container)
    for writer in writers:
        writer.close()
    await asyncio.gather(*(w.wait_closed() for w in writers), return_exceptions=True)
    for task in consumers:
        task.cancel()
    await asyncio.gather(*consumers, return_exceptions=True)
    latency.sort()
    quantile = lambda q: round(latency[min(len(latency) - 1, math.ceil(q * len(latency)) - 1)], 3)
    print(json.dumps({"url": args.url, "mode": "matrix" if args.matrix else ("cached" if args.cache else "uncached"), "requests": args.requests, "concurrency": args.concurrency, "subscriptions": args.connections, "topics": len(topics), "seconds": round(seconds, 3), "requests_per_second": round(args.requests / seconds, 1), "latency_ms": {"p50": quantile(.5), "p95": quantile(.95), "p99": quantile(.99)}, "http_errors": len(failures), "error_samples": failures[:5], "deliveries_expected": expected, "deliveries_received": received, "rss_bytes": {"idle": memory_idle, "connected": memory_connected, "after_load": memory_loaded}}, indent=2))
    if failures or received != expected:
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080")
    parser.add_argument("--push-origin", default="https://ntfy.aosn.de")
    parser.add_argument("--requests", type=int, default=3000)
    parser.add_argument("--concurrency", type=int, default=32)
    parser.add_argument("--connections", type=int, default=1000)
    parser.add_argument("--topics", type=int, default=100)
    parser.add_argument("--bytes", type=int, default=256)
    parser.add_argument("--container")
    parser.add_argument("--cache", action="store_true")
    parser.add_argument("--matrix", action="store_true")
    asyncio.run(asyncio.wait_for(main(parser.parse_args()), timeout=180))
