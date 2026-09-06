# AGENTS.md

本文件定义了本项目的 Rust 开发规范与自动化流程。所有贡献者必须严格遵守，并在每次修改代码后及时更新本文件（如有新增规范或调整）。

---

## 1. Git 提交规范

- **范围**：每次提交应独立且完整地对应一个逻辑变更（如单一功能点、缺陷修复或配置调整），禁止混合多个不相关改动；按功能批次顺序组织，单次提交代码变动量建议控制在 500 行以内(仅建议，非强制，可适当突破)，避免大批量改动挤在同一条提交信息中。
- **格式**：`<type>: <中文描述>`
- **常用 type**：
  - `feat` – 新功能
  - `fix` – 修复 bug
  - `docs` – 文档更新
  - `style` – 代码格式（不影响逻辑）
  - `refactor` – 重构
  - `perf` – 性能优化
  - `test` – 测试相关
  - `build` – 构建系统或外部依赖变更
  - `ci` – CI 配置变更
  - `chore` – 杂项（如工具、配置等）
  - `revert` – 回退提交

示例：`feat: 添加用户登录接口`

---

## 2. Rust CI 标准（GitHub Actions）

确保 `.github/workflows/rust-ci.yml` 存在，内容如下：

```yaml
name: Rust CI

on:
  push:
    branches: [ "main", "master" ]
    paths:
      - "**.rs"
      - "**.proto"
      - "**/Cargo.toml"
      - "**/Cargo.lock"
      - ".rustfmt.toml"
      - ".clippy.toml"
      - "rust-toolchain.toml"
      - ".github/workflows/rust-ci.yml"
  pull_request:
    branches: [ "main", "master" ]
    paths:
      - "**.rs"
      - "**.proto"
      - "**/Cargo.toml"
      - "**/Cargo.lock"
      - ".rustfmt.toml"
      - ".clippy.toml"
      - "rust-toolchain.toml"
      - ".github/workflows/rust-ci.yml"

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    name: Check & Test
    runs-on: ubuntu-latest
    steps:
      - name: Checkout repository
        uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@master
        with:
          toolchain: "1.98.1"
          components: rustfmt, clippy

      - name: Show rustup info
        run: rustup show

      - name: Cache Cargo dependencies
        uses: Swatinem/rust-cache@v2

      - name: Check formatting
        run: cargo fmt --all -- --check

      - name: Run Clippy (lints)
        run: cargo clippy --all-targets -- -D warnings

      - name: Build the project
        run: cargo build --verbose

      - name: Run tests
        run: cargo test --verbose
```

---

## 3. Cargo.toml 配置

- 必须包含完整的包元数据（满足可发布到 [crates.io](https://crates.io) 的要求），例如：
  - `name`、`version`、`edition`、`authors`、`description`、`license`、`repository` 等。
- 依赖项必须**归类**，使用 `#` 注释说明每组依赖的用途。
- 每个依赖必须使用 `version = "x.y.z"` **锁定具体版本**（使用 `=` 号），不得使用范围限定符。
- 本项目为 Cargo workspace:**第三方依赖统一声明在根 `Cargo.toml` 的 `[workspace.dependencies]`**(`=` 锁定 + 分组注释),子 crate 以 `workspace = true` 引用、feature 差异增量声明;细则见第 10.1 节。
- 使用 `edition = "2024"` 以及环境中的 Rust 版本，例如:`rust-version = "1.95"`

示例结构：

```toml
[package]
name = "my_crate"
version = "0.1.0"
edition = "2024"
rust-version = "1.95"
authors = ["Your Name <email@example.com>"]
description = "A short description"
license = "MIT OR Apache-2.0"
repository = "https://github.com/your/repo"

# 核心依赖
[dependencies]
# 序列化
serde = { version = "=1.0.210", features = ["derive"] }

# 异步并发
tokio = { version = "=1.42.0", features = ["full"] }

# 开发依赖
[dev-dependencies]
# 基准测试
criterion = { version = "=0.5.1" }

# 编译优化配置
[profile.dev]
opt-level = 1
[profile.dev.package."*"]
opt-level = 3
```

---

## 4. 代码格式化（.rustfmt.toml）

项目根目录必须包含 `.rustfmt.toml`，内容如下：

```toml
edition = "2024"
max_width = 100
tab_spaces = 4
reorder_imports = true
reorder_modules = true
newline_style = "Auto"
match_block_trailing_comma = true
```

所有代码必须通过 `cargo fmt --all -- --check` 检查。

---

## 5. Clippy 配置（.clippy.toml）

项目根目录必须包含 `.clippy.toml`，内容如下：

```toml
# ── OY Clippy Configuration ──
cognitive-complexity-threshold = 15
too-many-arguments-threshold = 5
too-many-lines-threshold = 30
allow-unwrap-in-tests = true
msrv = "1.95.0"
```

所有代码必须通过 `cargo clippy --all-targets -- -D warnings` 检查，无警告。

---

## 6. README.md

- 每次完成任务后需及时更新 `README.md`，至少包含：
  - 项目简介
  - 构建与运行说明
  - 主要功能或使用示例
  - 贡献指南（引用本 AGENTS.md）

---

## 7. .gitignore

必须排除以下内容（示例）：

```
# Rust
/target/
**/*.rs.bk
*.pdb

# macOS
.DS_Store

# IDE
.vscode/
.idea/
*.swp
```

---

## 8. 项目结构

- **使用DDD+洋葱结构，严格遵循此结构开发**
- **禁止单`mod.rs`文件写入大量代码，代码较多时候将代码拆分到更小的带具体名称的代码文件中(如 utils.rs)**
- **保持单代码文件简洁和更细致的 crate 划分以加速增量编译**

---

## 9. 通用原则

- **保持本文件（AGENTS.md）更新**：每次修正代码或引入新规范后，请同步更新此文档。
- **所有变更**必须通过 CI 检查（格式、lint、构建、测试）。
- **版本锁定**：工具链版本统一使用 Rust 1.98.1（如 CI 和 clippy 配置所示）。
- **遵循设计**：改动必须遵循原有结构设计，不得私自添加和修改，除非用户发出明确重构指令。
- **后续开发追加 AGENTS.md 内容**：写入第 10 章节。
- **测试**：编写单元测试，如果已经安装`cargo-llvm-cov`则检测测试覆盖率>=80%。

---

**本文件是项目的“开发宪法”，所有 pull request 和代码审查均应参照其内容。**

## 10. 其他追加内容

### 10.1 Workspace 布局与 crate 命名

- 项目为 Cargo workspace(根 `Cargo.toml` 为 virtual manifest,`resolver = "3"`),成员见根 `Cargo.toml` 的 `members`。
- crate 包名统一加 `longshipx-` 前缀(如 `longshipx-domain`),保证 crates.io 可发布;目录名保持 PRD 中的 `domain`/`application`/`net-kit`/`protocol`/`infrastructure`/`gateway`/`server-bin` 与 `migration`。
- 每个 crate 的 `Cargo.toml` 必须内联完整元数据(name/version/edition/rust-version/authors/description/license/repository)。
- **第三方依赖统一在根 `Cargo.toml` 的 `[workspace.dependencies]` 管理**:`=` 锁定版本、按用途分组注释;子 crate 一律 `dep = { workspace = true }` 引用。仅 tokio 等 feature 需求不同的依赖,在子 crate 内增量追加 `features = [...]`(与 workspace 声明叠加)。新依赖先入根清单,禁止在子 crate 内直接写版本号。

### 10.2 分层红线(代码评审 checklist)

- `domain`:仅允许 `uuid`、`chrono`、`thiserror`、`serde`(标记)、`async-trait`(仓储接口,PRD 5.2 明确使用);禁止 tokio/sea_orm/redis/prost。
- `application`:业务依赖只通过端口 trait(`ports.rs`)声明,实现注入一律 `Arc<dyn Trait>`(PRD 6.1 允许);Room Actor 是应用层组件,允许 tokio 原语。
- `gateway`:不直接调用 domain 业务逻辑,只消费 application 用例与 DTO;domain 类型仅作为数据载体出现。
- `infrastructure`:SeaORM 实体/Redis 类型不得泄漏出本层,Model ⇄ 领域聚合必须显式转换(`persistence/converters.rs`)。
- 模块文件名避免与父模块同名(clippy `module_inception`),聚合实现放 `aggregate.rs`。

### 10.3 配置管理

- 全部可调参数经环境变量注入,默认值集中在 `crates/infrastructure/src/config.rs`(与 PRD 第 18 章一一对应);模板见 `.public_env`,真实 `.env` 已被 `.gitignore` 排除。
- 配置解析通过 `Config::from_lookup` 注入读取器(edition 2024 下避免 `unsafe set_var`),新增配置项必须同步:config 结构体 + 默认值 + `.public_env` + 单测。

### 10.4 测试与覆盖率

- 单元测试必须可离线运行(不依赖 PG/Redis/容器);SeaORM/Redis 具体实现仅编译验证,真实库集成测试另行规划。
- 端到端测试(`crates/gateway/tests/e2e.rs`、`tests/http.rs`)使用 rcgen 自签证书走真实 TLS,是协议/鉴权/房间链路的验收基线,改协议或网关后必须先跑。
- 覆盖率命令:`cargo llvm-cov --workspace --summary-only`;当前基线:全仓行覆盖 ≈ 88%,核心 crate(domain/application/protocol/net-kit/gateway-http)≈ 83%–100%,SeaORM 仓储与 server-bin 启动序列为 0%(需真实服务)。不得使核心 crate 覆盖率显著回落。
- 覆盖率工具链:`cargo-llvm-cov` + `rustup component add llvm-tools-preview`。

### 10.5 网络层约定(net-kit)

- 帧格式 `[4B len u32 BE][2B opcode][payload]`,长度前缀必须校验上限(`SERVER_MAX_FRAME_SIZE`);写 task 每次 `write_all` 后必须 `flush`(TLS/缓冲流语义,已修复过一次漏 flush 导致数据滞留的缺陷)。
- 发送队列必须有界且队列满即断开(PRD 8.5 策略);新增传输(KCP/QUIC)只实现 `Transport` trait,不改上层。
- 测试中涉及双工流的数据流向:`split(io)` 的写半部数据流向**对端**读半部,写测试时先画方向图,避免再次出现"读错半边"的死锁式用例。

### 10.6 协议演进(protocol)

- `.proto` 位于 `crates/protocol/proto/`,构建期由 protox(纯 Rust,无需 protoc)编译 + prost 生成;字段号一经发布禁止改作他用,新增字段一律新字段号且保持可选(PRD 8.2 🔴)。
- 新增消息必须:定义 proto → `messages.rs` 增加入/出站枚举分支与 `opcodes.rs` 常量 → gateway 注册路由 → 端到端测试补用例。

### 10.7 环境备注

- 若本机 `~/.cargo/config.toml` 配置了 `rustc-wrapper`(如 sccache),其值必须写**绝对路径**(`~` 不会被展开,写成 `~/...` 会导致所有构建失败)。
- 长时间运行的测试命令建议带超时包装执行,防止缺陷导致的挂死阻塞会话。



### 10.8 CI 触发过滤(paths 过滤器)

- `.github/workflows/rust-ci.yml` 使用 GitHub Actions 原生 `paths` 白名单:仅当 **`.rs`、`.proto`、`Cargo.toml`、`Cargo.lock`、rustfmt/clippy 配置、workflow 文件自身** 变更时才执行;纯文档/配置模板提交(`*.md`、`docs/**`、`.public_env` 等)自动跳过,不占用 runner 时长。
- ⚠️ 向白名单**新增会参与构建的文件类型时必须同步该列表**(如新增 `build.rs` 读取的外部资源、`rust-toolchain.toml` 已预置);宁可多触发,不可漏触发。
- 白名单语义是"漏改即漏检",若希望改为"仅文档变更跳过"的黑名单语义(`paths-ignore`),需评审后统一调整第 2 节与本节。

### 10.9 性能基准(benchmarks)

- 框架热路径必须有 criterion 基准,统一 `harness = false` 声明在对应 crate 的 `Cargo.toml`;criterion 版本锁定在根 `Cargo.toml` 的 `[workspace.dependencies]`。现有套件:`net-kit` 的 `frame_codec`/`connection`、`protocol` 的 `codec`、`gateway` 的 `http`/`tcp_room`。
- 基准必须**可离线运行**:网络类基准一律 loopback(`127.0.0.1:0`)真实 TCP/TLS + 内存版基础设施,禁止依赖 PG/Redis/外部服务。
- 计时循环内不得引入额外分配以外的人为开销;屏蔽编译器优化用 `std::hint::black_box`(勿用 criterion 已废弃的 `criterion::black_box`)。异步代码用常驻 `Runtime::block_on` 驱动,运行时在基准装置阶段创建。
- 端到端类基准(`tcp_room`)的客户端接收逻辑必须**确定性排空**事件流(过滤非目标消息),禁止依赖超时猜测,防止基准自身挂死。
- 修改网络/协议/网关层后,除跑 e2e 外建议运行对应基准对比 `target/criterion/` 历史基线;**基准暴露的资源泄漏/挂死必须转为确定性回归测试**(如连接名额泄漏之于 `connection_permits_are_released_after_disconnect`)。
- 基准代码同样受 `cargo fmt`/`cargo clippy --all-targets -- -D warnings` 约束(CI 自动覆盖);运行方式与参考数值见 README"性能基准"一节。

### 10.10 版本与变更记录

- 版本号遵循语义化版本(SemVer)。升级版本时必须同步:根 `Cargo.toml` 的 `[workspace.package]` 与 `[workspace.dependencies]` 内部依赖约束(`=`)、全部成员 crate 的内联 `version`,并执行 `cargo update -w` 同步 `Cargo.lock`。
- 变更记录位于 `docs/versions/`,**每个发布版本一个独立 Markdown 文件**,命名为 `vX.Y.Z.md`(如 `v0.1.0.md`、`v0.2.0.md`);不再维护单一 CHANGELOG 文件。
- 版本文件按 Keep a Changelog 风格以 `Changed` / `Added` / `Fixed` / `Removed` 等小节记录用户可感知的变更;**发布新版本时新建对应文件**,与版本号改动在同一发布批次提交。
- 发布流程:新增 `docs/versions/vX.Y.Z.md` + 版本号同步(`chore: 版本更新至 X.Y.Z`)→ 打注解 tag `vX.Y.Z` → push 分支与 tag。
