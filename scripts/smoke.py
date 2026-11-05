#!/usr/bin/env python3
"""Real HTTP Matrix/UnifiedPush smoke test. Uses only Python's standard library."""
import base64
import json
import sys
import time
import urllib.request
import uuid

base = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080").rstrip("/")
push_origin = (sys.argv[2] if len(sys.argv) > 2 else "https://ntfy.aosn.de").rstrip("/")
topic = "upSmoke" + uuid.uuid4().hex[:12]


def request(path, data=None, headers=None):
    req = urllib.request.Request(base + path, data=data, headers=headers or {})
    with urllib.request.urlopen(req, timeout=10) as response:
        return response.read()


for attempt in range(30):
    try:
        assert json.loads(request("/v1/health"))["healthy"]
        break
    except Exception:
        if attempt == 29:
            raise
        time.sleep(1)

assert json.loads(request("/_matrix/push/v1/notify")) == {"unifiedpush": {"gateway": "matrix"}}
assert json.loads(request(f"/{topic}?up=1")) == {"unifiedpush": {"version": 1}}
body = json.dumps({"notification": {
    "event_id": "$smoke-" + uuid.uuid4().hex, "room_id": "!smoke:example.org",
    "counts": {"unread": 1}, "devices": [{"app_id": "io.element.android.x",
    "pushkey": f"{push_origin}/{topic}?up=1"}]}}).encode()
for _ in range(2):
    assert json.loads(request("/_matrix/push/v1/notify", body)) == {"rejected": []}
messages = [json.loads(line) for line in request(f"/{topic}/json?poll=1").splitlines()]
assert len(messages) == 1, messages
assert messages[0]["message"].encode() == body
binary = bytes([0, 1, 255, 128, 10])
published = json.loads(request(f"/{topic}?up=1", binary))
assert published["encoding"] == "base64"
assert base64.b64decode(published["message"]) == binary
print(json.dumps({"status": "passed", "checks": ["health", "matrix discovery", "UnifiedPush discovery", "Matrix delivery", "retry deduplication", "durable replay", "binary payload"], "topic": topic}, ensure_ascii=False))
