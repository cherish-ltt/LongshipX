# Changelog

本文件记录 LongshipX 每个版本的变更要点,格式参考 Keep a Changelog;版本号遵循语义化版本(SemVer)。

## [0.1.0] - 2026-09-05

**LongshipX 首个公开版本**:基于 tokio + TCP/TLS 1.3 的 Rust 长连接游戏服务器框架,以 DDD + 洋葱架构组织为 Cargo workspace 多 crate 单体——**长船·长连接·长长久久**。

### 核心特性

**架构(名字里的"X")**

- 7 个 crate + 迁移 crate 的 workspace:依赖方向由编译器强制,domain 与 net-kit 是互不相识的两个零依赖核心;
- 换存储(MySQL)、换实现、加传输(KCP/QUIC),只动出站适配器,业务核心不动。

**传输与协议**

- TCP + rustls(TLS 1.3 only),长度前缀帧 `[4B len][2B opcode][protobuf]`,帧长上限防内存耗尽;
- protox + prost 构建期生成,**无需安装 protoc**;Server/Client 双 Codec,压测与测试客户端开箱即用;
- opcode 路由表:新增一条命令 = proto → opcode → 枚举分支 → 处理器 → 路由注册,五处一目了然。

**长连接管理**

- 连接读写分离,发送队列有界(慢客户端断开策略);心跳超时、未鉴权连接限时限量、每连接令牌桶限流;
- Room Actor:每个房间一个 task 串行处理命令,无锁;成员广播走各自有界通道,房间随最后一名成员离开自然终结;
- 优雅停机:SIGTERM → 停 accept → 房间广播 Closed → 在线连接收到维护通知 → 排空 → 关闭连接池。

**安全**

- argon2id 密码哈希(参数可配)、opaque token + Redis 存储(立即吊销/单会话顶号)、日志脱敏(密码/哈希 Debug 全程掩码)、服务端权威数值。

**存储与可观测**

- PostgreSQL 18 + SeaORM 2.0(`uuidv7()` 主键、CITEXT 用户名);迁移三表:accounts / players / audit_logs;
- Redis 承载 token(支持跨重启与主动吊销);
- `tracing` 结构化日志(json/pretty)、Prometheus `/metrics`、`/healthz`;全量参数环境变量注入(模板 `.public_env`)。

### 质量

- 142 个测试:领域/应用/协议/网络层单元测试 + 真实 TLS 端到端 + HTTP 路由测试,**全部可离线运行**;
- cargo-llvm-cov 行覆盖 ≈ 88%,核心 crate 83%–100%;
- `fmt` / `clippy -D warnings` / `build` / `test` CI 全绿,GitHub Actions 按 paths 过滤——纯文档提交不触发。

### 已知边界

- SeaORM/Redis 的生产仓储实现需真实服务,不在离线单测覆盖范围;
- WSS 接入、OpenTelemetry、UnitOfWork 跨聚合事务为预留扩展点,尚未启用;
- 房间状态为进程内 Actor 持有,暂未做跨进程持久化(预留 RoomRepository)。

### 环境要求

Rust 1.98.1(stable)· PostgreSQL 18 · Redis · mkcert(本地开发证书)。

```bash
cp .public_env .env && cargo run -p longshipx-server-bin
```
