# Socket 层深度补全计划（TCP/UDP）

> TCP 端到端当前完全不可用（`bind`/`listen`/`accept`/`connect` 四处硬编码或空壳）；UDP 核心收发路径已通，但连接态与选项类存在简化缺口。本计划按「相对完整」路径合并补全 TCP/UDP socket 层，smoltcp vendored 零修改。
> **重写**：口径由「A 路径（非阻塞 + 状态语义）」升级为「相对完整」——纳入阻塞睡眠、组播、`bind(port = 0)` 自动端口与 poll 就绪接线至 `fs` 层；accept 交接算法由「迁移 + 重臂」改为「先建后换」（消除监听槽空闲窗口）；新增 D8b / D10 / D11 与决策记录章节。

---

## 背景

Edgine 网络栈已通过 [ipv6-dual-stack.md](./ipv6-dual-stack.md)（DECISION-032）完成 IPv4/IPv6 双栈抽象层改造，`sm_socket` / `sm_bind` / `sm_sendto` / `sm_recvfrom` 等 FFI 入口按 sockaddr 族分流可用。但 socket 层存在两处结构性欠账：

- **TCP 全链路不可用**：`bind` / `listen` / `accept` / `connect` 四个环节分别被"缺分支""硬编码 port=0""空壳返回""硬编码 local port=0"阻断，任何 TCP 客户端或服务端都无法建立连接。
- **UDP 只覆盖简化子集**：SLAAC 场景下 UDP6 回显端到端已验证，但"已连接 UDP"语义缺失、`getpeername` 恒失败、`recvfrom` 系统调用丢弃对端出参、无临时端口分配、无组播。

---

## 目标

在不修改 smoltcp vendored 源码的前提下，把 socket 层补全到「相对完整」程度：

- TCP 客户端/服务端可完整建链、收发、`accept`、半关闭，且 `accept` 循环可长期运行（FD 可回收）。
- UDP 具备连接态、完整对端语义（`getpeername` / `recvfrom` 出参 / 通配族如实回填）与组播能力。
- `poll` / `ppoll` 对 socket fd 返回真实就绪位（可读/可写/挂断/错误），并接入 `fs` 层系统调用路径。
- 阻塞 I/O 语义可用：默认阻塞，`SOCK_NONBLOCK` 与 `fcntl(F_SETFL)` 可控，未就绪时经等待队列睡眠而非忙轮询。

**范围口径（相对完整）**：路径 / 边界 / 错误分支完整闭合 + 单元测试覆盖 + 可长期稳定运行。**不**追求"工业级功能最全"或"对齐 Linux 全部语义"。

**明确排除项**（破 smoltcp 零修改门禁或超出口径）：

- 精确 `SO_ERROR` 真值 → 仅由 `state()` 近似派生（D8）。
- 精确 `ECONNREFUSED` → TCP 连接错误近似统一映射（D5）。
- UDP `MSG_PEEK` → smoltcp `recv_slice` 无窥视语义，不纳入。

---

## 现状与缺口

### TCP 缺口

- **G1: `sm_bind` 无 TCP 分支**
  - 描述：TCP 无法绑定本地端口，`bind` 恒失败，后续 `listen` 无从取得端口。
  - 方案：新增 `fd_type == 1` 分支（D3）。
  - 状态：[X]
  - 详情：原 `match raw::fd_type(fd)` 仅 `2 =>`（UDP）分支调 `sock.bind(endpoint)`，TCP 落 `_ => -E_NOTSUPP`。注：smoltcp `tcp::Socket` **无** `bind`，故 TCP 的 bind 只能是 Edgine 侧登记（D3）。P2 已落地 TCP `1 =>` 分支（[sm_fi.rs:485-505](../../src/kernel/privileged/net/init/sm_fi.rs)），纯登记本地端点，供 D4/D5 复用。

- **G2: `sm_listen` 硬编码 `port = 0`**
  - 描述：`listen` 恒返回 `-E_ADDRINUSE`，监听器无法建立。
  - 方案：从端点表读取真实本地端口（D4）。
  - 状态：[X]
  - 详情：原 [sm_fi.rs:409-415](../../src/kernel/privileged/net/init/sm_fi.rs) 硬编码 `IpListenEndpoint { addr: None, port: 0 }` 后调 `sock.listen(local)`；smoltcp `listen` 要求 `port != 0`（[tcp.rs:942](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)），故恒 `Err`。P2 已落地：改由 D1 local 端点（缺省走 D2 临时端口并回写）构造监听端点（[sm_fi.rs:545-570](../../src/kernel/privileged/net/init/sm_fi.rs)）。

- **G3: `sm_accept` 为空壳**
  - 描述：即使有连接到达也无法交付给用户态。
  - 方案：实现"分配新 fd + 建立新监听 + 索引交换交接会话 + 回写对端"（D6）。
  - 状态：[X]
  - 详情：P3 已落地 D6「先建后换」交接（`sm_accept_locked`）：门控 1 接受 `Established` 与 `CloseWait`（后者覆盖客户端 send 后立即 `shutdown(SHUT_WR)`、FIN 在 accept 轮询前到达的场景——只认 `Established` 会让监听槽永久卡死返回 `-E_AGAIN`）；随后槽间 O(1) 交换 handle 与 TCP 缓冲，监听槽换新监听 socket 持续可 accept，新 fd 承载已连接会话；`FdPlan::SMOLTCP` 基址改 64（方案 C）消除与 VFS `[0,64)` 命名空间重叠，fd→槽位经 `sm_slot`/`sm_alloc_slot` 单点换算；`close_syscall` 按 `subsystem_of == Smoltcp` 分流 `sm_close`，FD 归还后 accept 循环复用同一编号（回归：QEMU e2e `accepted fd` 去重恰为 1）。

- **G4: `accept` 系统调用忽略对端出参**
  - 描述：用户态 `accept` 拿不到新连接的对端地址。
  - 方案：透传 `addr`/`addrlen` 到 `sm_accept` 并在其中回写（D6）。
  - 状态：[X]
  - 详情：P3 已落地：`accept_syscall` 透传 `addr_ptr`/`addrlen_ptr` 至 `sm_accept`（[syscall.rs](../../src/kernel/privileged/net/syscall.rs)），`sm_accept_locked` 步骤 6 从已交接会话回写对端地址（沿用 getsockname/getpeername 直写约定，二者可为 NULL）；失败返回值按精确 errno 透传（G8 口径），不再统一压 `EBADF`。

- **G5: `sm_connect` 硬编码 local `port = 0`**
  - 描述：TCP 无法发起连接，`connect` 恒返回 `-E_CONNREFUSED`。
  - 方案：从端点表读取（或临时端口分配器分配）本地端口（D5）。
  - 状态：[X]
  - 详情：原 [sm_fi.rs:496-503](../../src/kernel/privileged/net/init/sm_fi.rs) 硬编码 `IpListenEndpoint { addr: None, port: 0 }` 后调 `sock.connect(iface.context(), endpoint, local)`；smoltcp `connect` 要求 `local.port != 0`（[tcp.rs:1013](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)），故恒 `Err`。P2 已落地：local 由 D1 端点表提供（缺省走 D2 分配并回写）（[sm_fi.rs:652-687](../../src/kernel/privileged/net/init/sm_fi.rs)）。

- **G6: `shutdown` 等同 `close`**
  - 描述：无半关闭语义，`SHUT_WR` 无法只关发送侧。
  - 方案：新增 `sm_shutdown(fd, how)` 并透传 `how`（D7）。
  - 状态：[X]
  - 详情：见 [syscall.rs:513-519](../../src/kernel/privileged/net/syscall.rs) —— `shutdown_syscall(fd, _how)` 忽略 `_how`，直接调 `sm_close`。

- **G7: poll 就绪链路整条无消费者**
  - 描述：`sm_poll_sockets` 恒返回 `0`，且 `poll_syscall` 对 socket fd 恒报可读/可写，用户态 `poll` 拿不到真实就绪位。
  - 方案：新增 `sm_socket_poll(fd, events)` 按 `can_recv`/`can_send` 派生 revents，并在 `fs` 层 `poll_syscall` 经 FD 命名空间判定接线（D8b）。
  - 状态：[X]
  - 详情：`sm_poll_sockets` 循环内仅 `let _sock = sockets.get_mut::<tcp::Socket>(handle);` 未查询状态（[sm_fi.rs:1145-1152](../../src/kernel/privileged/net/init/sm_fi.rs)）后恒 `0`（[sm_fi.rs:1153](../../src/kernel/privileged/net/init/sm_fi.rs)）；[file_ops.rs:107-115](../../src/kernel/functions/fs/file_ops.rs) 的 `poll_syscall` 对 `POLLIN` 仅查 VFS handle、对 `POLLOUT` 恒置位，**不触达任何 socket 逻辑**。

- **G8: 错误映射粗化**
  - 描述：精确 errno 丢失，用户态无法区分"地址被占用""连接被拒""资源不足"。
  - 方案：按 `sm_*` 返回值透传（负值即 `-errno`）。
  - 状态：[X]
  - 详情：原 `bind_syscall` 恒 `EINVAL`（[syscall.rs:295](../../src/kernel/privileged/net/syscall.rs)）、`listen_syscall` 恒 `EINVAL`（[syscall.rs:307](../../src/kernel/privileged/net/syscall.rs)）、`connect_syscall` 恒 `ECONNREFUSED`（[syscall.rs:358](../../src/kernel/privileged/net/syscall.rs)）、`recvfrom_syscall` 恒 `EAGAIN`（[syscall.rs:448](../../src/kernel/privileged/net/syscall.rs)）。P2 已落地端到端透传：`syscall.rs` 9 处（`bind`/`listen`/`accept`/`connect`/`sendto`/`recvfrom`/`setsockopt`/`getsockopt`/`shutdown`）统一改 `i64::from(rc)` —— `Errno::as_ret()`（[errno.rs:110](../../src/kernel/privileged/errno.rs)）与 sm_fi 的 `-E_XXX` 常量逐一吻合，无需映射表。

- **G9: sockopt 近空实现**
  - 描述：`TCP_NODELAY` / `SO_ERROR` / `SO_TYPE` 等常用选项不可用。
  - 方案：精简集实装（D8）。
  - 状态：[X]
  - 详情：`sm_setsockopt` 仅 `level == 1 && optname == 16`（`SO_PASSCRED`）路由 UDS，其余返回 `0`（no-op，[sm_fi.rs:964-1004](../../src/kernel/privileged/net/init/sm_fi.rs)）；`sm_getsockopt` 恒 `0`（[sm_fi.rs:996-1003](../../src/kernel/privileged/net/init/sm_fi.rs)）。

- **G10: FD 位图泄漏（accept 的实现前置）**
  - 描述：`sm_close` 未归还 FD 编号；accept 循环每接受一个连接即永久消耗一个 FD 位，`MAX_SM_FD`（256）次后 `alloc_fd` 失败。
  - 方案：`sm_close` 补调 `free_fd(FdSubsystem::Smoltcp, fd)`（D6 前置），并清 D1 local 槽位。
  - 状态：[X]
  - 详情：见 [sm_fi.rs](../../src/kernel/privileged/net/init/sm_fi.rs) —— `sm_close` 完成 `sockets.remove` + 4 个缓冲 `k_free` + 清 handle/fd_type 后，原先**无** `free_fd`。全仓 `FdSubsystem::Smoltcp` 原先仅出现于分配点（[sm_fi.rs:292-294](../../src/kernel/privileged/net/init/sm_fi.rs)）；`free_fd` 见 [fd_alloc.rs:265](../../src/kernel/privileged/proc/fd_alloc.rs)。P1 已落地：`sm_close` 尾部在清 handle/fd_type 后补调 `free_fd(FdSubsystem::Smoltcp, fd)` 并清 D1 local 槽位，回归测试 `test_free_fd_allows_realloc` 覆盖"归还后可重分配"。

- **G11: 无阻塞语义（默认非阻塞 + `SOCK_NONBLOCK` 被拒）**
  - 描述：socket I/O 未就绪时一律返回 `-E_AGAIN`，无阻塞等待；且 `SOCK_NONBLOCK` 标志位被 `SockType::from_i32` 拒绝，`fcntl(F_SETFL)` 为空实现，用户态无法表达"阻塞 / 非阻塞"意图。
  - 方案：`socket_syscall` 剥离 `SOCK_NONBLOCK` 位 + per-slot 阻塞标志 + 等待队列睡眠（D10）。
  - 状态：[]
  - 详情：`SockType::from_i32` 仅接受 `1`/`2`（[socket_types.rs:61-65](../../src/kernel/privileged/net/socket_types.rs)），`SOCK_NONBLOCK`（`0x800`）落 `None` → `EINVAL`（[syscall.rs:249](../../src/kernel/privileged/net/syscall.rs)）；`F_SETFL => Ok(0)` 空实现（[io.rs:331](../../src/kernel/functions/fs/io.rs)）。

### UDP 缺口

- **U1: 无"已连接 UDP"语义**
  - 描述：UDP `send` 恒返回 `-E_NOTCONN`，无目的地址可复用。
  - 方案：`sm_connect` 增 UDP 分支登记对端，`sm_send` 走登记的 remote（D9）。
  - 状态：[X]
  - 详情：见 [sm_fi.rs:542-546](../../src/kernel/privileged/net/init/sm_fi.rs) —— UDP 分支直接 `-E_NOTCONN`，注释"UDP 无目的地址: 依赖 socket 已连接…简化处理"；而 `sm_connect` 仅接受 `fd_type == 1`（[sm_fi.rs:484-486](../../src/kernel/privileged/net/init/sm_fi.rs)）。

- **U2: `sm_getpeername` UDP 恒失败**
  - 描述：已连接的 UDP socket 也无法查询对端。
  - 方案：读端点表的 remote（D1/D9）。
  - 状态：[X]
  - 详情：见 [sm_fi.rs:1111-1114](../../src/kernel/privileged/net/init/sm_fi.rs) —— UDP 分支直接 `return -E_NOTCONN`，注释"remote 由 last_recv_meta 取, 但 Socket 没暴露"。

- **U3: `recvfrom` 系统调用丢弃对端出参**
  - 描述：UDP 用户态 `recvfrom` 拿不到发送方地址。
  - 方案：`recvfrom_syscall` 改调 `sm_recvfrom` 并透传 `src_ptr`/`src_len_ptr`（D9）。
  - 状态：[X]
  - 详情：见 [syscall.rs:421-455](../../src/kernel/privileged/net/syscall.rs) —— 忽略 `_src_ptr`/`_src_len_ptr`，且实际调用 `sm_recv`（[syscall.rs:446](../../src/kernel/privileged/net/syscall.rs)）而非 `sm_recvfrom`。底层 `sm_recvfrom` 的 UDP 分支**已能**回写对端（[sm_fi.rs:699-709](../../src/kernel/privileged/net/init/sm_fi.rs)），故此为纯系统调用层缺口。

- **U4: 无临时端口分配**
  - 描述：`bind(port = 0)` 无自动分配语义，无法作为客户端发起 UDP/TCP。
  - 方案：Edgine 侧临时端口分配器（D2）。
  - 状态：[X]
  - 详情：smoltcp 公开 API 的 `listen`/`connect` 均要求 `port != 0`，其内部不提供 `bind(0)` 自动端口；须 Edgine 侧维护 ephemeral 端口游标。P1 已完成 UDP 半：UDP `bind(port = 0)` 现可自动分配临时端口；P2 已完成 TCP 半：TCP `bind`/`listen`/`connect` 的缺省端口均走 D2 分配并回写 D1。

- **U5: 无组播**
  - 描述：无法加入/离开组播组。
  - 方案：`sm_setsockopt` 增 `IP_ADD_MEMBERSHIP` 等选项，调用 `Interface::join_multicast_group`（D11）。
  - 状态：[]
  - 详情：见 [multicast.rs:104](../../src/kernel/functions/net/smoltcp/src/iface/interface/multicast.rs) / [multicast.rs:130](../../src/kernel/functions/net/smoltcp/src/iface/interface/multicast.rs)；错误类型 `MulticastError::{Unaddressable, GroupTableFull}`（[multicast.rs:14-18](../../src/kernel/functions/net/smoltcp/src/iface/interface/multicast.rs)）。需 `Interface` 可变借用，与 `sm_*` 的 `NET_STATE` 锁路径耦合（`stack.iface` 与 `socket_set()` 为两个独立 static，无借用冲突）。

- **U6: 无 per-slot 端点表**
  - 描述：无 local/remote 端点记录，导致 U1/U2/G3/G5 缺公共地基。
  - 方案：`NetState` 增端点表 + `raw` accessor（D1）。
  - 状态：[X]
  - 详情：端点信息目前只能从 smoltcp socket 反查（`local_endpoint`/`endpoint`），而 TCP bind 端口、UDP 已连接对端、accept 交接所需的监听端点均**无法**从 socket 反查。

- **U7: `sm_getsockname` 通配强制 V4**
  - 描述：IPv6 通配绑定会误报为 `0.0.0.0`。
  - 方案：按 socket 的族或端点表如实回填（D9）。
  - 状态：[X]
  - 详情：见 [sm_fi.rs:1042-1057](../../src/kernel/privileged/net/init/sm_fi.rs) —— UDP 通配（`ep.addr == None`）时强制构造 `IpAddress::Ipv4(Ipv4Address::UNSPECIFIED)`。

- **U8: sockopt no-op / `getsockopt` 恒 0**
  - 描述：与 TCP G9 同一实现路径，UDP 同样不可用。
  - 方案：见 D8。
  - 状态：[X]

- **U9: 错误映射粗化**
  - 描述：与 TCP G8 同一实现路径。
  - 方案：见 G8。
  - 状态：[]

---

## 方案

### D1: per-slot 端点表（地基，DECISION-085）

- **描述**：为每个 slot 记录 local/remote 端点，承载 TCP bind 端口、UDP 已连接对端、accept 交接所需的监听端点。
- **方案**：
  - `NetState` 增 `local_endpoints: Vec<Option<NetEndpoint>>` 与 `remote_endpoints: Vec<Option<NetEndpoint>>`（各 `TOTAL_SLOTS` 项），`empty()` 置 `Vec::new()`、`allocate()` 填 `None`（对齐既有 8 张数组的模式，见 [state.rs:81-114](../../src/kernel/privileged/net/init/state.rs)）。
  - `raw.rs` 增 4 个 accessor：`socket_local_endpoint(fd) -> Option<NetEndpoint>` / `set_socket_local_endpoint(fd, ep)` / `socket_remote_endpoint(fd)` / `set_socket_remote_endpoint(fd, ep)`（对齐 [raw.rs:100-111](../../src/kernel/privileged/net/init/raw.rs) 的 handle accessor 形态：普通 `pub fn` + 内部 `unsafe` 块 + "调用方持 `NET_STATE` 锁"注释）。
  - `sm_close` 清空两表对应槽位。
- **状态**：[]
- **详情**：`NetEndpoint` 定义于 [iface_trait.rs:1327](../../src/kernel/privileged/net/iface_trait.rs)，已属 privileged 类型（`Copy`），可直接入表，无跨界问题。P1 仅落地 `local_endpoints` 表 + 2 个 local accessor + `sm_bind` UDP 分支写入 + `sm_close` 清理 + 契约测试；`remote_endpoints` 与 2 个 remote accessor 在本期无 release 消费者（F9 死代码零容忍），推迟至 P4 随 D9（UDP 连接态/对端语义）一并落地 —— TCP 的 remote 端点由 smoltcp socket 自身持有（`tcp::Socket::remote_endpoint()`），**不**入 D1 remote 表，故该表定位为 **UDP 专属**。

### D2: 临时端口分配器（Edgine 侧，DECISION-085）

- **描述**：为 `bind(port = 0)` 与无本地端口的 `connect` 提供 ephemeral 端口。
- **方案**：
  - `static EPHEMERAL_CURSOR: AtomicU16 = AtomicU16::new(49152);`（区间 49152–65535，共 16384 个端口）。
  - `next_ephemeral() -> Option<u16>`：循环至多 16384 次，每次读游标、`cur == 65535` 时回绕至 49152、否则 `cur + 1`，写回游标；每个候选端口扫 D1 的 local 表避让已占用项；全表占满则返回 `None`。
  - 调用方取 `None` 时统一返回 `-E_ADDRINUSE`。
- **状态**：[X]
- **详情**：smoltcp 公开 API 不提供 `bind(0)` 自动端口，此为本项目自定义语义，须在文档与测试中显式登记。P1 已落地：游标用 `load` + `store` 而非 `fetch_add`（u16 的 `fetch_add` 越过 65535 会静默回绕到 0，落在动态端口区间外；且分配在 `NET_STATE` 锁下串行，无需原子读改写），代码中以 `SIMPLIFIED` 注释登记。测试 `test_next_ephemeral_unique_and_in_range` / `test_next_ephemeral_wraps_to_start` 覆盖唯一性与回绕。

### D3: TCP `sm_bind`

- **描述**：让 TCP 能登记本地端口。
- **方案**：在 [sm_fi.rs:445](../../src/kernel/privileged/net/init/sm_fi.rs) 的 `match raw::fd_type(fd)` 增 `1 =>` 分支：
  1. `parse_endpoint_trait`（[sm_fi.rs:217](../../src/kernel/privileged/net/init/sm_fi.rs)）解析 sockaddr → `NetEndpoint`；解析失败 → `-E_INVAL`。
  2. `port == 0` → 走 D2 分配。
  3. 冲突检测：`local_endpoint_in_use`（[sm_fi.rs:379](../../src/kernel/privileged/net/init/sm_fi.rs)）扫 D1 local 表，同 `(族, port)` 已占用 → `-E_ADDRINUSE`。
  4. `set_socket_local_endpoint(fd, Some(ep))`。
  - **不**调用 smoltcp `bind`（`tcp::Socket` 无此方法）。
  - 同轮为 UDP 分支（`2 =>`，[sm_fi.rs:446-484](../../src/kernel/privileged/net/init/sm_fi.rs)）补写 D1 local。
- **状态**：[X]
- **详情**：通配地址保持 smoltcp 侧 `None` 语义（[sm_fi.rs:460-467](../../src/kernel/privileged/net/init/sm_fi.rs) 注释说明原因），但 D1 表内**保留族**（存 `V4(0.0.0.0)` / `V6(::)`），供 U7 如实回填。P1 已前移落地其中的 UDP 半 —— 现有 `2 =>` 分支接线 D1（写 local 端点）+ D2（`port == 0` 走临时端口分配），行为变更为 UDP `bind(port = 0)` 由原先 smoltcp `bind()` 失败恒 `-E_ADDRINUSE` 改为成功分配。TCP 的 `1 =>` 分支已随 P2 落地（[sm_fi.rs:485-505](../../src/kernel/privileged/net/init/sm_fi.rs)）：纯登记本地端点（TCP 无 smoltcp `bind`），不触碰 socket 状态。

### D4: TCP `sm_listen`

- **描述**：按登记的本地端点建立监听。
- **方案**：
  - 读 D1 local endpoint；缺省时走 D2 取端口并回写 D1。
  - 构造 `IpListenEndpoint { addr: 通配 → None, port }`（`port != 0`）→ `sock.listen(local)`（[tcp.rs:942](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）。
  - `Ok(())` → `0`；`Err(_)` → `-E_INVAL`（**修正**原 `-E_ADDRINUSE`：smoltcp `listen` 失败源于 socket 状态非法或地址不可用，非地址占用）。
- **状态**：[X]
- **详情**：替换原 [sm_fi.rs:409-415](../../src/kernel/privileged/net/init/sm_fi.rs) 的硬编码（现由 `sm_listen_locked` 实装，[sm_fi.rs:545-570](../../src/kernel/privileged/net/init/sm_fi.rs)）。smoltcp 允许同端口多 socket 同时 listen（acceptor loop 模式），无需 backlog 语义。缺省端口分支复用 D2 并回写 D1 local（族取 `V4(UNSPECIFIED)`），`fd_type != 1` → `-E_NOTSUPP`。

### D5: TCP `sm_connect`

- **描述**：让 TCP 能发起连接。
- **方案**：
  - local 从 D1 取；缺省走 D2 分配端口并回写 D1。
  - `sock.connect(iface.context(), endpoint, local)`（[tcp.rs:1013](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）。
  - 成功：`set_socket_local_endpoint`（**仅** local —— TCP remote 端点由 smoltcp socket 自身持有，见 D1 详情；D1 remote 表不适用 TCP）。
  - 错误映射：`ConnectError::Unaddressable → -E_INVAL`；其余 → `-E_CONNREFUSED`。
- **状态**：[X]
- **详情**：替换原 [sm_fi.rs:496-503](../../src/kernel/privileged/net/init/sm_fi.rs) 的硬编码（现由 `sm_connect_locked` 实装，[sm_fi.rs:652-687](../../src/kernel/privileged/net/init/sm_fi.rs)）。前置门控 `NET_CONFIGURED` 未置位 → `-E_NODEV`；`fd_type != 1` → `-E_NOTSUPP`；`parse_endpoint` 失败 → `-E_INVAL`。D1/D2 写入须在 `stack_mut()` 借用之前完成，避免 `&mut` 别名（见代码注释）。精确 `ECONNREFUSED` 需 smoltcp 内部错误状态，属排除项（见范围口径）。

### D6: TCP accept 先建后换交接（核心，DECISION-088）

- **描述**：把 smoltcp 监听 socket 上到达的连接交接给一个新 fd，且新监听在交接前已就绪（消除监听槽空闲窗口）。
- **方案**（算法步骤）：
  1. **门控**：监听 socket 满足 `state() == Established && listen_endpoint().port != 0`（[tcp.rs:900](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [tcp.rs:882](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)），否则 `-E_AGAIN`；且活跃 socket 数 `< get_max_sockets()`（[sockets.rs:68](../../src/kernel/privileged/net/init/sockets.rs)），否则 `-E_NFILE`。
  2. `alloc_fd(FdSubsystem::Smoltcp)`（[fd_alloc.rs:242](../../src/kernel/privileged/proc/fd_alloc.rs)）取新 fd `n`；失败 → `-E_NFILE`。
  3. 槽 `n` 调 `raw::socket_open_stub(sockets, Tcp, n)`（[raw.rs:305](../../src/kernel/privileged/net/init/raw.rs)）建**新监听 socket**（新 handle + 新缓冲）；失败 → `free_fd` + `-E_NOMEM`。
  4. 对**新 socket** 调 `listen(local_l)`（`local_l` = 监听槽 L 的 D1 local）→ 新监听立即就位。
  5. **索引交换**（O(1) 指针/值交换，无内存拷贝）：槽 `n` ← 已连接会话（handle / tcp_rx_buf / tcp_tx_buf / local，`fd_type = 1`）；槽 `L` ← 新监听（handle / 缓冲 + `local = local_l`，`fd_type = 1`）。TCP remote 端点由 smoltcp socket 自身持有并随会话迁移，**不**经 D1 remote 表（见 D1 详情）。
  6. 从已连接会话读 `remote_endpoint()`（[tcp.rs:894](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）→ `write_sockaddr`（[sm_fi.rs:140](../../src/kernel/privileged/net/init/sm_fi.rs)）回写到 `*addr` / `*addrlen`。
  7. 返回 `n`。
- **状态**：[]
- **前置**：G10（`sm_close` 补 `free_fd`），否则 accept 循环会在 256 次后耗尽 FD 位图。
- **详情**：**唯一回滚点**为步骤 3–4 失败（释放新 fd + 新缓冲）；步骤 5 交换后无失败路径，故无回滚。原 DECISION-086「迁移 + 重臂」在步骤 3→4 间存在"监听槽短暂空闲"窗口，本算法以"先建后换"消除该窗口。全程持 `NET_STATE` 锁，无并发窗口。`accept_syscall`（[syscall.rs:311](../../src/kernel/privileged/net/syscall.rs)）须透传 `_addr_ptr` / `_addrlen_ptr`（当前忽略）。
- **SIMPLIFIED**：无 backlog 队列长度限制（`_backlog` 忽略），并发溢出由 `get_max_sockets()` 与 `-E_NFILE` 兜底；需扩展时引入 per-listener 待接受队列。

### D7: `shutdown` 半关闭

- **描述**：区分 `SHUT_RD` / `SHUT_WR` / `SHUT_RDWR`。
- **方案**：新增 `sm_shutdown(fd, how) -> i32`：
  - `SHUT_WR(1)` / `SHUT_RDWR(2)` → `sock.close()`（[tcp.rs:1084](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)；smoltcp `close()` 关闭发送侧，接收侧仍可读，符合 `SHUT_WR` 语义）。
  - `SHUT_RD(0)` → no-op 返 `0`（SIMPLIFIED，无 smoltcp 直接对应物）。
  - 同步 `shutdown_syscall`（[syscall.rs:513-519](../../src/kernel/privileged/net/syscall.rs)）透传 `how`。
- **状态**：[X]
- **详情**：需同步在 [net_socket.rs](../../src/kernel/privileged/net_socket.rs) 增 safe 包装 + kernel_test 桩模块镜像同名桩（签名对齐）。

### D8: sockopt 精简集

- **描述**：实装常用 socket 选项。
- **方案**：
  - `sm_setsockopt`（[sm_fi.rs:964](../../src/kernel/privileged/net/init/sm_fi.rs)）增：
    - `TCP_NODELAY`（`level = 6`, `optname = 1`）→ `sock.set_nagle_enabled(val == 0)`（[tcp.rs:821](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）。
    - `SO_KEEPALIVE`（`level = 1`, `optname = 9`）→ `sock.set_keep_alive(val != 0 ? Some(Duration::from_secs(7200)) : None)`（[tcp.rs:844](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）。
    - `SO_REUSEADDR`(2) / `SO_REUSEPORT`(15) 等：接受但忽略，返 `0`。
  - `sm_getsockopt`（[sm_fi.rs:996](../../src/kernel/privileged/net/init/sm_fi.rs)）增：
    - `SO_TYPE`（`level = 1`, `optname = 3`）→ `SOCK_STREAM(1)` / `SOCK_DGRAM(2)`。
    - `TCP_NODELAY`（`level = 6`, `optname = 1`）→ 当前 Nagle 使能状态（smoltcp 默认启用 → `0`）。
    - `SO_ERROR`（`level = 1`, `optname = 4`）→ 由 `state()` 近似派生。
- **状态**：[X]
- **详情**：`set_keep_alive` 签名取 `Option<Duration>`，`Duration` 当前未在 [sm_fi.rs:13-18](../../src/kernel/privileged/net/init/sm_fi.rs) 导入，须补 `use smoltcp::time::Duration;`。`SO_ERROR` 精确值需 smoltcp 内部错误状态，属排除项（仅近似）。

### D8b: poll 就绪接线至 `fs` 层（DECISION-089）

- **描述**：为 socket fd 提供真实 revents（可读/可写/挂断/错误），并接入内核 `poll` / `ppoll` 系统调用路径。
- **方案**：
  1. 新增 privileged `sm_socket_poll(fd, events) -> i16`（返回 revents 位掩码）：
     - TCP（`fd_type == 1`）：`events & POLLIN != 0` 且 `can_recv()`（[tcp.rs:1234](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）→ 置 `POLLIN`；`events & POLLOUT != 0` 且 `can_send()`（[tcp.rs:1212](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）→ 置 `POLLOUT`；`state() == Closed`（[tcp.rs:900](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs)）→ 置 `POLLHUP | POLLERR`。
     - UDP（`fd_type == 2`）：`can_recv()`（[udp.rs:268](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs)）/ `can_send()`（[udp.rs:262](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs)）同理。
     - `fd` 非法或 `fd_type == 0` → `POLLNVAL`。
  2. [net_socket.rs](../../src/kernel/privileged/net_socket.rs) 增 safe 包装 + kernel_test 桩镜像。
  3. [functions/net/socket.rs](../../src/kernel/functions/net/socket.rs)（路径乙）增 `poll_fd(fd, events) -> i16`。
  4. [file_ops.rs](../../src/kernel/functions/fs/file_ops.rs) 的 `poll_syscall`（[file_ops.rs:69](../../src/kernel/functions/fs/file_ops.rs)）与 `ppoll_syscall`（[file_ops.rs:136](../../src/kernel/functions/fs/file_ops.rs)）：对每个 `pfd` 先判 `subsystem_of(fd) == Some(FdSubsystem::Smoltcp)`（[fd_alloc.rs:282](../../src/kernel/privileged/proc/fd_alloc.rs)）→ 走 `net::socket::poll_fd` 取 revents；否则维持现 VFS 路径。
  - 常量：`POLLIN = 1`（[file_ops.rs:20](../../src/kernel/functions/fs/file_ops.rs)）/ `POLLOUT = 4`（[file_ops.rs:21](../../src/kernel/functions/fs/file_ops.rs)）/ `POLLERR = 8` / `POLLHUP = 16` / `POLLNVAL = 32`（后三者需新增）。
- **状态**：[X]
- **详情**：`sm_poll_sockets`（返回 `i32`，[sm_fi.rs:1138](../../src/kernel/privileged/net/init/sm_fi.rs)）**保留**恒返回 `0` 的契约，仅用于 `poll_network` 的唤醒路径（[init.rs:288](../../src/kernel/privileged/net/init.rs)）；不改其语义，避免影响自我唤醒链路。

### D9: UDP 连接态 + 对端语义

- **描述**：补齐已连接 UDP、`getpeername`、`recvfrom` 回写、通配族回填。
- **方案**：
  - D1 的 `remote_endpoints` 表与 2 个 remote accessor 随本项落地（**UDP 专属**；TCP remote 端点由 smoltcp socket 自身持有，见 D1 详情）—— 此即 P1 将其推迟至本期的原因（F9：须有真实 release 消费者）。
  - `sm_connect` 增 UDP 分支：登记 remote 到 D1；若 local 为空则由 D2 分配本地端口并 `sock.bind(D1 local)`。
  - `sm_send` UDP 分支（[sm_fi.rs:542-546](../../src/kernel/privileged/net/init/sm_fi.rs)）：D1 有 remote → `sock.send_slice(data, remote)`（[udp.rs:375](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs)）；否则维持 `-E_NOTCONN`。
  - `sm_getpeername` UDP 分支（[sm_fi.rs:1111-1114](../../src/kernel/privileged/net/init/sm_fi.rs)）：读 D1 remote，无则 `-E_NOTCONN`。
  - `recvfrom_syscall`（[syscall.rs:421](../../src/kernel/privileged/net/syscall.rs)）改调 `sm_recvfrom`（[sm_fi.rs:672](../../src/kernel/privileged/net/init/sm_fi.rs)）并透传 `_src_ptr` / `_src_len_ptr`。
  - `sm_getsockname`（[sm_fi.rs:1020](../../src/kernel/privileged/net/init/sm_fi.rs)）：TCP 在 `local_endpoint()` 为 `None` 时回退 D1；UDP 通配按 D1 族如实回填（修正 U7）。
- **状态**：[X]
- **详情**：`sm_recvfrom` 已能回写对端（[sm_fi.rs:699-709](../../src/kernel/privileged/net/init/sm_fi.rs)），故 U3 仅需改系统调用层调用点。functions 侧 `recvfrom_syscall` 已透传出参（[functions/net/syscall.rs:252](../../src/kernel/functions/net/syscall.rs)）。

### D10: 阻塞睡眠（DECISION-090，实施取路径 B「相对完整」，机制细化见 DECISION-092）

- **描述**：实现 POSIX 默认阻塞语义：I/O 未就绪时**真阻塞睡眠**（非忙轮询），由 poll 事件 / 信号（EINTR，P5d）唤醒；`SOCK_NONBLOCK` / `fcntl(F_SETFL)` 切换非阻塞；accept/connect 完成语义对齐 POSIX（A2）。
- **调研结论（P5 前置调研坐实，修正原 D10 假设）**：
  1. **真阻塞原语已存在**：`scheduler_block(BlockReason::WaitingForIo)` + `scheduler_schedule()`（`timer_sleep` 同款，[sleep.rs:221](../../src/kernel/privileged/timer/sleep.rs)）；事件唤醒 `scheduler_unblock(pid)`（[sched_ops.rs:40](../../src/kernel/privileged/proc/sched_ops.rs)）。futex（`ipc/types.rs`）、`epoll.rs:405`、`uffd.rs` 三处均为「**锁内收集 pid → 释放锁后逐个 unblock**」范式。
  2. `wait_with_timeout`（[sleep.rs:302](../../src/kernel/privileged/timer/sleep.rs)）只 `scheduler_yield_ex` 让出自旋、任务保持 runnable，**非真阻塞**——原 D10 以其为睡眠原语与目标「睡眠而非忙轮询」矛盾，本路径**弃用**。
  3. **表容量是潜伏 bug**：`SocketWaitQueueTable` 固定 16（[wait_queue.rs:130](../../src/kernel/privileged/net/wait_queue.rs)），`MAX_SM_FD=256`（`FdPlan::SMOLTCP.capacity`）；`poll_network` 已按 `0..MAX_SM_FD` 扫（[init.rs:268](../../src/kernel/privileged/net/init.rs)），故 slots 16..255 的 `try_wake` 恒 None，唤醒静默失效（非「将来才需要」）。
  4. **唤醒模型缺 accept/connect 条件**：现仅 `can_recv`/`can_send`（[init.rs:276](../../src/kernel/privileged/net/init.rs)）；阻塞 accept 就绪=监听槽→`Established`、阻塞 connect 就绪=connector→`Established/Closed`，需给 `poll_network` 新增**状态迁移唤醒**。
  5. **connect 现状「发起即返 0」**（`sock.connect()` Ok 即返，[sm_fi.rs:891](../../src/kernel/privileged/net/init/sm_fi.rs)），无 `EINPROGRESS`、不等握手——A2 须改造。
  6. 信号可打断 Blocked 任务（`do_signal_send` 置 `Ready`，[signal.rs:231](../../src/kernel/privileged/proc/signal.rs)），但全仓无阻塞 syscall 返 `EINTR`——EINTR 为新开垦（P5d）。
- **方案（分 P5a-P5d 子轮，见分期）**：
  1. **阻塞范式**（取代原「`mark_waiting`+`wait_with_timeout`」）：未就绪且阻塞 → 持 `NET_STATE` 下 `q.set_waiter(current_pid)`（仿 uffd `fault_pid`）→ 释放 `NET_STATE` → `scheduler_block(WaitingForIo)` + `scheduler_schedule()` →（被 poll/信号唤醒）→ 重抢 `NET_STATE` → 重查就绪，未就绪则循环。`recv`/`send`/`recvfrom`/`sendto`（[sm_fi.rs](../../src/kernel/privileged/net/init/sm_fi.rs)，现单函数内联持 `NET_STATE`）拆出 `_locked` 临界区以支持「释放锁→等→重抢→重试」。
  2. **F8 锁序铁律**：唤醒侧（`poll_network` 等）持 `NET_STATE` 只**收集待唤醒 pid 列表**，**释放 `NET_STATE` 后**再 `scheduler_unblock(pid)`——杜绝 `NET_STATE → SCHEDULER` 嵌套锁。丢失唤醒约束：置 waiter pid 必须在持 `NET_STATE`（与 socket 状态读取同一临界区）内完成后才释放。
  3. **表扩容**：`SocketWaitQueueTable` 16→`MAX_SM_FD`（`[const { SocketWaitQueue::new() }; MAX_SM_FD]`），`get()` 边界随之（slot ≥ `MAX_SM_FD`→`None`）；`SocketWaitQueue` 增 `waiter_pid`；同步 `table_lookup_bounded` 单测（[wait_queue.rs:200](../../src/kernel/privileged/net/wait_queue.rs)）与陈旧「=16」注释（`wait_queue.rs:128`/`168` + `save.rs:41`）。
  4. **非阻塞开关**：`socket_syscall`（[syscall.rs:244](../../src/kernel/privileged/net/syscall.rs)）在 `SockType::from_i32` 前剥离 `SOCK_NONBLOCK`（`0x800`）并透传给 `sm_socket` 置 slot 阻塞标志；`fcntl` `F_GETFL`/`F_SETFL`（[io.rs:331](../../src/kernel/functions/fs/io.rs)，现 `F_SETFL => Ok(0)` 空实现）经新增 functions→privileged safe API 落 `O_NONBLOCK`。
  5. **per-slot 阻塞标志**：`NetState.blocking: Vec<bool>`（`allocate()` 默认 `true`=阻塞）+ `raw.rs` accessor。
- **状态**：[]（分 P5a-P5d 子轮推进）
- **详情**：EINTR 与 `SO_RCVTIMEO`/`SO_SNDTIMEO` per-op 超时**移出 P5 首轮**，独立轮（P5d）推进——超时须给 D8 sockopt 再增两选项 + per-slot timeval 存储 + 「事件/超时/信号」三路竞态。connect 完成语义取 A2（对齐 POSIX：阻塞等 Established、非阻塞 `-EINPROGRESS`、状态→errno 映射），归 P5c。

### D11: 组播（DECISION-091）

- **描述**：支持加入/离开 IPv4 与 IPv6 组播组。
- **方案**：`sm_setsockopt`（[sm_fi.rs:964](../../src/kernel/privileged/net/init/sm_fi.rs)）增：
  - `IP_ADD_MEMBERSHIP`（`level = 0`, `optname = 35`）/ `IP_DROP_MEMBERSHIP`（`optname = 36`）：解析 `struct ip_mreq`（`imr_multiaddr` + `imr_interface`）→ `stack.iface.join_multicast_group(addr)`（[multicast.rs:104](../../src/kernel/functions/net/smoltcp/src/iface/interface/multicast.rs)）/ `leave_multicast_group(addr)`（[multicast.rs:130](../../src/kernel/functions/net/smoltcp/src/iface/interface/multicast.rs)）。
  - `IPV6_ADD_MEMBERSHIP`（`level = 41`, `optname = 20`）/ `IPV6_DROP_MEMBERSHIP`（`optname = 21`）：解析 `struct ipv6_mreq`，同上。
  - 错误映射：`MulticastError::Unaddressable → -E_INVAL`；`MulticastError::GroupTableFull → -E_NOMEM`。
- **状态**：[]
- **详情**：`stack.iface` 属 privileged `NetworkStack`（[smoltcp_impl.rs:164-167](../../src/kernel/privileged/net/smoltcp_impl.rs)），与 `socket_set()` 为两个独立 static，可同时可变借用，无借用冲突；全程持 `NET_STATE` 锁。多播收发复用既有 UDP 收发路径，本项仅补组管理入口。

---

## 实施分期

### P1: 地基

- **条目**：D1 端点表（local 半）+ D2 临时端口分配器 + G10 `free_fd` 修复 + D3 UDP 半前移
- **描述**：铺设 local 端点表与临时端口数据面、修复 FD 泄漏；并将 D3 的 UDP 半（`sm_bind` UDP 分支接线 D1/D2）前移，以满足 F9 真实使用路径约束（见 详情）。
- **方案**：见 D1/D2/D3；G10 在 `sm_close` 补 `free_fd(FdSubsystem::Smoltcp, fd)` 并清 D1 local 槽位。
- **状态**：[X]
- **详情**：F9 死代码零容忍与原条目冲突 —— D1 的 `remote_endpoints` 与 D2 的 `next_ephemeral` 若不加 UDP 半接线，在本期无 release 消费者（dead_code 触发 §2.3 门槛 1 与 F9，亦违反 [§风险 L414](#风险与约束) 自身约束）。故本期调整为：D1 仅落 `local_endpoints`（`remote_endpoints` 及 2 个 remote accessor 推迟至 P2）；D3 UDP 半前移接入；行为变更 —— UDP `bind(port = 0)` 由原先 smoltcp `bind()` 失败恒 `-E_ADDRINUSE`，改为按 D2 分配临时端口并成功（POSIX 语义修正）。TCP 侧行为不变。
- **验证**：新增 host 契约测试（端点表 alloc/clear 往返、临时端口唯一性与回绕、`free_fd` 后可重分配）。全部 §2.3 门槛通过：双架构 build 0w0e / clippy pedantic 0 warning / fmt 三 crate 0 漂移 / `audit.sh quick` + 4 独立审计（`functions_boundary` + `safety_coverage` + `deadlock_matrix` + `coupling`）+ `audit_smoltcp_purity` / host-tests / QEMU 双架构 / kernel host 单测 953 passed。

### P2: TCP 建链

- **条目**：D3 `bind` + D4 `listen` + D5 `connect`
- **描述**：TCP 可绑定/监听/发起连接（尚未 accept）。
- **方案**：见 D3/D4/D5。落码要点有二：（1）`sm_bind` / `sm_listen` / `sm_connect` 各拆为「FFI wrapper（自锁 `NET_STATE`）+ `*_locked`（锁自由内核实现，要求调用方持锁）」两段，使契约测试可持单锁直调 `*_locked`（`NET_STATE` 为**非可重入** `IrqSpinLock`，持锁再调 wrapper 会死锁）；（2）`syscall.rs` 9 处 errno 由归一化改为 `i64::from(rc)` 端到端透传 —— `Errno::as_ret()` 与 sm_fi 的 `-E_XXX` 常量逐一吻合，无需映射表，`bind`/`listen`/`accept`/`connect`/`sendto`/`recvfrom`/`setsockopt`/`getsockopt`/`shutdown` 统一修正。
- **状态**：[X]
- **详情**：D5 仅写 D1 local（TCP remote 端点由 smoltcp socket 自身持有，D1 remote 表延至 P4 且为 UDP 专属，见 D1 详情）；`sm_connect` 前置 `NET_CONFIGURED` 门控（host-test 默认 false → `-E_NODEV`）；host-test 下 kmalloc 仅 4 KiB `early_buffer`，无法经受保护路径分配 TCP 缓冲，故契约测试以 `Box::leak` 自建 `'static` 缓冲直接装配 socket 绕开 `k_malloc`。
- **验证**：新增 host 契约测试 `test_tcp_bind_listen_connect_contract`，覆盖 bind →（同族同端口冲突 `-E_ADDRINUSE`）→ listen（进入 `State::Listen`）→ 缺省 listen（动态端口分配并回写 D1）→ connect（`NET_CONFIGURED` 未置位 → `-E_NODEV`）状态迁移。全部 §2.3 门槛通过：双架构 build 0w0e（`build.sh all` 5/0 + `build.sh aarch64` 2/0）/ clippy pedantic 0 warning / fmt 三 crate 0 漂移 / `audit.sh quick` RC=0 + 4 独立审计 RC=0（`functions_boundary` 问题总数 0 / `safety_coverage` 1767/1767 = 100% / `deadlock_matrix` RC=0 / `coupling` 通过）/ `make test-kernel-host` 956 passed / QEMU 双架构 2/2 通过。惟原验证项「QEMU 端到端 `connect` 到对端可达」未取得 —— 端到端用户态连通需 P3 的 accept 交接路径与用户态测试程序配合，QEMU 本期仅验证到启动层（双架构启动 / DHCP 租约 / virtio-net），该项顺延至 P3。

### P3: TCP accept 交接

- **条目**：D6 先建后换交接
- **描述**：TCP 服务端可接受连接并交付用户态。
- **方案**：见 D6。
- **状态**：[X]
- **验证**：QEMU 端到端 echo 服务端（accept → recv → send）+ 客户端三向握手与回显；连续多连接验证 FD 回收与监听槽持续可用。
- **详情**：已达成。落地要点：D6 先建后换交接（G3 [X]）+ `accept` 出参透传（G4 [X]）+ `FdPlan::SMOLTCP` 基址 64 方案 C 重排（`sm_slot`/`sm_alloc_slot` 单点换算，`sm_fi.rs` 内 `fd as usize` 归零；VFS 段 `[0,64)` 纳入 `ALL` 重叠校验）+ `close_syscall` 按 `subsystem_of` 分流 `sm_close`（修复 close 对 socket fd 的 no-op，G10 归还链路在 syscall 层闭合）+ 用户库 `print_dec` 切片修复（`&buf[i..20]`，消除多输出的 1 个 NUL）。§2.3 六门槛全绿：`build.sh all` 5/0 / clippy pedantic（含 feature 双维）/ fmt 三 crate / `audit.sh quick` / host-tests 全过 / `make test-kernel-host` 957 passed。e2e：`scripts/qemu_boot_test.sh x86_64` 3/3 轮回显、`accepted fd` 去重 == 1（fd=65，监听 fd=64 共存印证基址重排）、启动层里程碑/KPTI/双核断言全过。多轮复跑确认 accept 门控需含 `CloseWait`（脚本客户端 `shutdown(SHUT_WR)` 半关场景），已并入实现。

### P4: 语义完善

- **条目**：D7 shutdown + D8 sockopt + D8b poll 接线 + D9 UDP 连接态/对端
- **描述**：半关闭、常用选项、poll 就绪、已连接 UDP 与对端语义补齐。
- **方案**：见 D7/D8/D8b/D9。
- **状态**：[X]
- **验证**：`TCP_NODELAY` round-trip、`shutdown(SHUT_WR)` 半关验证、用户态 `poll` 得到真实 revents、UDP `connect`+`send`+`getpeername`+`recvfrom` 对端回填。
- **详情**：已达成（含 recvfrom 活体回填腿）。§2.3 六门槛全绿：`build.sh all`（双架构 0w0e）/ `audit.sh`（clippy pedantic lib + kernel_test + host-test 三维 0 warning + fmt 三 crate + 全审计含 FP-06）/ `make test-host` / `make test-kernel-host` 960 passed（+3 契约单测：`test_remote_endpoint_table_roundtrip` / `test_shutdown_contract` / `test_socket_poll_contract`）/ `scripts/qemu_boot_test.sh x86_64` e2e 里程碑 —— TCP 侧 `NODELAY roundtrip`/`POLLIN revents=1`/`SHUTDOWN_WR` 各 3 轮、UDP 侧 `CONNECT`/`PEERNAME`/`SOCKNAME`/`RECVFROM src ok` 各 1 次。e2e 落地：`userlib` 补 `poll`/`shutdown`/`getpeername` wrapper 与 `TCP_NODELAY`/`POLLIN`/`POLLOUT` 常量；`tcp_echo_server` 在真实已建连接上跑 setsockopt→getsockopt round-trip + `poll` 就绪 + `shutdown(SHUT_WR)` 半关；`udp_connect_probe`（启动期同步、echo fork 前释放 fd）跑 connect→getpeername→getsockname 纯本地端点态；`udp_echo_probe` 跑 connect→send→recvfrom 活体回填——脚本 e2e 阶段起宿主 UDP 回显服务（绑 127.0.0.1:9090），guest 经 slirp 网关 `10.0.2.2:9090` 发出 `EDGINE-UDP` 并校验回显一致 + `recvfrom` 回填 src == `10.0.2.2:9090`（实活验证 syscall 层 U3 透传 + `sm_recvfrom` UDP 分支 `write_sockaddr`）。普通启动无应答者时探针有界重试后打印 FAIL 并继续（与 `ipv6_udp_probe` 同款容错，不断言）。

### P5: 阻塞睡眠（路径 B，分 P5a-P5d 四子轮）

- **条目**：D10 阻塞语义（真调度阻塞 + accept/connect A2）
- **描述**：默认阻塞 I/O 真睡眠等待，`SOCK_NONBLOCK` / `fcntl(F_SETFL)` 可控；accept/connect 完成语义对齐 POSIX（A2）。
- **方案**：见 D10（B 路径，DECISION-092 细化）。子轮分解：
  - **P5a（地基，不改行为）**：等待表 16→`MAX_SM_FD` + `waiter_pid` 化 + `get`/注释/单测修订；`NetState.blocking` 标志 + `raw` accessor；`socket_syscall` 剥离 `SOCK_NONBLOCK` 透传 `sm_socket`；`fcntl` `F_GETFL`/`F_SETFL` 的 `O_NONBLOCK` 桥。
  - **P5b（阻塞核心，F8 高危轮）**：`recv`/`send`/`recvfrom`/`sendto` 接入真 block（置 pid under `NET_STATE`→释放→`scheduler_block`+`schedule`→重抢→重试）+ `poll_network` 锁序重构（锁内收集 pid、锁外 `scheduler_unblock`）+ 锁序/丢失唤醒回归测试。
  - **P5c（accept/connect A2）**：`poll_network` 增状态迁移唤醒；阻塞 connect 等 Established/失败 errno 映射、非阻塞 `-EINPROGRESS`；阻塞 accept 等连接到达。
  - **P5d（EINTR + 超时，独立轮）**：唤醒后查 `signal_pending`→`-EINTR`；`SO_RCVTIMEO`/`SO_SNDTIMEO` + 到期 `-EAGAIN`。
- **状态**：[]
- **验证**：P5a 契约测试（表边界 `get(255)` 可唤醒 / `get(256)`→`None`、`blocking` 默认 true、`SOCK_NONBLOCK` 剥离、`fcntl` `O_NONBLOCK` 往返）；P5b 丢失唤醒 + 锁序（`audit_deadlock_matrix`）回归；P5c connect/accept 完成语义 e2e（QEMU 阻塞 recv 唤醒、非阻塞 connect `-EINPROGRESS`）；每子轮独立过 §2.3 六门槛。

### P6: 组播

- **条目**：D11 组播
- **描述**：IPv4/IPv6 加入/离开组播组。
- **方案**：见 D11。
- **状态**：[]
- **验证**：`setsockopt(IP_ADD_MEMBERSHIP)` 后 `Interface` 组表包含目标组；`IP_DROP_MEMBERSHIP` 后可离开；错误分支返回对应 errno。

---

## smoltcp vendored 零修改论证

「相对完整」口径所需能力均落在 smoltcp **公开 API**，无需修改 vendored 源码（`audit_smoltcp_purity.py` + `SMOLTCP_LOCAL_SRC_HASH` 门禁不变）：

| 需要能力 | smoltcp 公开 API | 位置 |
|---|---|---|
| TCP 监听 | `tcp::Socket::listen` | [tcp.rs:942](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| TCP 发起连接 | `tcp::Socket::connect` | [tcp.rs:1013](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| TCP 关闭/中止 | `close` / `abort` | [tcp.rs:1084](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [tcp.rs:1114](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| TCP 状态 | `state` / `is_active` / `is_open` / `is_listening` | [tcp.rs:900](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [1138](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [1159](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [1122](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| TCP 收发 | `send_slice` / `recv_slice` | [tcp.rs:1295](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [tcp.rs:1359](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| TCP 端点 | `local_endpoint` / `remote_endpoint` / `listen_endpoint` | [tcp.rs:888](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [894](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [882](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| TCP 可读可写 | `may_recv` / `may_send` / `can_recv` / `can_send` | [tcp.rs:1197](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [1178](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [1234](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [1212](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| Nagle / 保活 / 超时 | `set_nagle_enabled` / `set_keep_alive` / `set_timeout` | [tcp.rs:821](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [844](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) / [798](../../src/kernel/functions/net/smoltcp/src/socket/tcp.rs) |
| UDP 绑定/关闭 | `bind` / `close` | [udp.rs:217](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs) / [udp.rs:239](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs) |
| UDP 收发 | `send_slice` / `recv_slice` | [udp.rs:375](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs) / [udp.rs:408](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs) |
| UDP 端点/元数据 | `endpoint` / `UdpMetadata` | [udp.rs:181](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs) / [udp.rs:16](../../src/kernel/functions/net/smoltcp/src/socket/udp.rs) |
| 组播（D11） | `join_multicast_group` / `leave_multicast_group` | [multicast.rs:104](../../src/kernel/functions/net/smoltcp/src/iface/interface/multicast.rs) / [multicast.rs:130](../../src/kernel/functions/net/smoltcp/src/iface/interface/multicast.rs) |

**唯三会破零修改门禁之处，均属排除项**：

- UDP `SO_ERROR` 真值 → 仅近似派生（D8），排除。
- TCP 精确 `ECONNREFUSED` → 近似替代（D5），排除。
- `bind(port = 0)` 自动端口 → 由 Edgine 侧临时端口分配器绕过（D2），排除。

---

## 工作量估算

| 分期 | 文件 | 新增行数 | 修改行数 | 难度 |
|---|---|---|---|---|
| P1 | state.rs + raw.rs + sm_fi.rs | +130 | -5 | 低 |
| P2 | sm_fi.rs + privileged/net/syscall.rs | +90 | -30 | 中 |
| P3 | sm_fi.rs + privileged/net/syscall.rs + net_socket.rs | +150 | -20 | 高 |
| P4 | sm_fi.rs + net_socket.rs + functions/net/socket.rs + functions/fs/file_ops.rs | +220 | -40 | 中 |
| P5 | sm_fi.rs + wait_queue.rs + init.rs + functions/fs/io.rs | +180 | -30 | 高 |
| P6 | sm_fi.rs | +70 | -5 | 中 |
| 测试 | host-tests/ + privileged/tests/ | +380 | - | 中 |
| **总计**（相对完整） | **~10 文件** | **~1220 行** | **~-130 行** | - |

涉及文件清单：`privileged/net/init/state.rs`、`privileged/net/init/raw.rs`、`privileged/net/init/sm_fi.rs`、`privileged/net/wait_queue.rs`、`privileged/net/init.rs`（注释修正）、`privileged/net/syscall.rs`、`privileged/net_socket.rs`、`functions/net/socket.rs`、`functions/fs/file_ops.rs`、`functions/fs/io.rs`。

---

## 风险与约束

- **accept 先建后换为最高风险点**：步骤 3–4（建新监听）失败须回滚新 fd 与新缓冲；步骤 5 索引交换须原子完成（全程持 `NET_STATE` 锁）。任何遗漏将泄漏 FD 位图与 `k_malloc` 缓冲。
- **阻塞睡眠的锁序约束**：必须先锁内 `mark_waiting()` 再释放 `NET_STATE`，否则丢失唤醒；`wait_with_timeout` 不释放调用方锁，故必须主动释放，否则 `poll_network` 取锁饿死。此为 F8 死锁矩阵敏感点。
- **等待队列扩容**：`SocketWaitQueueTable` 由 16 扩至 256，须同步 4 个单测（含 `get(16)` 边界）与 `wait_queue.rs:128` / `:168` 陈旧注释。
- **默认阻塞的行为变更**：当前默认非阻塞，D10 后默认阻塞，用户态未设 `SOCK_NONBLOCK` 的既有调用点可能阻塞——须在实施时排查内核内调用点与测试。
- **poll 接线新增 fs → net 单向依赖**：`file_ops.rs` 依赖 FD 命名空间判定（`subsystem_of`）与 functions 侧 `net::socket::poll_fd`；须确认 `functions::fs` 与 `functions::net` 无既有反向依赖，避免 F3 循环依赖。
- **F9 死代码零容忍**：D1/D2/D10/D11 引入的表、分配器与选项分支必须有真实使用路径；若某分期暂不落地，不得先加"预留"字段或函数。
- **F1/F2 边界**：核心改动落 privileged（`sm_fi.rs` / `syscall.rs` / `raw.rs` / `state.rs` / `net_socket.rs` / `wait_queue.rs`）；functions 侧改动（`file_ops.rs` / `io.rs` / `socket.rs`）**不**引入 unsafe。
- **F4 SAFETY 注释**：新增 `unsafe` 块（sockopt 指针解引用、`sm_socket_poll` 的 socket 访问、索引交换）须 100% 配 `// SAFETY:`。
- **net_socket.rs 桩同步**：每个新增 `sm_*`（`sm_shutdown` / `sm_socket_poll`）须在 kernel_test 桩模块（[net_socket.rs:30](../../src/kernel/privileged/net_socket.rs)）镜像同名桩，签名对齐。
- **smoltcp 零修改**：由 `audit_smoltcp_purity.py` 与 `SMOLTCP_LOCAL_SRC_HASH` 机器化保证；排除项（精确 `SO_ERROR` / `ECONNREFUSED`）若未来纳入须重新评估门禁。
- **并行旁路一致性**：`functions/net/smoltcp_impl.rs` 的 `bind_fd` / `listen_fd` / `accept_fd` / `connect_fd`（[smoltcp_impl.rs:258-310](../../src/kernel/functions/net/smoltcp_impl.rs)）为另一条调用路径，本轮以 `sm_fi.rs` 为主线；两路径语义差异须在实施时核对，避免分裂。
- **init.rs 陈旧注释**：`init.rs:677-686` 注释含过时数值（"MAX_SOCKETS=1024"），实际 `MAX_SOCKETS = 256`（[sockets.rs:17](../../src/kernel/privileged/net/init/sockets.rs)）、`TOTAL_SLOTS = 512`（[init.rs:686](../../src/kernel/privileged/net/init.rs)）；D1 落地时一并修正。

---

## 验证门槛

每期完成后必须满足 §2.3 全部 6 条：

1. 双架构 `./ci/build.sh all` 0 error / 0 warning
2. clippy 0 warning（`cargo clippy --release -- -D warnings`）+ `cargo fmt --check` 三 crate 0 漂移
3. 核心审计通过（`functions_boundary` + `safety_coverage` + `deadlock_matrix` + `comment_language` + `coupling`）+ `audit_smoltcp_purity.py` 本地 hash 一致
4. host-tests 全通过（含新增 TCP/UDP 契约测试）
5. QEMU 集成测试通过（启动层双架构自 P2 起恒验证；端到端用户态验证自 P3 起：accept 交接 + 用户态 echo/建链程序；P4 起：shutdown/sockopt/poll/UDP 连接态；P5 起：阻塞唤醒；P6 起：组播）
6. host 侧内核单元测试 0 failed（`make test-kernel-host`）

---

## 决策记录

### DECISION-085: 合并补全口径与地基

- **描述**：TCP/UDP socket 层合并补全；per-slot 端点表与 Edgine 侧临时端口分配器作为公共地基。
- **裁定**：引入 `NetState` 端点表（D1）承载 TCP bind 端口与 UDP 对端；`bind(port = 0)` 由 Edgine 侧 `AtomicU16` 游标分配（D2），不触碰 smoltcp；全程 smoltcp vendored 零修改。
- **状态**：[X]

### DECISION-086: accept 交接与重臂算法

- **描述**：原定以"迁移已连接 handle + 重臂监听槽"实现 accept 交接。
- **裁定**：**已由 DECISION-088 取代**（该算法在迁移与重臂之间存在监听槽空闲窗口）。本条保留为历史记录。
- **状态**：[X]

### DECISION-087: 实现路径口径改「相对完整」

- **描述**：原口径为 A 路径（非阻塞 + 状态语义完整），阻塞睡眠、组播、`bind(port = 0)` 自动端口移入 B 阶段。
- **裁定**：口径升级为「相对完整」——纳入阻塞睡眠（D10）、组播（D11）、`bind(port = 0)` 自动端口（D2/D3）与 poll 就绪接线（D8b）为主路径；仍排除精确 `SO_ERROR` 真值、精确 `ECONNREFUSED`、UDP `MSG_PEEK`（破零修改门禁或超出口径）。
- **状态**：[X]

### DECISION-088: accept 采用「先建后换」无回滚算法

- **描述**：取代 DECISION-086 的「迁移 + 重臂」。
- **裁定**：先对 `alloc_fd` 得到的新槽建新监听并 `listen(local_l)`，成功后再与监听槽做纯索引交换——新监听就位后旧监听槽方被替换，消除空闲窗口；唯一回滚点为建新监听失败。
- **状态**：[X]

### DECISION-089: poll 就绪在 functions 层接线

- **描述**：内核内 poll 就绪链路无消费者：`sm_poll_sockets` 恒返 0，`poll_syscall` 不触达 socket。
- **裁定**：新增 privileged `sm_socket_poll(fd, events)` 派生 revents，并在 `functions/fs/file_ops.rs` 的 `poll_syscall` / `ppoll_syscall` 中经 `subsystem_of(fd)` 判定 socket 命名空间后接线；`sm_poll_sockets` 保留原契约仅用于自我唤醒。
- **状态**：[X]

### DECISION-090: 阻塞语义与 `SOCK_NONBLOCK`

- **描述**：实现 POSIX 默认阻塞语义；`SOCK_NONBLOCK` 当前被 `SockType::from_i32` 拒绝，`fcntl(F_SETFL)` 为空实现。
- **裁定**：`socket_syscall` 剥离 `SOCK_NONBLOCK` 位并置 per-slot 阻塞标志；`fcntl(F_SETFL)` 支持 `O_NONBLOCK`；未就绪时锁内 `mark_waiting` → 释放 `NET_STATE` → `wait_with_timeout` → 重抢锁重试；等待队列表由 16 扩至 `MAX_SM_FD`（256）。
- **状态**：[X]

### DECISION-091: 组播纳入主路径

- **描述**：原将组播留 B 阶段。
- **裁定**：纳入主路径（D11）；`sm_setsockopt` 增 `IP_ADD/DROP_MEMBERSHIP` 与 `IPV6_ADD/DROP_MEMBERSHIP`，经 `Interface::join_multicast_group` / `leave_multicast_group` 实装。
- **状态**：[X]

### DECISION-092: D10 采用真调度 block/unblock（取代 wait_with_timeout）

- **描述**：原 DECISION-090 以 `wait_with_timeout`（`scheduler_yield` 自旋、任务保持 runnable）为睡眠原语，与目标「睡眠而非忙轮询」矛盾；P5 前置调研另发现等待表仅 16（slots 16..255 唤醒静默失效，潜伏 bug）、唤醒模型缺 accept/connect 状态迁移。
- **裁定**：取路径 B「相对完整」。改用 `scheduler_block(BlockReason::WaitingForIo)` + `scheduler_schedule()` + `scheduler_unblock(pid)`（仿 futex/epoll/uffd 既有范式）；`SocketWaitQueue` 增 `waiter_pid`；唤醒侧持 `NET_STATE` 收集 pid、**释放锁后** unblock（F8：禁 `NET_STATE → SCHEDULER` 嵌套锁）；等待表扩至 `MAX_SM_FD`。accept/connect 完成语义取 A2（对齐 POSIX，非阻塞 `-EINPROGRESS`）。EINTR 与 per-op 超时（`SO_RCVTIMEO`/`SO_SNDTIMEO`）移出首轮，独立轮（P5d）推进。
- **状态**：[X]

---

## 关联文档

- [docs/explain/explain-framekernel.md](../explain/explain-framekernel.md) — framekernel 架构与 privileged/functions 边界
- [docs/plan/ipv6-dual-stack.md](./ipv6-dual-stack.md) — IPv4/IPv6 双栈改造（DECISION-032），本计划的前置抽象层
- [src/kernel/privileged/net/init/sm_fi.rs](../../src/kernel/privileged/net/init/sm_fi.rs) — FFI 实现层（TCP/UDP 缺口集中地）
- [src/kernel/privileged/net/syscall.rs](../../src/kernel/privileged/net/syscall.rs) — 网络系统调用分发层
- [src/kernel/privileged/net/init/state.rs](../../src/kernel/privileged/net/init/state.rs) — `NetState` 统一状态结构
- [src/kernel/privileged/net/init/raw.rs](../../src/kernel/privileged/net/init/raw.rs) — 集中 static mut 访问与槽位 accessor
- [src/kernel/privileged/net/wait_queue.rs](../../src/kernel/privileged/net/wait_queue.rs) — per-fd 等待队列
- [src/kernel/functions/net/smoltcp_impl.rs](../../src/kernel/functions/net/smoltcp_impl.rs) — functions 侧并行 socket 实现
- [src/kernel/functions/fs/file_ops.rs](../../src/kernel/functions/fs/file_ops.rs) — `poll` / `ppoll` 系统调用路径

---

## 状态记录

- 创建文档，完成现状与缺口梳理（TCP G1-G10 / UDP U1-U9）与方案设计（D1-D9），登记 DECISION-085（A 路径 + 合并补全 + smoltcp 零修改口径）与 DECISION-086（accept 交接与重臂算法及 FD 回收前置）。
- 口径由 A 路径升级为「相对完整」：补齐 D1-D9 到实现级，accept 改为「先建后换」（DECISION-088），新增 D8b（poll 接线）、D10（阻塞睡眠）、D11（组播）与 G11（无阻塞语义）缺口，实施分期重构，新增决策记录章节并登记 DECISION-087 至 DECISION-091。全部分期与条目状态为 `[]`（未实施）。

---

## 变更历史

- 见 `git log -- docs/plan/socket-layer-completion.md`。
