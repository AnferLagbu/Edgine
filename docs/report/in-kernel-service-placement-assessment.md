# 内核内用户态服务归属偏离评估报告

> 总体判断：Edgine 当前有一批**本应在用户态的服务与策略**落在内核态。按用户裁定的通用内核（Linux 模型）判据，这些不是连贯的设计选择，而是**监管疏忽**——按便利性（引导自足、smoltcp 已在内核）未经归属审查滑入内核。权威判据与偏离定性见 [service-placement-design.md](../design/service-placement-design.md)；本报告是该立场册 §三偏离清单的一次性**证据化记录**，逐项落到当前源码位置，作为后续制定迁移 plan 与修复工程的输入依据。

本报告是「哪些代码根本不该在内核内」的归属快照，与框内核「内核内代码如何可信」（privileged/functions 分离、TCB 最小化）正交：前者解决归属，后者解决可信。评估范围 = 设计立场册 §三所列七个未经归属审查的内核内服务/策略子系统，另引用 §四已裁迁移（SGEG v2）与刻意例外（WASM）作对照，不重裁。报告只记录「当时观测到的事实」，正文只读；可执行分解归 `plan/`，本文不立施工清单。

## 一、判据：机制留内核，服务与策略退用户态

通用内核的分野不在功能多寡，而在**服务放在哪一侧**。Linux 模型下内核留机制（核心协议栈、驱动、netfilter 数据路径、文件系统数据路径、调度/内存/IPC），用户态承担服务与策略（DHCP/DNS 解析/NTP/认证/热插拔策略/固件供给）。框内核结构与此兼容而非冲突：机制留内核并以 safe 代理封装，服务退用户态。

新服务落地内核前须过**四问判据**：它是机制还是可替换策略？Linux 把等价物放哪一侧？故障是否应隔离到用户态进程而非拖垮内核？生态是否需要用户态守护接管它（dhclient / systemd-resolved / chrony）？四问偏向用户态者不得默认落内核。**引导自足例外**：启动早期无用户态守护生态，允许内核内临时客户端承担引导（Linux `ipconfig.c` 同款），但必须带**关闭开关**并标注为过渡，不作为长期形态。例外是权宜，不是立场。

## 二、监管疏忽偏离清单总览

下列子系统当前落在内核，且在 `docs/design/` 无各自设计立场册（该目录仅有 fd-object-model / freg / sgeg / wasm 四册）——即未经归属审查。这是「监管疏忽」的具体含义。

| 子系统 | 内核内形态（源码证据） | 业界用户态惯例 | 目标归属 | 关闭开关现状 |
|---|---|---|---|---|
| DHCPv4 客户端 | smoltcp vendored 内置 socket-dhcpv4 + 编译期策略 [smoltcp_impl.rs:773](../../src/kernel/functions/net/smoltcp_impl.rs#L773) `static CFG` | dhclient / systemd-networkd | 用户态守护 | 无 |
| DHCPv6 客户端 | 自研 functions 状态机 [driver.rs:134](../../src/kernel/functions/net/dhcpv6/driver.rs#L134) `poll_hook` | 同上 | 用户态守护（自研，迁移成本低于 v4） | **已有**（`enabled`，见 §六） |
| DNS 解析器 | 三级解析 [dns.rs:74](../../src/kernel/privileged/net/init/dns.rs#L74) `dns_resolve` + getaddrinfo syscall | libc getaddrinfo + systemd-resolved | 用户态解析器，内核留 socket 机制 | 无 |
| NTP / PTP 客户端 | [time_sync.rs:230](../../src/kernel/functions/timer/time_sync.rs#L230) `TimeSyncSubsystem` | chrony / systemd-timesyncd（内核仅 adjtimex 机制） | 用户态客户端，内核留时钟调整机制 | 无 |
| 热插拔策略 | [hotplug.rs:17](../../src/kernel/privileged/driver/hotplug.rs#L17) 已注「未来: 用户态通知」 | 内核发 uevent，udev 用户态做策略 | 内核留事件，策略退用户态 | 不适用 |
| 固件加载 | [firmware.rs:51](../../src/kernel/privileged/egdf/firmware.rs#L51) blob 库 + `EG_FW_LOAD=730` | udev + /lib/firmware，内核 request_firmware | 用户态供给，内核留请求机制 | 不适用 |
| 路由控制面 | `EG_ROUTE_*` 807-809 部分用户面 | rtnetlink 完整用户面 | 补齐用户态控制面（**补全，非迁移**） | 不适用 |

## 三、逐项源码证据与迁移考量

**DHCPv4 客户端** —— smoltcp vendored 内置 `socket-dhcpv4`，策略以编译期 `static CFG: DhcpPolicyConfig` 写死于 [smoltcp_impl.rs:773](../../src/kernel/functions/net/smoltcp_impl.rs#L773)，经 [smoltcp_impl.rs:1489](../../src/kernel/functions/net/smoltcp_impl.rs#L1489) `socket_open(SocketKind::Dhcpv4)` 建槽驱动。它是可替换的地址配置策略（Linux 放 dhclient/systemd-networkd），故障应隔离到独立进程，却常驻内核。**迁移成本不对称的高侧**：v4 是第三方锁定子树内的 vendored socket，抽取须动 smoltcp，成本高于自研 v6。当前无关闭开关。

**DHCPv6 客户端** —— 自研 functions 状态机（[dhcpv6/](../../src/kernel/functions/net/dhcpv6/) 子树），由 privileged `poll_network` 经擦除钩子在 `NET_STATE` 锁内驱动 [driver.rs:134](../../src/kernel/functions/net/dhcpv6/driver.rs#L134) `poll_hook`。同为可替换地址配置策略。**迁移成本低于 v4**：纯自研 functions 状态机，整体可搬，不需动第三方。运行时策略配置面（#298）已带 `enabled` 关闭开关（[driver.rs:218](../../src/kernel/functions/net/dhcpv6/driver.rs#L218) `set_policy`），是设计立场册 §五所指「第一个钩子」，让用户态守护接管前可先关内核客户端。

**DNS 解析器** —— 三级解析（IPv4 字面量 → 静态 hosts → smoltcp `dns::Socket` 真实查询）[dns.rs:74](../../src/kernel/privileged/net/init/dns.rs#L74) `dns_resolve`，经 getaddrinfo syscall（[syscall.rs:680](../../src/kernel/privileged/net/syscall.rs#L680)）对用户态暴露。Linux 把解析放 libc getaddrinfo + systemd-resolved，内核只留 socket 机制。**迁移牵涉 getaddrinfo syscall 去留**（设计立场册 §六开放议题）：迁移后它保留为内核 fallback，还是退化为 libc 用户态实现，待定。当前无关闭开关。

**NTP / PTP 客户端** —— [time_sync.rs:230](../../src/kernel/functions/timer/time_sync.rs#L230) `TimeSyncSubsystem`（NTP 客户端 + PTP IEEE 1588），经 [time_sync.rs:462](../../src/kernel/functions/timer/time_sync.rs#L462) `timesync_syscall` 暴露。Linux 放 chrony/systemd-timesyncd，内核仅留 adjtimex 时钟调整机制。此处**机制与策略混居**：时钟调整（adj_freq/adj_time/set_time）属内核机制应留，NTP/PTP 协议客户端与同步策略属服务应退用户态。当前无关闭开关。

**热插拔策略** —— [hotplug.rs:97](../../src/kernel/privileged/driver/hotplug.rs#L97) `HotplugManager` 在内核扫描槽位、分发事件并调用重枚举回调，源码 [hotplug.rs:17](../../src/kernel/privileged/driver/hotplug.rs#L17) 已自注「未来: 用户态通知 (/dev/hotplug)」。Linux 模型下内核发 uevent（机制），udev 在用户态做策略。目标归属是**内核留事件、策略退用户态**，而非整体迁移——事件检测/上报机制留内核，「对热插拔做什么」的策略退用户态。

**固件加载** —— [firmware.rs:51](../../src/kernel/privileged/egdf/firmware.rs#L51) `FirmwareBlob` 把固件 blob 紧耦合到 EGDF 设备树节点，另经 `EG_FW_LOAD=730`（[types.rs:752](../../src/kernel/privileged/syscall/types.rs#L752)）从用户态路径读取文件附着。Linux 模型是 udev + /lib/firmware 用户态供给、内核仅 request_firmware 请求机制。此处已有部分用户态供给面（EG_FW_LOAD 读用户态路径），偏离点在内核侧 blob 库与供给策略。目标是**用户态供给、内核留请求机制**。

**路由控制面** —— `EG_ROUTE_ADD=807` / `EG_ROUTE_DEL=808` / `EG_ROUTE_QUERY=809`（[types.rs:813](../../src/kernel/privileged/syscall/types.rs#L813)）已是部分用户面。Linux 用 rtnetlink 完整用户面。此项**定性为补全而非迁移**：路由数据路径属内核机制应留，缺的是完整用户态控制面，补齐即可，不属「服务滑入内核」的监管疏忽，列此仅为对照完整。

## 四、已裁迁移与刻意例外（引用而非重裁）

设计立场册 §四对已有各自立场册的子系统「引用而非重裁」，本报告同样不重裁：

**SGEG（已裁迁移）** —— [sgeg-design.md](../design/sgeg-design.md) v2 目标已裁「内核侧收敛为默认关闭、启用即强制的安全模块，只留主体标签/客体标签/转移规则/判定四原语；身份、策略语言、可信启动与审计消费全部退用户态」。与本文判据完全一致：内核留判定机制，服务退用户态。属已裁迁移目标，引为对齐范例。

**WASM（刻意例外，张力开放）** —— [wasm-runtime-design.md](../design/wasm-runtime-design.md) 把内核内 safe 解释器论证为刻意立场：F1（functions 0 unsafe）排除 JIT，单地址空间 + 函数调用替代 IPC 是性能立场。这与「服务退用户态」存在张力（通用内核并不排斥用户态 WASM，Wasmtime 即用户态），但其内核内立场源自框内核约束而非 RTOS 惯性。设计立场册不擅自重裁，张力列为开放议题：保留为显式例外，还是重开用户态迁移评估，待用户裁定。

## 五、正当留内核的机制（非偏离，防过度迁移）

下列在 Linux 模型下本就属内核，**不是偏离，不迁移**——记录于此以防把「服务退用户态」误读为「一切退用户态」：netfilter（Linux nftables 也在内核）、SLAAC / RA、核心协议栈（TCP/UDP/IP）、文件系统数据路径、驱动（经 safe 代理）、调度/内存/IPC。文件系统缺的是用户态**选项**（FUSE 通道），不是把内核文件系统搬走。判据是「机制 vs 服务」，不是「内核 vs 用户态」的情绪化二分。

## 六、迁移前提与关闭开关现状

用户态服务基建当前缺失，迁移不能凭空发生。设计立场册 §五的前提面：raw socket 面（AF_PACKET 等价，DHCP/DNS 守护配地址前收发报文所需，当前 raw 捕获仅内核内部使用）；地址配置面（rtnetlink 等价，增删地址/路由）；内核客户端关闭开关；EGDF syscall 接线（bind/map/forward_irq 已建并有 C ABI 导出，但无 syscall 入口、无生产消费者）；FUSE 通道。

**关闭开关现状**：DHCPv6 运行时策略面的 `enabled` 字段（#298 已实装）是第一个钩子——`enabled=false` 使内核客户端 poll 钩子惰性返回，供用户态守护接管。其余内核内客户端（DHCPv4 / DNS / NTP）尚无关闭开关。**次序主张**（立场，非施工清单）：先补前提面，再迁服务客户端（DHCP/DNS/NTP），系统服务按各自设计册推进，不做大爆炸迁移。**共存过渡**：迁移期内核客户端与用户态守护共存，靠关闭开关切换，内核客户端保留为引导 fallback，保证任一步不破坏「开机即有网」。

## 七、结论与后续输入

本报告确认：Edgine 存在七个未经归属审查的内核内服务/策略偏离（其中路由控制面定性为补全而非迁移），属监管疏忽而非连贯设计。它们按便利性滑入内核，违背通用内核「机制留内核、服务退用户态」判据。证据已逐项落到当前源码位置。

后续输入方向（可执行分解归 `plan/`，本文不立清单）：迁移优先级与时间窗取决于用户态服务基建（raw socket / 地址配置 / EGDF / FUSE）的立项次序；DHCPv6 因自研 + 已有关闭开关，迁移成本最低，可作首个试点；DNS 迁移须先裁 getaddrinfo syscall 去留；审查门槛的执行形态（review checklist 约定 vs 机器审计腿「内核内新增服务客户端须登记例外」）待定，以防服务再次未经审查滑入内核。权威判据以 [service-placement-design.md](../design/service-placement-design.md) 为准，本报告是其偏离清单的证据化快照。