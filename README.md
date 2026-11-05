# ntfy-rs · Matrix / Element X Android 推送网关

用 Rust 重写的 ntfy 自托管服务端，重点服务 **Matrix → UnifiedPush → ntfy Android → Element X Android**。采用 Tokio + Axum、SQLite WAL、共享广播消息和有界队列。Docker 镜像同时支持 `linux/amd64` 与 `linux/arm64`，运行时不包含 Go、Node.js 或 Rust 编译器。

这是独立实现，**不是上游全部功能的等价替换**。保留上游 Web 应用，支持常规发布订阅、附件和定时消息；未实现 Firebase/FCM、iOS/APNs 上游唤醒、Web Push、邮件、计费、集群和账号管理 API。配置格式及数据库格式也不同，详见 [兼容范围](docs/compatibility.md)。

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

本仓库的默认 Compose 仅对 `up*` 主题开放匿名读写，其他主题禁止访问。随机的 `up*` 地址相当于访问凭据，不应公开。需要限制读取时，使用下面的私有配置。

## 私有订阅与鉴权

Matrix homeserver 必须能匿名写入推送主题，因此不能简单地要求所有请求登录。推荐匿名 `up*` **write-only**，手机用户 `up*` **read-write**。

通过标准输入生成密码哈希，避免把明文密码写进配置或命令参数：

```sh
read -rs NTFY_PASSWORD
printf '%s' "$NTFY_PASSWORD" | docker run --rm -i utoptop/ntfy:main hash-password
unset NTFY_PASSWORD
```

将输出写入 `server.local.yml`（该文件已被 Git 忽略）：

```yaml
auth-default-access: deny-all
auth-users:
  - name: phone
    password-hash: '$argon2id$...实际生成的哈希...'
auth-access:
  - { user: '*', topic: 'up*', permission: write-only }
  - { user: phone, topic: 'up*', permission: read-write }
```

把该文件只读挂载至 `/etc/ntfy/server.yml`，并**删除 Compose 中的 `NTFY_AUTH_ACCESS` 环境变量**，否则它会覆盖 YAML。在手机 ntfy 中配置 `phone` 的用户名/密码。也支持 Bearer token：将随机 token 的 SHA-256 小写十六进制哈希写入用户的 `token-hashes` 数组；可通过 `ntfy token-hash` 从 stdin 生成。token 应使用至少 32 字节的安全随机值。

ACL 中具体用户优先于 `*`，同类规则中更具体的主题模式优先；支持 `*` 通配符。配置修改后重启容器。Web 端可配置订阅凭据，但账号注册、会话登录和账号同步没有启用。

## API 示例

部署示例仅开放 `up*`，以下主题因此可直接测试：

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
- 附件采用流式上传下载，限制单文件与总磁盘占用，过期文件每分钟清理。需要新数据库，禁止直接挂载上游的 cache.db/auth.db。
- `GET /v1/health` 检查数据库；`GET /metrics` 提供基础 Prometheus 指标，要求具有 `admin: true` 的用户凭据。
- 默认不信任 `X-Forwarded-For`，反向代理后的客户端共享代理 IP 限额。Matrix 网关不使用普通发布的 IP 速率限额，但仍受并发/队列/请求大小限制；请按实际部署在代理侧设置合适的限制。

本机 arm64 对照测试（1,000 个订阅、32 并发、每轮 30,000 次 Matrix 推送、三轮中位数）：Rust 约 **5,257 次/秒**，上游 Go 2.37.2 约 **2,467 次/秒**；负载后的进程 RSS 为 **34.46 MiB / 128.95 MiB**。两版均使用 SQLite FULL + WAL 和 Docker volume，所有投递数量吻合、HTTP 错误为零。这是特定短时负载的结果，不代表持续生产容量或所有功能的性能；原始数据与限制见 [性能报告](docs/performance.md)。

## 来源与许可

Rust 服务端为本仓库实现，Web 应用来自 [binwiederhier/ntfy](https://github.com/binwiederhier/ntfy)，上游快照见 [NOTICE](NOTICE)。按 Apache-2.0 发布。协议依据 [ntfy API](https://docs.ntfy.sh/subscribe/api/) 和 [Matrix Push Gateway API](https://spec.matrix.org/latest/push-gateway-api/)。
