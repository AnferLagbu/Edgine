# 通用内核服务归属设计立场

> Edgine 是**通用内核**（Linux 模型）：内核提供机制，用户态承担服务与策略。当前内核内的服务层是需逐步迁移的偏离，而非目标形态；本文立下跨子系统的归属判据与审查门槛，防止服务再次未经审查滑入内核。

## 一、定位：通用内核，而非 RTOS 或微内核

用户裁定 Edgine 的设计理念是**通用内核**（General-Purpose Kernel），不是实时操作系统（Real-Time Operating System, RTOS），也不是微内核（Microkernel）。这三者的分野不在功能多寡，而在**服务放在哪一侧**：

- **RTOS 形态**：内核即自足服务提供者，DHCP / DNS / 时间同步等客户端常驻内核，开机即有网、无用户态守护生态。Zephyr / lwIP 是这一形态的代表。
- **微内核形态**：核心协议栈、驱动、文件系统也搬用户态，内核只留 IPC / 调度 / 内存机制。seL4 / Fuchsia 是这一形态的代表。
- **Linux 模型（本文立场）**：内核留机制（核心栈、驱动、netfilter 数据路径、文件系统数据路径），用户态承担服务与策略（DHCP / DNS 解析 / NTP / 认证 / 热插拔策略 / 固件供给）。

Edgine 的框内核（Framekernel）结构——privileged（TCB，可信计算基）与 functions（100% safe Rust）分离——是**内核内 TCB 最小化**，与 Asterinas 同源（见 [explain-framekernel.md](../explain/explain-framekernel.md)）。它与 Linux 模型兼容而非冲突：机制留内核并以 safe 代理封装，服务与策略退用户态。框内核解决的是「内核内的代码如何可信」，本文解决的是「哪些代码根本不该在内核内」——两者正交。

设计立场一句话：**机制（mechanism）留内核，服务与策略（service / policy）退用户态；内核内服务客户端只能是带关闭开关的引导过渡，不是长期形态**。

本文是跨子系统的**顶层判据**。SGEG 与 WASM 已有各自的设计立场册（[sgeg-design.md](./sgeg-design.md)、[wasm-runtime-design.md](./wasm-runtime-design.md)），本文引用其结论而不取代其论证；其余子系统的设计立场以本文为准。

## 二、归属判据：机制 vs 服务与策略

**机制**的特征：硬件或性能关键、不含可替换的策略选择、Linux 也将等价物放在内核。核心协议栈（TCP / UDP / IP）、netfilter 数据路径、SLAAC / RA、驱动（经 safe 代理）、文件系统数据路径、调度 / 内存 / IPC 都属此类。它们留在内核不是疏忽，而是 Linux 模型的本来归属。

**服务与策略**的特征：策略可替换、故障应隔离到独立进程、生态存在用户态等价物、Linux 将等价物放在用户态。DHCP 客户端、DNS 解析器、NTP / PTP 客户端、认证 / 身份 / 会话、热插拔策略分发、固件加载策略都属此类。

新服务落地内核前，须过**四问判据**：

- 它是机制还是策略？策略可替换吗？
- Linux 把等价物放在哪一侧？
- 它的故障是否应隔离到用户态进程，而非拖垮内核？
- 生态是否需要用户态守护接管它（如 dhclient / systemd-resolved / chrony）？

四问偏向用户态者，**不得默认落内核**。确需内核内过渡的（见下），须先有设计立场册论证例外理由。

**引导自足例外**：启动早期没有用户态守护生态，允许内核内临时客户端承担引导（Linux 的 `ipconfig.c` 是同款例外）。但这类客户端必须带**关闭开关**（让用户态守护接管前先关内核客户端），并在代码与文档中标注为过渡，不作为长期形态。例外是权宜，不是立场。

## 三、偏离清单：未经归属审查的内核内服务

下列子系统当前落在内核，且在 `docs/design/` 无设计立场册（该目录仅有 fd-object-model / freg / sgeg / wasm 四册）——即未经归属审查、按便利性（引导自足、smoltcp 已在内核）滑入内核。这是「监管疏忽」的具体含义，而非连贯的设计选择。

| 子系统 | 当前内核内形态 | 业界用户态惯例 | 目标归属 |
|---|---|---|---|
| DHCPv4 客户端 | smoltcp 内置 socket-dhcpv4 + 编译期 [smoltcp_impl.rs:773](src/kernel/functions/net/smoltcp_impl.rs) `static CFG` | dhclient / systemd-networkd | 用户态守护 |
| DHCPv6 客户端 | 自研状态机 [driver.rs:131](src/kernel/functions/net/dhcpv6/driver.rs) 默认 cfg | 同上 | 用户态守护（自研，迁移成本低于 v4） |
| DNS 解析器 | [dns.rs:74](src/kernel/privileged/net/init/dns.rs) 三级解析 + getaddrinfo syscall | libc getaddrinfo + systemd-resolved | 用户态解析器，内核留 socket 机制 |
| NTP / PTP 客户端 | [time_sync.rs:230](src/kernel/functions/timer/time_sync.rs) `TimeSyncSubsystem` | chrony / systemd-timesyncd（内核仅 adjtimex 机制） | 用户态客户端，内核留时钟调整机制 |
| 热插拔策略 | [hotplug.rs:17](src/kernel/privileged/driver/hotplug.rs) 已注「未来: 用户态通知」 | 内核发 uevent，udev 用户态做策略 | 内核留事件，策略退用户态 |
| 固件加载 | [firmware.rs:49](src/kernel/privileged/egdf/firmware.rs) blob 库 + EG_FW_LOAD=730 | udev + /lib/firmware，内核 request_firmware | 用户态供给，内核留请求机制 |
| 路由控制面 | EG_ROUTE_* 807-809 部分用户面 | rtnetlink 完整用户面 | 补齐用户态控制面（补全，非迁移） |

DHCPv4 与 DHCPv6 的迁移成本不对称：v4 是 smoltcp vendored 内置 socket，抽取须动第三方锁定子树；v6 是自研 functions 状态机，整体可搬。DNS 解析器迁移还牵涉刚接线的 getaddrinfo syscall 的去留（见§六开放议题）。

## 四、既有立场的子系统：引用而非重裁

**SGEG**：[sgeg-design.md](./sgeg-design.md) 的 v2 目标已裁「内核侧收敛为默认关闭、启用即强制的安全模块，只留主体标签 / 客体标签 / 转移规则 / 判定四原语；身份、策略语言、可信启动与审计消费全部退用户态」。这与本文判据**完全一致**——内核留判定机制，服务退用户态。SGEG 属已裁迁移目标，本文引为对齐范例，不重复其论证。

**WASM**：[wasm-runtime-design.md](./wasm-runtime-design.md) 把内核内 safe 解释器论证为**刻意立场**：F1（functions 0 unsafe）排除 JIT（Just-In-Time，即时编译），单地址空间 + 函数调用替代 IPC（Inter-Process Communication，进程间通信）是性能立场。这与「服务退用户态」存在**张力**：通用内核并不排斥用户态 WASM（Wasmtime 即用户态），但 WASM 的内核内立场源自框内核约束，而非 RTOS 惯性。本文**不擅自重裁**这一既有立场，张力列为开放议题（§六），由用户裁定是保留为显式例外，还是重开用户态迁移评估。

**对齐项（留内核，非偏离）**：netfilter（[netfilter.rs](src/kernel/privileged/net/netfilter.rs)，Linux nftables 也在内核）、SLAAC / RA、核心协议栈、文件系统数据路径（15 个文件系统，Linux 也在内核）、驱动。这些在 Linux 模型下本就属内核，不迁移。文件系统缺的是用户态**选项**（FUSE 通道，见§五），不是把内核文件系统搬走。

## 五、迁移前提与次序

用户态服务基建当前缺失，迁移不能凭空发生。前提面：

- **raw socket 面**（AF_PACKET 等价）：DHCP / DNS 守护在配地址前收发报文所需。当前 raw 捕获仅内核内部使用（如 RA 捕获），无用户态面。
- **地址配置面**（rtnetlink 等价）：增删地址 / 路由。路由 EG_ROUTE_* 已有部分，地址配置面待补。
- **内核客户端关闭开关**：避免内核客户端与用户态守护双客户端打架。DHCPv6 运行时策略面的 `enabled` 字段是第一个钩子。
- **EGDF syscall 接线**：[user_driver.rs:63](src/kernel/privileged/egdf/user_driver.rs) 的 bind / map / forward_irq 已建并有 C ABI 导出，但无 syscall 入口、无生产消费者（[api.rs:16](src/kernel/privileged/syscall/api.rs) 仅文档提及）。这是用户态驱动的前提，框架已造好、差最后一公里接线。
- **FUSE 通道**：用户态文件系统前提。当前 [virtiofs.rs](src/kernel/functions/fs/virtiofs.rs) 仅客户协议，无 FUSE（Filesystem in Userspace，用户态文件系统）通道。

**次序主张**（立场，非施工清单）：先补前提面（raw socket + 地址配置 + 关闭开关），再迁服务客户端（DHCP / DNS / NTP）；系统服务（hotplug / 固件 / SGEG v2）按各自设计册推进。**不做大爆炸迁移**。可执行分解归 `plan/`，本文只立主张。

**共存过渡**：迁移期内核客户端与用户态守护共存，靠关闭开关切换；内核客户端保留为引导 fallback。这保证迁移任一步都不破坏「开机即有网」。

## 六、审查门槛与开放议题

**审查门槛（防再次疏忽）**：新增内核内服务 / 策略代码，落地前须过§二四问判据；偏用户态者须先有设计立场册论证为何例外（如引导过渡），否则归用户态。门槛的执行形态——文档 review 约定，还是机器审计腿——待定。立判据的价值在于：服务归属从此有据可查，不再靠便利性默认。

**开放议题**：

- **WASM 内核内立场 vs 通用判据的张力**：保留为显式例外（框内核约束推导），还是重开用户态迁移评估？待用户裁定。本文不预判。
- **审查门槛的执行形态**：review checklist 约定 vs 机器审计腿（如「内核内新增服务客户端须登记例外」）？待定。
- **迁移优先级与时间窗**：取决于用户态服务基建（raw socket / 地址配置 / EGDF / FUSE）的立项次序，待 `plan/` 分解。
- **DNS 解析器迁移与 getaddrinfo syscall 的关系**：getaddrinfo 刚作为内核 syscall 接线；迁移后它保留为内核 fallback，还是退化为 libc 用户态实现？待定。