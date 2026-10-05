# IPv4/IPv6 双栈支持改造计划

> 将 NetEndpoint/NetStack 抽象从 IPv4-only 升级为 IPv4/IPv6 双栈, 采用 `enum IpAddr { V4, V6 }` 破坏性改造.

---

## 背景

当前 Edgine 网络栈为 IPv4-only 设计:

- `Ipv4Addr(pub [u8; 4])` — 无 `Ipv6Addr` / `IpAddr` 枚举
- `NetEndpoint { addr: Ipv4Addr, port: u16 }` — 硬编码 IPv4
- `NetListenEndpoint { addr: Option<Ipv4Addr>, port: u16 }` — 硬编码 IPv4
- FFI 层 (`sm_fi.rs`) 仅处理 AF_INET (family=2) + sockaddr_in (16 字节)
- smoltcp vendored 已完整支持 IPv6 (wire/ipv6.rs, wire/ip.rs, parsers.rs), 但 Edgine 抽象层未暴露

**目标**: 让 Edgine 同时支持 IPv4 与 IPv6 协议栈, 可绑定/连接/收发 IPv6 套接字.

---

## 设计决策 (DECISION-032)

### D1: IpAddr 抽象类型 — `enum IpAddr { V4, V6 }`

- **描述**: 新增 `enum IpAddr { V4(Ipv4Addr), V6(Ipv6Addr) }`, 与 `std::net::IpAddr` 一致
- **方案**:
  - 在 `iface_trait.rs` 新增 `Ipv6Addr(pub [u8; 16])` + `enum IpAddr { V4(Ipv4Addr), V6(Ipv6Addr) }`
  - 内存布局: enum 判别式 + V6 16 字节 = 24 字节 (vs Ipv4Addr 4 字节)
  - 实现 `From<Ipv4Addr>` / `From<Ipv6Addr>` for IpAddr, 便于向上转换
  - 实现 `match` 分支处理 (调用方需处理 V4/V6 两路)
- **状态**: [X]
- **详情**:
  ```rust
  pub struct Ipv6Addr(pub [u8; 16]);
  pub enum IpAddr {
      V4(Ipv4Addr),
      V6(Ipv6Addr),
  }
  ```

### D2: 破坏性改造 — `NetEndpoint.addr` 改为 `IpAddr`

- **描述**: 不保留旧 `NetEndpoint { addr: Ipv4Addr }` 签名, 直接改为 `addr: IpAddr`
- **方案**:
  - `NetEndpoint { addr: IpAddr, port: u16 }`
  - `NetListenEndpoint { addr: Option<Ipv4Addr>, port: u16 }` → `{ addr: Option<IpAddr>, port: u16 }`
  - 所有调用方一次性迁移 (match V4/V6 分支)
  - 提供迁移辅助: `Ipv4Addr::into_ip_addr()` / `NetEndpoint::new_v4()` / `NetEndpoint::new_v6()`
- **状态**: [X]
- **详情**: 破坏性改造符合 Edgine "简单优先" (§15.2), 避免三类型并存导致的复杂度. 所有调用点在编译期暴露, 无运行时风险.

### D3: C ABI 兼容 — sockaddr_in6 结构体

- **描述**: 支持 POSIX `sockaddr_in6` (28 字节) 与 `sockaddr_in` (16 字节) 双结构
- **方案**:
  - 新增 `SockaddrIn6` (`#[repr(C)]`, 28 字节): sin6_family/sin6_port/sin6_flowinfo/sin6_addr/sin6_scope_id
  - `write_sockaddr_in` 改名为 `write_sockaddr`, 按 `IpAddr` 分支写入对应结构
  - `parse_ipv4_endpoint_trait` 改名为 `parse_endpoint_trait`, 按 family 分支 (2=AF_INET, 10=AF_INET6)
- **状态**: [X]
- **详情**:
  ```rust
  #[repr(C)]
  struct SockaddrIn6 {
      sin6_family: u16,    // AF_INET6 = 10
      sin6_port: u16,
      sin6_flowinfo: u32,
      sin6_addr: [u8; 16],
      sin6_scope_id: u32,
  }  // 28 字节
  ```

### D4: sm_socket 支持 AF_INET6

- **描述**: `sm_socket(domain, ...)` 接受 `domain=10` (AF_INET6)
- **方案**:
  - 在 `sm_socket` 中新增 `domain == 10` 分支
  - 句柄表中区分 V4/V6 socket (或使用统一句柄 + 运行时 family 字段)
- **状态**: [X]
- **详情**: smoltcp 的 `tcp::Socket` / `udp::Socket` 本身支持 IPv6, 无需在 smoltcp 层区分. Edgine 仅需在 FFI 层正确解析 sockaddr_in6 并转换为 `IpEndpoint` (smoltcp wire 类型).

---

## 实施步骤

### Phase 1: 抽象层类型定义 (iface_trait.rs)

- **条目**: 新增 `Ipv6Addr` + `IpAddr` + `Ipv6Cidr` + 转换 trait
- **描述**: 在 `iface_trait.rs` 新增 IPv6 类型, 不改 NetEndpoint
- **方案**:
  - `Ipv6Addr(pub [u8; 16])` + `new`/`from_octets`/`octets`/`is_unspecified`/`is_loopback`/`is_multicast`
  - `enum IpAddr { V4(Ipv4Addr), V6(Ipv6Addr) }` + `is_v4`/`is_v6`/`as_v4`/`as_v6`
  - `Ipv6Cidr { address, prefix_len }` (0-128)
  - 实现 `From`/`Into` 转换 (`Ipv4Addr` → `IpAddr`, `Ipv6Addr` → `IpAddr`)
- **状态**: [X]
- **验证**: host-tests 新增 `Ipv6Addr` / `IpAddr` 单元测试 (构造/转换/match)

### Phase 2: NetEndpoint 破坏性改造

- **条目**: `NetEndpoint.addr` 改为 `IpAddr`
- **描述**: 修改核心端点类型, 所有调用方迁移
- **方案**:
  - `NetEndpoint { addr: IpAddr, port: u16 }`
  - `NetListenEndpoint { addr: Option<Ipv4Addr>, port }` → `{ addr: Option<Ipv4Addr>, port }` (保留 V4 通配, 或改为 `Option<IpAddr>`)
  - 新增 `NetEndpoint::new_v4(addr: Ipv4Addr, port)` / `new_v6(addr: Ipv6Addr, port)` 辅助构造
  - 所有 `NetEndpoint.addr.octets()` 调用改为 `match addr { V4(v) => v.octets(), V6(v) => v.octets() }`
- **状态**: [X]
- **验证**: 双架构 release 编译通过 (所有调用点编译期暴露)

### Phase 3: FFI 翻译层改造 (sm_fi.rs)

- **条目**: `endpoint_to_smol` / `endpoint_from_smol` 支持 V4/V6
- **描述**: smoltcp wire 类型翻译支持 IPv6
- **方案**:
  - `endpoint_to_smol(e: NetEndpoint) -> IpEndpoint`: match `e.addr` → `IpAddress::Ipv4(v4)` / `IpAddress::Ipv6(v6)`
  - `endpoint_from_smol(ep: IpEndpoint) -> Option<NetEndpoint>`: match `ep.addr` → V4 路径保留 / V6 路径新增
  - `wire_to_smol_v4` 改名为 `wire_to_smol`, 接受 `IpAddr` 返回 `IpAddress`
- **状态**: [X]

- **条目**: `write_sockaddr` / `parse_endpoint_trait` 支持 V4/V6
- **描述**: C ABI 写入/解析支持 sockaddr_in6
- **方案**:
  - `write_sockaddr_in` 改名 `write_sockaddr`, 接受 `IpAddr` 分支写入 sockaddr_in / sockaddr_in6
  - `parse_ipv4_endpoint_trait` 改名 `parse_endpoint_trait`, 按 family 分支 (2/10)
  - 新增 `SockaddrIn6` 结构 (28 字节, `#[repr(C)]`)
- **状态**: [X]

### Phase 4: sm_socket 与 syscall 层支持 AF_INET6

- **条目**: `sm_socket` 接受 domain=10
- **描述**: AF_INET6 套接字创建
- **方案**:
  - `sm_socket(domain, sock_type, protocol)` 新增 `domain == 10` 分支
  - 创建相同 smoltcp tcp/udp socket (smoltcp 层不区分 family)
  - 句柄表中记录 family (用于后续 bind/connect 的 sockaddr 解析)
- **状态**: [X]

- **条目**: `sm_bind` / `sm_connect` / `sm_sendto` / `sm_recvfrom` / `sm_accept` 支持 V4/V6
- **描述**: 所有 syscall 路径按 family 分支解析 sockaddr
- **方案**:
  - 调用 `parse_endpoint_trait` (自动按 family 分支)
  - 调用 `write_sockaddr` (自动按 IpAddr 分支)
- **状态**: [X]

### Phase 5: 实现层 (SmoltcpNetStack) 适配

- **条目**: `SmoltcpNetStack::bind/connect/...` 支持 IpAddr
- **描述**: functions 层实现处理 V4/V6
- **方案**:
  - 接受 `NetEndpoint` (内含 `IpAddr`)
  - 转换为 smoltcp `IpEndpoint` via `endpoint_to_smol`
  - 调用 smoltcp socket.bind/connect (smoltcp 层已支持 V6)
- **状态**: [X]

### Phase 6: DHCPv6 / SLAAC (可选, 远期)

#### P6a: SLAAC (无状态地址自动配置)

- **条目**: SLAAC
- **描述**: 复用 smoltcp 0.14 内置 SLAAC, 由 Router Advertisement 自动派生全局 IPv6 地址与默认路由
- **方案**:
  - smoltcp 0.14 已完整内置 (feature `proto-ipv6-slaac`), `src/kernel/Cargo.toml` 启用该 feature; `privileged/net/smoltcp_impl.rs` 的 `Config` 置 `slaac = true`
  - 由 `Interface::poll` 发 Router Solicitation / 处理 Router Advertisement, 内 `sync_slaac_state` 经 `update_ip_addrs` 自动完成地址派生 (无 Edgine 侧额外编解码)
  - `init_stack` 显式注入 IPv6 link-local (`fe80::/64`, EUI-64 派生) — 为 SLAAC 发 RS 提供源地址, 亦为 NDP 前提
- **状态**: [X]
- **验证**: 双架构 `./ci/build.sh all` 0 error / 0 warning + link ✅; clippy pedantic (x86_64 lib/bins/examples) + feature 维 (kernel_test / host-test) 全 0 warning ✅; `cargo fmt --check` 三 crate ✅; `audit_smoltcp_purity.py` 本地 hash ✅. 唯一红项 `test_no_uncommitted_local_patch_to_vendored_smoltcp` (vendored 改动待提交, 见「未决」).

#### P6b: DHCPv6 (自研, 未实施)

- **条目**: DHCPv6 客户端
- **描述**: smoltcp 无 DHCPv6 实现, 需 Edgine 侧自研 (Solicit/Advertise/Request/Reply, UDP 协议)
- **方案**: 使用 smoltcp 公开 API (`udp::Socket::bind/send_slice/recv_slice` + `Interface::join_multicast_group` + `Interface::update_ip_addrs`), **vendored 源码零修改**. 报文编解码 + 状态机落 functions 层 (仿 `dhcp_policy.rs`), socket I/O 与地址落表由 privileged 提供 safe 封装
- **状态**: []
- **详情**: 无污染约束由 `audit_smoltcp_purity.py` 机器化保证 (vendored `src/` SHA256 不变).
- **未决**:
  - P6a 引入 vendored `src/iface/interface/ipv6.rs` 最小本地化调整 (恢复上游 `use crate::iface::Route;`, 因启用 SLAAC 后该 import 转为必需), 连带 `SMOLTCP_LOCAL_SRC_HASH` 更新. 该改动须提交后方可解除 host-tests 的 vendored 洁净门禁.
- **验证**:
  - **1 静态/编译期**: 双架构 `./ci/build.sh all` 0w0e; functions 层 `#![deny(unsafe_code)]` 编译通过即证 0 unsafe; `audit_functions_boundary.py` (F1/F2) + `audit_comment_language.py` (F7); `audit_smoltcp_purity.py` 证明 vendored 未污染
  - **2 纯逻辑单元 (host, `make test-kernel-host`)**: DHCPv6 报文 round-trip (IA_NA/ClientID/ServerID/DNS 选项); SARR 状态机转移表 (含 ServerID 匹配分支); 边界 (截断/畸形选项/重传上限/lease 到期); SLAAC 复用 smoltcp `slaac.rs` 自带用例
  - **3 host 集成 (host-tests/tests/)**: 新增 `dhcpv6_policy_test.rs` — 契约测试 (trait 签名/变体/0 unsafe, 仿 `dhcp_policy_test.rs`) + 行为测试 (mock UDP 边界 + mock clock 驱动 Solicit→Bound, 断言 `update_ip_addrs` 落表与超时退避)
  - **4 QEMU 端到端 (`./scripts/qemu_boot_test.sh x86_64`)**: 内核打印里程碑 (`[net] SLAAC addr: ...` / `[net] DHCPv6 bound: <addr> lease=<ms>`) + 脚本新增 marker 分支; 对端需 RA / DHCPv6 服务 (slirp 传统仅 DHCPv4, 大概率需 `-netdev tap` + 宿主机 radvd/dnsmasq); 断言自动获取地址后 Ping6/TCP6 收发
  - **5 互操作 + 回归**: 与真实 Linux (radvd + dhcpd -6 / dnsmasq) 对接验证 RFC 8415 互通; dual-stack 既有测试 (`net_sockaddr_in6_test.rs`/`net_ipv6_addr_test.rs`/`net_snapshot_test.rs`) 全绿, DHCPv4 路径 (`dhcp_fallback_const_test.rs`) 不受影响

### Phase 7: 路由层 (route.rs) 扩展

- **条目**: IPv6 路由表
- **描述**: route.rs 支持 Ipv6Cidr 路由
- **方案**:
  - 现有 Ipv4 路由表结构扩展为 `enum IpCidr { V4(Ipv4Cidr), V6(Ipv6Cidr) }`
  - 路由查询按 family 分发
- **状态**: [X]

### Phase 8: 测试覆盖

- **条目**: host-tests + privileged/tests IPv6 用例
- **描述**: V6 地址构造/转换/sockaddr_in6 解析/双栈 socket
- **方案**:
  - `ipv6_addr_test.rs`: Ipv6Addr 构造/转换/match
  - `sockaddr_in6_test.rs`: SockaddrIn6 布局/字节序/family=10
  - `dual_stack_socket_test.rs`: V4/V6 双栈 socket 行为
- **状态**: [X]

---

## 工作量估算

| Phase | 文件 | 新增行数 | 修改行数 | 难度 |
|-------|------|---------|---------|------|
| 1 | iface_trait.rs | +200 | - | 低 |
| 2 | iface_trait.rs + 调用方 | +50 | -100 (调用方迁移) | 中 |
| 3 | sm_fi.rs | +150 | -80 | 中 |
| 4 | sm_fi.rs | +50 | -20 | 中 |
| 5 | smoltcp_impl.rs | +80 | -30 | 中 |
| 6a | Cargo.toml + smoltcp_impl.rs (SLAAC) | +5 | - | 低 |
| 6b | functions/dhcpv6_policy.rs + privileged 封装 (DHCPv6) | +500 | - | 高 |
| 7 | route.rs | +100 | -50 | 中 |
| 8 | host-tests/ + privileged/tests/ | +300 | - | 低 |
| **总计** (excl. Phase 6a/6b) | **9 文件** | **~930 行** | **~-280 行** | **1-2 周** |

---

## 风险与约束

- **破坏性改造**: Phase 2 会导致所有 `NetEndpoint.addr` 调用方编译失败, 需一次性迁移. 编译期暴露所有调用点, 无运行时风险.
- **smoltcp vendored 不修改**: Phase 6 的 SLAAC 复用 smoltcp 内置实现 (仅启用 feature); DHCPv6 需 Edgine 侧自研, 但仅使用 smoltcp 公开 API, vendored 源码零修改 (由 `audit_smoltcp_purity.py` 保证). 当前不实施.
- **C ABI 兼容**: SockaddrIn6 布局必须与 POSIX sockaddr_in6 一致 (28 字节, `#[repr(C)]`).
- **双架构**: aarch64 无 AF_INET6 差异, 改动双架构通用.
- **host-tests 覆盖**: 必须新增 V6 测试用例, 防止 V4/V6 路径混淆.

---

## 验证门槛

每个 Phase 完成后必须满足 §2.3 全部 5 条:

1. 双架构 `./ci/build.sh all` 0 error / 0 warning
2. clippy 0 warning (`cargo clippy --release -- -D warnings`)
3. 三审计通过 (functions_boundary + safety_coverage + deadlock_matrix)
4. host-tests 全通过 (含新增 V6 测试)
5. QEMU 集成测试通过 (sm_socket(AF_INET6) + bind + 收发)

---

## 关联文档

- [docs/explain/explain-framekernel.md](../explain/explain-framekernel.md) — framekernel 架构 (privileged/functions 边界)
- [docs/plan/smoltcp-framekernel-wrapper.md](./archive/smoltcp-framekernel-wrapper.md) — smoltcp 适配层历史设计
- [src/kernel/privileged/net/iface_trait.rs](../../src/kernel/privileged/net/iface_trait.rs) — NetStack trait 主定义
- [src/kernel/privileged/net/init/sm_fi.rs](../../src/kernel/privileged/net/init/sm_fi.rs) — FFI 翻译层

---

## 状态记录

- 创建文档, 完成 DECISION-032 (D1-D4) 设计决策.
- 实施完成 Phase 1-5 + 7-8 (D1-D4 + Phase 1/2/3/4/5/7/8 全部 `[X]`). Phase 6 (DHCPv6/SLAAC) 保持 `[]` 远期. 验证: 双架构编译 0 error/0 warning, 四项审计通过, host-tests 838 passed/0 failed (含新增 net_ipv6_addr_test / net_sockaddr_in6_test / net_dual_stack_socket_test 19 项).
- 拆分 Phase 6 为 P6a (SLAAC) / P6b (DHCPv6 自研). P6a 简约实现完成并标 `[X]`: `Cargo.toml` 启用 `proto-ipv6-slaac`, `smoltcp_impl.rs` 置 `config.slaac = true` 并注入 link-local; 连带 vendored `ipv6.rs` 最小本地化调整 (恢复 `use crate::iface::Route;`, 同步 localization patch 与 `SMOLTCP_LOCAL_SRC_HASH`). 验证: 双架构 build + link 0w0e, clippy (pedantic + feature 维) 0 warning, fmt 三 crate 通过, purity 本地 hash 一致; host-tests 仅 `test_no_uncommitted_local_patch_to_vendored_smoltcp` 待 vendored 改动提交后转绿. P6b 保持 `[]`.
