# 网络栈 smoltcp 功能覆盖度评估报告

> 总体判断：Edgine 把 smoltcp 当作核心互联网传输栈在用——以太网 + IPv4 + IPv6(静态) + TCP + UDP + DHCPv4 这条主干是真实接线、真跑包的，形态上足以支撑一个能收发数据的内核网络面。但存在两处结构性缝隙：一是"启用 feature ≠ 接线使用"（ICMP / DNS / raw 三个开关开了没消费者）；二是"有 v6 协议栈却拿不到 v6 地址"（启用 proto-ipv6 却未启 SLAAC）。且决定"真实用户态软件能否跑起来"的关键瓶颈其实在 smoltcp 之外——面向用户态的 Linux socket ABI 兼容层。

本报告是网络栈对 vendored smoltcp 功能覆盖度的一次性快照，评估范围 smoltcp 0.14.0（锁定于 `src/kernel/functions/net/smoltcp/`）+ privileged/net 约 7.6K 行真实协议栈驱动 + functions/net 约 5.0K 行策略/句柄层。按"feature 空间 → 启用集 → 实际接线"三层对照，并给出目标档位的收敛建议。作为后续制定 plan 与修复工程的输入依据。

## 一、做得好的（主干接线证据）

**1. 接入纪律正：`default-features = false` + 显式列表** —— `src/kernel/Cargo.toml` L47-61 不盲从 smoltcp 默认全开（默认集含 6LoWPAN/RPL/tuntap 等），而是按需勾选 13 个功能 feature。这符合框架最小化立场。

**2. smoltcp 真被 privileged 层驱动跑包，不是 W3.2 骨架** —— W4 已落地：`privileged/net/smoltcp_impl.rs` 的 `EGDFNetDevice` 实现 smoltcp `phy::Device` trait（L85），经 EGDF `NetOps` 桥接真实网卡；`init_stack()` L177 `Config::new(HardwareAddress::Ethernet(...))` + `Interface::new(...)` 真构造接口；L167-168 `iface.poll(now, device, sockets)` 真轮询收发。

**3. SocketSet 真实装配、TCP/UDP 真建真用** —— `init.rs` L93 `SocketSet::new(&mut storage[..])`；`raw.rs` L322 `tcp::Socket::new()` + L326 `sockets.add()`、L349/L376 `udp::Socket::new()` + add；`sm_fi.rs` 17 处 `get_mut::<tcp::Socket>` / 11 处 `get_mut::<udp::Socket>` 覆盖 accept/connect/listen/send/recv。functions 层 `SmoltcpNetStack`（`smoltcp_impl.rs`）另做类型擦除句柄表 + 幂等/回滚不变式（DECISION-025/027）。

**4. DHCPv4 与双栈路由到位** —— DHCP：`init.rs` L506 `dhcpv4::Socket::new()` + L507 add + `raw.rs` L431 `get_mut::<dhcpv4::Socket>` 真驱动。IPv6：`sm_fi.rs` L19/L86 `Ipv6Address`/`IpAddress::Ipv6` 端点、`route.rs` L184 `ipv6_cidr_contains` + L312/L313 `Ipv6Cidr` 双栈路由；拥塞控制选 Cubic（`socket-tcp-cubic`）。

## 二、功能覆盖度矩阵与"启用未用"缝隙

以"协议/介质/socket 类功能 feature"（排除 `iface-*` 容量型与 std/alloc/log/defmt 构建型，共 33 个）为分母：Edgine 启用 13 个（≈40%），真正在数据路径上被消费的约 10 个。

| feature | 启用 | 实际接线 | 证据 |
|---|---|---|---|
| medium-ethernet | ✅ | ✅ 真用 | `smoltcp_impl.rs` L177 `Config::new` Ethernet |
| proto-ipv4 / -fragmentation | ✅ | ✅ 真用 | `sm_fi.rs`/`raw.rs` Ipv4Address、iface 内建重组 |
| proto-ipv6 | ✅ | ⚠️ 部分 | 寻址/路由真用，但无 SLAAC（见下 #2） |
| socket-tcp / -cubic | ✅ | ✅ 真用 | `raw.rs` L322 建 + `sm_fi.rs` 17 处操作 |
| socket-udp | ✅ | ✅ 真用 | `raw.rs` L349 建 + `sm_fi.rs` 11 处操作 |
| proto-dhcpv4 / socket-dhcpv4 | ✅ | ✅ 真用 | `init.rs` L506-507 建+add |
| **socket-icmp** | ✅ | ❌ **空开** | 全 privileged/functions `icmp::Socket` 创建数 = 0 |
| **proto-dns / socket-dns** | ✅ | ❌ **空开** | `dns::Socket` 创建 = 0；`init/dns.rs` L51 `dns_resolve()` 走内置静态 hosts（L17 自陈"D 阶段后续可换 smoltcp wire/dns"） |
| **socket-raw** | ✅ | ❌ **空开** | `raw::Socket` 创建 = 0；`raw.rs` 名指 syscall 直通路径，实建 tcp/udp/dhcp |

质量问题清单：

| # | 问题 | 证据 | 定性 |
|---|---|---|---|
| 1 | **三个"启用未用"开关**（socket-icmp / proto-dns+socket-dns / socket-raw）：进编译面却零 `::Socket` 消费者，虚增 vendored 面积与"支持 ICMP/DNS/raw"的叙事口径 | `icmp::Socket`/`dns::Socket`/`raw::Socket` 全仓创建数均 = 0；DNS 实为静态表 | 纪律缝隙（启用≠使用） |
| 2 | **IPv6 自相矛盾**：启用 `proto-ipv6` 做寻址/路由，却未启 `proto-ipv6-slaac`，等于"有 v6 协议栈但拿不到 v6 地址"（无 RA 自动配址，只能手工静态） | `src/kernel/Cargo.toml` L47-61 无 slaac | 半成品点 |
| 3 | **ping 不通 / 域名解析未接**：ICMP socket 未接 + `auto-icmp-echo-reply` 未开 → 内核不收发 ping；DNS 客户端未接 → 真实域名解析缺失 | 同 #1 | 可用性缺口 |
| 4 | **组播未开**：`multicast`（IGMP/MLD）未启用，真实用户态软件（mDNS/avahi、路由器发现、DHCP 广播、`IP_ADD_MEMBERSHIP`）依赖它 | `multicast` 不在启用列表 | 按目标而定（见第三节） |
| 5 | **真正的门槛在 smoltcp 之外**：`sockopt`(SO_REUSEADDR/IP_ADD_MEMBERSHIP/TCP_NODELAY/IPV6_V6ONLY…)、errno 语义、epoll/poll 就绪联动、`getaddrinfo` 路径——这些 Linux socket ABI 兼容度才决定"curl 能否真跑"，smoltcp 只是协议引擎 | 对照 Asterinas：网络"能用"的工程主体在 Linux 兼容 socket 层 | 战略提醒（非 feature 开关可解） |

## 三、目标档位收敛建议与后续方向

本报告采纳"可用通用 OS 栈"档位（目标：跑得动真实用户态网络软件）作为收敛基准。据此对 smoltcp 启用集的建议动作：

- **关掉**：`socket-raw`（无消费者；ARP 由 smoltcp iface 内部处理，ping 走 icmp socket，raw 仅抓包/自定义 L3 才需）；`socket-mdns`/`packetmeta-id` 暂缓（抓包工具链再开）。
- **接线补实现**（保持开启但补真实使用）：`socket-icmp`（建 icmp::Socket + ping）、`proto-dns`+`socket-dns`（真 DNS 客户端替换 `dns_resolve` 静态表）——把 40% 启用面转成真实使用面的关键两项。
- **新增开启**：`proto-ipv6-slaac`（消解问题 #2）、`auto-icmp-echo-reply`（内核自动回 ping，成本极低）、`multicast`（真实用户态软件需要）、`proto-ipv6-fragmentation`（完整 v6 上网，可后置）。
- **保持关闭**（与通用 OS 目标无关，符合简约原则）：`medium-ieee802154`/`proto-sixlowpan*`/`proto-rpl`/`proto-ipsec*`/`medium-ip`/`phy-tuntap_interface`/`phy-raw_socket`。

**建议推进顺序（性价比）**：

1. 先做纪律收口——关 `socket-raw`、把空开的 icmp/dns 要么接线要么关闭，让"启用面 = 使用面"（当前即可落、近零风险）；
2. 接 ICMP + `auto-icmp-echo-reply`（最小成本拿到 ping，立刻可诊断）；
3. 开 `proto-ipv6-slaac` + `multicast`（消解 IPv6 矛盾 + 铺组播）；
4. 接真实 DNS 客户端（中成本，"可用通用 OS"的分水岭）；
5. 单独立项攻 socket ABI 兼容层（问题 #5）——这是让真实用户态软件跑起来的主体工程，非 feature 开关可解。

后续据此制定 plan 时，建议以第二节编号 # 为条目索引；#1 纪律收口、#2 SLAAC、#5 socket ABI 为三条主线，其中 #1 与 #2 成本最低、可先行。
