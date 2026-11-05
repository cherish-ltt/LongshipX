# KCP 集成验证记录

本页保留 kcp-ovo 0.5.1 接入时的历史结果,不代表当前依赖的重测结果。当前依赖及逐版本应用说明见 [v0.8.0 升级验证](kcp-upgrade-0.8.0.md)。

日期: 2026-11-03。范围: LongshipX #1,基线 `55b4204` (v0.3.0),本分支新增可选 KCP/TLS;不发布版本、不修改 kcp-ovo。

## 环境与证据口径

- macOS 27.2 / arm64,`rustc 1.99.1 (58cf8f9c3 2026-10-13)`,LLVM 23.1.1,`cargo-llvm-cov 0.9.1`。
- 注册表 `kcp-ovo =0.5.1`,Tokio `=1.52.3`,rustls `=0.23.43`,bytes `1.11.1`,log `0.4.29`;无 path/git patch 上游。
- 默认测试用真实 loopback TCP/UDP/TLS,内存仓储/token,rcgen 证书;HTTPS 黑盒使用真实 Argon2 参数 8192/1/1,不是生产成本参数。没有访问真实 PG/Redis、修改系统网络配置或信任库。
- 性能是共享开发机上的单次观测,并非隔离 benchmark 主机;虽然本任务未并行跑压力与编译,主机上存在其他项目活动,不据此承诺生产容量或稳定收益。
- 本次本地原始日志位于 `/tmp/longshipx-issue1-evidence/` (临时,不保证长期保留);下文保留命令、精确结果与可复跑测试,远端结果以 PR CI 链接为准。

## 原始约束实测

1. 基线 `cargo +1.99.1 test --workspace --locked`:188 passed、4 ignored,未实现 KCP(功能缺失,不是已有功能回归)。
2. 独立最小 Cargo 工程同时依赖 `tokio = "=1.53.1"` 和 `kcp-ovo = "=0.5.0"`:解析 exit 101,上游要求 `tokio =1.52.3`。本分支统一 workspace 并实测,未使用多 Tokio 补丁绕过。
3. 对齐 Tokio 后,最小工程声明 `#[global_allocator] static A: std::alloc::System = std::alloc::System;`,调用 `kcp_ovo::core::kcp::Kcp::new(1, kcp_ovo::KcpConfig::default())`:build exit 101,`global_allocator ... conflicts with global allocator in: kcp_ovo`。关闭上游 default features 仍冲突;已查重关联 [kcp-ovo #36](https://github.com/cherish-ltt/kcp-ovo/issues/36),不重复提交;最终把相同最小工程只改为0.5.1,`cargo run --offline` exit 0。
4. 开发期间上游独立合并 [#37](https://github.com/cherish-ltt/kcp-ovo/pull/37) 并发布0.5.1;本任务重新下载发布包,比较两版src只有lib.rs移除allocator声明,最终升级到0.5.1。删除临时mimalloc FFI适配及依赖,恢复原有两个System计数分配器目标与100000B泄漏阈值;压力装置也使用System计数。Linux footprint 同步修正statm resident字段和实际page size,不再把virtual size当RSS。上游修改由独立工作完成,本任务只消费发布包。

## 测试矩阵

| 层次 | 自动化位置 | 实际覆盖 |
| --- | --- | --- |
| KCP 单元/集成 | `crates/net-kit/src/kcp/tests.rs` (10 tests) | TLS 65536B 往返、ACK flush、重复 flush 错误、无对端 deadline、非法参数/截断包/消息模式包、窗口慢读背压、重复包 flood、session 上限及 idle 回收、关闭唤醒 pending accept、迟到包不可复活、IPv6 与 UDP 端口释放 |
| 准入并发 | `crates/gateway/src/tcp/connections.rs` | 16 线程竞争 4 个全局名额、RAII 归还、0 上限 |
| 网关 E2E | `crates/gateway/tests/e2e.rs` 与 `e2e/kcp.rs` | 保留 TCP 基线,混合 TCP/KCP 同房 profile/heartbeat/join/chat/leave/shutdown,跨传输全局/单 IP 配额,重复连接期限,未 Bind/超大帧/限流,错误/过期/吊销 token,已绑定过期、出站撤销、同玩家多连接与玩家隔离、存储错误/停顿、两级队列过载只断受影响成员 |
| 外部接口黑盒 | `e2e/kcp_blackbox.rs` | 真实 HTTPS 原始请求注册→登录→KCP Bind→档案→再次 HTTPS 登录撤销旧 token;没有直接调用用例替代外部请求 |
| 独立进程冒烟 | `quickstart_client_process_smoke_tcp_and_kcp` | 编译后的真实示例二进制,分别 TCP/KCP exit 0,输出 Bind/profile/room/chat 成功;本项显式执行,默认 ignored |
| 安全与弱网 | `e2e/kcp_network.rs` | 无信任 CA 被拒、明文不能鉴权、TLS 记录篡改(握手及绑定后应用记录)、捕获数据改 conv 重放不能新建 TLS 会话、捕获 UDP 不含 token 明文、双向丢每11包/重复每13包/每7包额外25ms乱序下20个业务心跳成功 |
| 配置装配 | `crates/server-bin/src/bootstrap_tests.rs` | 从真实 Config 装配 TCP-only/KCP-only/both 并完成 TLS;UDP 冲突不会遗留部分 TCP 监听;无真实数据库请求 |
| 压力/资源 | `e2e/kcp_stress.rs` | 64/256 连接 × 每连接30次心跳,双方均真实 TLS/鉴权,全部响应成功,结束回收;另100次/传输单向20ms代理延迟对比 |

弱网代理队列/capture 有界;TCP 对照仅施加字节流转发延迟,**不是 TCP 内核包级丢包/乱序仿真**,不能用它证明 TCP/KCP 在相同丢包网络上的优劣。丢包/重复/乱序这里只验证 KCP 正确性。

## 本地检查结果与复跑

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --all-targets --locked
cargo test --workspace --locked
PATH="$(rustc --print sysroot)/bin:$HOME/.cargo/bin:$PATH" \
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_OPT_LEVEL=0 CARGO_PROFILE_TEST_OPT_LEVEL=0 RUSTC_WRAPPER= \
  cargo llvm-cov --workspace --summary-only --locked

cargo test -p longshipx-net-kit --test completed_connections --locked -- --nocapture
LONGSHIPX_QUICKSTART_CLIENT_BIN="$PWD/target/debug/examples/quickstart_client" \
  cargo test -p longshipx-gateway --test e2e quickstart_client_process --locked -- --ignored --nocapture
cargo test -p longshipx-gateway --test e2e kcp_stress:: --locked -- --ignored --test-threads=1 --nocapture
cargo bench -p longshipx-net-kit --bench connection --locked -- \
  net-kit/transport_tls --warm-up-time 0.5 --measurement-time 1
```

- fmt、Clippy(-D warnings)、all-targets build 均 exit 0;包括所有 bench 和 example targets,没有排除 allocator 冲突目标。
- workspace: **213 passed / 0 failed / 7 ignored**;7 ignored = 4 项专用 PG/Redis 回归(本次未运行) + 2 项压力 + 1 项进程冒烟(后三项已单独通过)。新增25个默认测试入口,多模式参数化覆盖不重复计数。
- 泄漏回归:10000 次连接完成后 retained **1296B**,shutdown 后 **544B**,原阈值100000B不变。
- 优化配置的覆盖报告出现 `180 functions have mismatched data`,清理 workspace 覆盖数据后仍有警告,其92.2%数字不作验收;进一步发现PATH中的mbx Cargo代理仍会影响源码路径/缓存,只清理或设置RUSTC_WRAPPER为空不够,最终使用原生工具链PATH、无优化、禁用增量及干净覆盖目录重测。所有默认测试真实执行,未排除网络模块以抬高覆盖率。

最终干净、原生Cargo覆盖率(0.5.1): **90.37% (5714/6323行)**,无函数数据不匹配或源码路径重复,213项默认测试全部通过。core crate 未明显回落:

| crate | 覆盖行/总行 | 行覆盖 |
| --- | ---: | ---: |
| application | 1311/1366 | 95.97% |
| domain | 551/600 | 91.83% |
| gateway | 1230/1381 | 89.07% |
| infrastructure | 1077/1159 | 92.92% |
| net-kit | 1154/1202 | 96.01% |
| protocol | 265/280 | 94.64% |
| server-bin | 126/311 | 40.51% |

server-bin未覆盖生产数据库/信号/metrics装配,migration默认不连接真实库;总覆盖率包含这些未覆盖行,不作排除。重测前以相同原生PATH运行`cargo llvm-cov clean`,不要复用构建代理生成的覆盖目标。

## Loopback 压力结果

每客户端30次串行 heartbeat,并发客户端数64/256;计时含 send/flush/完整业务响应,不含握手/Bind。两端在一个 current-thread runtime;关闭限流,延长准入/心跳期限、发送队列256、握手并发64,建连分批4以保持单 IP 未鉴权配额8。不是默认生产配置容量测试。

| TLS 底层 | 连接 | 成功请求 | p50 / p95 / p99 (µs) | 请求/s | ΔRSS KiB | Δ活跃堆 B |
| --- | ---: | ---: | --- | ---: | ---: | ---: |
| TCP | 64 | 1920 | 1585 / 2659 / 2819 | 38232.6 | 5232 | 3117944 |
| TCP | 256 | 7680 | 7457 / 10466 / 12528 | 33974.7 | 9872 | 12269360 |
| KCP | 64 | 1920 | 19383 / 30150 / 31063 | 3217.9 | 1616 | 4802455 |
| KCP | 256 | 7680 | 22019 / 39388 / 42135 | 8890.0 | 5760 | 18127420 |

共19200次成功请求;TCP 明显快于当前10ms update/ACK-flush语义下的 KCP。RSS/堆是同进程两端相对快照,后续档位复用 allocator arena,不可由较小 ΔRSS 推断 KCP 每连接内存更小。

## 可控延迟结果

单连接、100次相同心跳,双向各20ms用户态代理延迟,不注入丢包。TCP 设置 NODELAY,KCP MTU1200/window64/interval10ms。

| TLS 底层 | p50 / p95 / p99 (µs) | 请求/s | ΔRSS KiB | Δ活跃堆 B |
| --- | --- | ---: | ---: | ---: |
| TCP | 44138 / 47653 / 55468 | 22.4 | 0 | 52574 |
| KCP | 58390 / 69138 / 81407 | 17.7 | 0 | 82111 |

这是有限样本的观测,不对单个p99差异作优劣结论;UDP 定时器与 TCP 按读取块转发的细节不同,不等价于相同物理链路。

## Criterion 帧往返

release优化、常驻多线程runtime、每次回包断言载荷一致,服务端走同一帧解析/有界发送队列;表中为 criterion 时间估计区间及中间估计,非业务压力的p50。

| 底层/载荷 | 时间估计区间 | 中间估计 | 有效载荷吞吐估计 |
| --- | --- | --- | --- |
| TCP / 32B | 61.619–94.117µs | 75.265µs | 415.20KiB/s |
| TCP / 65536B | 226.39–237.83µs | 230.45µs | 271.21MiB/s |
| KCP / 32B | 22.735–24.198ms | 23.508ms | 1.3293KiB/s |
| KCP / 65536B | 22.698–24.324ms | 23.563ms | 2.6524MiB/s |

每项10 samples;TCP小帧2个outliers(含high severe),大帧1个high mild;KCP各2个outliers(含high severe),两项自动延长measurement。gnuplot缺失用plotters回退。历史criterion自动对比混有开发过程不同依赖/主机争用,其improved/regressed提示不作为收益结论。吞吐按单向载荷/往返时间计,不是链路带宽,未运行全部历史criterion套件。

## 开发中发现并修复

- macOS 已 connect 的 UDP socket 使用 send_to 会报 EISCONN,输出回调据socket连接状态使用send/send_to。
- select 分支禁用也会求值切片表达式,接收缓冲清空时必须同时复位offset,否则产生越界panic;大帧与慢读回归覆盖。
- 对端写半关闭时驱动不能提前停止ACK/接收;TLS大帧往返及flush验证。
- 监听 close 与 pending accept 共用mutex曾有死锁风险,改watch先唤醒accept再join/排空;pending accept与端口释放测试覆盖。
- 过载断连后的旧包曾复活同一conv使客户端EOF超时,改有界closed-route隔离一个idle周期;确定性迟到包及双传输两级过载回归通过。
- 错误flush Future需要在Ready后移除,否则再次flush会poll已完成Future;无对端连续flush错误测试覆盖。
- 测试不再把强制断开的最后一次写入ACK当作必达保证;明确断连观测异步回收,保持健康成员心跳断言而不是放宽业务正确性。

## 未验证与发布边界

未运行真实PG/Redis回归、生产server完整数据库启动、真实公网/内核级丢包对比、长期soak、256以上连接、Windows及移动NAT迁移。Linux由PR CI验收,本地macOS不替代Linux结论。

默认仍TCP-only;启用KCP需开放UDP端口并验证证书。最终0.5.1不再强制分配器;不将过渡0.5.0结果冒充最终版本性能。KCP头/ACK未认证、无wire FIN、session隔离占槽、drop不保交付均为明确限制;详情见README,不宣称安全审计或生产准入完成。

署名: GPT-6-Astra-Ultra
