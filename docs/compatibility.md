# Compatibility

This release targets self-hosted Matrix / Element X Android notifications through an ntfy Android UnifiedPush distributor. It is not a complete port of the upstream Go server. The original web client is bundled as a static application; optional backend features remain disabled.

| Area | Support |
| --- | --- |
| Matrix `GET/POST /_matrix/push/v1/notify` | Gateway discovery, multiple devices, same-origin pushkey validation, rejected invalid keys, persistent per-topic event-ID deduplication |
| UnifiedPush `GET /topic?up=1` | Version 1 discovery |
| UnifiedPush POST/PUT | UTF-8 bytes preserved including whitespace and NUL; invalid UTF-8 encoded as base64 with `encoding=base64`; `Content-Encoding: aes128gcm` selects the same payload handling |
| Publish | POST/PUT topic; POST/PUT root JSON; GET `/topic/publish`, `/send`, `/trigger`; common header/query aliases |
| Subscribe | NDJSON, SSE, raw, WebSocket; multiple topics; open/keepalive; poll; resume by ID/time/duration; latest; filters; bounded automatic live replay |
| Message fields | title, priority, tags, click, icon, Markdown, actions, remote attachment URLs |
| Local attachments | Streamed upload/download, configured base URL, size/quota/expiry limits; authenticated downloads check topic read ACL |
| Scheduling | Persistent duration/Unix-timestamp delivery; scheduled replay; one-second scheduler resolution |
| Authentication | Basic with Argon2id password hashes, Bearer tokens stored as SHA-256 hashes, empty Basic username with token password, URL-safe base64 `auth` query parameter |
| Access control | Static YAML/JSON environment users and wildcard topic rules; admin role; no runtime account database |
| Web client | Bundled upstream UI, config.js, manifest, static assets, subscriptions and ordinary publishing; optional account/Web Push/payment capabilities disabled |
| Account info | Read-only `/v1/account` supplies server attachment limits; account mutation/login/token APIs not implemented |
| Health/metrics | Database health plus authenticated basic Prometheus counters |

## Deliberate differences and unsupported features

- **No Firebase/FCM, Web Push/VAPID, APNs/iOS upstream poll forwarding.** Android must keep ntfy's UnifiedPush distributor running.
- No SMTP publishing/forwarding, calls/Twilio, Stripe, topic reservations, email verification, password reset, account subscription/settings sync, multi-node clustering, PostgreSQL/S3, message templates, ntfy CLI publish/subscribe commands, or upstream administrative CLI commands.
- No notification sequence updates/deletes/clears, delayed-message update/cancel, natural-language schedules or timezone parsing. Requests using unsupported publication options return an error instead of reporting successful execution.
- Error bodies follow ntfy's `code/http/error` envelope; exact upstream numeric error codes are not exhaustively reproduced. Unknown routes return 404.
- Unknown YAML keys fail startup. The environment accepts the fields in `src/config.rs` as `NTFY_<UPPER_SNAKE_CASE>`; complex arrays are JSON. Upstream's colon-separated auth-users syntax and bcrypt hashes are not accepted.
- SQLite files are **not** compatible with the Go version. Startup refuses an existing foreign database. Start with a new named volume, preserve the old volume for rollback, and transfer credentials through the new config format. Existing client subscriptions can keep their URLs if the hostname and topic ACLs remain the same; old cached messages are not migrated.
- Matrix gateway accepts only pushkeys under the configured base origin and valid single topic paths. It never performs outgoing requests to pushkey URLs. Unknown topics are valid ntfy topics and are not treated as expired devices. Device liveness/410 abandonment tracking is not implemented.
- Matrix request bodies are bounded by `message-size-limit` (default 4096 bytes), matching the common upstream default. Per-topic event-ID deduplication lasts while the corresponding cached message is retained; count-only updates are not deduplicated.
- Matrix multi-device requests may be partially delivered if a later device encounters a transient error; homeserver retries are safe for event notifications because successful device deliveries are deduplicated. Do not interpret transient HTTP errors as permanent pushkey rejection.
- The Compose default grants `up*` anonymous read/write access by unguessable URL. For authenticated readers, retain anonymous write-only access for homeservers and grant read access to a configured phone user.
- No built-in TLS listener. HTTPS and reverse proxy configuration belong to your deployment environment. The image has no preset public domain; configure `base-url` at runtime as an origin, not a URL path prefix. Forwarded IP headers are not trusted by this release.
- Raw streams flatten line breaks, as the upstream protocol does. SSE message events have `data:` without a named `event: message`, preserving EventSource.onmessage behavior.
- Retention is bounded by age and count. Polling is paginated and indicates truncation in a response header. Live subscriptions page internally before switching to live events; under sustained pressure the client may need multiple reconnects to catch up.

## Validation scope

`cargo test` exercises real protocol behavior, including a TCP WebSocket connection, restart persistence, Matrix retry deduplication, multi-device delivery, hostile pushkeys, private ACLs, binary payloads, attachment quotas, slow consumers, replay boundaries and delayed delivery. `scripts/smoke.py` verifies a running Docker container using HTTP.

Tests emulate the homeserver and distributor protocol. They do not prove delivery on a particular physical phone, Android power-management setup, or public TLS deployment. Finish deployment by running Element X's notification diagnostics on the actual device.
