# 双栈 DNS 解析改造计划 (IPv6/AAAA + DHCPv6 DNS 喂入)

> 把 Edgine 现有 smoltcp `dns::Socket` 解析路径从 IPv4-only 升级为 IPv4/IPv6 双栈,
> 复用 DECISION-032 已落地的 `IpAddr` 枚举作返回面, 并接通 DHCPv6 学到的 IPv6 DNS
> 服务器. 实现路径已裁定: **相对完整 (B) + 双栈 `IpAddr` 枚举返回面**.

---

## 背景与纠错

**前置纠错**: 早期评估曾称「Edgine 自研 DNS 解析器, 未接 smoltcp DNS」, 经源码核实
**该结论错误**. 实际 [privileged/net/init/dns.rs](../../src/kernel/privileged/net/init/dns.rs)
的 `dns_query_net` 早已走 smoltcp `dns::Socket` (`start_query` / `get_query_result` /
`cancel_query`), 服务器经 `apply_dns_servers` 的 `update_servers` 喂入. 本工程不是
「替换自研解析器」, 而是「把已有的 smoltcp DNS 路径扩到双栈」.

当前 IPv4-only 是**多层硬接线**, 非 smoltcp 能力缺失:

- 查询只发 `DnsQueryType::A`, 结果只取 `IpAddress::Ipv4` (dns.rs `dns_query_net`).
- 返回面 `dns_resolve -> Option<[u8; 4]>` / `get_dns_servers -> [Option<[u8; 4]>; 3]`
  ([api.rs](../../src/kernel/privileged/net/api.rs)).
- 服务器存储 `G_DNS: [AtomicU32; 3]` 只装 IPv4 ([state.rs](../../src/kernel/privileged/net/init/state.rs)),
  全仓无 IPv6 DNS 存储.
- DHCPv6 学到的 `dns_servers: Vec<[u8; 16]>` ([dhcpv6/state.rs](../../src/kernel/functions/net/dhcpv6/state.rs))
  经 `out.apply_options` 输出, 但 [dhcpv6/driver.rs](../../src/kernel/functions/net/dhcpv6/driver.rs)
  `execute` 的 SIMPLIFIED 丢弃了它.

**smoltcp 侧已就绪** (vendored, 零修改):

- `wire::DnsQueryType` (= `wire::Type` re-export) 含 `A` / `Aaaa`.
- `get_query_result` 返 `Vec<IpAddress, DNS_MAX_RESULT_COUNT>`, 本就含 IPv6.
- `update_servers(&[IpAddress])` 接受 v4/v6 混合.
- `register_query_waker` 提供异步查询唤醒 (增强阶段可用).

---

## 设计决策

### D1: 返回面复用现有 `IpAddr` 枚举 (破坏性改造)

- **描述**: `dns_resolve` 返回类型由 `Option<[u8; 4]>` 改为 `Option<IpAddr>`, 复用
  DECISION-032 已落地的 [iface_trait.rs `IpAddr { V4(Ipv4Addr), V6(Ipv6Addr) }`](../../src/kernel/privileged/net/iface_trait.rs),
  不新造类型.
- **方案**:
  - `api.rs::dns_resolve` / `init::dns::dns_resolve` 签名改 `Option<IpAddr>`.
  - 一次性迁移调用方: boot 探针 (init.rs `dns_resolve("example.com")`) + 既有单元测试.
  - `get_dns_servers` 服务器观测面增 IPv6 (见 D3), 与解析返回面分离.
- **状态**: [X]
- **详情**: 破坏性改造符合双栈工程 D2 先例 (编译期暴露所有调用点). 用户已裁定双栈枚举形态.
  已落地: `api.rs` / `dns.rs` 签名改 `Option<IpAddr>`; boot 探针 (init.rs) 改 `IpAddr::V4/V6`
  匹配 (V6 打印原始 16 字节, 诊断用); 既有 5 个 DNS 单测迁移后全绿. 全仓核查 `dns_resolve`
  仅 boot 探针 + api 包装 + 单测三处消费, 无 syscall/ABI 调用方, 破坏面收敛.

### D2: 解析语义 — A + AAAA 双查询, IPv6 优先

- **描述**: `dns_resolve` 发 `Aaaa` 与 `A` 查询, 返回首个成功地址, 优先 IPv6.
- **方案**:
  - 结果映射 `IpAddress::Ipv4 -> IpAddr::V4` / `Ipv6 -> IpAddr::V6`.
  - 优先级: AAAA 成功返 V6, 否则 A 返 V4 (RFC 6724 目的地址选择倾向 IPv6; 用户已裁定 IPv6 优先).
  - 静态 hosts / IPv4 字面量短路路径保持 (返 `IpAddr::V4`); IPv6 字面量文本解析延后 (见局限).
- **状态**: [X]
- **详情**: `DNS_MAX_RESULT_COUNT = 1` → 每查询返单地址, A/AAAA 各一, 语义清晰.
  **施工偏差 (顺序 vs 并发)**: 原方案设想 A + AAAA 并发双查询 (借 `DNS_QUERY_SLOTS = 2`);
  实际取**顺序双查询** (先 AAAA 后 A), 已在 dns.rs `dns_resolve` 以 SIMPLIFIED 注释标记.
  理由: AAAA 无记录时 smoltcp 立即返 `Failed` (非满窗超时), 实践延迟与并发等价; 顺序复用
  既有单查询有界等待/取消机制, 外科手术低风险. 影响面: AAAA 满窗超时后才发 A, 最坏 2× 有界
  自旋. 何时扩展: E2 异步化时改并发双查询省延迟.

### D3: IPv6 DNS 服务器存储 + DHCPv6 喂入

- **描述**: 新增 IPv6 服务器存储, 接通 DHCPv6 `apply_options.dns_servers`.
- **方案**:
  - `NetState` 增 `dns6_servers: [Option<[u8; 16]>; 3]` (锁保护; 存裸字节而非 `Ipv6Addr`,
    与 functions 侧 `[u8; 16]` 直通, 免类型往返; `AtomicU128` 不可用故落 `NET_STATE`;
    与既有 `dns_handle` 同结构). 另增 lock-free 观测位 `G_DNS6_PRESENT: AtomicBool`
    (供 `dns_query_net` 无锁短路判, 镜像 `G_DNS`).
  - `Dhcpv6PollCtx` 增 `apply_dns6(&[[u8; 16]])` safe 方法 (F2 类型擦除, 不泄漏 smoltcp 类型):
    写 `NetState.dns6_servers` + 置 `G_DNS6_PRESENT` + 调 `update_servers`.
  - `driver.rs::execute` 消费 `out.apply_options.dns_servers` 调 `ctx.apply_dns6`
    (SIMPLIFIED 收窄到仅 `refresh_s` 未消费, DNS 喂入已接线).
  - `apply_dns_servers` 合并 IPv6 (`dns6_servers`) + IPv4 租约为 `&[IpAddress]` 喂 `update_servers`,
    IPv6 优先排前 (截断到 1 时即优先 IPv6); 两侧皆空落 `FALLBACK_DNS`.
- **状态**: [X]
- **详情**: **关键容量约束** — smoltcp `DNS_MAX_SERVER_COUNT = 1` (build.rs), `update_servers`
  截断到首个服务器. 故 `G_DNS[3]` 三槽对解析冗余, socket 只用首个. 又因 **DNS 服务器地址族
  与查询类型无关** (IPv4 服务器可答 AAAA, IPv6 服务器可答 A), 双栈解析只需 1 个服务器.
  喂入策略: 有 IPv6 服务器优先用之, 否则 IPv4 (待用户确认). 多服务器冗余 (抬 `DNS_MAX_SERVER_COUNT`)
  涉及 vendored build.rs 配置 + purity 门禁, 列为可分离增强, 默认不做.

### D4: 服务器观测面双栈

- **描述**: 暴露 IPv6 DNS 服务器观测面, 与 v4 面并行.
- **方案**: 增 `get_dns6_servers() -> [Option<[u8; 16]>; 3]` (query.rs 取 `NET_STATE` 锁读
  `raw::dns6_servers`, api.rs 加 cfg 门控包装); v4 面 `get_dns_servers` 不破坏既有消费者.
- **状态**: [X]
- **详情**: **施工偏差 (跳过 `NetStatus.dns6`)**: 原方案设想 `NetStatus` 增 `dns6` 字段;
  实际**未加**. 理由: `NetStatus::capture()` 当前逐字段 lock-free 原子读 (观测/debug 用,
  文档明示「单次原子读, 多字段可能轻微不一致」), 而 IPv6 服务器本体存于 `NET_STATE` 锁内,
  加 `dns6` 会迫使 `capture()` 取锁, 破坏其无锁契约. 故 IPv6 观测改由独立的
  `get_dns6_servers()` (显式取锁) 承担, `NetStatus` 保持纯 v4 lock-free 快照. 何时扩展:
  若将来 `capture()` 整体改取锁快照, 可一并纳入 `dns6`.

---

## 实施步骤 (核心)

### Phase 1: IPv6 服务器存储 + DHCPv6 喂入

- **条目**: D3 落地
- **描述**: `NetState.dns6_servers` + `Dhcpv6PollCtx::apply_dns6` + driver 接线 + `apply_dns_servers` 合并喂入.
- **方案**: 见 D3. 涉及 state.rs / raw.rs (accessor) / dhcpv6.rs (ctx 方法) / dns.rs (apply) / driver.rs (execute).
- **状态**: [X]
- **验证**: host 单测 (IPv6 服务器写入 + update_servers 合并); DHCPv6 host 集成断言 apply_options 喂入.

### Phase 2: 双栈解析返回面

- **条目**: D1 + D2 + D4 落地
- **描述**: `dns_resolve -> Option<IpAddr>` + A/AAAA 双查询 + 服务器观测面双栈.
- **方案**: 见 D1/D2/D4. 涉及 dns.rs / api.rs / query.rs / init.rs (探针 + 测试迁移).
- **状态**: [X]
- **验证**: host 单测 (IpAddr 返回 / v4 回归 / 短路护栏) 迁移后全绿 (三成员 12+796+527);
  AAAA 真实解析需真网络, host 侧 `NET_READY=false` 短路, 归 E1 用户态探针 e2e 验证.

### Phase 3: 测试 + 七门槛

- **条目**: 回归锁与门禁收口
- **描述**: host-tests 双栈 DNS 契约锁 + privileged 合并算法单测 + §2.3 七门槛.
- **方案**: 新增 `dns_dual_stack_test.rs` (5 契约锁: D1 返回面 / D4 观测面 / D3 存储短路 / DHCPv6 喂入接线 / IPv6 优先合并序);
  抽 `merge_dns_servers` 纯函数 + 5 内联单测 (IPv6 优先 / 跳零槽 / 纯 IPv6 / 回退 / 满容量). 七门槛全绿.
- **状态**: [X]
- **验证**: build.sh all (6/6) / audit full (clippy 双架构 + fmt + perf + doc paths + Lockbud + QEMU 2/2 + feature 4/4) /
  test-host (0 failed) / test-kernel-host (三成员 12+796+527) / ci-rustdoc (双架构 exit 0) 全绿.
- **详情 (真实 AAAA e2e 并入 E1, 用户裁定)**: 原方案设想复用 `qemu_slaac_test.sh` 增 IPv6 DNS 记录 + AAAA 断言,
  调研发现现有基建缺可观测面: 内核唯一 DNS 观测是 boot `dns-selftest` 探针 (`net_init_register` 内一次性),
  运行时机**早于 DHCPv6 绑定** (DHCPv6 由 poll 钩子在 RA M=1 后数秒才 bound), 此刻 IPv6 DNS 服务器未学到,
  `dns_query_net` 无锁短路返 `None`; 用户态 DNS syscall 属 E1 未实装; DHCPv6 poll 路径持 `NET_STATE` 锁,
  `dns_resolve` 的有界等待 (`poll_network` + 自旋) 嵌套必死锁, 不能就地加阻塞探针. 故真实
  「DHCPv6 学 IPv6 DNS → AAAA 解析」e2e 随 E1 落地: getaddrinfo syscall + 用户态进程上下文探针
  (可阻塞等待绑定后解析) 自然提供可观测面, 届时扩 `qemu_slaac_test.sh` dnsmasq (开 DNS port + AAAA 记录 + 断言).

---

## 可分离增强 (B 捆绑, 待裁范围)

> 按 §11.3「相对完整 ≠ 工业级功能最全」与 §11.2 外科手术原则, 以下超出「IPv6 DNS 可用」
> 核心需求, 单列阶段, 由用户裁定是否纳入本工程或延后独立立项.

### E1: getaddrinfo 类 syscall (用户态双栈消费者) + 真实 AAAA e2e

- **描述**: 新增用户态 syscall 返回双栈地址 (当前 `dns_resolve` 仅 boot 探针 + api 内部消费, 无用户态面).
  附带承接 Phase 3 移交的真实 AAAA e2e: 用户态进程上下文探针在 DHCPv6 绑定后调 getaddrinfo 解析,
  提供 boot 探针缺位的可观测面 (见 Phase 3 详情).
- **方案**: net syscall 分发面增原生 `EG_GETADDRINFO` (getaddrinfo 非 Linux syscall, 取空闲编号 812),
  返单个 `EgAddrInfo { family: u32, addr: [u8; 16] }` (repr(C), 20 字节); 复用 D2 双查询.
  e2e: 扩 `qemu_slaac_test.sh` dhcpv6 模式 dnsmasq (去 port=0 开 DNS + 本地 AAAA 记录 + 经 DHCPv6 选项 23
  通告自身为 IPv6 DNS), 用户态探针解析该记录并 printk, 断言 AAAA 命中 (用户 sudo 腿).
- **状态**: [X]
- **详情**: 用户裁定返回面为**双栈 `IpAddr` 枚举单地址** (非列表). 分五批落地:
  - **E1-a** ABI 单源: `edgine-abi/src/net.rs` 新增 `EG_GETADDRINFO=812` / `EG_AF_INET=2` / `EG_AF_INET6=10` /
    `EgAddrInfo` (repr(C)); `privileged/syscall/types.rs` 镜像编号 (单源对账).
  - **E1-b** 内核后端 + functions 代理 + 分发: `privileged/net/syscall.rs::getaddrinfo_syscall`
    (`raw_copy_in` 取名 → UTF-8 校验 → `dns_resolve` → `ip_to_addrinfo` 映射 → `write_struct_to_user`);
    错误面 None→ENOENT / 非 UTF-8→EINVAL / 坏缓冲→EFAULT; functions 代理 + dispatch 接线;
    全链 `#[cfg(not(feature = "kernel_test"))]` 门控 (与既有 net syscall 同口径).
  - **E1-c** userlib 包装 + init 探针: `sys.rs::getaddrinfo(name, out)` 经 sys3 发 `EG_GETADDRINFO`;
    init `dns_getaddrinfo_probe` (x86_64, DHCPv6 绑定后有界重试 120×50ms, 命中打印 `[dns6] AAAA ok`).
  - **E1-d** 回归测试: `dns_getaddrinfo_test.rs` 4 host 契约锁 (ABI 单源 / `EgAddrInfo` 布局 /
    dispatch 不回落 ENOSYS+ENOENT / userlib 接线) + 后端 `ip_to_addrinfo` 2 内联单测 (V4 零扩展 / V6 原样).
  - **E1-e** e2e 扩展: `qemu_slaac_test.sh` dhcpv6 模式 dnsmasq 开 DNS + `address=/test6.edgine/fd00::53` +
    `dhcp-option=option6:dns-server,[fd00::1]` (选项23) + `[dns6] AAAA ok` 断言; slaac 模式不变 (port=0 保留).
  - **施工偏差 (单地址非列表)**: 原方案设想返 `IpAddr` 列表; 实际返单个 `EgAddrInfo`. 理由: 用户裁定双栈
    枚举单地址返回面, 且 smoltcp `DNS_MAX_RESULT_COUNT=1` 每查询本就单地址, 列表无信息增量. 何时扩展:
    E4 抬容量后若需多地址再改列表面.
  - **host 测试覆盖局限 (预存, 非本批引入)**: 非 UTF-8→EINVAL / 坏缓冲→EFAULT 两条错误分支需真实用户内存
    搬运 (`raw_copy_in` / `write_struct_to_user`), 而 `copy_from_user` 的 SMAP 护栏 `smap_is_enabled` 读 CR4
    (ring-0 特权指令), host x86_64 (ring-3) 必 SIGSEGV, 故无法在 host 腿覆盖; 其映射逻辑由后端源码契约 +
    e2e 真机腿保证. 此为 host/裸机分歧的预存测试缺口 (任何走真实用户内存搬运的 syscall 同受影响),
    未改 privileged 强测绿 (§11.2), 处置待用户裁定 (§11.5).

### E2: 异步化 (waker → wait_queue)

- **描述**: 替换 `dns_query_net` 的有界自旋 SIMPLIFIED, 改 `register_query_waker` + wait_queue 阻塞 (复用 P5 形状).
- **方案**: smoltcp `register_query_waker` 已就绪; 接 functions/net/wait_queue.rs.
- **状态**: [] (待裁)

### E3: DNS 缓存 + search domain

- **描述**: 查询缓存 (TTL) + search domain 补全.
- **方案**: 独立缓存表 + 名字补全策略.
- **状态**: [] (延后独立立项; 用户裁定本工程仅核心 + E1/E2)

### E4: 多服务器冗余 (抬 DNS_MAX_SERVER_COUNT)

- **描述**: 支持同时 v4+v6 多服务器.
- **方案**: 改 vendored build.rs `DNS_MAX_SERVER_COUNT` + 同步 `SMOLTCP_LOCAL_SRC_HASH` / purity 门禁.
- **状态**: [] (延后独立立项; 涉 vendored 配置, 用户裁定本工程不做)

---

## 风险与约束

- **TCB 边界**: 改动落 privileged/net (state.rs / dns.rs / dhcpv6.rs), 须守 I1-I6 不变式与 F1-F9.
  `Dhcpv6PollCtx::apply_dns6` 不泄漏 smoltcp 类型 (F2); 新增 unsafe 块配 SAFETY (F4); 中文注释/文档 (F7/F8).
- **vendored 零修改**: 核心阶段不改 smoltcp src/ (purity 门禁); E4 抬容量涉 build.rs 配置, 单独评估.
- **破坏性返回面**: D1 改 `dns_resolve` 签名, 调用方编译期暴露, 一次性迁移.
- **容量约束**: `DNS_MAX_SERVER_COUNT=1` / `DNS_MAX_RESULT_COUNT=1` 为 vendored 默认, 核心阶段沿用.
- **锁域**: `dns_query_net` 仅进程上下文 (poll_network + 自旋), IPv6 喂入在 NET_STATE 锁内 (同 DHCPv4 契约).

---

## 关联文档

- [docs/plan/ipv6-dual-stack.md](./ipv6-dual-stack.md) — DECISION-032 双栈地基 (`IpAddr` 来源, 不含 DNS).
- [docs/plan/dhcpv6-client.md](./dhcpv6-client.md) — DHCPv6 客户端 (`apply_options.dns_servers` 来源).
- [docs/plan/archive/net-feature-alignment.md](./archive/net-feature-alignment.md) — S2 DNS 接线历史 (smoltcp dns::Socket).
- [src/kernel/privileged/net/init/dns.rs](../../src/kernel/privileged/net/init/dns.rs) — 现有 DNS 解析主体.

---

## 状态记录

- 创建文档. 源码调研完成: 纠正「自研解析器」误判 (实为 smoltcp dns::Socket); 确认 `IpAddr` /
  `DnsQueryType::Aaaa` / `register_query_waker` 已就绪; 发现 `DNS_MAX_SERVER_COUNT=1` 截断约束.
  实现路径裁定: 相对完整 (B) + 双栈 `IpAddr` 枚举. 核心 Phase 1-3 与可分离增强 E1-E4 分列.
- 用户裁定施工范围: **核心 Phase 1-3 先行 + E1 (getaddrinfo) + E2 (异步)**; E3 (缓存/search domain) /
  E4 (多服务器) 延后独立立项. 双栈优先策略: **IPv6 优先** (D2 AAAA 成功返 V6 否则 A; D3 服务器
  有 IPv6 优先用之否则 IPv4).
- Phase 1 完成 (D3): `NetState.dns6_servers` (裸 `[u8;16]` 三槽) + `G_DNS6_PRESENT` lock-free 观测位 +
  raw accessor + `Dhcpv6PollCtx::apply_dns6` + driver 接线 (SIMPLIFIED 收窄到 refresh_s) +
  `apply_dns_servers` IPv6 优先合并喂入. x86_64 release check 干净.
- Phase 2 完成 (D1+D2+D4): `dns_resolve -> Option<IpAddr>` 双栈返回面; boot 探针 + 5 个单测迁移;
  `get_dns6_servers` 观测面. 三成员 host 单测全绿 (12+796+522). 两处施工偏差已记入对应决策详情:
  **D2 顺序双查询** (非并发, SIMPLIFIED 标记, E2 异步化时改并发); **D4 跳过 `NetStatus.dns6`**
  (保 `capture()` 无锁契约, IPv6 观测由独立取锁的 `get_dns6_servers` 承担).
- Phase 3 完成: 抽 `merge_dns_servers` 纯函数 + 5 内联单测 (privileged 522→527); 新增
  `dns_dual_stack_test.rs` 5 契约锁 (跨 crate 公共面 + 接线). §2.3 七门槛全绿: build.sh all 6/6 /
  audit full (clippy 双架构 + fmt + perf + doc paths + Lockbud + QEMU 2/2 + feature 4/4) / test-host 0 failed /
  test-kernel-host 12+796+527 / ci-rustdoc 双架构 exit 0. clippy 红归因: 16 处 doc_markdown / 按值传参 /
  冗余闭包 / find_map 均为本批新代码, 已修; `lockdep.rs name_of` 死代码仅孤立 `-p kernel-privileged --release`
  跑出现 (门禁 `--workspace` feature 统一使 cfg 块编译进来, 不报), 属预存非本批, 未动 (§11.2).
- 用户裁定: 真实 AAAA e2e **并入 E1** (boot 探针早于 DHCPv6 绑定 + 用户态 DNS syscall 未实装 + poll 路径持锁
  不能就地加阻塞探针, 现有基建缺可观测面). Phase 3 以单测 + 契约 + 七门槛绿收口, e2e 待 E1 用户态探针.
- E1 完成 (getaddrinfo syscall + 真实 AAAA e2e), 分五批落地:
  - **E1-a** ABI 单源 `edgine-abi/src/net.rs` (`EG_GETADDRINFO=812` / `EG_AF_INET=2` / `EG_AF_INET6=10` /
    `EgAddrInfo` repr(C) 20 字节 family@0 addr@4); `privileged/syscall/types.rs` 镜像编号 + 唯一性对账块.
  - **E1-b** 后端 `privileged/net/syscall.rs::getaddrinfo_syscall` (raw_copy_in → UTF-8 校验 → dns_resolve →
    ip_to_addrinfo → write_struct_to_user; None→ENOENT / 非 UTF-8→EINVAL / 坏缓冲→EFAULT) + functions 0-unsafe
    代理 + `dispatch_net_dns` 接线; 全链 `#[cfg(not(feature = "kernel_test"))]` (镜像既有 net syscall 门控,
    已登记 `audit_feature_semantics` P-49 白名单 2 键: net/syscall.rs×1 + dispatch.rs×2).
  - **E1-c** userlib `sys.rs::getaddrinfo` 经 sys3 + init `dns_getaddrinfo_probe` (x86_64, DHCPv6 绑定后
    120×50ms 有界重试, 命中打印 `[dns6] AAAA ok: test6.edgine -> fd00::53`).
  - **E1-d** `dns_getaddrinfo_test.rs` 4 host 契约锁 (ABI 单源 / repr(C) 布局 / dispatch 不回落 ENOSYS+ENOENT /
    userlib 接线) + 后端 `ip_to_addrinfo` 2 内联单测 (V4 零扩展 / V6 原样), privileged 527→529.
  - **E1-e** `qemu_slaac_test.sh` dhcpv6 模式 dnsmasq 去 port=0 开 DNS + `address=/test6.edgine/fd00::53` +
    `dhcp-option=option6:dns-server,[fd00::1]` (选项23) + `[dns6] AAAA ok` 断言 (slaac 模式 port=0 保留).
  - **§2.3 七门槛全绿**: build.sh all 6/6 / audit full (19/19 + 10/10 腿, QEMU 2/2, feature 4/4, Lockbud 4/4
    零发现, perf 24 项在预算, doc-paths 17 份无漂移) / test-host 1545 passed 0 failed / test-kernel-host
    12+796+529 / ci-rustdoc 双架构 exit 0. 真实 AAAA e2e sudo 腿 (`MODE=dhcpv6 scripts/qemu_slaac_test.sh`) 待用户跑.
  - **host 测试覆盖局限 (预存, §11.5 待裁)**: 非 UTF-8→EINVAL / 坏缓冲→EFAULT 两分支需真实用户内存搬运,
    `copy_from_user` 的 SMAP 护栏 `smap_is_enabled` 读 CR4 (ring-0 特权指令), host x86_64 (ring-3) 必 SIGSEGV,
    无法 host 腿覆盖; 映射逻辑由后端源码契约 + e2e 真机腿保证, 未改 privileged 强测绿 (§11.2).