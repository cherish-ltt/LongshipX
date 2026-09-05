# ppt-tcp

基于 **tokio + TCP/TLS 1.3** 的 Rust 长连接游戏服务器后端,采用 **DDD + 洋葱架构** 组织为 Cargo workspace 多 crate 单体(PRD:`prd.md`)。

## 项目简介

面向中频实时玩法(回合制 / MMORPG / 卡牌 / 社交对战)的长连接服务端:

* **传输**:TCP + rustls(TLS 1.3 only,ring 提供器),长度前缀帧 `[4B len u32 BE][2B opcode][protobuf payload]`;
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
├── gateway/           接口层:TCP+TLS 入口(绑定/心跳/房间/档案/聊天 + 限流/鉴权门)、axum HTTP 入口
└── server-bin/        组装根:唯一可执行文件(DI、启动、信号处理、优雅停机)+ 可运行示例
migration/             SeaORM 迁移:accounts / players / audit_logs
```

依赖方向:`server-bin → gateway/infrastructure → application → domain`;`protocol → net-kit`。domain 与 net-kit 是两个互不相知的零依赖核心。

## 快速开始

环境要求:Rust 1.98.1(stable)、PostgreSQL 18、Redis、mkcert(本地证书)。

```bash
# 1. 配置(示例已指向 ~/certs 下的 mkcert 开发证书,见 .public_env)
cp .public_env .env                 # 按需修改 DATABASE_URL / REDIS_URL 等

# 2. 本地开发证书(mkcert 已含 localhost/127.0.0.1 SAN,rootCA 已装入系统信任)
mkcert -install
mkcert -cert-file ~/certs/localhost.pem -key-file ~/certs/localhost-key.pem localhost 127.0.0.1

# 3. 构建 & 启动(启动时自动执行 migration)
cargo run -p ppt-tcp-server-bin
```

> 配置路径支持 `~/` 前缀展开(如 `TLS_CERT_PATH=~/certs/localhost.pem`)。

## 示例 API:注册角色(HTTP)→ 获取角色信息(TCP+TLS protobuf)

完整用户旅程分两段:**低频操作走 HTTP,高频玩法走 TCP 长连接**,两者共享同一套 application 用例与 token。

### 第 1 段:HTTP 注册角色并登录(拿到 opaque token)

```bash
# 注册(201,返回 account_id / player_id)
curl -s -XPOST localhost:8081/register \
  -H 'content-type: application/json' \
  -d '{"username":"quickstart","password":"super-secret","nickname":"快跑选手"}'

# 登录(200,返回 token / expires_in_secs)
curl -s -XPOST localhost:8081/login \
  -H 'content-type: application/json' \
  -d '{"username":"quickstart","password":"super-secret"}'
# => {"token":"...64位hex...","player_id":"...","nickname":"快跑选手","expires_in_secs":604800}
```

### 第 2 段:TCP+TLS 通道获取角色信息

建连后**第一条消息必须是 `Bind{token}`**(opcode `0x0001`),之后即可发 `GetProfile`(opcode `0x0013`)查询服务端权威档案。帧格式:

```
[4B 长度 u32 BE(= opcode+payload 字节数)][2B opcode][protobuf payload]
GetProfile 空消息 → 实际帧:00 00 00 02 | 00 13
```

**可运行的完整客户端**(绑定 → 档案 → 进房 → 聊天)已内置,直接跑:

```bash
cargo run -p ppt-tcp-server-bin --example quickstart_client -- \
  --token <第1段登录拿到的token> --server 127.0.0.1:8080 \
  --root-ca "$(mkcert -CAROOT)/rootCA.pem"
# 输出示例:
#   TLS 握手完成:127.0.0.1:8080
#   BindResult ok=true player=Some("0198...")
#   Profile: ok=true nickname=Some("快跑选手") level=Some(1) exp=Some(0) last_login=Some(...)
#   房间事件:MemberJoined { ... }
```

核心代码即 `crates/server-bin/examples/quickstart_client.rs`:`ClientCodec` 负责消息 ⇄ 帧,`net_kit` 负责帧 ⇄ TLS 流,约 120 行完成全链路。其余 TCP 消息:`Heartbeat(0x0002)`、`JoinRoom(0x0010)`、`LeaveRoom(0x0011)`、`RoomChat(0x0012)`,全部 proto 定义见 `crates/protocol/proto/game.proto`。

## 从哪些文件逐步开发(阅读路线图)

按依赖方向**从内向外**读,每层都能独立理解、独立测试:

| 顺序 | 文件/目录 | 你会看到什么 |
| --- | --- | --- |
| 1 | `crates/domain/src/shared/value.rs` | 值对象与模型校验(用户名/昵称/密码,日志脱敏) |
| 2 | `crates/domain/src/account/aggregate.rs`、`player/`、`session/`、`room/` | 聚合根与状态机(纯业务规则,0 框架依赖) |
| 3 | `crates/domain/src/*/repository.rs` | 仓储**接口**(实现在 infrastructure,依赖倒置) |
| 4 | `crates/application/src/ports.rs` | 对外部技术的端口 trait(token 存储/密码哈希/审计/事件) |
| 5 | `crates/application/src/auth/`、`player/` | 用例编排:注册/登录/档案/成长 |
| 6 | `crates/application/src/room/actor.rs`、`service.rs` | Room Actor(串行无锁)与跨房间门面 |
| 7 | `crates/net-kit/src/codec.rs`、`connection.rs`、`transport.rs` | 帧协议、读写分离、有界背压、Transport 抽象 |
| 8 | `crates/protocol/proto/game.proto` + `src/messages.rs`、`router.rs` | 消息定义与 opcode 路由表 |
| 9 | `crates/infrastructure/src/config.rs`、`persistence/`、`cache/` | 配置注入、SeaORM 仓储、Redis/内存实现 |
| 10 | `crates/gateway/src/tcp/`、`http/` | 入站适配:连接生命周期、鉴权门、限流、HTTP 路由 |
| 11 | `crates/server-bin/src/bootstrap.rs` | 组装根:谁实例化谁、怎么注入、如何优雅停机 |

## 如何做常见调整(对应文件速查)

| 想做什么 | 改哪些文件 |
| --- | --- |
| 调端口/帧上限/心跳/背压队列等参数 | `.env`(模板见 `.public_env`)→ 默认值与校验在 `crates/infrastructure/src/config.rs` |
| **新增一条 TCP 消息** | 见下方 8 步清单(真例:GetProfile) |
| 新增 HTTP 接口 | `crates/gateway/src/http/routes.rs`(路由+DTO)→ 需要新用例时再加 `application` |
| 新增业务用例(登录类) | `crates/application/src/<域>/` 用例 + `dto.rs` + `ports.rs`(如需新端口)→ `gateway` 调用 |
| 新增表/字段 | `migration/src/` 新迁移 → `infrastructure/persistence/entities/` → `converters.rs` → `repositories/` |
| 新增领域规则 | 对应聚合 `crates/domain/src/<聚合>/aggregate.rs` + 内嵌单测 |
| 新增领域事件 | `crates/domain/src/events.rs` 定义 → 发布方在 application → 分发实现在 `infrastructure/src/events.rs` |
| 更换/新增传输(KCP/QUIC) | 实现 `crates/net-kit/src/transport.rs` 的 `Transport` trait,上层不动(PRD 3.4/8.1) |
| 调整限流/慢客户端策略 | `crates/gateway/src/tcp/rate_limit.rs`、`handler.rs` |
| 调整优雅停机行为 | `crates/server-bin/src/bootstrap.rs`(`graceful_teardown`)+ `gateway/src/tcp/server.rs` |
| 换 token/密码实现 | `infrastructure/src/cache/`、`password.rs`(实现 application 端口,业务层零改动) |
| 加指标项 | 各处 `metrics::counter!/gauge!` 调用 + `server-bin/src/observability.rs` |

### 示例:新增一条 TCP 消息(以刚落地的 `GetProfile` 为真例)

1. **定义协议**:`crates/protocol/proto/game.proto` 加 `GetProfileRequest`/`ProfileResponse`(字段号新增不复用,PRD 8.2 🔴);
2. **分配 opcode**:`crates/protocol/src/opcodes.rs`(C2S 用 `0x0013`,S2C 用 `0x8003`);
3. **接入编解码**:`crates/protocol/src/messages.rs` —— `InboundMessage`/`OutboundMessage` 各加枚举分支,`decode_inbound`/`encode_outbound`/`decode_outbound` 各加 match 分支;
4. **客户端侧编解码**:`crates/protocol/src/lib.rs` 的 `ClientCodec` 加对应分支;
5. **(如需新用例)** 在 `crates/application/src/` 写用例;本例复用现成的 `GetPlayerProfile`;
6. **处理器**:`crates/gateway/src/tcp/handlers.rs` 写 `handle_get_profile`(鉴权门已由连接主循环统一把关);
7. **注册路由**:`crates/gateway/src/tcp/router_setup.rs`(单测同步更新 opcode 清单);
8. **验证**:`crates/gateway/tests/e2e.rs` 加端到端步骤 → `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test --workspace`。

## 开发与测试

```bash
cargo fmt --all -- --check                  # 格式
cargo clippy --all-targets -- -D warnings   # lint
cargo test --workspace                      # 单元测试 + TCP/TLS 端到端(无需数据库)
cargo llvm-cov --workspace --summary-only   # 覆盖率
```

* 单元测试全部可离线运行;SeaORM/Redis 具体实现需要真实服务,不在单测中覆盖;
* 端到端测试使用 rcgen 自签证书走真实 TLS 握手(见 `crates/gateway/tests/e2e.rs`);
* 当前覆盖率基线:全仓行覆盖 ≈ 88%,核心 crate(domain/application/protocol/net-kit/gateway-http)≈ 83%–100%。

## 贡献指南

* **必须**遵守 [AGENTS.md](AGENTS.md)(提交格式 `<type>: <中文描述>`、版本锁定 `=`、CI 检查、DDD 分层红线,追加规范写入其第 10 章);
* CI(GitHub Actions)执行 `fmt --check` / `clippy -D warnings` / `build` / `test`,见 `.github/workflows/rust-ci.yml`;
* 提交前请本地跑通上述"开发与测试"命令。

## License

MIT OR Apache-2.0(见 `LICENSE-MIT` / `LICENSE-APACHE`)。
