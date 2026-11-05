# kcp-ovo 0.8.0 升级验证

日期: 2026-11-05。范围: 在 LongshipX PR #34 的 `0e03c6e` 上追加升级,不修改已有 PR 评论、不合并、不发布、不修改上游。旧版结果保留在 [初次接入记录](kcp-integration.md),不得当作本次重测数据。

## 逐版本应用范围

| 上游版本 | 与本项目有关的变化 | 本项目实际应用与验收 |
| --- | --- | --- |
| [0.5.2](https://github.com/cherish-ltt/kcp-ovo/blob/v0.8.0/docs/rto-backoff-v0.5.2.md) | 修正超时退避和下一次重传期限;fast 为旧 RTO 加一半,normal 为旧 RTO 加 `max(旧 RTO, rx_rto)`。 | 本项目 `stream_mode()` 使用 nodelay/fast,直接经底层 `Kcp::update` 获得修复。新增虚拟时间测试验证 RTO 200/300/450 与期限 200/500/950,并检查到期前不重传。不新增 normal 模式配置,不把修正宣传为所有弱网负载都会更快。 |
| [0.6.0](https://github.com/cherish-ltt/kcp-ovo/blob/v0.8.0/docs/core-perf-2026-11-04.md) | 核心减少输入/单段接收复制、临时发送批次分配、乱序队列重建及重复 ACK 扫描;只在输出成功后提交发送状态。 | 继续直接使用核心 input/recv/update,不复制上游实现。新增单段所有权移动、输入缓冲可复用、乱序去重、重复 ACK 不触发虚假快速重传回归;既有大帧/慢读/弱网/压力覆盖实际网关调用。 |
| [0.7.0](https://github.com/cherish-ltt/kcp-ovo/blob/v0.8.0/docs/stream-v0.7.0.md) | 高层消息 API 新增有界 drain/drain_to、显式 KcpConfig 入口,并修正高层 UDP 输出错误及 ACK/WINS 收尾边界。 | 高层 API 拒绝 stream_mode,不能替换本项目的 AsyncRead/AsyncWrite + TLS 字节流。保持 default-features=false、现有 KcpSettings/flush;新增真实 UDP 输出失败不提交发送状态、收到数据但未输出 ACK 时 flush 不提前成功、取消后恢复回归。不启用不需要的高层 feature。 |
| [0.8.0](https://github.com/cherish-ltt/kcp-ovo/blob/v0.8.0/docs/public-api-v0.8.0.md) | 公共载荷/输出/recv 使用 Vec,包头解析接受切片;移除 Tokio/bytes 重导出,解除二者精确版本约束。 | 包头校验直接借用 UDP 切片,删除整包复制;驱动直接接收并移动 Vec,读完即释放,不 clone 载荷。恢复本项目 main 原有 Tokio 1.53.1 和 bytes 1.12.1,不盲目追最新版本。高层地址解析变化不在本项目调用链内,不迁移。 |

确认等待语义没有扩大:本项目 `flush` 仅等待此前写入数据的 KCP 累积确认,不是高层 drain 的 ACK/WINS 本地输出屏障,更不是应用处理或落盘确认。保持 peer idle、网关写排空期限和 Drop 立即取消;不增加无限等待或强制 graceful drain。TLS close_notify、迟到包隔离与关闭资源回收继续由现有回归验收。TLS 不认证 KCP 控制头,伪造 ACK 仍可能影响可用性或让 flush 失去可信送达含义。

## 实际依赖组合

- 注册表发布包 `kcp-ovo =0.8.0`,无 path/git patch;workspace 仍严格精确版本。
- Rust/MSRV/CI 1.99.1 不变,Tokio `=1.53.1`,bytes 锁定 `1.12.1`,rustls `=0.23.43`,log 保持 `0.4.29`。log 仍受上游 `=0.4.29` 约束,不宣称已完全解除所有依赖耦合。
- Cargo.lock 仅 kcp-ovo/Tokio/bytes 三个包的版本与 checksum 改变;`cargo tree` 确认 workspace 只有一个 Tokio 和一个 bytes 版本。
- 仍使用 System 计数分配器,没有 mimalloc/分配器 FFI;不改变 TCP-only 默认、环境变量、TLS/Bind、房间业务、protobuf 或帧格式。

## 负向对照与新增回归

1. 在旧 `kcp-ovo 0.5.1` 上先加入相同 `stream_config_uses_corrected_fast_timeout_backoff` 测试:exit 101,首个超时后实际 `(xmit,rto,resendts)=(2,400,400)`,期望 `(2,300,500)`。升级后原断言不改。
2. 仅更新依赖、保留旧驱动,`cargo check -p longshipx-net-kit --locked` exit 101:3 处 E0433,`cannot find bytes in kcp_ovo`;适配标准库类型后通过 KCP 定向测试。
3. `crates/net-kit/src/kcp/upstream_tests.rs` 新增 5 项:fast 期限、接收所有权/重排、ACK 去重、取消 flush 恢复、UDP 输出错误。扩展原包边界测试覆盖 conv=0、非法 cmd、MTU 恰好上限、声明载荷短缺与超长包;不新增生产配置或绕过校验。

所有权测试检查单段队列载荷地址与 recv 返回 Vec 地址一致,且复用输入缓冲后内容不变;它只证明该单段路径移动现有分配,不证明 TLS/UDP/多段组装端到端零复制。重复 ACK 测试同时验证选择性 ACK 不能代替 una 累积确认清空 waitsnd。

## 重测命令与环境

本地 macOS arm64 / Rust 1.99.1;默认测试全部 loopback TCP/UDP/TLS + 内存设施。独立目标目录绕开本机 mbx 的失效 target 链接及编译代理,不修改该链接或系统配置。第一次未指定目标目录的旧版测试因 `failed to create directory .../target: Not a directory` 未执行,不计入负向行为证据;改用如下目录后实际复现。`cargo info` 首次因源替换要求指定 registry 失败,补 `--registry crates-io` 后确认发布包。

```bash
export PATH="$(rustup which cargo | xargs dirname):$HOME/.cargo/bin:$PATH"
export RUSTC_WRAPPER=
export CARGO_TARGET_DIR=/tmp/longshipx-kcp080-target
cargo test -p longshipx-net-kit kcp:: --locked
cargo test -p longshipx-gateway --test e2e --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --all-targets --locked
cargo test --workspace --locked --offline
cargo test -p longshipx-net-kit --test completed_connections --locked -- --nocapture
LONGSHIPX_QUICKSTART_CLIENT_BIN="$CARGO_TARGET_DIR/debug/examples/quickstart_client" \
  cargo test -p longshipx-gateway --test e2e quickstart_client_process --locked -- --ignored --nocapture
cargo test -p longshipx-gateway --test e2e kcp_stress:: --locked -- --ignored --test-threads=1 --nocapture
cargo bench -p longshipx-net-kit --bench connection --locked -- \
  net-kit/transport_tls --warm-up-time 0.5 --measurement-time 1
CARGO_TARGET_DIR=/tmp/longshipx-kcp080-coverage \
  CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_OPT_LEVEL=0 CARGO_PROFILE_TEST_OPT_LEVEL=0 \
  cargo llvm-cov --workspace --summary-only --locked --fail-under-lines 80
```

本地原始日志目录 `/tmp/longshipx-kcp080-evidence/` 为临时证据,不保证长期保留;复跑和远端 CI 是持久验收入口。性能测试不与本任务的编译/覆盖率同时运行,但主机不隔离,不将单次测量当作生产收益。

## 本次检查结果

| 检查 | 结果 |
| --- | --- |
| fmt / Clippy `-D warnings` / workspace all-targets build | 全部 exit 0;所有 example/bench 和自定义 System allocator 目标均包含。 |
| KCP 定向回归 | 15 passed,其中新增 5 项;大帧、IPv6、慢读、关闭隔离/端口回收继续通过。 |
| TCP/KCP 网关 E2E | 30 passed / 3 ignored;包含混合房间/配额、鉴权期限/撤销/过期、两级背压、TLS 篡改/重放、丢包/乱序/重复、HTTPS 注册登录黑盒。 |
| 默认全仓离线测试 | **218 passed / 0 failed / 7 ignored**,比旧 PR 多 5 项;7 项为 4 项独立 PG/Redis 回归 + 2 项压力 + 1 项进程冒烟。 |
| 进程冒烟 | 显式执行 1 passed;真实 quickstart_client 分别 TCP/KCP 完成 Bind/profile/room/chat。 |
| 压力/延迟 | 显式执行 2 passed;19200 次并发心跳 + 200 次延迟心跳全部成功,连接结束回收。 |
| 泄漏回归 | 10000 次连接完成后 retained **1296B**,shutdown 后 **160B**;100000B 原阈值不变。 |
| Criterion | 4 组真实 TCP/KCP TLS 帧往返全部完成并校验载荷;数据见下表。 |
| 干净覆盖率 | **90.46% (5719/6322 行)**,218 项默认测试执行;69 个唯一源码路径,无数据不匹配/重复路径警告,80% 门槛 exit 0。不计入 ignored 压力/冒烟的覆盖。 |

覆盖率与旧 PR 0.5.1 的 90.37% 都来自原生工具链、无优化/禁用增量和干净目录,但均为各自版本的独立测量。核心 crate 未回落:

| crate | 覆盖行/总行 | 行覆盖 |
| --- | ---: | ---: |
| application | 1312/1366 | 96.05% |
| domain | 551/600 | 91.83% |
| gateway | 1230/1381 | 89.07% |
| infrastructure | 1077/1159 | 92.92% |
| net-kit | 1158/1201 | 96.42% |
| protocol | 265/280 | 94.64% |
| server-bin | 126/311 | 40.51% |
| migration | 0/24 | 0.00% |

## 本次性能观测

沿用原 PR 装置和放宽的测试防护配置:64/256 连接,每连接 30 次串行心跳,双方同进程 current-thread runtime;计时含 send/flush/业务回执,不含握手/Bind。没有调整生产默认参数或测试阈值。

| 底层 | 连接 | 请求 | p50/p95/p99 (µs) | 请求/s | ΔRSS KiB | Δ活跃堆 B |
| --- | ---: | ---: | --- | ---: | ---: | ---: |
| TCP | 64 | 1920 | 709/2395/2433 | 64304.2 | 4864 | 3117752 |
| TCP | 256 | 7680 | 2755/4999/5610 | 90280.2 | 9824 | 12268592 |
| KCP | 64 | 1920 | 18948/30355/31827 | 2787.3 | 1376 | 4260843 |
| KCP | 256 | 7680 | 20262/32514/40011 | 9776.4 | 3968 | 16231764 |

双向各 20ms 用户态转发延迟、每种传输 100 次心跳、不注入丢包:

| 底层 | p50/p95/p99 (µs) | 请求/s | ΔRSS KiB | Δ活跃堆 B |
| --- | --- | ---: | ---: | ---: |
| TCP | 45837/47612/51999 | 21.8 | 0 | 52571 |
| KCP | 58490/69790/79629 | 17.5 | 0 | 73630 |

Criterion 使用 release、多线程 runtime、每项 10 samples;吞吐按单向有效载荷/往返时间计,不是链路带宽:

| 底层/载荷 | 时间估计区间 | 中间估计 | 有效载荷吞吐估计 |
| --- | --- | --- | --- |
| TCP / 32B | 45.519–46.587µs | 45.956µs | 679.99KiB/s |
| TCP / 65536B | 161.96–164.92µs | 163.47µs | 382.32MiB/s |
| KCP / 32B | 20.944–22.576ms | 21.810ms | 1.4328KiB/s |
| KCP / 65536B | 21.819–24.912ms | 23.419ms | 2.6688MiB/s |

KCP 两组自动延长 measurement,32B 有 1 个 low severe/1 个 high mild outlier;gnuplot 未安装,使用 plotters。没有运行全部历史 Criterion 套件。当前 KCP 的 10ms 更新与 ACK-flush 成本仍明显,不能仅凭上游优化宣称比 TCP 更快。

与旧 PR 表格不是同机同刻 A/B,且本次恢复了 Tokio/bytes,不可把差值单独归因于 KCP 核心优化。Δ堆为同进程两端快照,ΔRSS 受 allocator 复用影响,不能当作单连接内存定额;本次仅据功能、错误边界和所有权测试确认升级实际生效。

## 验收边界

不执行真实 PG/Redis 专用回归、生产完整数据库启动、Windows、公网/NAT、内核包级丢包对照、长期 soak、256 以上连接。用户态 20ms 代理不是等效物理链路;丢包/乱序/重复测试只证明 KCP 正确性。原 PR 已有集成数据和上游自测都不能代替本次下游重测。

署名: GPT-6-Astra-Ultra
