# ppt-tcp

基于 **tokio + TCP/TLS 1.3** 的 Rust 长连接游戏服务器后端,采用 **DDD + 洋葱架构** 组织为 Cargo workspace 多 crate 单体(PRD:`prd.md`)。

## 项目简介

面向中频实时玩法(回合制 / MMORPG / 卡牌 / 社交对战)的长连接服务端:

* **传输**:TCP + rustls(TLS 1.3 only,ring 提供器),长度前缀帧 `[4B len][2B opcode][protobuf]`;
* **架构**:依赖方向永远指向 domain,crate 边界由编译器强制(见下文分层);
* **并发模型**:每个 Room 一个 Actor task,串行处理命令、无锁;连接读写分离,发送队列有界(慢客户端直接断开);
* **安全**:argon2id 密码哈希、opaque token(可立即吊销)、未鉴权连接限时限量、每连接令牌桶限流、日志脱敏;
* **存储**:PostgreSQL 18(SeaORM 2.0,`uuidv7()` 主键)+ Redis(token 存储,MultiplexedConnection);
* **可观测**:`tracing` 结构化日志(json/pretty)、Prometheus `/metrics`、`/healthz`;
* **优雅停机**:SIGTERM → 停 accept → 广播房间关闭与维护通知 → 排空连接 → 关闭连接池。

## Workspace 分层

```
crates/
├── domain/            领域层:聚合根(账号/玩家/会话/房间)、值对象、领域事件、仓储 trait(零技术栈依赖)
├── application/       应用层:注册/登录/档案/成长用例、端口 trait、UnitOfWork、Room Actor 与 RoomService
├── net-kit/           通用网络框架:Transport trait、TLS 监听、帧编解码、读写分离、有界背压(零业务依赖)
├── protocol/          协议层:proto/game.proto → prost 生成、GameCodec/ClientCodec、opcode 路由表
├── infrastructure/    基础设施:环境变量配置、SeaORM 仓储、Redis/内存 token 存储、argon2id、事件分发
├── gateway/           接口层:TCP+TLS 入口(绑定/心跳/房间/聊天 + 限流/鉴权门)、axum HTTP 入口
└── server-bin/        组装根:唯一可执行文件(DI、启动、信号处理、优雅停机)
migration/             SeaORM 迁移:accounts / players / audit_logs
```

依赖方向:`server-bin → gateway/infrastructure → application → domain`;`protocol → net-kit`。domain 与 net-kit 是两个互不相知的零依赖核心。

## 构建与运行

环境要求:Rust 1.98.1(stable)、PostgreSQL 18、Redis。

```bash
# 1. 准备配置
cp .public_env .env            # 修改 DATABASE_URL / REDIS_URL 等实际值

# 2. 准备 TLS 证书(本地自签示例)
mkdir -p certs
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
  -keyout certs/server.key -out certs/server.crt -nodes -subj "/CN=localhost" -days 365

# 3. 构建
cargo build

# 4. 运行(启动时自动执行 migration)
cargo run -p ppt-tcp-server-bin
```

启动后:

| 端点 | 说明 |
| --- | --- |
| `GET /healthz` | HTTP 健康检查(HTTP 监听地址,默认 8081) |
| `POST /register` | 注册 `{username, password, nickname}` |
| `POST /login` | 登录,返回 opaque token |
| `GET /me` | `Authorization: Bearer <token>` 查询玩家档案 |
| `GET /metrics` | Prometheus 指标(默认 9090) |
| TCP+TLS 端口 | 长连接玩法通道(默认 8080):`Bind{token}` → `Heartbeat` / `JoinRoom` / `RoomChat` |

协议详见 `crates/protocol/proto/game.proto`。

## 开发与测试

```bash
cargo fmt --all -- --check            # 格式
cargo clippy --all-targets -- -D warnings   # lint
cargo test --workspace                # 单元测试 + TCP/TLS 端到端测试(无需数据库)
cargo llvm-cov --workspace --summary-only    # 覆盖率
```

* 单元测试全部可离线运行;SeaORM/Redis 仓储实现需要真实服务,不在单测中覆盖;
* 端到端测试使用 rcgen 自签证书走真实 TLS 握手(见 `crates/gateway/tests/e2e.rs`);
* 当前覆盖率:行覆盖 ≈ 88%,核心 crate(domain/application/protocol/net-kit)≈ 83%–100%。

## 常用配置

全部参数经环境变量/`.env` 注入(默认值见 `crates/infrastructure/src/config.rs` 与 `.public_env`),涵盖:网络监听、帧上限、心跳/未鉴权超时、背压队列容量、限流、TLS 路径、数据库/Redis 连接池、argon2id 参数、日志与指标端口。敏感信息仅允许环境注入。

## 贡献指南

* **必须**遵守 [AGENTS.md](AGENTS.md)(提交格式 `<type>: <中文描述>`、版本锁定 `=`、CI 检查、DDD 分层红线);
* CI(GitHub Actions)执行 `fmt --check` / `clippy -D warnings` / `build` / `test`,见 `.github/workflows/rust-ci.yml`;
* 提交前请本地跑通上述"开发与测试"命令。

## License

MIT OR Apache-2.0(见 `LICENSE-MIT` / `LICENSE-APACHE`)。
