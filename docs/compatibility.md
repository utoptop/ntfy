# Compatibility

The target is the standard ntfy self-hosted server, with upstream v2.37.2 as the current reference. Version 0.2.0 expands the original Matrix-focused implementation. **The complete upstream port is not finished.** This table describes working code, not a claim that every upstream option or API is implemented.

| Area | Status in 0.2.0 |
| --- | --- |
| Publish and subscribe | POST/PUT/root JSON; GET publish/send/trigger; NDJSON, SSE, raw, WebSocket; multiple topics, filters, polling, ID/time replay, live replay |
| Matrix / UnifiedPush | Discovery, multiple devices, same-origin pushkey validation, persistent event deduplication, UTF-8/binary payload preservation |
| Notifications | Title, priority, tags, actions, click, icon, Markdown, local/remote attachments |
| Message sequences | Sequence header/JSON/path; update, delete, clear/read events; replace or cancel a scheduled sequence |
| Scheduling | Persistent duration or Unix timestamp; scheduler resolution one second |
| Reverse proxies | `behind-proxy`, `proxy-forwarded-header`, RFC 7239, IPv4/IPv6 and trusted proxy IP/CIDR/host lists |
| Authentication | Standard bcrypt and existing Argon2 hashes; Basic/Bearer/query auth; standard SQLite `auth-file`; token expiry/revocation; topic ACLs and admin role |
| Declarative authentication | Upstream `name:hash:role`, `user:topic:permission`, `user:token[:label]`; startup reconciliation; comma-separated environment lists or JSON |
| Accounts | Signup/login/delete/password, tokens, settings, subscription sync and private sync topics; live streams recheck database credentials/ACLs before subsequent delivery |
| Administrative CLI | `user` add/list/remove/change-pass/change-role/hash; `access` list/set/reset; `token` add/list/remove/generate |
| Administrative HTTP | User/password CRUD, grants and tokens for ordinary users; sensitive administrator changes and bulk token disclosure remain local CLI operations |
| Topic reservations | Ownership and ACL creation/update/removal, list in account info, delete-messages; existing tier reservation limits checked. Tier administration and other tier quotas remain incomplete |
| iOS upstream | Empty POST with `X-Poll-ID` to the SHA-256 topic URL; optional upstream Bearer token; UnifiedPush/Matrix payloads excluded |
| Browser Web Push | VAPID key CLI, AES128GCM encryption, upstream-compatible subscription database, topic read ACLs, current permission checks, expiry warnings and invalid endpoint cleanup |
| Email publishing | SMTP listener, domain/prefix validation, one recipient, token in address or AUTH PLAIN, MIME/plain/HTML decoding, HTTP ACL and rate-limit enforcement |
| Email notifications | SMTP sender, optional STARTTLS or TLS on port 465, authentication, bounded delivery queue, per-visitor email burst limit, optional verified recipients |
| Account email/recovery | Email verification/resend/delete/primary address, hashed one-time links, uniform reset-request response, atomic password-reset consumption |
| Static UI | Upstream web application; login/signup/email/Web Push features advertised when configured |
| Configuration | YAML, underscore aliases, CLI field overrides and `NTFY_*`; K/M/G byte units; unknown options fail startup |
| Health/metrics | Database health and authenticated basic Prometheus counters |

## Databases and migration

- **Authentication:** standard schema version 11 is used directly. Versions 1–10 migrate transactionally in upstream order. Passwords, users, grants, tokens, account preferences and other existing tables are preserved. Declarative entries are subsequently reconciled with configuration, as upstream does; keep their declarations when migrating.
- **Web Push:** standard SQLite schema version 1 is supported directly.
- **Message cache:** recognized Go schemas 1–17 are imported one row at a time into the Rust cursor/JSON schema. IDs, order, event/sequence fields, attachment metadata/deleted state, schedules and expiration are retained. Existing attachment files must remain in the configured attachment directory. Go sender/user usage metadata is not currently used by the Rust cache.
- A migration creates a WAL-aware, mode-0600 snapshot beside the database, e.g. `auth.db.pre-rust-v1.bak` or `cache.db.pre-rust-go-v17.bak`. The backup name becomes visible only after the snapshot is complete. Existing backups are never overwritten. Unknown databases and unsupported cache versions fail without rewriting their tables.
- Stop the prior process before starting another implementation against its files. A converted message cache is not readable by Go: use its backup when rolling back. Do not treat the automatic backup as a substitute for your own retained backup.
- The Docker process runs as UID/GID `10001`. Mounted configuration and data must have the corresponding read/write permissions.

## Remaining work and differences

- Firebase/FCM service-account delivery, Twilio calls, Stripe/billing, tier management/enforcement, PostgreSQL, S3 and multi-node clustering are not implemented.
- Full per-account/day usage accounting and per-visitor attachment bandwidth/storage quotas are not implemented. `/v1/account` currently provides configured global attachment limits, reservation counts and zero usage counters for messages/emails/calls. Existing tier metadata is preserved; do not rely on this release to enforce purchased-plan quotas.
- Go/Sprig templates, natural-language/timezone schedules, ntfy client CLI publish/subscribe, built-in HTTPS/Unix/admin listeners and all upstream config knobs are not implemented. Unsupported publication options fail instead of claiming execution. `web-root` still names the static asset directory in this Rust implementation, not upstream's virtual web-app prefix.
- Error envelopes use `code/http/error`, but exact upstream numeric error codes are not exhaustively matched. Third-party clients relying on particular numeric codes need additional compatibility testing.
- Authentication is queried on a bounded SQLite worker. Identical concurrent reads share the in-flight query and prepared statements are reused; completed results are not retained in a timed permission cache. Account mutations invalidate pending read groups before replying. Bcrypt/Argon2 work has bounded blocking concurrency. There is no upstream access-cache setting yet. Legacy Rust static users, token hashes and object ACL syntax remain available without `auth-file`; standard database-managed deployments should use bcrypt and normal ntfy tokens.
- Reverse-proxy trust is opt-in. The last forwarded-header field is used, scanning from right to left and removing configured trusted hops. Malformed addresses fall back to the TCP peer. Hostnames in `proxy-trusted-hosts` resolve at startup.
- Upstream, Web Push and SMTP have bounded queues and timeouts. External provider failure is logged after local acceptance; it does not undo a committed notification. This is not an exactly-once external delivery system. Scheduled messages are not accepted with email forwarding.
- Web Push endpoints are restricted to recognized HTTPS push-service hosts; redirects are disabled. Transient failures preserve subscriptions; 404/410 remove them. Provider acceptance, APNs delivery and device power-management behavior are not proven by local tests.
- Matrix gateway pushkeys must be same-origin single-topic URLs under the runtime base URL. No outgoing request is made to a pushkey. Request size defaults to 4096 bytes. Device liveness/410 expiration tracking is not implemented. Successful event deliveries are deduplicated across homeserver retries; count-only notifications are not.
- Cache is bounded by age and message count. Poll replies are bounded to one page and indicate truncation; streaming replay pages internally. Slow consumers disconnect and can resume from the durable cache. `Cache: no` messages cannot be recovered after disconnect.
- Standard `listen-http: ":80"` is accepted; the container default is port 8080. `base-url` is an HTTP(S) origin configured at runtime, required for Matrix, attachment links, iOS upstream and verification/reset links. No deployment domain or reverse proxy is built into the image.

## Validation

Rust integration tests cover persistent auth/ACL migration, declarative reconciliation, account/token isolation and revocation, administrative restrictions, live-stream revocation, cache migration/restart, sequence replacement/cancellation, proxy IP isolation, Web Push cryptographic round trips/registration, SMTP ingress/forwarding and one-time email/reset links, in addition to the original Matrix, attachment, replay and WebSocket tests.

External delivery tests use local mock services. They do not send production notifications or deploy a public service. `scripts/smoke.py` validates a running container. `docs/performance.md` includes a 0.2 benchmark with the authentication database enabled and separately labels historical 0.1 data. External forwarding providers are disabled in throughput tests.
