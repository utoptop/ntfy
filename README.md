# ntfy-rs · Rust ntfy 服务端

以标准 ntfy 自托管功能为兼容目标的 Rust 服务端，采用 Tokio + Axum、SQLite WAL、共享广播消息和有界队列。保留上游 Web 应用，同时支持 Matrix / Element X Android 的 UnifiedPush 网关。镜像支持 `linux/amd64`、`linux/arm64`，运行时不包含 Go、Node.js 或 Rust 编译器。

0.2.0 增加反向代理客户端 IP、原版 `auth-file`、bcrypt 用户与 Token、账号登录/注册/同步、Web Push、SMTP 邮件入口与转发、邮箱验证/密码重置、iOS 上游唤醒和消息序列操作。**完整上游功能重构仍在进行中**：Firebase、付费套餐与配额、Twilio、Stripe、模板、集群等尚未实现，不能宣称已与上游完全等价。已实现功能、数据库迁移及具体限制见 [兼容范围](docs/compatibility.md)。

## 本地构建与推送

在本地仓库目录执行，Docker Hub 需已登录 `utoptop`：

```sh
docker buildx build --platform linux/amd64,linux/arm64 -t utoptop/ntfy:main --push .
```

此命令构建两个架构并推送镜像。镜像使用运行时配置，不包含具体服务域名。服务器部署由使用者自行完成。

## 服务器运行配置

`compose.yaml` 是可选的单服务运行示例。将镜像拉取到自己的服务器后启动：

```sh
docker compose pull
docker compose up -d
curl http://127.0.0.1:8080/v1/health
```

Matrix 网关和附件链接需要在部署时设置 `NTFY_BASE_URL` 为实际外部访问地址；可通过 shell 环境或 `.env` 传给 Compose，也可以使用 `server.yml` 的 `base-url`。仓库不预设域名，HTTPS、反向代理和证书由自己的部署环境管理。

持久化数据写入独立的 `ntfy-data` Docker volume，容器使用 UID/GID `10001`，示例端口只绑定本机 `127.0.0.1:8080`，可按自己的网络环境调整。

验证运行中的本地容器时，第二个参数传入该容器配置的外部访问地址：

```sh
python3 scripts/smoke.py http://127.0.0.1:8080 "$NTFY_BASE_URL"
```

下文 `https://ntfy.example.com` 仅为文档和协议测试使用的示例地址，请在部署后按实际地址使用。

## Element X Android 设置

1. 手机安装支持 UnifiedPush 的 **ntfy Android**，将默认服务器改为 `https://ntfy.example.com`，启用其 UnifiedPush 分发功能。
2. 在 Element X 的通知设置中选择 UnifiedPush / ntfy。必要时重新启用推送，让客户端重新注册端点。
3. 在 Element X 的通知故障排查中确认网关为 `https://ntfy.example.com/_matrix/push/v1/notify`，并执行测试。
4. 允许 ntfy 和 Element X 后台运行；ntfy 依靠持续连接接收消息。这个服务端没有实现 Google FCM，不能替代被系统终止的 Android 分发器进程。

不需要手工创建 `up*` 主题，也通常不需要改 Synapse 的服务器配置；客户端通过 Matrix pusher 注册自己的端点。具体客户端版本的菜单名称可能不同。参考 [UnifiedPush 排查指南](https://unifiedpush.org/users/troubleshooting/setup/) 和 [Element 官方说明](https://docs.element.io/latest/element-support/element-androidios-client-settings/using-unified-push-and-ntfy-for-push-notifications/)。

默认访问权限与 ntfy 的默认值一致，为匿名 read-write；主题地址需要保密。可通过 `auth-default-access`、CLI 或声明式 ACL 限制权限。

## 账号与原版配置

支持以下配置项，无需删除 `behind-proxy` 等字段绕过启动错误：

```yaml
listen-http: ":80"
behind-proxy: true
cache-file: "/var/lib/ntfy/cache.db"
cache-duration: "72h"
auth-file: "/var/lib/ntfy/auth.db"
auth-default-access: "read-write"
enable-login: true
enable-signup: false
# iOS 唤醒按需启用；必须同时通过 YAML 或 NTFY_BASE_URL 提供实际 base-url。
# upstream-base-url: "https://ntfy.sh"
log-level: "info"
```

只读挂载到 `/etc/ntfy/server.yml`。优先级为 CLI 参数 > `NTFY_*` 环境变量 > YAML；不要让 Compose 中的旧环境变量覆盖你需要的配置。`base-url` 在部署时设置，镜像没有预设域名。`behind-proxy: true` 使用转发头最右端的客户端地址；多层代理可设置 `proxy-trusted-hosts`，支持 IP、CIDR 和启动时解析的主机名。

可在运行中的容器内管理账号，数据库修改对后续请求立即生效：

```sh
docker compose exec ntfy ntfy user add --role=admin admin
docker compose exec ntfy ntfy user add phone
docker compose exec ntfy ntfy access phone 'up*' read-write
docker compose exec ntfy ntfy token add --label=phone phone
docker compose exec ntfy ntfy user list
```

密码由交互输入；自动化可使用 `NTFY_PASSWORD` 或 `NTFY_PASSWORD_HASH`。原版声明式配置也可使用：

```yaml
auth-users:
  - 'phone:$2b$10$...由 ntfy user hash 生成的完整 bcrypt 哈希...:user'
auth-access:
  - 'everyone:up*:write-only'
  - 'phone:up*:read-write'
auth-tokens:
  - 'phone:tk_...由 ntfy token generate 生成的完整 Token...:phone'
```

声明式用户、ACL、Token 在启动时按配置协调，不能通过账号 API 修改；从配置移除后会从数据库删除。CLI 创建的普通用户不会因此删除。旧版 Rust 的对象式 `auth-users` / `auth-access` 和无数据库的 Argon2/SHA-256 鉴权仍可使用。

Matrix homeserver 通常需要匿名写入 `up*`，手机用户可以使用私有读权限。不能在不提供写入凭据的情况下把所有 Matrix 推送主题设为匿名 deny-all。

原版 `auth.db` 的用户、bcrypt 密码、ACL 和 Token 可直接读取，旧 schema 自动备份并迁移。原版消息缓存 schema 1–17 会先备份，再逐条转为 Rust 缓存格式；回退 Go 时使用迁移前备份。首次切换前停止使用同一数据库的旧进程；容器 UID/GID 为 `10001`，数据目录须可读写。

## 浏览器、iOS 与邮件

- iOS：配置 `upstream-base-url` 和运行时 `base-url`，向上游仅发送消息 ID 和哈希主题，不转发通知正文。
- Web Push：用 `ntfy webpush keys` 生成密钥，配置 `web-push-public-key`、`web-push-private-key`、`web-push-file`、`web-push-email-address` 和 `base-url`。浏览器端点保存在兼容原版的 SQLite 数据库中。
- 邮件转发：配置 `smtp-sender-addr`、`smtp-sender-from`，按需设置用户名/密码。发布时可使用 `Email` 头；`smtp-sender-verify: true` 仅允许发送到当前账号已验证的地址。
- 邮件发布：配置 `smtp-server-listen`、`smtp-server-domain`，可选 `smtp-server-addr-prefix`。支持 `topic+token@domain` 和 SMTP AUTH PLAIN，仍执行主题 ACL。
- 账号邮箱验证、主邮箱设置和一次性密码重置链接需要 SMTP sender、`auth-file` 和运行时 `base-url`。

## 私有订阅与鉴权

设置 `auth-default-access: deny-all` 后，可用 CLI 或上述声明式配置逐个授权。ACL 中用户规则优先于 everyone，同类规则更长的匹配优先。支持 Basic、Bearer token、空 Basic 用户名的 Token 和 `auth` 查询参数。撤销 Token 或调整 ACL 后，现有流在下一次投递或保活前复查权限并关闭失效连接。

## API 示例

以下使用示例主题，请按实际 ACL 授权：

```sh
# 实时 NDJSON / SSE / WebSocket 订阅
curl -N https://ntfy.example.com/upDemo/json
curl -N https://ntfy.example.com/upDemo/sse
# WebSocket: wss://ntfy.example.com/upDemo/ws

# 常规通知
curl -H 'Title: Backup' -H 'Priority: high' -d 'Backup complete' https://ntfy.example.com/upDemo

# 历史消息，since 支持 all / latest / none / 时间戳 / 10m / 消息 ID
curl 'https://ntfy.example.com/upDemo/json?poll=1&since=all'

# Matrix 网关测试，不需要 homeserver 凭据
curl -H 'Content-Type: application/json' -d '{"notification":{"event_id":"$example-1","room_id":"!example:matrix.org","devices":[{"app_id":"io.element.android.x","pushkey":"https://ntfy.example.com/upDemo?up=1"}]}}' \
  https://ntfy.example.com/_matrix/push/v1/notify
```

## 开发验证

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Dockerfile 使用锁定的 Cargo/npm 依赖，Web 应用在构建阶段编译，最终镜像仅包含 Rust 二进制、静态资源和 Alpine 运行环境。CI 检查 Rust 格式、Clippy、集成测试及容器 smoke test；CI 中的示例域名仅用于本地协议验证，不会连接或部署公网服务。

## 性能与运维

- 每个连接是异步任务，无专用线程；默认 2 个 Tokio worker。
- 同一消息以 `Arc` 共享，NDJSON/SSE/raw 按需编码一次；不为每个订阅者复制整份消息。
- SQLite 在专用线程处理，有界入口队列；并发写入合并为最多 64 条一个事务，提交成功才返回成功并广播。持久化使用 WAL + `synchronous=FULL`。
- 历史留在磁盘。实时订阅自动按页恢复离线消息，默认每页最多 1,000 条 / 1 MiB，同时最多 8 个历史回放。一次 `poll=1` 返回一页，若有更多消息会带 `X-Messages-Truncated: 1`，可从最后一条消息 ID 继续。
- 慢订阅者超过环形缓冲后断开，重连时可从最后消息 ID 补发。`Cache: no` 消息不具备离线恢复能力。
- 历史保留时间及总条数都有上限；达到容量时淘汰最旧的已投递消息。**不能把有限缓存当作永久消息队列**。
- 附件采用流式上传下载，限制单文件与总磁盘占用，过期文件每分钟清理。支持标准 SQLite auth.db；Go cache.db 会自动备份并转换，详见兼容范围。
- `GET /v1/health` 检查数据库；`GET /metrics` 提供基础 Prometheus 指标，要求具有 `admin: true` 的用户凭据。
- 默认不信任转发头；反向代理部署可配置 `behind-proxy` 和 `proxy-trusted-hosts`。Matrix 网关不使用普通发布的 IP 速率限额，但仍受并发/队列/请求大小限制；请按实际部署在代理侧设置合适的限制。

0.2.0 的本机 arm64 对照测试同时开启两版的 `auth-file` 和登录功能，采用匿名 read-write 访问（1,000 个订阅、32 并发、每轮 30,000 次 Matrix 推送、三轮中位数）：Rust **4,928 次/秒**，上游 Go 2.37.2 **2,323 次/秒**；负载后进程 RSS 为 **39.54 MiB / 125.61 MiB**。这组负载下吞吐约为 **2.12 倍**，RSS 降低 **68.5%**。两版使用相同 2 CPU / 512 MiB 限制、SQLite FULL + WAL 和 Docker volume，所有投递数量吻合、HTTP 错误为零。测试不包含密码哈希、多账号身份或外部转发，也不代表持续生产容量；原始数据、波动范围及限制见 [性能报告](docs/performance.md)。

## 来源与许可

Rust 服务端为本仓库实现，Web 应用来自 [binwiederhier/ntfy](https://github.com/binwiederhier/ntfy)，上游快照见 [NOTICE](NOTICE)。按 Apache-2.0 发布。协议依据 [ntfy API](https://docs.ntfy.sh/subscribe/api/) 和 [Matrix Push Gateway API](https://spec.matrix.org/latest/push-gateway-api/)。
