# 网络栈功能面收口与双架构观测对齐（档位1+2）

> **定位**：承 `docs/report/network-stack-smoltcp-coverage.md` 的建议推进顺序，落地用户 2026-10-19 裁定"按序推进，先采用软观测，接线 DNS"。档位1 = 协议功能面差集收口（纪律/ICMP/DNS/台账），档位2 = aarch64 virtio-net 数据面**软观测**对齐（warn 不阻断，先拿数据）。档位3（soak 基建复制）不在本 plan，待档位2 首轮数据后另立。
>
> **报告快照更正**：报告问题 #2（SLAAC 未开）与 #4（组播未开）已分别随 P6a（`proto-ipv6-slaac` + `smoltcp_impl.rs config.slaac`）与 D11/DECISION-097（`multicast` + `privileged/net/mcast.rs` 按 socket 引用计数，commit `474295a6`）消解；`auto-icmp-echo-reply` 亦已随 P6a 联调开启（内核自动回 ping，报告 #3 的 ICMP 回显半边已闭）。本 plan 处理剩余差集。
>
> **关联**：`docs/plan/socket-layer-completion.md`（U5 失真项见 S3）；`docs/plan/socket-option-constants-unification.md`（S1/S2 卡 A/B 裁定，本 plan 不碰）；`scripts/qemu_boot_test.sh`（档位2 载体）。

## 1. 源码调研结论（S2 施工锚点）

| 面 | 位置 | 现状 |
| --- | --- | --- |
| 解析入口 | `privileged/net/init/dns.rs` `dns_resolve` | 静态 hosts 表 + IPv4 字面量；**零生产消费者**（api.rs re-export + 测试 only） |
| DNS server 来源 | `raw.rs:614`（DHCP Bound → `PREV_DHCP_DNS` 原子槽）/ `dhcp_dns_servers()` | 租约 option 6 已流动；static fallback 路径无 server |
| smoltcp 客户端 | `functions/net/smoltcp/src/socket/dns.rs`（`Socket::new/update_servers/start_query/get_query_result`）+ iface dispatch `Socket::Dns` 分支 | feature 已开（`proto-dns`/`socket-dns`），socket 从未加入 `SocketSet` |
| 驱动环 | `init.rs` `poll_network` + boot DHCP 等待环（`poll_network` + spin ×500 范式） | `iface.poll` 内部 dispatch 自动驱动 dns socket 收发/超时/重传 |
| 注册点 | `init.rs` Step3 `init_sockets()` + `set_dhcp_handle` 模式 | dns socket 仿 dhcp handle 模式注册 |

## 2. 施工条目

### S0 纪律收口：关闭"启用未用"开关

描述：`socket-raw` / `socket-icmp` 两个 feature 进编译面但零 `Socket` 消费者，虚增 vendored 面积与"支持 raw/ICMP socket"的叙事口径。
方案：从 `src/kernel/Cargo.toml` smoltcp features 移除两项；ICMP echo 应答由已开启的 `auto-icmp-echo-reply`（iface 层）承担，不依赖 `socket-icmp`。验证 = 双架构 0w0e + `audit_smoltcp_purity` + host-tests。
状态：[X]

### S1 ping 可用性（调研即闭合，不施工）

描述：宿主机 ping 内核地址能否被自动应答。
方案：调研确认 `auto-icmp-echo-reply` 已随 P6a 联调开启（`Cargo.toml` 注释自陈验证过），报告 #3 的回显半边已闭；用户态 ping 工具与 ICMP socket 语义不在本档位目标内。
状态：[X]

### S2 DNS 客户端接线（用户点名工程）

描述：把"启用未用"的 `proto-dns`/`socket-dns` 转为真实消费者——`dns_resolve` 升级为三级解析：IPv4 字面量 → 静态 hosts → 真实 DNS 查询（smoltcp `dns::Socket`），失败/无服务器/未初始化回退 `None`（保持既有测试语义）。
方案：① `init.rs` Step3 仿 dhcp 模式 add `dns::Socket::new(&[], [])` 并存 `DNS_HANDLE`；② DHCP Bound 处理点 `update_servers`（租约 server 为空时用 `FALLBACK_DNS`，QEMU slirp 惯例 10.0.2.3，入 `types.rs` 集中常量满足 I-46）；③ `dns.rs` 新增阻塞查询：持锁 `start_query`（`Waker::noop` Context）→ 出锁 → `poll_network` + 自旋轮询 `get_query_result`，有界轮次超时（复用 boot DHCP 等待环形状，持锁等待禁止——同锁域串口教训 P-27）；④ `dns_resolve` 末尾接入 ③，`NET_READY=false` 短路回 None（host 测试语义不变）。SIMPLIFIED 标注：阻塞同步语义（无异步/缓存/search domain/AAAA）、slirp 单 server 惯例。
状态：[X]
详情（实装后回填）：落地 `privileged/net/init/dns.rs`（三级解析 + `dns_query_net` 阻塞有界查询 + `apply_dns_servers`）/ `raw.rs`+`state.rs`（`dns_handle` accessor）/ `init.rs`（Step3 经 `Box::leak` 借出定长查询槽表注册 `dns::Socket`；DHCP Bound 与 static fallback 双路注入 servers）/ `types.rs`（`FALLBACK_DNS=[10,0,2,3]`）.
方案修正（实施时证实）：① smoltcp 0.14 `start_query` 的 `cx` 是 `iface::Context`（= `InterfaceInner`，经 `Interface::context()` 取，供 rand 源 txid/源端口），**不是** `core::task::Context`（方案③原文误写 Waker::noop）；② 查询类型公开路径为 `wire::DnsQueryType`（`socket::dns` 不再导出 `Type`）；③ 无 alloc feature → 槽表经 `Box::leak` 借出 `&'static mut [Option<DnsQuery>]`（同 socket_storage 先例）；④ 实机验证钩子 = `dns-selftest` 诊断 feature（仿 lockdep/e1000-verbose，`KERNEL_FEATURES=` 点亮，boot 尾发一次真实查询只观测不断言）.
验证实录：双架构 `./ci/build.sh all` 6/0；`ci-audit` 全绿（含 pedantic/SMOLTCP 纯度/SAFETY 100%）；kernel-host 单测 986/0（含新增 `test_dns_query_net_host_short_circuit`）；host-tests 全绿；QEMU x86_64 boot+e2e 1/1；实机 `dns-selftest`：DHCP 租约 option 6 → `104.20.23.154`（`dns-selftest: example.com -> 104.20.23.154`，1.086s），实验后镜像已复原生产态（kernel.flat 不含 selftest 符号）。

### S3 U5 组播台账失真同步（§9.2 流程）

描述：`socket-layer-completion.md` U5"无组播"仍标 `[]`，但组播已随 `474295a6` 实装（`mcast.rs` 按 socket 引用计数 + `IP_ADD/DROP_MEMBERSHIP` + `qemu_mcast_test.sh`）。
方案：grep 核实使用路径 → U5 改 `[X]` 并补详情（指向 mcast.rs/commit），重跑文档门槛。
状态：[X]
详情：grep 核实 `sm_fi.rs:93` `IP_ADD_MEMBERSHIP=35` → `mcast.rs` 登记表 → `iface.join_multicast_group` 全使用路径在链；U5 已改 `[X]` 并回填实装详情（纯文档同步，无代码变更）。

### T1 aarch64 virtio-net e2e 软观测（档位2，用户裁定口径 = 软）

描述：aarch64 网络数据面（virtio-net RX/TX + GIC INTx）从未被真实流量验证（boot test 注释自陈"偶发 TX 超时 WARN 属预期"）；x86 侧成熟的 tcp-echo/UDP 回显 e2e 与 liveness 探针全部只跑 x86。本项把最小 e2e 轮挂到 aarch64，**软观测**：结果只记 warn/数据行，不进 RESULT 阻断，先拿失真数据。
方案：① `qemu_boot_test.sh` aarch64 分支 `-netdev` 加 hostfwd（宿主端口与 x86 隔离，两 arch 串行跑不冲突）；② 复用 arch 无关的 `e2e_tcp_echo` 函数体在 aarch64 分支调用，失败仅 `warn`；③ 汇总行输出 `A64_E2E observe=pass/fail` 供台账统计；④ `qemu_liveness_probe.sh` 的 TICK_ADDR 自动解析已 arch 无关，纳入手动观测；⑤ 首轮数据（tick 速率/echo 成败/TX 超时计数）写回本节详情，作为档位3 与 P 系列立项的输入。
状态：[X]
详情（首轮实装+观测回填）：实装于 `scripts/qemu_boot_test.sh`：抽出共享宿主客户端 `e2e_host_client`（x86 硬门禁腿行为不变，已回归 1/1）；新增 `a64_e2e_observe`（TCP echo 入站单腿，SIMPLIFIED 标注；失败只 warn + 数据行，不进 RESULT）；aarch64 主 boot 与观测 boot 均带 hostfwd tcp::8081（与 x86 8080 隔离）；数据行扩展归因字段 `listen=/addr=/tx_timeout=`。首轮实测（最新码镜像）：boot 里程碑全过（含 KPTI-09/SMP/TLB shootdown/EL0 IRQ）；软观测 `A64_E2E observe=fail reason=not_ready listen=0 addr=1 tx_timeout=11`——失真坐实：virtio-net TX 自 0.035s 起持续超时（60s 内 11 次，含 4/8/10s 密集段与 20-60s 每 10s 周期段，周期段疑为 IGMP/MLD report 或 DHCP renew），DHCP 因 TX 不成落 fallback 静态（addr=1，S2 fallback 接线在 aarch64 无死锁验过）；用户态 `[tcp] Listening` 未出现（listen=0，aarch64 用户态 echo 服务链路不可达，待归因：服务未启动 vs 启动卡网络 syscall）. 本观测为档位3/P 系列（virtio-net TX 根因）的输入；qemu_liveness_probe.sh 手动观测按需跑.

## 3. 验收

- S0-S3：§2.3 六门槛全过（双架构 0w0e / clippy+fmt / 核心审计 / host-tests / QEMU x86_64 / kernel-host 单测）；生产路径行为变更仅限 `dns_resolve` 语义扩展（旧输入族输出不变）。
- T1：软观测不改变现有门禁判定（x86 硬门禁原样，aarch64 RESULT 不因 e2e 失败翻转）；首轮 aarch64 数据落 plan。
