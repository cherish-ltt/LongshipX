# Rust 长连接游戏服务器后端 PRD

**文档版本**：v1.0　|　**编写日期**：2026-09-04　|　**状态**：待评审（Draft for Review）
**实现语言**：Rust（2024 edition，stable 工具链）
**核心技术栈**：tokio · tokio-rustls · SeaORM 1.1.x · PostgreSQL 18 · Redis · protobuf(prost)

---

## 0. 阅读说明

本文标记三类提示，贯穿全文，第 16 节有汇总表方便速查：

- 🔴 **强制项**：违反会直接导致安全 / 数据 / 稳定性事故，不允许跳过，代码评审时应作为 checklist。
- ⚠️ **重要提醒**：工程中最容易踩的坑，团队新人尤其要看。
- 💡 **可选/后续项**：当前阶段可以不做，但必须预留扩展点，不能做死。

### 0.1 前提假设（请核对，如不符请调整第 3、9、18 节）

需求未指定具体玩法类型，本 PRD 按以下假设编写：

1. 游戏偏"中频实时"（回合制 / MMORPG / 卡牌 / 社交对战 / 轻量竞技），**不是**每秒需要 20 次以上位移同步的强竞技 FPS/MOBA/吃鸡。若是后者，第 3.4 节和第 18 节的 KCP/QUIC 接入方案权重要提前。
2. 第一阶段单机单进程部署，不要求跨地域部署，不要求真正 0 停机热更新。
3. 客户端大概率是 Unity / Cocos / Unreal 等非 Rust 客户端 → 协议序列化选跨语言方案（Protobuf），不是 Rust 专属的 bincode。
4. 团队具备中级以上 Rust async 经验；本文不讲 Rust 语法基础，只讲工程决策。

---

## 1. 背景与目标

构建一个 Rust 编写的长连接网络游戏服务器后端，作为单体部署单元（monolith），内部按 DDD + 洋葱架构（Onion Architecture）组织多个 crate，以：

- 支撑尽可能高的单机并发连接数与消息 QPS；
- 保证传输安全（加密、防篡改、防重放）；
- 保证代码可维护、边界清晰，为未来玩家规模增长后的分布式/微服务拆分留好接口，**不需要推倒重来**。

### 1.1 非目标（Out of Scope）

- 不在本阶段设计跨服/跨地域架构（见第 18 节，作为演进指南而非当前实现）。
- 不设计具体玩法数值系统（战斗公式、经济系统等）——domain 层给出的是骨架和示例，需按实际玩法扩展。
- 不包含客户端（Unity/Cocos 等）实现，只约定服务端对外协议。

---

## 2. 名词与范围界定

| 术语 | 含义 |
| --- | --- |
| 长连接 | 客户端与服务端建立后长期保持、双向主动推送的连接（区别于短连接 HTTP 请求-响应） |
| Room/Scene | 一局对局、一个地图实例或一个聊天室等"共享上下文"的抽象，本文统称 Room |
| 洋葱架构 | 依赖方向永远指向中心（Domain），外层可替换、内层不知道外层存在 |
| 网关 (Gateway) | 负责协议解析、连接管理、鉴权、把网络消息转成应用层用例调用的入站适配器 |
| 用例 (Use Case) | 应用层对一次业务操作的编排单元，如"登录"、"加入房间"、"结算战斗" |

---

## 3. 网络传输协议选型（🔴 结论已定，作为全文基础）

### 3.1 六种方案对比

| 方案 | 加密 | 队头阻塞 | 单机连接数表现 | Rust 生态成熟度 | 一句话结论 |
| --- | --- | --- | --- | --- | --- |
| TCP（裸） | 无 | 有 | 好 | 极高 | 无加密，公网游戏禁止使用 |
| **TCP + TLS1.3（本 PRD 采用）** | TLS1.3 | 有 | 好 | 极高（tokio-rustls） | 稳、快、安全，通用长连接首选 |
| wss | TLS1.3 | 有（仍是TCP） | 好，多一层帧开销 | 高（tokio-tungstenite） | 仅浏览器/H5 客户端需要 |
| nginx(wss)+tcp | 网关侧TLS | 有 | 好，多一跳延迟 | 高，运维复杂度上升 | 网页接入网关方案，非核心协议 |
| KCP | 无（需自建） | 无（ARQ快速重传） | 弱网下更优，带宽利用率低于TCP | 中，弱于TCP生态 | 强实时对抗类游戏可选项 |
| QUIC | 强制TLS1.3 | 无（多路复用） | 优秀，支持连接迁移 | 中高，"自定义游戏协议"场景验证偏少 | 面向未来，架构预留而非首发 |

### 3.2 结论与理由

**核心长连接协议：TCP + TLS 1.3**，实现方式 `tokio`（异步运行时）+ `tokio-rustls` + `rustls`（纯 Rust TLS 实现，不用 OpenSSL，避免 C 依赖与相关 CVE 历史包袱）。

理由对应四个约束逐一说明：

1. **最靠谱稳定**：TCP 协议栈本身经过数十年验证；tokio 对海量长连接（C10K/C100K）有成熟解法（epoll 驱动、多线程 reactor）；简单架构 = 更少边界情况 = 更少生产事故。KCP/QUIC 的丢包处理、拥塞控制调参、UDP 穿透防火墙等问题，本质上是"用更高实现复杂度换取更低延迟"，在没有强实时诉求时是不必要的风险敞口。
2. **单机高 QPS**：游戏消息通常是"小包高频"而非"大流量吞吐"，TCP+TLS 与 UDP 方案在这种模式下没有本质吞吐差距；瓶颈通常在业务逻辑 CPU 与序列化开销，而不是传输层本身。`rustls` 走硬件 AES-NI 加速，加解密开销可控。
3. **安全**：TLS1.3 强制前向保密，1-RTT 握手（比 TLS1.2 少一次往返），可选 0-RTT 恢复；不存在"忘记开加密"的裸奔风险——本 PRD 会在部署清单里把"禁止无 TLS 监听"设为强制项（见第 13 节）。
4. **快**：对绝大多数非强竞技类游戏，TCP+TLS 的延迟增量（相比 UDP 方案）在可接受范围内；而 TLS1.3 的握手/加密开销经过多年优化已经很小。

### 3.3 为什么不是另外几个方案

- **KCP**：弱网下延迟表现确实优于 TCP，是国内手游对战类的主流选择。但① 默认不加密，需要自己叠加安全层，工程上容易出纰漏；② Rust 生态的 KCP 实现远不如 tokio-rustls 成熟，缺少大规模生产验证；③ 如果不是强对抗 FPS/MOBA，这份复杂度大概率是不必要的技术负债。**结论：不在 MVP 阶段引入，作为第 18 节"按需接入的可选传输"。**
- **QUIC**：技术上更先进（内建 TLS1.3、多路复用无队头阻塞、连接迁移对移动端切网很友好），`quinn` 这个 Rust 实现也足够成熟。但作为"自定义游戏协议"而非"HTTP/3网页流量"使用时，生产验证案例仍偏少；部分网络环境（企业网络、部分移动运营商）对 UDP 限流是真实存在的风险。稳定性是本 PRD 排第一位的约束，因此**架构上要预留 Transport trait 让 QUIC 可以后接入（见 4.3、8.1），但不作为首发方案**。
- **wss / nginx(wss)+tcp**：本质是"浏览器/H5 客户端的接入方式"，不是更优的传输层，需不需要取决于要不要支持网页端客户端。若需要，推荐做法见 8.6（不强制引入 nginx，可用 `tokio-tungstenite` 在同一 Rust 进程内直接监听 WSS，减少一跳延迟和运维复杂度；只有当未来要多实例做负载均衡/证书统一管理时，nginx/网关层才真正划算——这一点在第 18 节会再展开）。

### 3.4 何时应该重新评估本结论 ⚠️

如果上线后出现以下任一情况，应重新评估引入 KCP 或 QUIC 作为"某些玩法/某些客户端"的**并行**传输方案（而不是替换 TCP+TLS）：

- 玩法迭代出强实时对抗内容（高频位移同步），玩家在弱网/移动网络下反馈"卡顿""闪现"明显；
- 移动端玩家占比高、且经常在 WiFi/4G/5G 间切换，QUIC 的连接迁移特性会显著改善体验。

因为 4.3 节把网络层封装成独立 crate（`net-kit`）并通过 trait 抽象，**这个决策变化不会影响 domain / application 层的任何代码**。

---

## 4. 总体架构：DDD + 洋葱架构

### 4.1 分层定义

自内向外四层，依赖方向永远指向内层（依赖倒置原则 DIP 的核心体现）：

| 层 | 职责 | 依赖谁 | 对应 crate |
| --- | --- | --- | --- |
| **领域层 Domain** | 业务实体、值对象、聚合根、领域事件、Repository trait **定义**（仅接口） | 不依赖任何其他层 | `domain` |
| **应用层 Application** | 用例编排（Command/Query Handler）、事务边界、DTO | Domain | `application` |
| **接口层 Interface**（入站适配器） | 网络协议解析、连接管理、鉴权、把外部请求翻译成用例调用 | Application（不直接依赖 Domain） | `gateway`、`protocol`、`net-kit` |
| **基础设施层 Infrastructure**（出站适配器） | Repository trait 的具体实现、数据库/缓存访问 | Domain + Application（实现其定义的 trait） | `infrastructure` |

🔴 **依赖方向铁律**：Domain 不知道 SeaORM、Redis、tokio、TCP 的存在。Infrastructure 依赖 Domain 去实现 Domain 定义的 trait，而不是反过来。这是"以后能不能优雅拆分布式服务"的根本前提——见第 18.4 节。

⚠️ **常见错误（代码评审必查项）**：不要在 Domain 层的结构体上直接打 `#[derive(sea_orm::DeriveEntityModel)]` 或 protobuf 生成的 trait；不要在 `application` 里 `use infrastructure::...` 的具体类型（只能依赖 `domain` 定义的 trait）。这是洋葱架构在项目压力下最容易被"图省事"破坏的两个地方，一旦破坏，后续拆分布式服务时 domain/application 代码就不再是"不用动"，而是要跟着重写。

### 4.2 Workspace / Crate 拆分方案与理由

采用 **Cargo workspace + 多 crate**，但仍编译为**一个可执行文件**（单体部署单元，不是微服务）。做此决定的理由：

1. Cargo 的 crate 边界是**编译期强制**的——模块（mod）边界谁都能"顺手"越界 `use`，crate 边界如果没有把对应类型 `pub use` 出来就是编译错误。用 crate 拆分层，等于让 Rust 编译器帮你守住洋葱架构的依赖方向，而不是仅靠代码评审人肉守。
2. 用户需求中特别提到"网络框架代码是否要与业务分离，由你决定"——**结论：分离**，独立出 `net-kit` crate，纯技术、零业务依赖，只做"连接生命周期管理 + 编解码 + 背压"，不知道"玩家""房间"这些概念。这样做的收益：① 未来可能复用到其他服务（聊天服务、大厅服务）；② 传输协议从 TCP+TLS 换/加 KCP、QUIC 时，改动范围严格限定在这一个 crate；③ 强制网络层保持通用，防止游戏业务逻辑意外耦合进网络细节。

```
game-server/                       ← Cargo workspace 根
├── Cargo.toml
├── crates/
│   ├── domain/                    领域层：零框架依赖，最核心
│   ├── application/                应用层：用例编排，依赖 domain
│   ├── net-kit/                    通用网络框架：零业务依赖，可独立复用
│   ├── protocol/                   游戏专属消息协议：依赖 net-kit 的 Codec trait
│   ├── infrastructure/              基础设施：SeaORM/Redis 实现，依赖 domain+application
│   ├── gateway/                     接口层：依赖 net-kit+protocol+application
│   └── server-bin/                  组装根：唯一的可执行文件
└── migration/                        SeaORM 官方推荐的独立 migration crate
```

依赖关系（编译期强制）：

```
server-bin (组装根 / composition root)
 ├─ gateway        → net-kit, protocol, application
 ├─ infrastructure  → domain, application（实现其 trait）
 ├─ application    → domain
 ├─ protocol       → net-kit
 ├─ net-kit        → （无内部依赖，可独立复用/发布）
 └─ domain         → （无内部依赖，最核心）
```

💡 domain 和 net-kit 是两个"零依赖核心"：一个是业务核心，一个是技术核心，彼此互不知道对方存在——这正是"网络框架"与"游戏业务"能被干净拆开的原因。

### 4.3 目录结构（完整）

```
game-server/
├── Cargo.toml
├── crates/
│   ├── domain/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── account/          # 账号聚合
│   │       ├── player/           # 玩家角色聚合
│   │       ├── session/          # 在线会话聚合
│   │       ├── room/             # 房间/对局聚合
│   │       ├── shared/           # 值对象、领域错误类型
│   │       └── events.rs         # 领域事件定义
│   ├── application/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── auth/             # 登录/鉴权用例
│   │       ├── player/           # 玩家相关用例
│   │       ├── room/             # 房间相关用例
│   │       ├── dto.rs            # 输入输出 DTO
│   │       └── unit_of_work.rs   # 事务边界抽象（trait）
│   ├── net-kit/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── listener.rs       # accept 循环、TLS 握手
│   │       ├── connection.rs     # 连接生命周期、读写分离
│   │       ├── codec.rs          # Codec trait（长度前缀帧）
│   │       ├── backpressure.rs   # 有界发送队列、慢客户端处理
│   │       └── transport.rs      # Transport trait（为 KCP/QUIC 预留）
│   ├── protocol/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── proto/            # .proto 源文件
│   │       ├── generated/        # prost 生成代码
│   │       └── router.rs         # 消息 ID → handler 路由表
│   ├── infrastructure/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── persistence/
│   │       │   ├── entities/     # SeaORM Entity（注意：不是 domain 实体！）
│   │       │   └── repositories/ # domain::XxxRepository 的具体实现
│   │       ├── cache/            # Redis 封装
│   │       └── config.rs         # 配置加载、密钥管理
│   ├── gateway/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── tcp/              # TCP+TLS 长连接入口
│   │       ├── http/             # HTTP 入口（登录/账号等低频操作，axum）
│   │       └── middleware/       # 鉴权、限流中间件
│   └── server-bin/
│       └── src/
│           └── main.rs           # 组装 DI、加载配置、启动监听、优雅停机
└── migration/
    └── src/
        └── m20260101_000001_xxx.rs
```

---

## 5. 领域层设计（Domain）

🔴 **红线**：本层禁止出现 `tokio`、`sea_orm`、`redis`、`prost` 等任何框架/技术依赖，`Cargo.toml` 里除了 `uuid`、`chrono`、`thiserror`（错误类型）、可选的 `serde`（仅做序列化标记，见下方说明）外不应有其他依赖。这是本 PRD 里最容易被"图省事"破坏、也是最需要在 code review 里卡住的一条。

⚠️ **serde 的例外处理**：domain 结构体可以 `#[derive(Serialize, Deserialize)]`（serde 是近乎标准库级别的、非侵入性依赖），但**不能**派生 SeaORM 的 `DeriveEntityModel` 或 protobuf 专属 trait——这两者必须在 `infrastructure`/`protocol` 里定义独立的映射结构体（Model ⇄ Domain、Proto ⇄ Domain），通过显式 `From`/`TryFrom` 转换。多写一点样板代码，换来的是任何一层的技术选型变化都不会传染到 domain。

### 5.1 核心聚合根（示例骨架，按实际玩法扩展）

```rust
// crates/domain/src/account/mod.rs
pub struct AccountId(pub Uuid);

pub struct Account {
    id: AccountId,
    username: String,
    password_hash: String,   // 🔴 argon2id 哈希，绝不允许明文或弱哈希（md5/sha1）
    status: AccountStatus,
    created_at: DateTime<Utc>,
}

pub enum AccountStatus {
    Active,
    Banned { reason: String, until: Option<DateTime<Utc>> },
    Suspended,
}

// crates/domain/src/player/mod.rs
pub struct PlayerId(pub Uuid);

pub struct Player {
    id: PlayerId,
    account_id: AccountId,
    nickname: String,
    level: u32,
    exp: u64,
    last_login_at: Option<DateTime<Utc>>,
    // ⚠️ 按实际玩法扩展：货币、背包、装备等建议拆成独立聚合（如 Inventory），
    // 不要把 Player 做成"上帝对象"，否则并发写冲突会集中在一个聚合上。
}

// crates/domain/src/session/mod.rs
pub struct SessionId(pub Uuid);

pub struct Session {
    id: SessionId,
    player_id: Option<PlayerId>,  // None = 已连接但未完成鉴权绑定
    connected_at: DateTime<Utc>,
    last_heartbeat_at: DateTime<Utc>,
}

// crates/domain/src/room/mod.rs
pub struct RoomId(pub Uuid);

pub struct Room {
    id: RoomId,
    members: Vec<PlayerId>,
    max_players: u32,
    state: RoomState,   // 按玩法定义：Waiting / InProgress / Settling / Closed
}
```

### 5.2 Repository trait 定义（仅接口，实现在 infrastructure）

```rust
// crates/domain/src/player/repository.rs
#[async_trait::async_trait]
pub trait PlayerRepository: Send + Sync {
    async fn find_by_id(&self, id: PlayerId) -> Result<Option<Player>, RepoError>;
    async fn save(&self, player: &Player) -> Result<(), RepoError>;
}
```

### 5.3 领域事件

聚合根的关键状态变化应产生领域事件（`PlayerLeveledUp`、`RoomClosed` 等），当前阶段可以只是"进程内 event bus"（一个简单的 `tokio::sync::broadcast` 或直接同步回调），💡 但事件的**定义**放在 domain 层，事件的**分发实现**放在 infrastructure——这样未来要接消息队列（NATS/Kafka）广播跨服事件时，只需要替换 infrastructure 里的事件分发器实现，domain/application 不用改一行。

---

## 6. 应用层设计（Application）

### 6.1 用例组织方式

按 Command（改变状态）/ Query（只读）分离组织，每个用例是一个独立的小结构体，依赖 domain 定义的 trait（通过泛型或 `Arc<dyn Trait>` 注入）：

```rust
// crates/application/src/auth/login.rs
pub struct LoginCommand {
    pub username: String,
    pub password: String,
}

pub struct LoginResult {
    pub player_id: PlayerId,
    pub session_token: String,
}

pub struct LoginUseCase<A: AccountRepository, S: SessionTokenStore> {
    account_repo: A,
    token_store: S,
}

impl<A: AccountRepository, S: SessionTokenStore> LoginUseCase<A, S> {
    pub async fn execute(&self, cmd: LoginCommand) -> Result<LoginResult, AppError> {
        // 1. 查账号  2. argon2::verify 校验密码  3. 生成随机 session token 并写入 token_store
        // 4. 更新 last_login_at（通过 PlayerRepository）
        // 5. 返回 LoginResult
    }
}
```

⚠️ **鉴权 token 设计**：推荐**不透明 token（随机串）+ Redis 存储**，而不是 JWT。理由：单体阶段 JWT 的"无状态自验证"优势体现不出来，反而带来"如何即时吊销"的额外麻烦（要额外做黑名单，等于绕回有状态方案）；opaque token + Redis 天然支持"立即踢下线"。JWT 更适合未来多服务间调用时的身份透传（见第 18 节），到时候可以在 gateway 的边界做一次转换，不影响 domain/application。

### 6.2 事务边界（Unit of Work）

跨多个聚合的写操作需要事务一致性时，在 application 层定义 `UnitOfWork` trait（由 infrastructure 用 SeaORM 的 `DatabaseTransaction` 实现），禁止在 application 里直接感知"这是一个数据库事务"，只感知"这是一个原子操作单元"。

---

## 7. 基础设施层：PostgreSQL 18 + SeaORM + Redis

### 7.1 PostgreSQL 18 选型说明

PostgreSQL 18 于 2025-09-25 正式 GA，是当前的稳定生产版本（PostgreSQL 19 截至本文档编写时仍处于 Beta 阶段，尚未 GA，**不建议在生产环境使用**）。PG18 对本项目直接有用的特性：

- **`uuidv7()` 内置函数**：生成时间有序的 UUID。💡 **强烈建议主键统一用 `uuidv7()` 而非 `gen_random_uuid()`（即 UUIDv4）**——随机 UUID 作为 B-tree 主键会导致索引页随机写入、局部性差，高频写入场景（玩家状态频繁落库）下这是实打实的性能损耗；UUIDv7 时间有序，插入局部性接近自增整型，同时保留了 UUID 不易被猜测枚举的安全属性。
- **异步 I/O（AIO）子系统**：默认开启，顺序扫描/位图扫描/VACUUM 最高有 3 倍读取性能提升，对后续做数据分析、大表维护有直接收益。
- **虚拗生成列（Virtual Generated Columns）默认化**：查询时计算而非存储，适合"衍生字段"（如按等级计算的战力值）不占用存储、不产生写放大。
- **多列 B-tree 索引 Skip Scan**：对"部分条件命中联合索引前导列"的查询有性能提升，实际建表时可以少建几个单列索引。
- **OAuth 2.0 身份验证支持**：如果后续需要数据库层面对接企业 SSO，PG18 原生支持，不需要额外插件。

### 7.2 SeaORM 版本选择 ⚠️

截至本文档编写时，SeaORM **2.0 仍处于 Release Candidate 阶段**（未正式 GA），生产环境**建议锁定使用 1.1.x 稳定版**。1.1.x 已经是"生产就绪、9.5k+ star、月下载量百万级"的成熟状态，风险最低，符合本 PRD"最靠谱稳定"的第一诉求。💡 待 SeaORM 2.0 正式 GA（预计会带来 dense entity 格式、嵌套持久化、自动 schema 同步等改进）后再评估升级，升级前务必先在测试环境跑一轮回归测试。

二编：seaorm 2.0 已经发布，优先使用 seaorm2.0，且在需要 seaorm 生成 entity 时检查seaorm-cli 是否已经安装。

### 7.3 Schema 草案（核心表，示例）

```sql
-- 账号表
CREATE TABLE accounts (
    id            UUID PRIMARY KEY DEFAULT uuidv7(),
    username      CITEXT UNIQUE NOT NULL,   -- CITEXT：大小写不敏感，避免"Tom"/"tom"重复注册
    password_hash TEXT NOT NULL,
    status        SMALLINT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 玩家表
CREATE TABLE players (
    id            UUID PRIMARY KEY DEFAULT uuidv7(),
    account_id    UUID NOT NULL REFERENCES accounts(id),
    nickname      TEXT NOT NULL,
    level         INT NOT NULL DEFAULT 1,
    exp           BIGINT NOT NULL DEFAULT 0,
    last_login_at TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_players_account_id ON players(account_id);

-- 操作审计表（🔴 强烈建议从第一天就有，排查线上问题/申诉纠纷依赖它）
CREATE TABLE audit_logs (
    id         UUID PRIMARY KEY DEFAULT uuidv7(),
    player_id  UUID REFERENCES players(id),
    action     TEXT NOT NULL,
    detail     JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_audit_logs_player_id_created_at ON audit_logs(player_id, created_at DESC);
```

⚠️ 以上仅为骨架示例，具体货币/背包/装备等表需按实际玩法设计；但**审计表、账号表、玩家基础表建议原样保留**，是后续任何玩法都用得到的基础设施。

### 7.4 SeaORM 落地约定

- 使用官方推荐的独立 `migration` crate 管理 schema 变更，🔴 **禁止手工在生产库上执行未纳入 migration 历史的 DDL**，否则环境之间 schema 会漂移。
- ⚠️ **SeaORM Entity ≠ Domain 实体**：`infrastructure/persistence/entities/player.rs` 里的 `Model`/`ActiveModel` 只在 infrastructure 内部可见，repository 实现里做 `Model ⇄ domain::Player` 的显式转换，绝不允许把 SeaORM 生成的类型泄漏到 application 层。
- 连接池：SeaORM 内置基于 sqlx 的连接池，需要显式设置 `max_connections`（建议初始值 = `min(数据库 max_connections * 0.6, 预估并发写请求数 * 2)`，具体数值需压测校准，见第 11 节）、`connect_timeout`、`idle_timeout`，🔴 不设置上限的连接池在流量突增时会打满数据库连接数，拖垮整个数据库（包括其他服务）。

### 7.5 Redis 落地约定

用途划分（🔴 单机阶段避免"能用内存解决的事，非要走一次 Redis 网络往返"）：

| 用途 | 是否现阶段用 Redis | 说明 |
| --- | --- | --- |
| 鉴权 token 存储 | ✅ | `token:{token} → player_id`，需要跨重启存活、需要主动吊销能力 |
| 心跳/在线状态 | 💡 单机阶段可选 | 单实例场景进程内 `HashMap` 更快；仅当需要给运维/监控提供跨进程可见性时才落 Redis |
| 限流计数 | ⚠️ 单机阶段不建议 | 单实例内存令牌桶（如 `governor` crate）比 Redis 往返快得多，Redis 限流是**多实例场景**才需要的方案，见第 18 节 |
| 幂等/防重放 nonce | ✅ | 需要重启后依然有效的短期状态，适合 Redis + TTL |
| 发布订阅（跨实例广播） | 💡 预留不启用 | 单体阶段没有"跨实例"这个概念，接口设计成"可插拔的事件分发器"即可，不需要真的接 Redis Pub/Sub |
| 排行榜 | ✅（如玩法需要） | Sorted Set 天然适合，比在 PG 里 `ORDER BY` 大表要快 |

Redis 客户端选型：使用官方文档推荐的 `redis` crate（redis-rs，社区最广泛使用），搭配 `MultiplexedConnection`（异步、可 clone、免去手动连接池管理，多个 tokio task 可以共享同一个连接并发发命令）。💡 如果团队更看重 API 易用性（免 `&mut self`、原生 RedisJSON 支持），可以用 `fred` 作为替代，两者都能满足本项目需求，选型可由团队偏好决定，不是架构级别的决策。

---

## 8. 接口层（Gateway）：TCP+TLS 网络实现细节

### 8.1 连接生命周期与 Transport 抽象

`net-kit` 定义一个 `Transport` trait，当前只有 `TcpTlsTransport` 一个实现，为未来 KCP/QUIC 预留：

```rust
// crates/net-kit/src/transport.rs
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    type Conn: AsyncRead + AsyncWrite + Send + Unpin;
    async fn accept(&self) -> std::io::Result<Self::Conn>;
}
```

连接建立流程：TCP accept → TLS 握手（`tokio-rustls`）→ 生成 `Session`（未鉴权状态）→ 启动读/写两个 tokio task。

⚠️ **读写分离，避免用锁**：用 `tokio::io::split()` 把连接拆成读半部分和写半部分，读 task 负责解析入站消息并转发给对应用例；写 task 从一个专属的 `mpsc::channel` 里取出待发送消息写入 socket。这样"发消息给某个连接"只是"往它的 channel 里塞一条消息"，不需要在多个 task 间竞争同一把锁。

### 8.2 帧协议与编解码

帧格式：`[4字节长度前缀 u32 BE][2字节消息类型 opcode][protobuf 消息体]`

序列化选型：**Protobuf（`prost` crate）**。理由：客户端大概率是 Unity/Cocos/Unreal 等非 Rust 技术栈，protobuf 是业界公认的跨语言方案，字段号机制天然支持向前/向后兼容的协议演进。

🔴 **长度前缀必须校验上限**（如 64KB 或按业务实际需要设置），拒绝并断开发送超大长度声明的连接——不校验上限等于给恶意客户端一个用极小流量耗尽服务器内存的攻击面。

⚠️ **协议演进规则**：protobuf 字段号一旦发布**禁止**重新赋值给其他含义的字段，新增字段一律用新字段号且保持 `optional`；否则线上新老客户端混跑期间会出现解析错乱，这是版本发布事故的常见根源。

### 8.3 鉴权流程

设计两个入站适配器，共享同一套 application 层用例：

- **HTTP（axum）**：登录、注册、找回密码、客户端版本/配置拉取等低频 CRUD 操作。登录成功后下发 opaque session token。
- **TCP+TLS（核心玩法通道）**：客户端建连后，第一条消息必须是携带 token 的绑定消息，网关校验 token（查 Redis）通过后才把该连接标记为"已鉴权"，此后才允许调用其他游戏用例。

🔴 **未鉴权连接必须有严格超时**（建议 5-10 秒内未完成鉴权则强制断开）与**限流**（同一 IP 并发未鉴权连接数上限），否则这是一个天然的资源耗尽攻击入口（构造大量连接、不发送数据、白占服务器连接数与内存）。

### 8.4 心跳与超时

客户端每 N 秒（建议 15-30s）发送心跳，服务端更新 `Session.last_heartbeat_at`；后台巡检任务定期扫描超时会话（建议超时阈值 45-60s，即 2-3 个心跳周期未收到）并强制断开、回收资源。⚠️ **仅靠 TCP 自身的 keepalive 不够**——操作系统默认 keepalive 探测间隔通常是几小时级别，远达不到游戏对"及时发现掉线玩家"的要求，应用层心跳是必须的。

### 8.5 背压与慢客户端处理 🔴

每个连接的发送队列（写 task 消费的 `mpsc::channel`）**必须是有界的**，并明确队列满后的处理策略（丢弃非关键消息 / 直接断开该连接，二选一，具体取决于业务对"消息必达"的要求）。

⚠️ **原因**：无界发送队列在客户端异常（网络拥塞、客户端卡死、恶意构造的慢速消费者）时会无限增长，最终耗尽服务器内存——这本质上是应用层的 Slowloris 变种攻击，即便没有恶意攻击者，普通玩家的弱网环境也可能触发同样的问题。

### 8.6 可选：浏览器/H5 客户端接入（WSS）

若产品需要网页端客户端，💡 推荐在同一 Rust 进程内用 `tokio-tungstenite` 直接监听一个 WSS 端口，与 TCP+TLS 监听器共享同一套 `gateway` 消息路由和 `application` 用例（协议帧格式可以保持一致，仅外层多一层 WebSocket 帧），**不强制引入 nginx**——单实例阶段，多一跳 nginx 只带来额外运维复杂度和延迟，收益（证书统一管理、负载均衡）在只有一个实例时体现不出来。等到第 18 节所说的多实例阶段，再评估引入独立网关层。

---

## 9. 并发与运行时模型（⚠️ 本节是单机 QPS 表现的关键）

### 9.1 Room/Scene 的 Actor 模式

🔴 **最重要的架构决策**：每个 Room 拥有自己的权威状态，作为独立的 tokio task 运行，用 `mpsc::channel` 接收来自各连接的命令，**单线程串行处理**（同一时刻只有一个任务在改这个 Room 的状态），处理结果通过每个成员连接各自的发送队列广播出去。

```
连接 A 的读 task ──┐
连接 B 的读 task ──┼──> Room 的命令 channel ──> Room task（串行处理，无锁）──> 各连接的发送 channel
连接 C 的读 task ──┘
```

这样设计的好处：

- Room 内部状态**完全不需要锁**——因为只有 Room 自己的 task 会修改它；
- 不同 Room 之间天然并行，充分利用 tokio 多线程 runtime 的所有核心；
- 单个 Room 的处理逻辑可以用最朴素的方式写（顺序执行的函数），不用操心并发安全，可读性和可测试性都好。

### 9.2 禁止的反模式 🔴

**禁止**在游戏热路径上用一个全局 `Mutex`/`RwLock` 包裹整个世界状态（如 `Arc<Mutex<HashMap<RoomId, Room>>>` 然后所有操作都先 `.lock()`）。这是 Rust 初学者写游戏服务器最常见、也最致命的性能反模式——它把本可以并行的多个 Room 的处理**人为串行化**成了一个全局临界区，QPS 会随连接数增长迅速见顶，而且长时间持锁还可能引发死锁或饥饿问题。9.1 节的 Actor 模式就是为了从架构上避免这个坑，而不是"等出了性能问题再优化"。

账号/玩家的持久化数据（登录、非房间内的物品变更等低频操作）不需要 Actor 模式，走正常的 application 用例 + 数据库事务即可，这类操作频率远低于房间内 tick，用传统方式没有问题。

### 9.3 Panic 隔离与 Mutex 中毒 ⚠️

每个连接的处理放在独立的 `tokio::spawn` 任务里——单个连接的处理逻辑 panic，tokio runtime 只会终止那一个 task，不会影响进程内其他连接/Room。但要注意：如果该任务持有一个**已中毒（poisoned）**的 `std::sync::Mutex`（Rust 标准库的 Mutex 在持锁线程 panic 时会把锁标记为中毒，后续 `.lock()` 会返回 `Err`），后续所有试图获取这把锁的代码都会跟着出错，形成级联故障。建议：需要跨 task 共享的少量状态优先用 `parking_lot::Mutex`（不会中毒）或 `tokio::sync::Mutex`，并对可能的 panic 路径做好审计。

🔴 **上线前必须审计 `unwrap()` / `expect()` 的使用**：网络输入解析、外部服务调用（数据库/Redis）等"不可信/可能失败"的路径，禁止用 `unwrap()`，必须走 `Result` 显式处理并转换成合适的错误响应或断开连接，而不是让整个 task panic。

### 9.4 Tokio Runtime 配置建议

- 使用多线程 runtime（`#[tokio::main(flavor = "multi_thread")]`），worker 线程数默认等于 CPU 核心数，一般不需要手动调整，除非压测证明有必要。
- 💡 极高连接数场景（单机数万以上）可以考虑 `SO_REUSEPORT` + 多个监听 socket 分散到不同线程，缓解 accept 环节的锁竞争；这是一个后续按需的性能优化项，不是 MVP 阶段的必需品。

---

## 10. 安全设计清单（🔴 重点章节）

| 项目 | 要求 |
| --- | --- |
| 传输加密 | 🔴 TLS 1.3 only，禁止任何环境（包括测试环境）存在无 TLS 明文监听的代码路径；证书用 Let's Encrypt 自动化或内部 CA，建立自动续期机制 |
| 证书校验 | 🔴 禁止出现"跳过证书校验"的代码路径（哪怕只在测试代码里），这种代码极易被误带入生产 |
| 密码存储 | 🔴 argon2id 哈希，禁止 md5/sha1/明文；本项目不建议自建密码存储时用 bcrypt（argon2id 是当前推荐标准） |
| 会话 token | 🔴 密码学安全随机数生成，短期有效，Redis 存储支持主动吊销 |
| 服务端权威 | 🔴 所有影响游戏结果的数值（位置、伤害、货币）以服务端计算为准，绝不信任客户端上报的结果，只信任客户端上报的"意图"（如"我按了攻击键"） |
| 输入校验 | 🔴 所有网络输入在进入业务逻辑前做边界校验（长度、范围、枚举合法性），protobuf 反序列化本身不代表数据合法 |
| SQL 注入 | 🔴 SeaORM 默认参数化查询；如需要 raw SQL，必须走参数化接口，禁止字符串拼接 SQL |
| 依赖安全 | ⚠️ CI 中接入 `cargo audit`/`cargo deny`，定期检查依赖 CVE |
| 秘钥管理 | 🔴 数据库密码、Redis 密码、TLS 私钥等通过环境变量或秘钥管理服务注入，禁止硬编码/提交到代码仓库 |
| 日志脱敏 | 🔴 日志中禁止出现明文密码、完整 token、支付信息 |
| DDoS 防护 | ⚠️ 应用层只能做连接数/速率限制，无法独自解决大流量卷积攻击，需要依赖云厂商的 DDoS 防护或专用抗 D 服务；这一点要提前和运维/采购对齐预期，不要指望纯代码层面解决 |
| 反重放 | ⚠️ 关键操作（如支付回调、一次性道具兑换）应有 nonce/幂等 key 机制，防止消息重放 |

---

## 11. 性能目标与压测方案

⚠️ 以下数值是**参考起点**，不是承诺值，必须在目标硬件、目标消息模式下实测校准后写入正式验收标准：

- 单实例（参考配置：8 核 / 16GB）目标：数万级并发长连接（具体取决于消息频率、单条消息处理复杂度），需实测；
- 心跳间隔 15-30s，超时判定 45-60s；
- P99 消息处理延迟目标 < 50ms（不含客户端到服务器的网络 RTT）。

**压测工具**：由于是自定义二进制协议，通用 HTTP 压测工具（wrk/k6/ab）无法直接使用。建议基于 `protocol` + `net-kit` crate 自行编写一个 Rust 压测客户端（模拟 N 个并发连接、按设定速率发送消息、统计延迟分布），这个压测客户端本身也可以作为回归测试套件的一部分长期维护。

---

## 12. 可观测性

- **日志**：`tracing` crate，结构化输出，按连接/请求生成 span，区分环境用不同日志级别；
- **指标**：`metrics` crate + Prometheus exporter，至少覆盖：当前在线连接数、每秒消息处理量、各用例处理耗时分布、数据库连接池使用率、Redis 命令延迟；
- **健康检查**：暴露一个简单的 HTTP `/healthz`，即便当前是单体单实例，也建议从第一天就有，方便基础监控/未来接入负载均衡；
- 💡 分布式追踪（OpenTelemetry）在单体阶段收益不大，留到第 18 节的多服务阶段再引入。

---

## 13. 部署与运维

### 13.1 配置管理

配置文件（TOML）+ 环境变量覆盖（推荐 `config` 或 `figment` crate），🔴 敏感配置（数据库密码等）只允许通过环境变量/秘钥管理服务注入，不允许写在配置文件里提交进代码仓库。

### 13.2 优雅停机 🔴

游戏服务器**必须**实现优雅停机，直接 `kill -9` 可能导致玩家数据丢失或房间状态不一致。建议流程：

1. 收到 `SIGTERM` → 立即停止 accept 新连接；
2. 广播通知所有在线连接"服务即将维护"（可选，取决于产品需求）；
3. 等待所有进行中的 Room 达到"可安全中断点"（如当前回合结束）或超时强制结算；
4. 落盘所有待持久化的状态变更；
5. 关闭数据库连接池、Redis 连接；
6. 进程退出。

### 13.3 部署形态

单一可执行文件 + systemd service（或容器化部署均可，本 PRD 不强制），配合上述优雅停机可以做到"发布时不强制踢掉所有在线玩家"（虽然做不到真正 0 感知，但至少不会粗暴掉线）。

---

## 14. 测试策略

| 层级 | 方式 |
| --- | --- |
| Domain | 纯单元测试，无需 mock，跑得最快，覆盖率要求最高 |
| Application | 用 mock 实现 domain 定义的 repository trait（`mockall` crate），测试用例编排逻辑 |
| Infrastructure | 集成测试，用 `testcontainers` 拉起真实 PostgreSQL/Redis 容器 |
| Gateway/端到端 | 用真实 TCP 客户端连接测试服务器实例，验证协议编解码、鉴权流程、心跳超时等 |
| 压测 | 见第 11 节自研压测客户端 |

---

## 15. 开发阶段规划（参考，可按团队节奏调整）

1. **阶段 0**：workspace 骨架 + `net-kit` 基础 echo server（验证 TCP+TLS 连通）；
2. **阶段 1**：`domain`/`application` 核心（账号、鉴权）+ `infrastructure` 数据库落地；
3. **阶段 2**：Room/对局核心玩法循环（Actor 模式落地）；
4. **阶段 3**：完整鉴权链路、心跳、背压、断线重连；
5. **阶段 4**：安全清单逐项过一遍（第 10 节）、压测校准性能目标；
6. **阶段 5**：可观测性接入、部署脚本、优雅停机验证。

---

## 16. 风险与提醒汇总表 🔴

| 编号 | 提醒 | 所在章节 |
| --- | --- | --- |
| R1 | 禁止无 TLS 明文监听，包括测试环境 | 3.2 / 10 |
| R2 | Domain 层零框架依赖，禁止 SeaORM/protobuf 派生宏泄漏进 domain | 4.1 / 5 |
| R3 | 长度前缀帧必须校验上限，防内存耗尽攻击 | 8.2 |
| R4 | 未鉴权连接必须有超时和限流 | 8.3 |
| R5 | 每连接发送队列必须有界，明确满队列策略 | 8.5 |
| R6 | 禁止全局 Mutex 包裹整个世界状态，用 Room Actor 模式 | 9.1 / 9.2 |
| R7 | 上线前审计 unwrap/expect，网络输入路径禁止 panic | 9.3 |
| R8 | 密码 argon2id，session token 随机生成、可主动吊销 | 10 |
| R9 | 所有影响游戏结果的数值以服务端为准，不信任客户端上报结果 | 10 |
| R10 | SeaORM 生产用 1.1.x 稳定版，2.0 尚在 RC 阶段 | 7.2 |
| R11 | 游戏服务器必须实现优雅停机，禁止直接 kill -9 | 13.2 |
| R12 | 性能目标数值需实测校准，不能直接当承诺值使用 | 11 |
| R13 | 数据库连接池必须设上限，防止流量突增打垮数据库 | 7.4 |

---

## 17. 后续分布式/微服务演进指南

🔴 **总原则：不要过早拆分**。分布式系统带来的复杂度（网络分区、一致性、部分失败处理、跨服务调试难度）是巨大的工程成本，只有在明确的业务/技术压力下才应该拆，并且应该按下面的顺序渐进式拆分，而不是一次性重写成微服务架构。

### 17.1 什么时候该考虑拆分（信号，不是时间表）

- 单机 CPU/内存/连接数已经接近上限，垂直扩容（加配置）性价比明显下降；
- 需要跨地域部署以降低不同地区玩家的延迟；
- 不同模块的扩缩容需求差异很大（登录/匹配这类无状态模块想水平扩展，但对局房间是有状态、需要连接亲和性）；
- 需要独立发布某个模块而不影响整体（比如更新聊天服务不希望打断正在进行的对局）；
- 团队规模扩大，多团队并行开发导致单体代码库冲突、编译时间成为明显瓶颈。

### 17.2 拆分顺序建议（由易到难）

**第一步：网关/接入层独立**（最容易，也最先做）
把 `gateway` crate 变成独立部署的 Gateway 服务，与后端 Game Logic 服务之间从"进程内方法调用"改为 gRPC 调用。得益于本 PRD 严格的洋葱架构，这一步**只需要把 `application` 层的调用方式从"进程内 trait 调用"换成"gRPC client 调用"**这一层适配代码，`domain` 和 `application` 内部逻辑完全不用改。

**第二步：无状态服务独立**（次容易）
登录/账号（Account 限界上下文）、好友/社交、排行榜等相对独立、无强实时状态的模块可以先拆成独立服务，水平扩容简单，风险低。

**第三步：有状态的房间/对局服务**（最难，放最后）
需要解决：

- **分片（Sharding）**：按 RoomId 或玩家地理位置做一致性哈希，把房间分散到不同节点；
- **路由发现**：需要一个"房间在哪个节点"的注册表（可以先用 Redis 存 `room_id → node_addr` 映射，规模再大后升级到 etcd/consul），网关根据这个路由表把消息转发到正确节点；
- **跨节点通信**：如果需要跨房间/跨服交互（跨服聊天、跨服排行榜），引入消息队列（NATS 延迟低、适合游戏场景；Kafka 更适合需要强持久化/回放能力的场景）做异步事件广播；
- **状态迁移**：房间从一个节点迁移到另一个节点是分布式游戏服务器最难的问题之一。⚠️ 建议先**避免动态迁移**，采用"房间生命周期内固定在一个节点，节点下线前等对局自然结束"的简单策略，除非有强业务需求，不要一开始就上动态迁移。

### 17.3 关键技术选型预告

| 领域 | 建议方案 |
| --- | --- |
| 服务间通信 | gRPC（`tonic` crate），Rust 生态成熟、强类型、性能好 |
| 服务发现 | 规模较小时用 Redis 做简化注册表；规模变大后升级 etcd/consul |
| 消息队列 | NATS（轻量低延迟，适合游戏）或 Kafka（需要强持久化/回放时） |
| 数据库演进 | PostgreSQL 读写分离（读副本）→ 按业务垂直拆库（账号库/游戏数据库分离）→ 单表数据量极大时考虑分片（Citus 或应用层分片） |
| 缓存演进 | 单实例 Redis → Redis Cluster（数据分片 + 高可用） |
| 可观测性 | 引入 OpenTelemetry 做分布式追踪，否则跨服务问题排查会非常痛苦 |
| 跨服务鉴权 | 单体阶段的 opaque token 在网关边界转换成 JWT 或内部签名 token，供下游服务无状态校验 |

### 17.4 为什么现在的架构选择能为这一步铺路

本 PRD 从一开始就坚持：

- `domain`/`application` 层完全不依赖任何网络/存储细节 —— 无论未来怎么拆分服务，这两层的代码大概率**不需要改动**，只需要替换 `infrastructure` 和 `gateway`/接口层的具体实现（进程内调用 → gRPC 调用，本地 repository → 远程服务调用）；
- `net-kit` 独立成一个零业务依赖的通用网络框架 crate —— 传输协议从 TCP+TLS 扩展到 KCP/QUIC，改动范围严格限定在这一个 crate，不会波及业务代码；
- Room 的 Actor 模式（第 9.1 节）天然对应未来的"分片节点"模型 —— 单机内是"一个 Room 一个 task"，分布式后是"一个 Room 固定在一个节点"，概念上是一致的，迁移成本低。

这就是"先做好单体、按需拆分"的技术基础：**不是为了拆分而过度设计，而是让正确的边界从第一天就存在，拆分时只是把边界从"进程内"挪到"进程间"。**

---

## 附录：关键依赖清单（参考，具体版本以 `cargo add` 时的最新稳定版为准）

```toml
# 网络/异步运行时
tokio = { version = "1", features = ["full"] }
tokio-rustls = "0.26"
rustls = "0.23"
tokio-tungstenite = "0.24"      # 仅当需要 WSS 接入时引入

# 协议
prost = "0.13"

# Web（HTTP 侧入口）
axum = "0.8"

# 数据库
sea-orm = { version = "1", features = ["sqlx-postgres", "runtime-tokio-rustls"] }
sea-orm-migration = "1"

# 缓存
redis = { version = "0.27", features = ["tokio-comp"] }

# 安全
argon2 = "0.5"

# 基础
uuid = { version = "1", features = ["v7"] }
chrono = "0.4"
thiserror = "2"
serde = { version = "1", features = ["derive"] }
async-trait = "0.1"

# 可观测性
tracing = "0.1"
tracing-subscriber = "0.3"
metrics = "0.24"

# 测试
mockall = "0.13"
testcontainers = "0.23"
```

---

*本文档为技术方案讨论稿，第 0.1 节列出的前提假设需要与产品/团队确认后再进入实现阶段；第 16 节的提醒清单建议作为代码评审 checklist 长期使用。*

## 18. 配置管理详解

> 本节补充原 PRD 第 13.1 节「配置管理」的具体实现细节，将所有可调节参数集中管理，避免硬编码，为运维调优和快速部署提供标准依据。

### 18.1 配置项清单（全部可配，默认值合理）

所有配置通过环境变量注入，实际运行时从项目根目录的 `.env` 文件加载（使用 `dotenv` crate）。代码中定义默认值，仅当环境变量缺失时使用。**禁止在源码中直接写死任何可变参数（除默认值外）**。

配置项按功能分组，如下所示：

#### 网络层（`net-kit` & `gateway/tcp`）

| 配置项                           | 类型     | 默认值           | 说明                                                         |
| -------------------------------- | -------- | ---------------- | ------------------------------------------------------------ |
| `SERVER_TCP_BIND_ADDR`           | `String` | `"0.0.0.0:8080"` | TCP+TLS 监听地址（IP:端口）                                  |
| `SERVER_HTTP_BIND_ADDR`          | `String` | `"0.0.0.0:8081"` | HTTP (axum) 监听地址，用于登录/注册等                        |
| `SERVER_WSS_BIND_ADDR`           | `String` | `"0.0.0.0:8082"` | WSS 监听地址（仅当启用 WSS 时有效）                          |
| `SERVER_MAX_CONNECTIONS`         | `usize`  | `10000`          | 最大同时在线连接数（超出后拒绝新连接）                       |
| `SERVER_CONNECTION_BACKLOG`      | `u32`    | `1024`           | TCP 监听队列的 backlog 大小（`listen` 参数）                 |
| `SERVER_TCP_READ_BUFFER_SIZE`    | `usize`  | `16384`          | 每个连接的读缓冲区初始大小（字节）                           |
| `SERVER_TCP_WRITE_BUFFER_SIZE`   | `usize`  | `16384`          | 每个连接的写缓冲区初始大小                                   |
| `SERVER_CHANNEL_PER_CONN_SIZE`   | `usize`  | `256`            | 每个连接发送队列（`mpsc::channel`）的容量，**必须有界**（PRD 8.5） |
| `SERVER_CHANNEL_PER_ROOM_SIZE`   | `usize`  | `512`            | 每个 Room 命令队列的容量                                     |
| `SERVER_MAX_FRAME_SIZE`          | `usize`  | `65536`          | 单条消息长度上限（64KB），防止恶意大包（PRD 8.2 强制）       |
| `SERVER_HEARTBEAT_INTERVAL_SECS` | `u64`    | `20`             | 客户端心跳间隔（秒）                                         |
| `SERVER_HEARTBEAT_TIMEOUT_SECS`  | `u64`    | `60`             | 服务端判定超时断连的阈值（建议 2~3 个心跳周期）              |
| `SERVER_UNAUTH_TIMEOUT_SECS`     | `u64`    | `10`             | 未鉴权连接最大存活时间（秒），超时强制断开                   |
| `SERVER_UNAUTH_MAX_PER_IP`       | `usize`  | `5`              | 同一 IP 允许的最大未鉴权连接数，防资源耗尽                   |
| `SERVER_TCP_NODELAY`             | `bool`   | `true`           | 是否开启 `TCP_NODELAY`（禁用 Nagle，降低延迟）               |
| `SERVER_TCP_KEEPALIVE_SECS`      | `u64`    | `30`             | 操作系统 TCP keepalive 空闲探测间隔（秒），仅作底层辅助      |

#### 速率限制（防滥用）

| 配置项                       | 类型   | 默认值 | 说明                                        |
| ---------------------------- | ------ | ------ | ------------------------------------------- |
| `SERVER_RATE_LIMIT_PER_CONN` | `u64`  | `100`  | 每个连接每秒允许的最大消息数（读/写均计入） |
| `SERVER_RATE_LIMIT_BURST`    | `u64`  | `10`   | 速率限制的突发容忍量（令牌桶突发容量）      |
| `SERVER_RATE_LIMIT_ENABLED`  | `bool` | `true` | 是否开启应用层速率限制（建议开启）          |

#### TLS/证书

| 配置项            | 类型             | 默认值                 | 说明                                             |
| ----------------- | ---------------- | ---------------------- | ------------------------------------------------ |
| `TLS_CERT_PATH`   | `String`         | `"./certs/server.crt"` | TLS 证书文件路径                                 |
| `TLS_KEY_PATH`    | `String`         | `"./certs/server.key"` | TLS 私钥文件路径                                 |
| `TLS_CA_PATH`     | `Option<String>` | `None`                 | 可选 CA 证书（用于客户端证书校验，本版本不启用） |
| `TLS_MIN_VERSION` | `String`         | `"TLSv1.3"`            | 强制最低 TLS 版本（固定为 1.3，不可降级）        |

#### 数据库（PostgreSQL）

| 配置项                          | 类型     | 默认值                                       | 说明                                                   |
| ------------------------------- | -------- | -------------------------------------------- | ------------------------------------------------------ |
| `DATABASE_URL`                  | `String` | `"postgres://user:pass@localhost:5432/game"` | SeaORM 连接字符串（**必须通过 env 覆盖，不得硬编码**） |
| `DATABASE_MAX_CONNECTIONS`      | `u32`    | `30`                                         | 连接池最大连接数（需根据 DB 配置校准）                 |
| `DATABASE_MIN_CONNECTIONS`      | `u32`    | `5`                                          | 连接池最小空闲连接数                                   |
| `DATABASE_CONNECT_TIMEOUT_SECS` | `u64`    | `5`                                          | 获取连接的超时秒数                                     |
| `DATABASE_IDLE_TIMEOUT_SECS`    | `u64`    | `300`                                        | 连接空闲最大存活秒数（超过后释放）                     |
| `DATABASE_SQLX_LOG_LEVEL`       | `String` | `"warn"`                                     | sqlx 日志级别（`trace`,`debug`,`info`,`warn`,`error`） |

#### Redis

| 配置项                       | 类型     | 默认值                     | 说明                                                         |
| ---------------------------- | -------- | -------------------------- | ------------------------------------------------------------ |
| `REDIS_URL`                  | `String` | `"redis://127.0.0.1:6379"` | Redis 连接 URL                                               |
| `REDIS_POOL_MAX_SIZE`        | `usize`  | `10`                       | 连接池最大连接数（使用 `MultiplexedConnection` 时可适当调低） |
| `REDIS_DEFAULT_TTL_SECS`     | `u64`    | `604800`                   | token 等默认过期时间（7 天），单位秒                         |
| `REDIS_CONNECT_TIMEOUT_SECS` | `u64`    | `2`                        | 连接超时秒数                                                 |

#### 日志与可观测性

| 配置项         | 类型     | 默认值   | 说明                                                      |
| -------------- | -------- | -------- | --------------------------------------------------------- |
| `LOG_LEVEL`    | `String` | `"info"` | 全局日志级别（`trace`,`debug`,`info`,`warn`,`error`）     |
| `LOG_FORMAT`   | `String` | `"json"` | 日志输出格式（`json` 或 `pretty`，生产用 json）           |
| `OTEL_ENABLED` | `bool`   | `false`  | 是否启用 OpenTelemetry 追踪（默认关闭，单体阶段暂不启用） |
| `METRICS_PORT` | `u16`    | `9090`   | Prometheus 指标暴露端口                                   |

#### 应用层通用

| 配置项                       | 类型  | 默认值   | 说明                                          |
| ---------------------------- | ----- | -------- | --------------------------------------------- |
| `APP_SESSION_TOKEN_TTL_SECS` | `u64` | `604800` | session token 有效期（秒），与 Redis TTL 联动 |
| `APP_PASSWORD_ITERATIONS`    | `u32` | `3`      | argon2id 迭代次数（根据性能调优）             |
| `APP_PASSWORD_MEMORY_KB`     | `u32` | `19456`  | argon2id 内存开销（KB）                       |
| `APP_PASSWORD_PARALLELISM`   | `u32` | `4`      | argon2id 并行度                               |
| `APP_MAX_PLAYERS_PER_ROOM`   | `u32` | `10`     | 单房间最大玩家数（按实际玩法可覆盖）          |
| `APP_SHUTDOWN_TIMEOUT_SECS`  | `u64` | `30`     | 优雅停机时等待现有事务完成的超时秒数          |

---

### 18.2 环境变量示例文件（`.public_env`）

以下为完整的环境变量模板，请复制到项目根目录，重命名为 `.env` 并修改实际值。**此文件应加入 `.gitignore`，切勿提交秘钥**。同时保留一份 `.public_env` 作为文档示例，供团队参考。

```bash
# ============================================================
# Rust 游戏服务器 环境变量配置模板
# 用法：复制为 .env，修改所需值；未设置项将使用代码中的默认值。
# ============================================================

# -------------------- 网络层 --------------------
# TCP+TLS 监听地址（IP:端口）
SERVER_TCP_BIND_ADDR=0.0.0.0:8080

# HTTP (axum) 监听地址（登录/注册等 REST API）
SERVER_HTTP_BIND_ADDR=0.0.0.0:8081

# WSS 监听地址（仅当启用 WebSocket 时使用）
SERVER_WSS_BIND_ADDR=0.0.0.0:8082

# 最大同时在线连接数（超出拒绝）
SERVER_MAX_CONNECTIONS=10000

# TCP listen backlog
SERVER_CONNECTION_BACKLOG=1024

# 读/写缓冲区初始大小（字节）
SERVER_TCP_READ_BUFFER_SIZE=16384
SERVER_TCP_WRITE_BUFFER_SIZE=16384

# 每个连接发送队列容量（有界，防止内存溢出）
SERVER_CHANNEL_PER_CONN_SIZE=256

# 每个 Room 命令队列容量
SERVER_CHANNEL_PER_ROOM_SIZE=512

# 单条消息最大长度（字节）
SERVER_MAX_FRAME_SIZE=65536

# 心跳间隔（秒）
SERVER_HEARTBEAT_INTERVAL_SECS=20

# 心跳超时（秒，建议 2~3 倍间隔）
SERVER_HEARTBEAT_TIMEOUT_SECS=60

# 未鉴权连接超时（秒）
SERVER_UNAUTH_TIMEOUT_SECS=10

# 同 IP 未鉴权连接数上限
SERVER_UNAUTH_MAX_PER_IP=5

# 启用 TCP_NODELAY（禁用 Nagle）
SERVER_TCP_NODELAY=true

# TCP keepalive 探测间隔（秒）
SERVER_TCP_KEEPALIVE_SECS=30

# -------------------- 速率限制 --------------------
# 每连接每秒最大消息数
SERVER_RATE_LIMIT_PER_CONN=100

# 令牌桶突发容量
SERVER_RATE_LIMIT_BURST=10

# 是否启用速率限制
SERVER_RATE_LIMIT_ENABLED=true

# -------------------- TLS 证书 --------------------
# 证书文件路径（PEM 格式）
TLS_CERT_PATH=./certs/server.crt

# 私钥文件路径
TLS_KEY_PATH=./certs/server.key

# CA 证书（可选，本版本不启用客户端证书校验）
# TLS_CA_PATH=

# -------------------- 数据库 (PostgreSQL) --------------------
# 连接字符串（必须填写实际值！）
DATABASE_URL=postgres://user:password@localhost:5432/game

# 连接池最大连接数
DATABASE_MAX_CONNECTIONS=30

# 最小空闲连接数
DATABASE_MIN_CONNECTIONS=5

# 获取连接超时（秒）
DATABASE_CONNECT_TIMEOUT_SECS=5

# 连接空闲超时（秒）
DATABASE_IDLE_TIMEOUT_SECS=300

# sqlx 日志级别（trace/debug/info/warn/error）
DATABASE_SQLX_LOG_LEVEL=warn

# -------------------- Redis --------------------
# Redis URL
REDIS_URL=redis://127.0.0.1:6379

# 连接池大小（MultiplexedConnection 可适当减小）
REDIS_POOL_MAX_SIZE=10

# 默认 TTL（秒），如 token 有效期
REDIS_DEFAULT_TTL_SECS=604800

# 连接超时（秒）
REDIS_CONNECT_TIMEOUT_SECS=2

# -------------------- 日志与可观测性 --------------------
# 日志级别（trace/debug/info/warn/error）
LOG_LEVEL=info

# 日志格式（json 或 pretty）
LOG_FORMAT=json

# 是否启用 OpenTelemetry（单体阶段建议 false）
OTEL_ENABLED=false

# Prometheus 指标端口
METRICS_PORT=9090

# -------------------- 应用层 --------------------
# Session token 有效期（秒）
APP_SESSION_TOKEN_TTL_SECS=604800

# argon2id 迭代次数
APP_PASSWORD_ITERATIONS=3

# argon2id 内存开销（KB）
APP_PASSWORD_MEMORY_KB=19456

# argon2id 并行度
APP_PASSWORD_PARALLELISM=4

# 单房间最大玩家数
APP_MAX_PLAYERS_PER_ROOM=10

# 优雅停机等待超时（秒）
APP_SHUTDOWN_TIMEOUT_SECS=30
```

### 18.3 使用说明（代码层面）

在 `server-bin/src/main.rs` 中，启动时执行：

```rust
use dotenv::dotenv;
use std::env;

fn load_config() {
    dotenv().ok();  // 加载 .env，若不存在则忽略
    // 然后通过 env::var("KEY").unwrap_or_else(|_| DEFAULT.to_string()) 读取每个配置
    // 建议将配置聚合到一个结构体中，统一从环境变量填充。
}
```

> **🔴 强制提醒**：`DATABASE_URL`、`REDIS_URL` 及 TLS 私钥路径等敏感信息**必须**通过环境变量提供，不得以默认值形式硬编码在源码中（默认值仅作为示例占位，实际生产必须覆盖）。所有配置变更无需重新编译，重启服务即可生效。

---

