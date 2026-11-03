<div align="center">
  <img src="docs/assets/logo.png" alt="LongshipX" width="420" />
  <h1>LongshipX</h1>
  <p><b>Longship(长船)· Long-connection(长连接)· Long-lasting(长长久久)</b></p>
</div>

**命名寓意**:Longship 是维京人的长船——木质龙骨历经风浪仍稳固耐航,寓意这套长连接游戏服务器**长长久久、稳定可靠**;"长船"与游戏服务器的立身之本"**长连接**"同名同源;"**X**"则是可自由组合的因子——接入 PostgreSQL、SeaORM、Redis,或将来切换 MySQL、更换传输协议,都只动出站适配器,业务核心分毫不变(得益于洋葱架构的端口 trait,见下文"从哪些文件逐步开发")。

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

## 整体架构图

**单条命令的完整管线**:客户端(TCP+TLS 长连接 / HTTP)→ net-kit → gateway 防线 → protocol/router → application → **domain 内核** ← infrastructure;右侧为错误通道与依赖注入(bootstrap.rs),左侧为加密响应回路。理解这条链路,就知道开发时的每一步该编辑哪个 crate:

![LongshipX 请求处理流程:单条命令的完整管线](docs/assets/flow-3.png)

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
cargo run -p longshipx-server-bin
```

> TLS 证书和私钥路径支持 `~/` 前缀展开(如 `TLS_CERT_PATH=~/certs/localhost.pem`),要求非空 `HOME`;绝对路径与普通相对路径保持原义,不执行 shell 展开。

连接发送队列与房间命令队列容量必须大于 0;`SERVER_CHANNEL_PER_ROOM_SIZE=0` 会在启动配置校验时明确报错,不会延迟至首次建房。

框架自带两个"已完成实现"作参照:HTTP 注册/登录(见 `crates/gateway/src/http/routes.rs`)与 TCP 获取档案命令(见 `crates/gateway/src/tcp/handlers.rs` 的 `handle_get_profile`);可运行客户端示例:`cargo run -p longshipx-server-bin --example quickstart_client -- --token <token> --root-ca "$(mkcert -CAROOT)/rootCA.pem"`。

---

## 开发指南:亲手实现"注册角色(HTTP)"与"获取角色信息(TCP 命令)"

以下按"**拿到一个空白 LongshipX 框架,我要自己把这两个接口做出来**"的视角,给出**逐文件编辑步骤**。每一步都回答三个问题:编辑哪个文件、写什么、为什么放这一层。

### 任务 A:开发"注册角色"HTTP 接口

> HTTP 侧低频 CRUD 走 axum。整个任务只涉及 6 个文件,顺序由依赖方向决定:**先定规则(domain),再定抽象与编排(application),最后接入口(gateway)与组装(server-bin)**。

**第 1 步 — 定义业务规则:`crates/domain/src/account/aggregate.rs`**

领域层是起点,零框架依赖。确认账号聚合有"注册"所需的一切规则(若要新增字段/校验,改这里):

```rust
// 注册工厂:uuid v7 主键、初始状态 Active;密码只收哈希,明文进不了领域层
pub fn register(username: Username, password_hash: PasswordHash, now: DateTime<Utc>) -> Self
```

配套的值对象校验在 `crates/domain/src/shared/value.rs`(`Username::try_new` 长度/字符集、`PlainPassword::try_new` 8~128 字节),注册的所有输入规则都应写在这里并配单测。

**第 2 步 — 声明仓储接口:`crates/domain/src/account/repository.rs`**

用例需要"按用户名查重"和"保存",在领域层声明接口,不关心是谁实现:

```rust
#[async_trait::async_trait]
pub trait AccountRepository: Send + Sync {
    async fn find_by_username(&self, username: &str) -> Result<Option<Account>, RepoError>;
    async fn save(&self, account: &Account) -> Result<(), RepoError>;
}
```

**第 3 步 — 声明技术端口:`crates/application/src/ports.rs`**

密码哈希是"外部能力",应用层只定义端口,argon2 实现由基础设施注入(将来换 bcrypt/云端 KDF,业务零改动——这就是名字里的"X"):

```rust
pub trait PasswordHasher: Send + Sync {
    fn hash(&self, password: &str) -> Result<String, AppError>;
    fn verify(&self, password: &str, hash: &str) -> Result<bool, AppError>;
}
```

**第 4 步 — 定义入参出参:`crates/application/src/dto.rs`**

```rust
pub struct RegisterCommand { pub username: String, pub password: String, pub nickname: String }
pub struct RegisterResult { pub account_id: AccountId, pub player_id: PlayerId, pub nickname: String }
```

**第 5 步 — 编排用例:`crates/application/src/auth/register.rs`**

Command/Query 风格的用例结构体,依赖全部是 trait 对象(`Arc<dyn Trait>`),顺序:校验 → 查重 → 哈希 → 建聚合 → 落库 → 审计:

```rust
pub struct RegisterUseCase { accounts: Arc<dyn AccountRepository>, players: Arc<dyn PlayerRepository>,
                             hasher: Arc<dyn PasswordHasher>, audit: Arc<dyn AuditLogger> }

impl RegisterUseCase {
    pub async fn execute(&self, cmd: RegisterCommand) -> Result<RegisterResult, AppError> {
        let username = Username::try_new(&cmd.username)?;           // 1. 领域校验
        if self.accounts.find_by_username(username.as_str()).await?.is_some() {
            return Err(AppError::Conflict("用户名已被占用".into()));  // 2. 查重
        }
        let hash = PasswordHash::new(self.hasher.hash(&cmd.password)?); // 3. 哈希
        let account = Account::register(username, hash, Utc::now());    // 4. 聚合
        let player = Player::create(account.id(), nickname, Utc::now());
        self.accounts.save(&account).await?;                            // 5. 落库
        self.players.save(&player).await?;
        Ok(RegisterResult { /* ... */ })                                // 6. 回执
    }
}
```

**第 6 步 — 暴露 HTTP 路由:`crates/gateway/src/http/routes.rs`**

axum 处理器只做"JSON ⇄ Command/Result"翻译;错误到状态码的映射在 `crates/gateway/src/http/error.rs`(`ApiError::from(AppError)`),鉴权提取器在 `auth_extractor.rs`:

```rust
async fn register(State(state): State<Arc<HttpState>>, Json(req): Json<RegisterRequest>)
    -> Result<(StatusCode, Json<RegisterResponse>), ApiError>
{
    let result = state.register.execute(RegisterCommand { /* req 映射 */ }).await?;
    Ok((StatusCode::CREATED, Json(RegisterResponse::from(result))))
}
// router() 里挂载:.route("/register", post(register))
```

**第 7 步 — 组装注入:`crates/server-bin/src/bootstrap.rs`**

洋葱架构在组装根闭合:把 infrastructure 的实现塞进用例。`build_services` 中:

```rust
let hasher = Arc::new(Argon2PasswordHasher::new(memory_kb, iterations, parallelism)?);
let register = Arc::new(RegisterUseCase::new(accounts, players, hasher.clone(), audit));
// HttpState { register, .. } → http::routes::router(state)
```

**第 8 步 — 验证**:`cargo run -p longshipx-server-bin` 后 `curl -XPOST localhost:8081/register -d '{...}'`;并补路由层测试(参考 `crates/gateway/tests/http.rs` 的 tower oneshot 写法)。

### 任务 B:开发"获取角色信息"TCP 命令

> TCP 侧走 protobuf 命令。核心是**协议五件套**(protocol crate)→ **用例**(application)→ **处理器与路由**(gateway)→ **端到端验证**。参考实现已内置:`git grep handle_get_profile`。

**第 1 步 — 定义消息:`crates/protocol/proto/game.proto`**

字段号一经发布禁止改作他用(PRD 8.2 🔴),新消息用新字段号并保持 optional:

```protobuf
message GetProfileRequest {}                     // 空入参
message ProfileResponse {
  bool ok = 1;
  optional string player_id = 2;
  optional string nickname = 3;
  optional uint32 level = 4;
  optional uint64 exp = 5;
  optional int64 last_login_at_ms = 6;
  optional string error = 7;
}
```

**第 2 步 — 分配 opcode:`crates/protocol/src/opcodes.rs`**

C2S 用 `0x0nxx` 段、S2C 用 `0x8nxx` 段:

```rust
pub const OP_C2S_GET_PROFILE: u16 = 0x0013;
pub const OP_S2C_PROFILE: u16 = 0x8003;
```

**第 3 步 — 接入编解码:`crates/protocol/src/messages.rs`**

四处 match 各加一个分支:`InboundMessage::GetProfile(...)` / `OutboundMessage::Profile(...)`,以及 `decode_inbound`、`encode_outbound`、`decode_outbound`。

**第 4 步 — 客户端侧编解码:`crates/protocol/src/lib.rs`**

`ClientCodec`(压测工具/测试客户端用)的 `encode` 加对应分支,把 `GetProfile` 编成 `OP_C2S_GET_PROFILE` 帧。

**第 5 步 — (如需新用例)`crates/application/src/player/profile.rs`**

本例是只读查询,直接复用已有 `GetPlayerProfile` 用例;新写法同样是一个结构体 + `execute`:

```rust
pub struct GetPlayerProfile { players: Arc<dyn PlayerRepository> }
impl GetPlayerProfile {
    pub async fn execute(&self, player_id: PlayerId) -> Result<PlayerProfile, AppError> { /* ... */ }
}
```

**第 6 步 — 写处理器:`crates/gateway/src/tcp/handlers.rs`**

鉴权门由连接主循环统一把关(未绑定只放行 Bind),处理器里只管业务;**要给处理器新能力时,加到 `crates/gateway/src/tcp/context.rs` 的 `GatewayDeps`**(本例即注入 `profile` 用例),并在 `server-bin/src/bootstrap.rs` 装配处补一行:

```rust
pub async fn handle_get_profile(ctx: ConnContext, message: InboundMessage)
    -> Result<Option<OutboundMessage>, ProtocolError>
{
    let InboundMessage::GetProfile(_) = message else { /* 路由异构防御 */ };
    let Some(player) = ctx.authed_player() else {
        return Ok(Some(error_notification(ERR_NOT_AUTHENTICATED, "请先完成绑定")));
    };
    match ctx.deps.profile.execute(player.player_id).await {
        Ok(profile) => Ok(Some(OutboundMessage::Profile(pb::ProfileResponse { ok: true, /* ... */ }))),
        Err(err) => Ok(Some(app_error_notification(err))),
    }
}
```

**第 7 步 — 注册路由:`crates/gateway/src/tcp/router_setup.rs`**

```rust
router.route(OP_C2S_GET_PROFILE, handlers::handle_get_profile);
```

**第 8 步 — 端到端验证:`crates/gateway/tests/e2e.rs`**

用 `ClientCodec` 走真实 TLS 加一段"绑定 → 发 GetProfile → 收 Profile"(参考该文件 `bind_heartbeat_and_room_flow_end_to_end`),然后:

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --workspace
```

> 顺序为什么是这样?对照上面的 16 步旅程图:消息先过 **protocol**(第 1~4 步决定"线上长什么样"),再进 **application**(第 5 步决定"业务怎么算"),最后由 **gateway**(第 6~7 步)把两者接起来——依赖方向永远指向内层,这正是改不动 domain 的原因,也是换 MySQL/换传输只动外层的保证。

---

## 从哪些文件逐步开发(阅读路线图)

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
| 新增 TCP 消息 | 上文"任务 B"的 8 步清单 |
| 新增 HTTP 接口 | 上文"任务 A"的 6~7 步(路由 + 组装),用例复用 `application` |
| 新增业务用例(登录类) | `crates/application/src/<域>/` 用例 + `dto.rs` + `ports.rs`(如需新端口)→ `gateway` 调用 |
| 新增表/字段 | `migration/src/` 新迁移 → `infrastructure/persistence/entities/` → `converters.rs` → `repositories/` |
| 新增领域规则 | 对应聚合 `crates/domain/src/<聚合>/aggregate.rs` + 内嵌单测 |
| 新增领域事件 | `crates/domain/src/events.rs` 定义 → 发布方在 application → 分发实现在 `infrastructure/src/events.rs` |
| 更换/新增传输(KCP/QUIC) | 实现 `crates/net-kit/src/transport.rs` 的 `Transport` trait,上层不动(PRD 3.4/8.1) |
| 切换数据库(如 MySQL) | `infrastructure` 仓储实现 + `sea-orm` feature(`sqlx-mysql`)+ `.env` 连接串;domain/application 零改动 |
| 调整限流/慢客户端策略 | `crates/gateway/src/tcp/rate_limit.rs`、`handler.rs` |
| 调整优雅停机行为 | `crates/server-bin/src/bootstrap.rs`(`graceful_teardown`)+ `gateway/src/tcp/server.rs` |
| HTTP 入口 TLS/跳转策略 | `crates/gateway/src/http/server.rs`(HTTPS 终结 + 308 跳转)+ 配置项 `SERVER_HTTP_REDIRECT_ADDR` |
| 换 token/密码实现 | `infrastructure/src/cache/`、`password.rs`(实现 application 端口,业务层零改动) |
| 加指标项 | 各处 `metrics::counter!/gauge!` 调用 + `server-bin/src/observability.rs` |

## 开发与测试

`RoomService::leave/leave_all` 等待有界命令队列容量及 Actor 离房完成回执,返回成功时成员清理已执行;最后成员离开后立即移除房间注册项,不依赖后续请求触发惰性回收。`RoomCommand::Leave` 新增完成回执字段。

TCP 绑定后每次入站业务请求及房间广播都会重新校验原会话 token;撤销、过期、存储错误或校验超过心跳超时均关闭连接,不再永久信任缓存身份。校验前已接收的请求/已入发送队列的数据不会撤回;无流量连接最迟在心跳超时清理。令牌仅私有保存,不输出到 Debug。

TCP 接入在 TLS 前占用全局连接名额与单 IP 未鉴权配额;`SERVER_UNAUTH_TIMEOUT_SECS` 是从 TCP 准入到 Bind 完成的一次性期限,包含 TLS 握手和存储查询,无效 Bind 不续期。`SERVER_ACCEPT_CONCURRENCY` 限制并行 TLS 握手,满时拒绝额外接入;错误/取消/超时自动归还未鉴权名额。
accept worker 持续回收已经结束的连接 task,记录异常退出,历史连接数量不再累积 JoinSet 分配;连接并发上限不变。
单 TCP 连接同时仅在一个房间:加入后再次创建或加入其他房间返回冲突,必须先显式离开;重复加入当前房间保持幂等且不重复广播。拒绝切换不会丢失当前成员身份。

登录签发 token 时由存储原子替换该玩家的旧会话,并发登录完成后最多一个 token 有效。替换失败不返回登录成功;网络超时或取消可能发生在提交之后,此时重试会再次替换会话。Redis 使用单实例 Lua 脚本,不支持跨槽 Redis Cluster。

真实 Redis 回归使用独立测试库:`LONGSHIPX_TEST_REDIS_URL=redis://127.0.0.1:6379/15 cargo test -p longshipx-infrastructure --test session_tokens -- --ignored`;默认测试不连接真实服务。
注册使用 `RegistrationStore` 原子创建账号与玩家档案,数据库错误或提交前取消会回滚整个事务;并发同名注册返回冲突。`RegisterUseCase::new` 第二个参数改为注册存储,生产使用 `SeaRegistrationStore`,内存装置使用 `InMemoryRegistrationStore`。提交结果因连接中断未知时,重试仍可能返回用户名冲突,不会新增半套数据。

真实 PostgreSQL 回归须指定专用测试库:`LONGSHIPX_TEST_PG_URL=postgres://postgres@127.0.0.1:5432/longshipx_test cargo test -p longshipx-infrastructure --test registration -- --ignored`。测试会执行迁移并临时添加故障约束,禁止指向业务数据库。
经验更新通过 `PlayerRepository::gain_exp` 原子读取并提交;登录使用 `record_login` 只更新登录时间,不再保存陈旧玩家快照。外部仓储实现须实现这两个方法。升级事件仅在提交后发布;发布失败返回 `AppError::PostCommit`,调用方不得重试整个经验奖励。未引入 outbox,发布失败仍需要业务侧补发。

真实 PostgreSQL 回归使用专用库:`LONGSHIPX_TEST_PG_URL=postgres://postgres@127.0.0.1:5432/longshipx_test cargo test -p longshipx-infrastructure --test player_updates -- --ignored`;测试执行迁移及故障约束,禁止连接业务库。

注册审计的 `detail.username` 保存与账号一致的归一化用户名,`player_id` 单独关联玩家;审计不记录密码或令牌。
HTTP 308 跳转保留请求主机、路径和查询,将明文端口替换为实际 HTTPS 监听端口(443 省略);非法或缺失 Host 返回 400。`redirect_router` / `serve_redirect` 调用方须传入 HTTPS 端口。


```bash
cargo fmt --all -- --check                  # 格式
cargo clippy --all-targets -- -D warnings   # lint
cargo test --workspace                      # 单元测试 + TCP/TLS 端到端(无需数据库)
cargo llvm-cov --workspace --summary-only   # 覆盖率
```

* 单元测试全部可离线运行;SeaORM/Redis 具体实现需要真实服务,不在单测中覆盖;
* 端到端测试使用 rcgen 自签证书走真实 TLS 握手(见 `crates/gateway/tests/e2e.rs`);
* 当前覆盖率基线:全仓行覆盖 ≈ 88%,核心 crate(domain/application/protocol/net-kit/gateway-http)≈ 83%–100%。

## 性能基准

基于 criterion 的基准套件覆盖框架全部热路径,全部可离线运行(真实 loopback TCP/TLS + 内存版基础设施):

| 位置 | 内容 | 典型结果(M3 级单核) |
| --- | --- | --- |
| `crates/net-kit/benches/frame_codec.rs` | 长度前缀帧编解码(0B–64KiB 载荷) | 空载荷 ~26ns,64KiB ~1µs |
| `crates/net-kit/benches/connection.rs` | TCP/TLS 小帧往返、TLS 1.3 握手、64KiB×32 大帧吞吐 | 往返 ~54µs,握手 ~420µs,TLS 吞吐 ~760MiB/s |
| `crates/protocol/benches/codec.rs` | 消息编解码往返、路由表分发 | C2S 全链路 ~70ns,分发 ~153ns |
| `crates/gateway/benches/http.rs` | axum 链路:healthz / Bearer 鉴权 / 登录(含 argon2) | healthz ~1.2µs,argon2 登录 ~6ms |
| `crates/gateway/benches/tcp_room.rs` | 端到端:建连绑定 / 心跳 / 单房间广播 8~256 人 / 多连接并发心跳 8~256 条 | 绑定 ~866µs,心跳 ~38µs,256 人广播 ~1.23ms(~209K 事件/s) |
| `crates/gateway/benches/footprint.rs` | 连接内存足迹:64/128/256 常驻 TLS 连接的 RSS 与活跃堆分配 | ~28KiB 堆 / ~37KiB RSS 每连接(含两端) |

```bash
cargo bench                                            # 全部基准(约 5–8 分钟)
cargo bench -p longshipx-net-kit --bench connection    # 单个套件
cargo bench -p longshipx-gateway --bench tcp_room -- \
    --warm-up-time 0.5 --measurement-time 2            # 自定义时长
cargo bench -p longshipx-gateway --bench footprint     # 内存足迹报告(非 criterion)
```

* 每次迭代都是真实网络 IO(`127.0.0.1`)与真实 TLS 握手,数字包含框架全部每连接开销;
* 基准公共装置 `benches/support/`:进程内提升 fd 上限(macOS 默认 256 不够)、建连固定并发 64(< macOS `kern.ipc.somaxconn=128`)、看门狗超时、短命连接 `SO_LINGER=0` 回收端口;服务端启用并发握手管线(`SERVER_ACCEPT_CONCURRENCY`)即时排空内核 accept 队列——否则 macOS 等系统 backlog 限制会在突发建连时丢弃/重置连接;
* 结果保存在 `target/criterion/`,打开 `report/index.html` 可看历史对比曲线;
* 基准曾直接暴露过"连接断开后信号量名额不归还"的缺陷(见 e2e 回归测试 `connection_permits_are_released_after_disconnect`)、"建连闸门 permit 随连接持有导致基准挂死"的缺陷与"心跳客户端装配段占住闸门"的死锁——改网络层后先跑基准与 e2e。

## 贡献指南

* **必须**遵守 [AGENTS.md](AGENTS.md)(提交格式 `<type>: <中文描述>`、版本锁定 `=`、CI 检查、DDD 分层红线,追加规范写入其第 10 章);
* CI(GitHub Actions)执行 `fmt --check` / `clippy -D warnings` / `build` / `test`,见 `.github/workflows/rust-ci.yml`;
* 提交前请本地跑通上述"开发与测试"命令。

## 房间队列过载

Actor 事件队列与 socket 出站队列都采用满即断开策略（包括聊天），不静默丢失关键回执后继续会话。
`RoomEventSink` 持有独立且持久的失败信号；网关观察该信号并关闭写任务、清理连接。
应用层调用者应保留 sink 并观察 `failed()`，原 `mpsc::Sender<RoomEvent>` 可通过 `into()` 适配。
`OutboundSender` 一旦溢出即永久不可写，即使旧帧随后被消费也不会恢复。
## 密码计算并发

注册/登录的 Argon2 运算在阻塞线程池运行，不占用异步调度线程。
`APP_PASSWORD_MAX_CONCURRENCY` 默认 2，限制同一共享哈希器中 hash/verify 的合计并发；
超限立即返回 HTTP 503，不累积等待队列。请求取消不会提前释放仍在计算的名额。
`PasswordHasher` 端口改为异步，`Argon2PasswordHasher::new` 第四参数为并发上限。

## 连接停机与排空

停机先停止 accept，连接在正在执行的请求结束后发出维护通知并离房，再有限排空写队列。
`APP_SHUTDOWN_TIMEOUT_SECS` 控制处理器的协作退出期限；超时才取消处理器，RAII 清理其子任务、注册表与鉴权配额，并补发房间清理。
`SERVER_WRITE_DRAIN_TIMEOUT_MS` 默认 1000，限制发送端全部关闭后 socket 写任务的排空时间；0 表示不等待排空。
`TcpGatewayConfig` 新增 `shutdown_timeout`/`write_drain_timeout`，`ConnectionConfig` 新增 `write_drain_timeout`，`AcceptLoopConfig` 新增 `shutdown_timeout`。
强制期限不保证尚未提交的业务完成；卡住的自定义房间事件发布器会在清理超时时记录错误，不能视为可靠投递。

## License

MIT OR Apache-2.0(见 `LICENSE-MIT` / `LICENSE-APACHE`)。

---

<div align="center">
  <sub>Built with ❤️ by the LongshipX team</sub>
</div>
