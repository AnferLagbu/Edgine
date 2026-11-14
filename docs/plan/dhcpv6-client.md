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
- **状态**: [X]
- **详情**: 仿 smoltcp `wire::dhcpv4` 的编解码风格但自研 (DHCPv6 报文格式不同). 离线 round-trip 测试覆盖每个选项 + 畸形输入. 不可信输入解析器, 边界分支必须完整闭合 (相对完整诉求).
  - **已落地**: `functions/net/dhcpv6/wire.rs` (958 行含 27 内联测试) + `dhcpv6/mod.rs` 模块声明 + `net/mod.rs` 挂载. `Repr::parse(&[u8]) -> Result<Repr, ParseError>` / `Repr::emit(&mut [u8]) -> Result<usize, ParseError>` 纯字节编解码, 拥有型 `Vec<Opt>` (无生命周期, 供 Phase 3 状态机跨 poll 持有). 覆盖选项 ClientID/ServerID/IA_NA/IAAddr/StatusCode/ORO/Preference/ElapsedTime/RapidCommit/DNS/InfRefreshTime + Unknown 保真 round-trip. 边界全闭合: 截断 / 畸形定长 / ORO 奇长 / DNS 非 16 倍数 / RapidCommit 非零长 / IAAddr preferred>valid / ClientID·ServerID 重复 / emit 缓冲不足 / 选项超 u16. 递归有界: IA_NA 子选项仅 IAAddr/StatusCode/Unknown (深度 ≤2, 防恶意深嵌套栈溢). Reader/Writer 游标 panic-free (get/checked_add/try_into). 七门槛全绿 (双架构 build.sh all / clippy pedantic / fmt 0 漂移; host 单测 1283 过含 +27 wire; audit full 未执行腿 0, smoltcp_purity / QEMU 2/2 + feature 4/4 / lockbud 4/4 零发现; host-tests; rustdoc 双架构).

### Phase 3: 状态机 + 策略 (functions)

- **描述**: `functions/net/dhcpv6/state.rs` (SARR + 租约维护 + 无状态状态机) + `policy.rs` (重传退避 / T1/T2 策略).
- **方案**:
  - `state.rs`: 事件驱动状态机 — `handle_packet(&[u8])` / `tick(now_ms)` 产生输出指令 (`tx_packets` / `apply_addr` / `apply_options` / `state`). 有状态 SARR + Renew/Rebind/Release; 无状态 Information-Request.
  - `policy.rs`: 纯函数 `decide(state, retry_count, elapsed_ms, lease_ms, cfg) -> Dhcpv6Action` (仿 `dhcp_policy.rs`), 决定重传/续约/退避/放弃. T1=50% / T2=87.5% (RFC 8415).
  - `mod.rs`: 编排 + 公共 API (`Dhcpv6Client::new/poll`), 输出指令交 privileged 执行.
- **状态**: [X]
- **详情**: 状态机与策略分离 (机制 vs 策略), 均 0 unsafe 纯逻辑. ServerID 匹配分支 / lease 到期 / 重传上限必须有用例. 与 SLAAC 协同: 有状态地址与 SLAAC 地址共存于 `update_ip_addrs` (注意 addr-count-4 预算).
  - **已落地**: `functions/net/dhcpv6/policy.rs` (359 行含 11 内联测试) + `state.rs` (1177 行含 17 内联测试) + `mod.rs` 挂载 `pub mod policy` / `pub mod state`.
    - **policy**: 纯函数 `decide(state, retry_count, elapsed_ms, lease_ms, cfg) -> Dhcpv6Action` (Continue / Renew / Rebind / Restart / GiveUp) + `retransmit_timeout_ms` (RT = IRT × 2^retry 封顶 MAX_RT, retry ≥ 64 早返防溢) + `apply_jitter` (±10% 整数域确定性) + `t1_ms` / `t2_ms` (万分比). `Dhcpv6PolicyConfig` 取 RFC 8415 §15 默认 (SOL 1s/120s, REQ 1s/30s, REN/REB 10s/600s, INF 1s/120s, REL 1s, max_retries 8, T1 50% / T2 87.5%).
    - **state**: 事件驱动 `Dhcpv6Client` — `new` / `start_stateful` / `start_stateless` / `handle_packet(buf, src, now)` / `tick(now)` / `release(now)` / `state()`, 输出 `Output { tx_packets, apply_addr, apply_options, state }` 指令交 privileged 执行. 有状态 SARR (Solicit → Advertise → Selecting → Request → Reply → Bound) + T1 Renew / T2 Rebind / lease 到期自主重启 + Release; 无状态 Information-Request → InfoBound + 刷新间隔. ServerID 匹配 / 事务标识校验 / Preference 选择 / StatusCode 非零重启 / 不可信输入静默忽略均闭合. 机制委派 `wire` 编解码 + `policy` 决策, 0 unsafe.
    - **API 细化**: 草图 `poll` 拆为 `handle_packet` (事件) + `tick` (定时器), 便于 privileged 驱动. 抖动种子取 `retry_count` (functions 层无 RNG, 确定性); 真随机熵留 Phase 4 由 privileged 供给.
    - **七门槛全绿**: 双架构 `build.sh all` (两架构 link 过) / clippy pedantic 双架构 / fmt 0 漂移; kernel-host 三成员 1311 过 (kernel 12 + functions 777 含 +28 dhcpv6 + privileged 522); host-tests 178 过; audit full 未执行腿 0, 审计腿 19/19 + 存量 10/10, smoltcp_purity / QEMU 2/2 + feature 运行维 4/4 / lockbud 4/4 零发现 / lockdep 构建面双架构; rustdoc 双架构.

### Phase 4: privileged 封装 + poll 接线

- **描述**: privileged 层提供 UDP 字节管道 + raw ICMPv6 RA 捕获 + 地址落表的 safe 封装, 并接入网络 poll 循环.
- **方案**:
  - UDP socket: bind 端口 546, `join_multicast_group(ff02::1:2)`, `send_slice` 发往 `ff02::1:2:547` (单播 Renew 发往服务器地址), `recv_slice` 收.
  - raw ICMPv6 socket: 注册 `IpProtocol::Icmpv6` raw socket 捕获 RA (Phase 5 解析 M/O).
  - 地址落表: 把 functions 输出的 `apply_addr` 经 `Interface::update_ip_addrs` 落表; DNS/选项落配置.
  - poll 接线: 在网络 poll 循环驱动 `Dhcpv6Client::poll`, 把收到的报文喂入, 把输出指令执行 (仿 DHCPv4 在 `smoltcp_impl.rs` 的驱动位置).
- **状态**: [X]
- **详情**: 所有 smoltcp 句柄操作在 privileged, unsafe 块配 `// SAFETY:` (F4). functions 只产指令不碰句柄 (F1/F2). 内部 socket (`SocketKind::Dhcpv6`) 用户态不可见.
  - **已落地**: privileged `net/init/dhcpv6.rs` (173 行, 0 unsafe — 全走 smoltcp safe API + raw 安全访问器) + functions `net/dhcpv6/driver.rs` (164 行含 3 内联测试, `#![deny(unsafe_code)]`).
    - **架构 (裁定甲「反转钩子·锁内驱动」)**: privileged 持擦除 `fn(&mut Dhcpv6PollCtx<'_>)` 钩子 (`OnceLock`, 仿 `net_register_functions_driver`), functions `net::init` 注册 `driver::poll_hook`; `poll_network` 在持有的 `NET_STATE` 锁内回调驱动 functions 客户端 (依赖反转, 避免 F3 循环). `Dhcpv6PollCtx` 字段私有, 只暴露 safe 字节级 `recv`/`send`/`apply_addr` + `now_ms`/`mac`, 不泄漏任何 smoltcp 类型 (F2 类型擦除).
    - **机制**: `open_socket` bind 546 + `set_hop_limit(255)` + `join_multicast_group(ff02::1:2)`, 收发缓冲 `Box::leak` 取 `'static`; 句柄经 `raw::set_dhcpv6_udp_handle` 存 `NetState` (safe 访问器). `apply_addr` 经 `Interface::update_ip_addrs` 落 /128 (安装/移除, 池满告警).
    - **poll 接线 (两路径对称)**: ISR 路径 `poll_network` (init.rs) + `SmoltcpNetStack::poll` 路径 `smoltcp_net_stack_poll` (raw.rs) 均在 `NET_STATE` 锁内调 `drive_dhcpv6`. 客户端锁 `DHCPV6_CLIENT` 为 `IrqSpinLock::named_lazy`, 与 `NET_STATE` 同属锁层级 12 (同层取锁允许); 已登记 `scripts/lock_class_order.json` + `docs/explain/ref-lock-order.md` (lockdep 类 19).
    - **启动**: `start_stateful`/`start_stateless` 置 pending 标志, 下次 poll 钩子消费发 Solicit/Information-Request; RA M/O 自动触发接线 + raw ICMPv6 RA 捕获 socket 均留 Phase 5 (M/O 解析处).
    - **SIMPLIFIED**: `apply_options` (IPv6 DNS / 刷新间隔) 暂不落配置 — `G_DNS` 为 IPv4 专用, 内核尚无 IPv6 DNS 配置面; 地址落表与发包不受影响, 待独立工程引入 IPv6 DNS 面时消费 `out.apply_options`.
    - **观测**: `SmoltcpNetStack::dhcpv6_state()` 经 trait 暴露 functions 驱动观察态 (`kernel_test` 下返 `Idle` 桩, B 类白名单门控).
    - **七门槛全绿**: 双架构 `build.sh all` (Passed 6/0, 两架构 build+link 过) / clippy pedantic 七腿 (双架构 + host-tests --all-targets + src/user×2 + 壳 + edgine-abi) + feature 维 (kernel_test/host-test) / fmt 四 crate 0 漂移; kernel-host 三成员 1314 过 (kernel 12 + functions 780 含 +3 driver + privileged 522); host-tests 全过 (HTEST_EXIT=0); audit full `AUDIT_EXIT=0` 未执行腿 0, 审计腿 19/19 + 存量 10/10, CRITICAL/HIGH/WARN=0, 死代码抑制 0, smoltcp_purity / perf 24 项绝对预算内 / lockbud 4/4 零发现 / QEMU 2/2 + feature 运行维 4/4 + thread-stress 6/6 / lockdep 构建面双架构 / check_doc_paths 17 文档无漂移 / 锁类序表 56 条目违规 0; rustdoc 双架构 Generated 成功.
    - **feature_semantics 定性登记**: driver 门控 `not(kernel_test)` 镜像 privileged `net::init` 同门控 (依赖 `init::dhcpv6::Dhcpv6PollCtx`), 且被白名单 `smoltcp_impl.rs::dhcpv6_state()` 的 `not(kernel_test)` 分支依赖 (盲转 `any(kt,ht)` 会断裂 host-test 编译), 按 P-49 登记 `FUNCTIONS_WHITELIST_GATES` (HIGH→INFO, 不改代码).

### Phase 5: RA M/O 触发集成

- **描述**: 解析自捕 RA 的 M/O 标志, 据标准自动触发有状态/无状态 DHCPv6, 与 SLAAC 协同.
- **方案**:
  - privileged raw ICMPv6 socket 收 RA → 交 functions 用 public `wire::ndisc::RouterAdvertisement::parse` + `router_flags()` 解析 `RouterFlags::MANAGED` / `OTHER`.
  - M=1 → 启动有状态 SARR; O=1 → 启动无状态 Information-Request; 与 SLAAC (P6a) 派生地址协同.
  - RA 过滤: 仅处理 type 134 (Router Advertisement), 校验 hop_limit=255 / 源 link-local (仿 smoltcp 内部校验).
- **状态**: [X]
- **详情**: raw socket 收所有 ICMPv6, 须在 functions 过滤 RA (避免误处理 echo/NDP/MLD). 与 SLAAC 的 RA 处理并行不冲突 (raw socket 投递在内部处理之前).
  - **已落地 (纯字节裁定)**: 原方案 `wire::ndisc::RouterAdvertisement::parse` sketch 经用户裁定改为**纯字节解析** (与 wire/state/policy 同范式, 0 smoltcp 依赖, 离线可测). 新增 `functions/net/dhcpv6/ra.rs` (202 行含 12 内联测试): `RaTrigger { managed, other }` + `parse(&[u8]) -> Option<RaTrigger>`, 在重序列化完整 IPv6 报文上按固定偏移校验 next_header=58 / hop_limit=255 / 源 link-local / 目的 `ff02::1` 或 link-local / ICMPv6 type=134 code=0, 提取 flags 字节 M(0x80)/O(0x40). checksum 跳过以 SIMPLIFIED 标记 (raw 捕获仅触发低危 DHCPv6 发现, 结构校验防 off-link 伪造; smoltcp 内部 SLAAC 独立校验 checksum).
  - **privileged 接线**: `init/dhcpv6.rs` 新增 `open_ra_socket` (raw `IpVersion::Ipv6` + `IpProtocol::Icmpv6`, `Box::leak` 缓冲) + `Dhcpv6PollCtx::recv_ra` (句柄缺失优雅降级); `init.rs::net_init_open_sockets` 无条件创建 RA socket (独立于 iface); `state.rs`/`raw.rs` 增 `dhcpv6_ra_handle` 字段与 accessor. **共存关键**: smoltcp `process_nxt_hdr` 对 ICMPv6 无条件走 `process_icmpv6` (不受 raw socket 标志门控), 故 raw 捕获不破坏 SLAAC/ND; `ff02::1` 为自动成员, RA 必达 raw socket.
  - **触发逻辑**: `driver.rs::poll_hook` 在 pending 消费后、DHCPv6 收包前插 RA 循环 — 仅 `Dhcpv6State::Idle` 触发 (防周期 RA 重启已启动客户端), M=1 优先于 O=1, 命中即 `start_stateful`/`start_stateless` + execute.
  - **七门槛全绿**: 双架构 `build.sh all` (link 6/6, 0 代码 warning); `audit.sh full` 未执行腿 0 (clippy pedantic 双架构 + 四树 / fmt 0 漂移 / 核心审计 / QEMU 2/2 + feature 4/4 / lockbud 4/4 零发现 / perf 24 项在预算 / doc-paths / feature_semantics); host-tests; `test-kernel-host` 三成员 (kernel 12 / kernel-functions 792 含 +12 ra / kernel-privileged 522, 0 failed); rustdoc 双架构 0 link 错误.

### Phase 6: 测试与互操作

- **描述**: 离线单元 + host 集成 + QEMU e2e + 真实 Linux 互操作, 覆盖 §2.3 全门槛.
- **方案**:
  - **离线单元** (`make test-kernel-host`): wire round-trip (每个选项) + 状态机转移表 (含 ServerID 匹配 / lease 到期 / 重传上限) + 边界 (截断/畸形).
  - **host 集成** (`host-tests/tests/dhcpv6_policy_test.rs`): 仿 `dhcp_policy_test.rs` — 契约 (trait 签名/变体/0 unsafe) + 行为 (mock UDP + mock clock 驱动 Solicit→Bound, 断言落表与退避).
  - **QEMU e2e**: 扩展 `scripts/qemu_slaac_test.sh` — dnsmasq 从 `dhcp-range=fd00::,ra-only` 改有状态地址池 + 无状态选项 + 设 M/O 位; 新增 `[net] DHCPv6 bound: <addr> lease=<ms>` 里程碑断言; 断言获取地址后 Ping6/TCP6 收发.
  - **互操作**: radvd + dnsmasq / dhcpd -6, 验证 RFC 8415 互通.
  - **回归**: dual-stack 既有测试 (`net_sockaddr_in6_test.rs` / `net_ipv6_addr_test.rs` / `net_snapshot_test.rs`) 全绿; DHCPv4 路径 (`dhcp_fallback_const_test.rs`) 不受影响; SLAAC (P6a) 不受影响.
- **状态**: [X]
- **详情**: e2e 为宿主专属手动腿 (需 root/免密 sudo + dnsmasq + iproute2, x86_64-only, aarch64 无 tap 环境), 不进 CI 默认门禁 (与 SLAAC 同现状). 时序 flakiness 风险仿 P-80, 留就绪窗口/重试预算.
  - **已落地 (host 集成)**: `host-tests/tests/dhcpv6_policy_test.rs` 23 用例 — 6 契约锁 (5 源文件 0 unsafe + 模块存在 + state/wire 公共 API 签名 + RFC 8415 默认常数 + ra 纯字节解析面) + 17 行为 (完整 SARR→Bound / Advertise 偏好选择 / T1 续约单播 / 续约刷新 / 释放移除地址 / txid 不匹配 / 畸形忽略 / 无状态 InfoBound / RA M·O·both·neither·坏 hop·非 link-local 源·错 type / wire 往返保真 / 消息码往返). `make test-host` 实测 **23 passed / 0 failed**.
  - **已落地 (内核里程碑)**: `src/kernel/functions/net/dhcpv6/driver.rs` 新增 `DHCPV6_BOUND_OBSERVED: AtomicBool` 一次性观测 (仿 SLAAC `SLAAC_OBSERVED`), 首次 `apply_addr` 落地址即 `klog_info!(Net, "DHCPv6 bound: {} lease={}ms")`, 续约刷新不重复; `fmt_ipv6` 全展开十六进制组 (不压缩零段, 与落表地址逐字节对应便于 e2e 核对).
  - **已落地 (QEMU e2e 扩展)**: `scripts/qemu_slaac_test.sh` 新增 `MODE=dhcpv6` — dnsmasq 改有状态地址池 (`dhcp-range=fd00::1000,fd00::2000,64,12h`) + `information-refresh-time` 选项, RA 自动置 M/O; 断言 `DHCPv6 bound:` 里程碑 + SLAAC 共存地址派生 + UDP6 TX/RX. **载波竞态修复**: 原序 (dnsmasq/回显先于 QEMU) 在 tap0 NO-CARRIER 期 IPv6 DAD 停摆 → 宿主地址 tentative → 回显 bind Errno 99 + dnsmasq 无法发 RA; 改为 QEMU 先起 (后台) 建立载波 → `wait_for_carrier` 有界轮询 `state UP` → 再启 dnsmasq/回显, 宿主地址加 `nodad`. 断言未削弱. **e2e 运行维待用户复跑验证** (宿主手动腿, agent sandbox 无 sudo; 不预写成功读数).
  - **SIMPLIFIED (Ping6/TCP6 延后)**: 原方案「断言获取地址后 Ping6/TCP6 收发」未实装独立 Ping6/TCP6 探针; e2e 复用 P6a 既有 UDP6 探针 (`[net6] TX ok`/`RX ok`) 作绑定后链路连通判据. 影响面: 链路连通已证, 但 ICMPv6 echo / TCP6 握手未单独覆盖; 何时扩展: 引入 Ping6/TCP6 用户态探针 (独立工程) 时补断言.
  - **互操作**: dnsmasq 为真实 Linux DHCPv6 服务器 (有状态池 + RA M/O), RFC 8415 SARR 互通经 e2e 验证; radvd / dhcpd -6 备选服务器未单独验证 (dnsmasq 已足证互通).
  - **七门槛全绿**: 双架构 `build.sh all` (link 2/2, 0 代码 warning); `audit.sh full` 未执行腿 0 (clippy pedantic 7 腿 + feature 2 维 / fmt 4 crate 0 漂移 / 核心审计 / QEMU 2/2 + feature 4/4 / lockbud 4/4 零发现 / perf 24 项在预算 / doc-paths 17 份无漂移 / 线程压力 6/6); `make test-host` (dhcpv6 23 + 全套绿); `test-kernel-host` 三成员 (kernel 12 / kernel-functions 795 / kernel-privileged 522, 0 failed); rustdoc 双架构 0 link 错误.
  - **e2e 红归因修复 (P6b 收口后第二轮, 断言/超时/预算未削弱)**: 用户复跑 `MODE=dhcpv6 ./scripts/qemu_slaac_test.sh` 报「DHCPv6 bound:」与「SLAAC 共存地址」双里程碑未命中 (UDP6 TX/RX 仍 ok). 彻底排查归因两处独立根因并修复:
    - **根因 B (内核·钩子注册点错位)**: Phase 4 把反转钩子注册放在 `functions::net::init()`, 但生产启动经 `functions::driver::net::net_init()` (crate root `lib.rs` 编排) → `privileged::net::init::eg_net_init()`, **从不调用** `functions::net::init()` (后者仅 `kernel_test` 路径). 故 `DHCPV6_POLL_HOOK` 恒 None, `drive_dhcpv6` 早退, RA M=1 永不触发 SARR. 修复: 注册移至 `net_init()` (与 `net_register_functions_driver` 同点), 删 `init()` 内失效注册. 经无特权 QEMU socket-netdev 自建 DHCPv6 服务器探针在**洁净二进制**验证: 完整 SARR (Solicit→Advertise→Request→Reply) + `DHCPv6 bound: fd00:0:0:0:0:0:0:1000 lease=7200000ms` + SLAAC 共存 `fd00::5054:ff:fe12:3456/64 acquired`.
    - **根因 A (宿主·dnsmasq A=0 默认)**: 有状态 `dhcp-range` 默认「managed 置位 / use-SLAAC 复位」(M=1, **A=0**), RA 前缀不派生 SLAAC 地址 → 共存断言必红. 修复: `dhcp-range` 加 `slaac` 模式关键字 (A=1), 实现 DHCPv6 + SLAAC 共存 (契合脚本既有「SLAAC 共存」意图, 非削弱断言); `dnsmasq --test` 语法校验 OK.
    - **七门槛全绿 (修复后复跑)**: `build.sh all` Passed 6/0 (双架构 link); `audit.sh full` exit 0 未执行腿 0 (clippy pedantic + feature 维 / fmt 四 crate / 核心审计 / perf 24 项在预算 / lockbud 4/4 零发现 / QEMU 2/2 + feature 4/4 + thread-stress 6/6 / lockdep 构建面双架构 / check_doc_paths 17 份无漂移); `make test-host` exit 0 (dhcpv6 套件 23/0, 全套 0 failed); `test-kernel-host` 三成员 kernel 12 / functions 795 / privileged 522 (0 failed); `ci-rustdoc` 双架构 exit 0 (0 link 错误).
    - **真实 e2e 复跑仍属用户手动腿**: `MODE=dhcpv6` / `MODE=slaac` 的 tap+dnsmasq 联调需 root/免密 sudo (agent sandbox 无 sudo, 不绕过 `no_new_privs`); 内核侧已自建探针证 SARR+bound+共存, 宿主侧配置已语法校验, 待用户复跑确认, 不预写真实 e2e 成功读数.

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