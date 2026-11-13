# DHCPv6 客户端工程计划 (P6b)

> Edgine 网络栈的有状态 / 无状态 IPv6 自动配置能力. 自研 DHCPv6 客户端 (RFC 8415), 仅用 smoltcp 公开 API, vendored 源码零修改.
>
> **实现路径裁定** (用户授权): 相对完整 + 有状态/无状态双形态 + RA M/O 标准触发 (启用 `socket-raw`). 「相对完整」按 §11.3 本项目定义 = 路径/边界/错误分支完整闭合 + 单元测试覆盖 + 可长期稳定运行; **不**追求工业级功能最全 (IA_PD / Rapid-Commit / Confirm / Decline 列为可选或远期).
>
> **登记入口**: [ipv6-dual-stack.md](./ipv6-dual-stack.md) P6b. 本册是其详细分阶段实施方案.
>
> **范式参考**: [dhcp_policy.rs](../../src/kernel/functions/net/dhcp_policy.rs) (DHCPv4 策略纯函数) / [dhcp_policy_test.rs](../../host-tests/tests/dhcp_policy_test.rs) (离线契约+行为测试) / [qemu_slaac_test.sh](../../scripts/qemu_slaac_test.sh) (tap+dnsmasq e2e 基建).

---

## 设计立场

### 与 DHCPv4 的本质差异

DHCPv4 的分工: smoltcp 出**机制** (内置 `dhcpv4::Socket` + 状态机 + 收发), Edgine functions 只出**策略** (`dhcp_policy.rs` 的纯函数 `decide(state, ...) -> DhcpAction`, 不碰 smoltcp 句柄).

DHCPv6 **没有 smoltcp 内置 socket**, 机制必须由 Edgine 自持:

- **functions 层 (0 unsafe)**: 报文编解码 + SARR/租约状态机 + 重传策略. 全部是**字节缓冲上的纯逻辑**, 不依赖 smoltcp 任何具体类型 (比 `dhcp_policy.rs` 更干净, 完全离线可测).
- **privileged 层 (safe 封装)**: UDP 字节管道 (`udp::Socket` bind 546 / 发往 `ff02::1:2:547` / 收) + raw ICMPv6 捕获 RA + `join_multicast_group` + `update_ip_addrs` 落表. 只在此层碰 smoltcp 句柄.
- **vendored 零修改**: 由 `audit_smoltcp_purity.py` 机器化保证 (vendored `src/` SHA256 不变); 仅用公开 API + feature 启用.

### 触发机制: RA M/O 标准触发

smoltcp 的 SLAAC 路径在 `src/kernel/functions/net/smoltcp/src/iface/interface/ipv6.rs:511-533` 把 RA 的 `flags: _` **丢弃**, `process_advertisement` 只收 `src_addr/router_lifetime/prefix_info/now`, 不暴露 M/O 位. 但 `src/kernel/functions/net/smoltcp/src/iface/interface/mod.rs:975-982` 的 `raw_socket_filter` 注释明言「Pass every IP packet to all raw sockets」——RA 在内部 SLAAC 处理**之前**已投递给所有注册的 raw socket.

故: 注册一个 ICMPv6 raw socket 自捕 RA, 用 public `wire::ndisc::RouterFlags::{MANAGED, OTHER}` 解析 M/O 位, 据此驱动有状态 (M=1) / 无状态 (O=1) DHCPv6. **vendored 源码零修改**, 只需 Cargo 启用 `socket-raw` (仿 P6a 启用 `proto-ipv6-slaac`, 属 feature 启用而非源码改动, 合规).

代价: 启用 `socket-raw` 会把 raw socket 代码纳入内核编译面, **TCB 占比上升** (§7.2 需在 PR 描述说明原因与后续降低计划). 这是 RA M/O 标准触发的必要代价, 已裁定接受.

---

## 可行性核实结论 (已 grounded, 非假设)

| 能力 | smoltcp 公开 API | 现状 |
|---|---|---|
| UDP 字节管道 | `udp::Socket::bind/send_slice/recv_slice/can_send/can_recv/set_hop_limit` | ✓ `socket-udp` 已启用 |
| 组播加入 `ff02::1:2` | `Interface::join_multicast_group` | ✓ `multicast` 已启用 |
| 地址落表 | `Interface::update_ip_addrs` | ✓ public |
| RA M/O 解析 | `wire::ndisc::RouterFlags::{MANAGED, OTHER}` | ✓ public bitflags |
| RA 自捕 | `raw_socket_filter` 投递每个 IP 包给 raw socket | ⚠ **需启用 `socket-raw`** (当前未启用) |

当前内核 smoltcp features (`src/kernel/functions/Cargo.toml:21-38`): `medium-ethernet` / `proto-ipv4` / `proto-ipv6` / `proto-ipv6-slaac` / `proto-dhcpv4` / `proto-dns` / `multicast` / `socket-udp` / `socket-tcp` / `socket-dhcpv4` / `socket-dns` 等; **`socket-raw` 与 `socket-icmp` 均未启用**.

地址预算: `iface-max-addr-count-4` — link-local (1) + SLAAC global (1) + DHCPv6 IA_NA (1) = 3, 够用; 多地址/前缀场景需留意上限.

---

## 消息覆盖

| 形态 | 消息 | 范围 |
|---|---|---|
| 有状态 | Solicit / Advertise / Request / Reply (SARR) | ✓ 核心 |
| 有状态 | Renew / Rebind (T1/T2 租约维护) | ✓ **必须** (否则地址到期失效, 非长期稳定) |
| 有状态 | Release | ✓ 完整闭合 |
| 有状态 | Decline / Confirm / Rapid-Commit | 可选 (不强制, §11.3) |
| 无状态 | Information-Request / Reply (取 DNS/选项) | ✓ |
| 触发 | RA M (有状态) / O (无状态) 标志 | ✓ 标准自动触发 |
| 前缀委派 | IA_PD | ✗ 远期, 不在范围 |

---

## 状态机设计

### 有状态 (stateful, IA_NA 地址分配)

`Dhcpv6State` (仿 `DhcpState`, 定义于 `iface_trait.rs`):

```
Idle → Soliciting → Selecting → Requesting → Bound
Bound → Renewing → (Reply) → Bound
Renewing → (T2 到, 无 Reply) → Rebinding → (Reply) → Bound
Rebinding → (lease 到期) → Idle (释放地址, 重启)
Bound → Releasing → Idle
任意 → (不可恢复错误) → Failed
```

| 当前态 | 事件 | 下一态 | 动作 |
|---|---|---|---|
| Idle | RA M=1 / 显式启用 | Soliciting | 发 Solicit → `ff02::1:2:547` |
| Soliciting | 收 Advertise | Selecting | 收集候选, 按 preference 选服务器 |
| Selecting | 选定服务器 | Requesting | 发 Request (含 ServerID) |
| Requesting | 收 Reply (IA_NA+IAAddr) | Bound | `update_ip_addrs` 落表, 记 T1/T2/lease |
| Bound | elapsed ≥ T1 | Renewing | 单播 Renew 到服务器 |
| Renewing | 收 Reply | Bound | 刷新 lease |
| Renewing | elapsed ≥ T2 无 Reply | Rebinding | 组播 Rebind → `ff02::1:2` |
| Rebinding | 收 Reply | Bound | 刷新 lease |
| Rebinding | lease 到期 | Idle | 释放地址, 重启 SARR |
| Bound | 关闭 | Releasing | 发 Release |

### 无状态 (stateless, Information-Request)

```
Idle → (RA O=1) → InfoRequesting → (Reply) → InfoBound
InfoBound → (refresh 定时器) → InfoRequesting
```

仅取 DNS/选项, **不分配地址** (地址仍靠 SLAAC P6a).

### 重传退避

按 RFC 8415 §15: Solicit/Request/Renew/Rebind/Information-Request 各自的最大重传时长 (SOL_MAX_RT / REQ_MAX_RT 等) + 指数退避 + 随机抖动. 重传上限与退避参数落 `policy.rs` 纯函数, 离线可测.

---

## 分阶段计划

### Phase 1: 基建与 feature 启用

- **描述**: 启用 `socket-raw`; 扩展 `iface_trait.rs` 的 `Dhcpv6State` / `SocketKind` / `NetStack` trait / `NetConfig` IPv6 字段, 为 DHCPv6 提供类型与配置边界.
- **方案**:
  - `src/kernel/functions/Cargo.toml` + `src/kernel/privileged/Cargo.toml` 的 smoltcp features 增 `socket-raw` (仿 P6a 启用 `proto-ipv6-slaac`).
  - `iface_trait.rs` 新增 `Dhcpv6State` enum (Idle/Soliciting/Selecting/Requesting/Bound/Renewing/Rebinding/Releasing/InfoRequesting/InfoBound/Failed); `SocketKind` 增 `Dhcpv6` 变体并纳入 `is_internal()`; `NetStack` trait 增 `dhcpv6_state()` 等观察方法 (默认实现仿 `dhcp_state()`).
  - `NetConfig` IPv6 字段**延后** (裁定见详情): 用户已选 RA M/O 标准触发, 无需 NetConfig 显式启用标志; `static_ipv6` 当前无消费者.
- **状态**: [X]
- **详情**: 类型擦除 — `Dhcpv6State` 是 Edgine 自有 enum, functions 层不依赖 smoltcp 类型 (F2). 启用 `socket-raw` 后跑 `audit_smoltcp_purity.py` 确认 vendored `src/` hash 不变 (feature 启用不改源码). TCB 占比变化记入 PR 描述.
  - **已落地**: `socket-raw` 双 `Cargo.toml` 启用 (vendored hash 不变, purity 腿绿); `Dhcpv6State` enum (11 变体 + `is_configured()` / `ipv6()` 观察方法); `SocketKind::Dhcpv6` 纳入 `is_internal()` 并同步 `init/raw.rs` 穷尽臂; `NetStack::dhcpv6_state()` 默认方法 (返回 `Idle`); 单测 `test_dhcpv6_state_configured` + `test_socket_kind_internal` 扩 `Dhcpv6` 断言. 七门槛全绿 (双架构 check/clippy pedantic/fmt 0 漂移; host 单测 1256 过; audit full 未执行腿 0, smoltcp_purity / QEMU 2/2 + feature 4/4 / lockbud 4/4 零发现; host-tests; rustdoc 双架构).
  - **NetConfig IPv6 延后裁定**: 原方案列 `static_ipv6` / DHCPv6 启用策略, 但 (1) 用户已裁定 RA M/O 标准触发而非 NetConfig 显式触发, 启用标志与所选设计相悖; (2) `static_ipv6` 无消费者 (SLAAC P6a 已覆盖 IPv6 寻址); (3) `NetConfig` 字面量构造点约 17 处, 加字段波及面大. 按 §11.3 不预留无消费者扩展点, 待 Phase 4 地址落表确需静态 IPv6 配置时再加.

### Phase 2: wire 编解码 (functions)

- **描述**: `functions/net/dhcpv6/wire.rs` — DHCPv6 报文头 + 选项的纯字节编解码.
- **方案**:
  - 消息头: msg-type (1B) + transaction-id (3B).
  - 选项: ClientID / ServerID (DUID) / IA_NA / IAAddr / StatusCode / OptionRequest / DNS / RapidCommit. TLV (type 2B + len 2B + value) 解析与序列化.
  - API: `parse(&[u8]) -> Result<Repr, ParseError>` + `Repr::emit(&mut [u8]) -> usize`; 全部 `#![deny(unsafe_code)]`, 仅用 `&[u8]` / 整数, 不依赖 smoltcp.
  - 边界闭合: 截断 / 畸形长度 / 未知选项跳过 / 选项重复 / IAAddr  lifetimes 校验.
- **状态**: []
- **详情**: 仿 smoltcp `wire::dhcpv4` 的编解码风格但自研 (DHCPv6 报文格式不同). 离线 round-trip 测试覆盖每个选项 + 畸形输入. 不可信输入解析器, 边界分支必须完整闭合 (相对完整诉求).

### Phase 3: 状态机 + 策略 (functions)

- **描述**: `functions/net/dhcpv6/state.rs` (SARR + 租约维护 + 无状态状态机) + `policy.rs` (重传退避 / T1/T2 策略).
- **方案**:
  - `state.rs`: 事件驱动状态机 — `handle_packet(&[u8])` / `tick(now_ms)` 产生输出指令 (`tx_packets` / `apply_addr` / `apply_options` / `state`). 有状态 SARR + Renew/Rebind/Release; 无状态 Information-Request.
  - `policy.rs`: 纯函数 `decide(state, retry_count, elapsed_ms, lease_ms, cfg) -> Dhcpv6Action` (仿 `dhcp_policy.rs`), 决定重传/续约/退避/放弃. T1=50% / T2=87.5% (RFC 8415).
  - `mod.rs`: 编排 + 公共 API (`Dhcpv6Client::new/poll`), 输出指令交 privileged 执行.
- **状态**: []
- **详情**: 状态机与策略分离 (机制 vs 策略), 均 0 unsafe 纯逻辑. ServerID 匹配分支 / lease 到期 / 重传上限必须有用例. 与 SLAAC 协同: 有状态地址与 SLAAC 地址共存于 `update_ip_addrs` (注意 addr-count-4 预算).

### Phase 4: privileged 封装 + poll 接线

- **描述**: privileged 层提供 UDP 字节管道 + raw ICMPv6 RA 捕获 + 地址落表的 safe 封装, 并接入网络 poll 循环.
- **方案**:
  - UDP socket: bind 端口 546, `join_multicast_group(ff02::1:2)`, `send_slice` 发往 `ff02::1:2:547` (单播 Renew 发往服务器地址), `recv_slice` 收.
  - raw ICMPv6 socket: 注册 `IpProtocol::Icmpv6` raw socket 捕获 RA (Phase 5 解析 M/O).
  - 地址落表: 把 functions 输出的 `apply_addr` 经 `Interface::update_ip_addrs` 落表; DNS/选项落配置.
  - poll 接线: 在网络 poll 循环驱动 `Dhcpv6Client::poll`, 把收到的报文喂入, 把输出指令执行 (仿 DHCPv4 在 `smoltcp_impl.rs` 的驱动位置).
- **状态**: []
- **详情**: 所有 smoltcp 句柄操作在 privileged, unsafe 块配 `// SAFETY:` (F4). functions 只产指令不碰句柄 (F1/F2). 内部 socket (`SocketKind::Dhcpv6`) 用户态不可见.

### Phase 5: RA M/O 触发集成

- **描述**: 解析自捕 RA 的 M/O 标志, 据标准自动触发有状态/无状态 DHCPv6, 与 SLAAC 协同.
- **方案**:
  - privileged raw ICMPv6 socket 收 RA → 交 functions 用 public `wire::ndisc::RouterAdvertisement::parse` + `router_flags()` 解析 `RouterFlags::MANAGED` / `OTHER`.
  - M=1 → 启动有状态 SARR; O=1 → 启动无状态 Information-Request; 与 SLAAC (P6a) 派生地址协同.
  - RA 过滤: 仅处理 type 134 (Router Advertisement), 校验 hop_limit=255 / 源 link-local (仿 smoltcp 内部校验).
- **状态**: []
- **详情**: raw socket 收所有 ICMPv6, 须在 functions 过滤 RA (避免误处理 echo/NDP/MLD). M/O 解析全用 public wire API, vendored 零修改. 与 SLAAC 的 RA 处理并行不冲突 (raw socket 投递在内部处理之前).

### Phase 6: 测试与互操作

- **描述**: 离线单元 + host 集成 + QEMU e2e + 真实 Linux 互操作, 覆盖 §2.3 全门槛.
- **方案**:
  - **离线单元** (`make test-kernel-host`): wire round-trip (每个选项) + 状态机转移表 (含 ServerID 匹配 / lease 到期 / 重传上限) + 边界 (截断/畸形).
  - **host 集成** (`host-tests/tests/dhcpv6_policy_test.rs`): 仿 `dhcp_policy_test.rs` — 契约 (trait 签名/变体/0 unsafe) + 行为 (mock UDP + mock clock 驱动 Solicit→Bound, 断言落表与退避).
  - **QEMU e2e**: 扩展 `scripts/qemu_slaac_test.sh` — dnsmasq 从 `dhcp-range=fd00::,ra-only` 改有状态地址池 + 无状态选项 + 设 M/O 位; 新增 `[net] DHCPv6 bound: <addr> lease=<ms>` 里程碑断言; 断言获取地址后 Ping6/TCP6 收发.
  - **互操作**: radvd + dnsmasq / dhcpd -6, 验证 RFC 8415 互通.
  - **回归**: dual-stack 既有测试 (`net_sockaddr_in6_test.rs` / `net_ipv6_addr_test.rs` / `net_snapshot_test.rs`) 全绿; DHCPv4 路径 (`dhcp_fallback_const_test.rs`) 不受影响; SLAAC (P6a) 不受影响.
- **状态**: []
- **详情**: e2e 为宿主专属手动腿 (需 root/免密 sudo + dnsmasq + iproute2, x86_64-only, aarch64 无 tap 环境), 不进 CI 默认门禁 (与 SLAAC 同现状). 时序 flakiness 风险仿 P-80, 留就绪窗口/重试预算.

---

## 工作量估算

| Phase | 文件 | 新增行数 | 难度 |
|---|---|---|---|
| 1 | Cargo.toml ×2 + iface_trait.rs | +120 | 中 |
| 2 | functions/net/dhcpv6/wire.rs | +350 | 高 (不可信输入解析) |
| 3 | functions/net/dhcpv6/{state,policy,mod}.rs | +450 | 高 |
| 4 | privileged/net/ 封装 + poll 接线 | +200 | 中 |
| 5 | RA M/O 触发集成 | +120 | 中 |
| 6 | host-tests + 单元 + e2e 脚本扩展 | +350 | 中 |
| **总计** | **~10 文件** | **~1590 行** | **相对完整** |

---

## 风险与约束

- **TCB 占比上升**: 启用 `socket-raw` 纳入 raw socket 代码. §7.2 需在 PR 描述说明原因 (RA M/O 标准触发) 与后续降低计划.
- **地址预算**: `iface-max-addr-count-4`, link-local + SLAAC + DHCPv6 = 3; 多地址场景需评估是否调上限.
- **不可信输入解析器**: wire codec 吃网络报文, 边界分支必须完整闭合 + 畸形输入用例全覆盖 (相对完整诉求, 安全敏感).
- **e2e 时序 flakiness**: 重传 / T1/T2 / lease 在宿主高负载下可能抖 (仿 P-80); e2e 宿主专属、x86_64-only、不进 CI.
- **vendored 零修改**: 全程仅公开 API + feature 启用, `audit_smoltcp_purity.py` 机器化保证.
- **与 SLAAC 协同**: 有状态 DHCPv6 地址与 SLAAC 地址共存, RA M/O 触发与 SLAAC RA 处理并行不冲突.

---

## 验证门槛 (§2.3, 每 Phase 完成后必满足)

1. 双架构 `./ci/build.sh all` 0 error / 0 warning.
2. clippy pedantic 0 warning + `cargo fmt --check` 0 漂移 (kernel 双架构三成员 + host-tests + src/user + src/rust + edgine-abi).
3. `bash ci/audit.sh full` 全绿 (含 functions_boundary / safety_coverage / comment_language / smoltcp_purity / dead_code 等).
4. host-tests 全通过 (含新增 DHCPv6 用例).
5. QEMU 集成通过 (e2e 扩展腿, 宿主专属).
6. `make test-kernel-host` 0 failed (三 workspace 成员).
7. `make -f Makefile.ci ci-rustdoc` 双架构全绿.

---

## 关联文档

- [ipv6-dual-stack.md](./ipv6-dual-stack.md) — P6b 登记入口 + dual-stack 重构 (Phase 1-5/7-8 已 [X], P6a SLAAC 已 [X]).
- [dhcp_policy.rs](../../src/kernel/functions/net/dhcp_policy.rs) — DHCPv4 策略纯函数范式.
- [dhcp_policy_test.rs](../../host-tests/tests/dhcp_policy_test.rs) — 离线契约+行为测试范式.
- [qemu_slaac_test.sh](../../scripts/qemu_slaac_test.sh) — tap+dnsmasq e2e 基建 (扩展对象).
- [iface_trait.rs](../../src/kernel/privileged/net/iface_trait.rs) — NetStack trait / SocketKind / NetConfig / DhcpState 主定义.
- [net-e2e-tcp-echo-flakiness.md](./net-e2e-tcp-echo-flakiness.md) — e2e 时序 flakiness 处理范式 (P-80).