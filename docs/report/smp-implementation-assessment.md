# SMP 现状评估：唯一核心方案、N=1 退化与双架构差异

**核心结论**：当前 SMP 是**唯一核心方案**——单核不是第二套实现，而是同一实现上约 10 处运行期闸门造成的 N=1 退化，`feature = "smp"` 只影响 caps 上报位。2 核有长期 QEMU 门禁，3/4 核有历史实测（含 `-smp 4` 的千次启动），5 核及以上没有任何实测记录。最先暴露的三处是：x86 AP 部分失败时逻辑索引与登记索引分叉、外设中断全部固定 BSP、AP 串行上线预算与单核路径零门禁覆盖。

## 一、分级结论

| 核数 | 运行证据 | 判断 | 主要限制 |
|---|---|---|---|
| 1 核 | 无门禁腿（仅临时脚本 `scripts/.tmp_slaac_smp1.sh` 用过 `-smp 1`） | 路径存在但**零门禁覆盖** | 各单核旁路分支从未在 CI 执行 |
| 2 核 | `scripts/qemu_boot_test.sh` 全部 QEMU 腿固定 `-smp 2`（[qemu_boot_test.sh](../../scripts/qemu_boot_test.sh#L235-L235)、L327、L476、L569、L776） | **已验证**，长期门禁 | 只证明 2 核形态 |
| 3/4 核 | `make test-smp-multicore` 循环 2/3/4（[Makefile](../../Makefile#L781-L792)）；`docs/plan/archive/tlb-shootdown-epoch.md` 记录 `targets` 恒等核数；`docs/report/p29-scheduler-ex-smp-livelock-route-c.md` 记录 `-smp 4` 千次启动抽样 | **历史实测通过**，未入门禁 | 一次性记录，无持续回归 |
| 5–64 核 | 全仓无 `-smp 8/16/64` 记录 | **未验证** | 结构上无 `MAX_CPUS` 级阻塞，风险见 §1.4–§1.9 |

结构上限：`MAX_CPUS = 1024`（[capacity.rs](../../src/kernel/privileged/config/capacity.rs#L20-L20)），所有 per-CPU 定长数组统一取该常量；但 xAPIC 的定向 IPI 与 LAPIC ID 只有 8 位（[apic.rs](../../src/kernel/privileged/arch/x86_64/apic.rs#L140-L145)、L183），实际硬上限是 256 核。64 核仍在两者之内。

## 二、当前不足

**2.1 检测能力：默认构建没有运行时锁检测**

- `lockdep::acquire()` 的每个调用点都带 `#[cfg(any(debug_assertions, feature = "lockdep"))]`（[spinlock.rs](../../src/kernel/privileged/sync/spinlock.rs#L96-L104)、[rwlock.rs](../../src/kernel/privileged/sync/rwlock.rs#L242)、[mutex.rs](../../src/kernel/privileged/sync/mutex.rs#L248)），而 `default = []`、`lockdep` 需显式开启（[Cargo.toml](../../src/kernel/Cargo.toml#L56-L78)）。release 默认构建因此**零运行时锁检测**：AB-BA、IRQ 取睡眠锁、递归、释放未持有四类违规都不会被发现。该事实在 [multithreading-project.md](../plan/multithreading-project.md#L236-L241) 的 A-1 条目中有完整裁定与判据（`strings` 计数法），并明说"文档需明确 release 无运行时锁检测"。
- 静态面同样为空：`audit_deadlock_matrix.py` 自认 AB-BA 环检测"需先建立锁顺序声明机制"（[audit_deadlock_matrix.py](../../scripts/audit_deadlock_matrix.py#L12-L14)），只覆盖中断上下文非法锁、非中断安全锁、sleep 锁原子上下文三类；叠加 A-1 后 release 上 AB-BA **无任何自动检测**（A-3，[multithreading-project.md](../plan/multithreading-project.md#L264-L265)）。
- 数据竞争与内存序没有自动证明手段（A-4，[multithreading-project.md](../plan/multithreading-project.md#L268-L276)）；PI Mutex 的退出强制释放时点仍在"僵尸被收割"而非线程 `exit`，`try_lock` 失败路径也会留下等待者条目（A-7，[multithreading-project.md](../plan/multithreading-project.md#L282-L285)）。当前 `PiMutex` 无生产使用者，故 A-7 风险未落地；一旦 K-06/K-07 启用 PI，这条边界会变成实际的自旋挂死来源。
- A-2 的"实现未接线"半边已收口：`lockdep::acquire` 现覆盖 `SpinLock` / `IrqSpinLock` / `RwLock` / `Mutex` 共 8 个调用点（[spinlock.rs](../../src/kernel/privileged/sync/spinlock.rs#L99)、L121、L211、[rwlock.rs](../../src/kernel/privileged/sync/rwlock.rs#L242)、L269、L302、L329、[mutex.rs](../../src/kernel/privileged/sync/mutex.rs#L248)）。但 A-1 的"release 为零"仍然成立。

**2.2 规模与正确性**

- **x86 AP 部分失败会导致逻辑索引与登记索引分叉**（代码可推得，非推测）。BSP 的 `cpu_index` 是**启动尝试序号**，每个 AP 无论成败都自增（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L148-L166)）；而 `register_cpu` 用独立计数器 `CPU_COUNT` 稠密分配（[smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L101-L112)）。若中间某个 AP 因 GDT/栈分配失败或握手超时而未登记，后续 AP 的 `gs:[64]` 缓存索引、`init_per_cpu_sched(cpu_index)`、`CPU_TLB_GEN[cpu_index]` 都停留在"尝试序号"，而 `CPU_HW_IDS` / `CPU_ONLINE` 只登记到"成功序号"。后果是三重的：`tlb_gen_min_online()` 只扫 `0..get_cpu_count()`，漏掉错位槽 ⇒ 最小代可能被旧值钉住、延迟释放不回收；`get_hw_id()` 对未登记槽返回 `0xFFFF` ⇒ 定向 resched IPI 静默丢弃；`per_cpu()` 对空槽回退 BSP 调度器 ⇒ 两个核共用一份运行队列。核数越多，"某个 AP 起不来"的概率越高，这条缺陷的触发面越大。aarch64 已经用权威计数器取索引（[smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L145-L148)），x86 未对齐。
- **AP 串行上线预算随核数线性增长**。每 AP 最坏等待为 ready 1000×100µs + done 500×100µs（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L24-L25)、L267、L282），加上 INIT 10ms + 两次 SIPI 约 0.4ms，**单 AP 最坏约 160ms**。按最坏路径估算：16 核（15 个 AP）约 2.4s，64 核（63 个 AP）约 10.1s。而 `make test-smp` 的整机超时是 `timeout 60`（[Makefile](../../Makefile#L732-L733)），64 核只剩约 6 倍余量，且这还没算用户态探针的耗时。该估算假设每个 AP 都走满超时；正常路径 ready 通常毫秒级返回。4 核千次抽样记录的唯一次整机冻结是 boot284，属调度死锁（已由 P-30 闭合），不是启动超时（见「一、分级结论」）。
- **上限校验在启动路径上宣告、但时机空转**。`validate_cpu_config()` 确实被接入启动自检（经 `ConfigValidateHook`：[config/mod.rs](../../src/kernel/privileged/config/mod.rs#L248-L256)），但其调用点是 `config::init()`（[lib.rs](../../src/kernel/lib.rs#L465-L465)），远早于 `smp::init()`（[lib.rs](../../src/kernel/lib.rs#L598-L598)）与 AP 上线（[lib.rs](../../src/kernel/lib.rs#L790-L790)），读数恒为 1；同时 `register_cpu` 自身就有 `count >= MAX_CPUS` 的容量拒绝（[smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L103-L106)）。因此这条校验既拦不住、也不需要拦——真正的容量约束在 `register_cpu` 的槽位判断里。

**2.3 中断与 IO：多核下最确定的负载不均**

- **外设中断全部固定 BSP**。net INTx 接线用**调用者本核**的 LAPIC ID（[irq.rs](../../src/kernel/privileged/net/init/irq.rs#L131-L132)），而 net 初始化在 BSP 上执行，故所有 INTx 目的核都是 BSP；MSI-X 同样用调用者本核 ID（[msi.rs](../../src/kernel/privileged/pci/msi.rs#L375-L376)）。没有轮询分发、没有亲和分配。4 核以上时网络/存储中断 100% 压在 BSP，是核数增加后最先出现的瓶颈。
- **`route_irq_to_cpu_gsi()` 的目的段只写 4 bit**：`(u32::from(apic_id) & 0x0F) << 24`（[ioapic.rs](../../src/kernel/privileged/arch/x86_64/ioapic.rs#L284-L290)）。该函数当前**无调用点**（全仓 grep 仅定义处），所以不是现网缺陷；但一旦被接成"中断亲和分配"，APIC ID ≥ 16 的核会收到错误目的的 RTE。同文件 L473 的 4 bit 掩码是 IOAPIC 自身 ID 寄存器的硬件规范，不属此列。
- **IPI 类型只有两个编号**：`IPI_STUBS` 长度 2（[idt/mod.rs](../../src/kernel/privileged/idt/mod.rs#L356-L356)），对应 0xFD TLB / 0xFE resched。多核下不会编号冲突，但新增第三类 IPI 必须同时扩 `IPI_STUBS` 与 `ipi_table`（[idt/mod.rs](../../src/kernel/privileged/idt/mod.rs#L398-L398)）。
- **TLB shootdown 的串口日志随事件线性放大**。单次 shootdown 打印**一行**（`targets=N`，不是每核一行），代码已以 `SIMPLIFIED` 标注该欠账（[smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L237-L258)）。放大机制是"N 个核各自触发 unmap ⇒ N 倍事件 × 每行常量长度"，核数越高串口总量越大，而这条路径还在持锁区间外、无采样无阈值。

**2.4 验证覆盖**

- **单核路径零门禁覆盖**。`scripts/qemu_boot_test.sh` 的 7 处 `-smp 2` 写死（L235 / L327 / L476 / L569 / L776 等），没有任何 `-smp 1` 腿；全仓只有临时脚本 `scripts/.tmp_slaac_smp1.sh:149` 用过 `-smp 1`，且不入门禁。因此 `ap_count <= 1` 早退（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L123-L136)）、`single_core_fallback()`（[aarch64/smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L116-L120)）以及 VMM 侧所有单核分支都只在"从不被门禁执行"的路径上。
- **`test-smp` 不在任何 CI 门禁**。`ci/audit.sh` 的 QEMU 腿只调 `qemu_boot_test.sh all`；`make test-smp` / `test-smp-multicore` 只能手工触发。核数虽可覆盖（`SMP_CORES ?= 2`，[Makefile](../../Makefile#L722-L723)），但没有持续回归。
- **5 核以上无记录**。3/4 核证据只存在于归档计划与结果报告中（[tlb-shootdown-epoch.md](../plan/archive/tlb-shootdown-epoch.md#L170-L172)、[p29-scheduler-ex-smp-livelock-route-c.md](p29-scheduler-ex-smp-livelock-route-c.md#L43-L56)），没有面向 8/16/64 核的任何一次启动记录。
- **aarch64 的核数脚本是可调的**：`GIC_STRESS_SMP` 默认 2，`-smp "$SMP"` 与 `online CPUs: ${SMP}` 断言成对（[gic_stress_test.sh](../../scripts/gic_stress_test.sh#L34-L38)、L67），比 x86 的硬编码 2 更可扩；但默认仍只跑 2 核。

## 三、"SMP 是唯一核心方案"的含义

- **结构上是单一实现 + 运行期旁路**。全仓没有第二套调度器、第二套 TLB 状态或第二套 AP 状态机：`PER_CPU_SCHED` 是唯一 per-CPU 槽表（[scheduler.rs](../../src/kernel/privileged/proc/scheduler.rs#L252-L252)），`CPU_TLB_GEN` / `CPU_ONLINE` 各一份（[smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L23-L30)），TLB 失效只有一条 `tlb_gen_publish_and_shoot()`（[smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L237-L258)）。
- **编译期没有任何 SMP 分支**。`feature = "smp"` 在全仓只有一处读取，且只写进 caps 上报结构（[caps.rs](../../src/kernel/privileged/config/caps.rs#L79-L79)），不门控任何代码。AP 上线在默认构建里无条件执行——默认构建 + `-smp 2` 已经是双核形态，只是 caps 行报 `smp=off`。
- **单核的身份由 `is_enabled()` 承载，而不是核数**。`SMP_ENABLED` 的语义是"至少一个 AP 已登记上线"（[smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L9-L9)、L52-L53），只有 `register_cpu` 会置位（[smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L111-L111)）；BSP 不走登记，故单核时它恒为 false。所有影响行为的闸门都写成 `is_enabled() && get_cpu_count() > 1` 的**合取**，共 7 处：x86 VMM 3 处（[vmm_x86_64.rs](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1137-L1137)、L2237、L2358）、arm VMM 3 处（[vmm_aarch64.rs](../../src/kernel/privileged/mm/vmm_aarch64.rs#L750-L751)、L892、L1400）、延迟释放 1 处（[deferred_free.rs](../../src/kernel/privileged/mm/deferred_free.rs#L331-L331)）。另有 3 处纯核数闸门：负载均衡早退（[scheduler.rs](../../src/kernel/privileged/proc/scheduler.rs#L2735-L2735)）与两个 tick/调度入口（[scheduler.rs](../../src/kernel/privileged/proc/scheduler.rs#L2646-L2646)、L2666、L2702）。合计约 10 处影响行为的运行期闸门，外加 1 处仅上报（caps）。
- **语义矛盾是可读性问题，不是运行缺陷**。默认构建 `-smp 2` 下 `caps.smp == false` 而 `is_enabled() == true`：caps 描述的是"构建特性开关"，`is_enabled()` 描述的是"是否有 AP 上线"。两者不同源但各自正确，代价是任何读者把 caps 行当作核数判据都会误判。
- **覆盖率不对称由此产生**：单核旁路（早退、`single_core_fallback`、各 `else` 侧）只被单核启动执行，而门禁不跑单核；多核主干（AP 握手、索引分配、shootdown）只在 ≥2 核执行，单核启动发现不了它的缺陷。上一节的"索引分叉"正是主干缺陷：它不会在单核暴露，也不会被现有 2 核门禁稳定暴露（需要"某个 AP 失败"这一事件）。

## 四、x86_64 与 aarch64 的差异

| 维度 | x86_64 | aarch64 | 定性 |
|---|---|---|---|
| AP 上线机制 | INIT×2 + SIPI×2，trampoline 单份复制到 `0x8000`，逐 AP 串行重写 `ApStartupInfo`（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L178-L189)、L264-L290） | `PSCI_CPU_ON = 0xC400_0003`，按 DT 拓扑逐 `mpidr` 调用（[psci.rs](../../src/kernel/privileged/arch/aarch64/psci.rs#L18-L18)；[smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L137-L163)） | **设计必然**：硬件启动协议不同 |
| 逻辑索引分配 | BSP 用"启动尝试序号"，失败会与登记计数器分叉（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L148-L166)） | 每 AP 启动前取 `get_cpu_count()` 作逻辑索引，与 `register_cpu` 同源（[smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L145-L148)） | **不一致债**：x86 应改成与 arm 同源 |
| 单核回退 | `ap_count <= 1` 早退 + 一行日志（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L123-L136)） | `single_core_fallback()`（[smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L116-L120)） | **设计必然**：入口来源不同（MADT vs DT） |
| 完成判据 | 循环结束后无条件置 `SMP_FULLY_INITIALIZED = true`，即便有 AP 失败（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L166-L169)） | `online == expected` 才置；有 AP 未上线则保持 false（[smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L151-L163)） | **不一致债**：x86 的"部分失败也算完成"与 arm 的 fail-closed 不同语义 |
| 中断控制器 | LAPIC + IOAPIC INTx；MSI-X 目的核写调用者 LAPIC ID（[ioapic.rs](../../src/kernel/privileged/arch/x86_64/ioapic.rs#L284-L296)、[msi.rs](../../src/kernel/privileged/pci/msi.rs#L375-L376)） | GICv3，per-CPU 初始化按**硬件亲和值**定位 Redistributor 帧，明确禁止按逻辑索引线性推算（[gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L158-L162)、L466） | **设计必然**（中断控制器不同）；MSI-X 在 arm 写 0 地址（[msi.rs](../../src/kernel/privileged/pci/msi.rs#L379-L383)）属**能力缺口**（GIC ITS 未接） |
| 磁盘根挂载 | `boot_rootfs_and_probes()` 的 EGFS 挂载整块带 `target_arch = "x86_64"` 门控（[lib.rs](../../src/kernel/lib.rs#L936-L945)） | 无对应分支 | **能力缺口**；"arm 无块设备驱动接线"为推测，未逐驱动核查 |
| 验证脚本核数 | QEMU 腿硬编码 `-smp 2`（7 处） | `GIC_STRESS_SMP` 可调，断言与 `-smp` 同源（[gic_stress_test.sh](../../scripts/gic_stress_test.sh#L34-L38)） | **不一致债**：x86 侧应变量化 |

两架构共享的部分：per-CPU 定长数组统一按 `MAX_CPUS`（[capacity.rs](../../src/kernel/privileged/config/capacity.rs#L20-L20) 与 [smp/mod.rs](../../src/kernel/privileged/smp/mod.rs#L20-L30)）；AP 自装配本核时钟（[smp_init.rs](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L411-L411) 的 `arm_local()`）；TLB 代机制与 shootdown 语义一致。

## 五、维护与增强路线

以下全部为**建议**，不是已批准计划；量级为估算，不代表承诺。

**5.1 缺陷修复（不修则核数增大后风险放大）**

1. **对齐 x86 索引分配**：`start_ap` 的 `cpu_index` 改为在发起前取 `get_cpu_count()`（与 arm 同源），或让 `start_ap` 返回实际登记索引并回写 `ApStartupInfo`；失败时不得推进后续 AP 的编号。配套一条 host 侧负控（注入"第 k 个 AP 失败"，断言后续槽位仍被 `tlb_gen_min_online` 统计）。
2. **收敛 7 处合取门**为单一谓词（如 `smp_remote_active()`），消除 x86/arm/延迟释放三处手写重复；否则 TLB 语义改动要同步 7 个点。
3. **给 `tlb_gen_publish_and_shoot()` 加内部 guard**：`get_cpu_count() <= 1` 时只发布代并本地失效，不发 IPI、不打印。当前单核安全完全依赖调用点的合取门，多一个调用点就多一处自伤（单核自己给自己发 IPI、`targets=1`）。
4. **修正 `route_irq_to_cpu_gsi` 目的段**：`& 0x0F` → `& 0xFF`，并在接线时补一条 APIC ID ≥ 16 的 host 契约测试。
5. **明确 release 锁检测策略**（A-1）：要么在文档与 caps 中显式声明"release 无运行时锁检测"，要么评估低开销采样检测进默认构建；A-3/A-4 属工具链能力，短期无低成本解。

**5.2 能力增强**

1. **中断多核分发**：IOAPIC RTE 目的核与 MSI-X 目的核从"调用者本核"改为可配置（轮询或按队列亲和），这是解除 BSP 单核瓶颈的唯一正解。
2. **核数门禁**：把 `qemu_boot_test.sh` 的 7 处 `-smp 2` 改为 `SMP_CORES`（默认 2，保持现有行为）；新增 `-smp 1` 腿覆盖单核旁路；把 `make test-smp-multicore`（2/3/4）列入 CI；再增 8 核腿，16/64 核按需手动长跑。
3. **shootdown 日志采样**：按"首次 + 每 N 次"打印，避免核数放大后串口成为热路径瓶颈。
4. **延迟释放读数**：现有 `targets=N` 与 `pending=false` 已能判绿，可再加"每核代追平时间"读数，用于 8 核以上的隐藏饥饿诊断。

**5.3 建议顺序与量级**

| 顺序 | 项目 | 类型 | 量级（估算） |
|---|---|---|---|
| 1 | x86 索引对齐 + 失败负控 | 缺陷修复 | ~40–80 行 + 1 个 host 测试 |
| 2 | 合取门收敛为 `smp_remote_active()` | 缺陷修复（结构） | ~30–60 行，7 处替换 |
| 3 | `tlb_gen_publish_and_shoot` 内部 guard | 缺陷修复 | ~5–10 行 + 1 个结构锁 |
| 4 | `route_irq_to_cpu_gsi` 目的段 8 bit | 缺陷修复（潜在） | 1 行 + 契约测试 |
| 5 | `SMP_CORES` 变量化 + `-smp 1` 腿 | 验证增强 | ~30–50 行脚本 |
| 6 | `test-smp` 入门禁（2/3/4 核） | 验证增强 | ~20–40 行 CI |
| 7 | 8 核腿 / 16-64 核长跑 | 验证增强 | ~20–40 行脚本 + 超时调参 |
| 8 | 中断亲和分配（IOAPIC + MSI-X） | 能力增强 | ~150–300 行，需真机/多队列验证 |
| 9 | release 锁检测通路（A-1） | 能力增强 | 需先做开销评估，量级未定 |

前 4 项属"修一处可独立验证"的缺陷修复，建议先做；第 5–7 项是把已有能力纳入持续回归；第 8 项才涉及调度/中断语义扩展，应在核数门禁建立后做。

## 六、未验证与推测清单

- **未验证（事实缺口）**：5 核及以上没有任何启动记录；单核路径从未在 CI QEMU 腿执行；`route_irq_to_cpu_gsi` 全仓无调用点，其目的段缺陷未被任何运行路径触发。
- **未验证（静态推演）**：x86 索引分叉的三重后果（TLB 代饥饿、IPI 丢弃、per-CPU 回退 BSP）由代码推导得出，尚未做"注入某个 AP 失败"的运行时复现。
- **估算（非实测）**：16 核约 2.4s、64 核约 10.1s 的启动上界，按"每 AP 最坏 160ms"逐 AP 串行累加；正常路径远低于此，4 核千次抽样没有启动超时记录。
- **推测**：aarch64 未接磁盘根挂载的原因（"无块设备驱动接线"）未逐驱动核查，仅确认代码侧有 `target_arch = "x86_64"` 门控；`caps.smp` 与 `is_enabled()` 的消费方只观察到上报点，未穷举全部读者。
- **推测**：中断全压 BSP 对吞吐的实际影响未做基准测试，仅在结构上确定为单核热点。
- **未验证**：`gic_stress_test.sh` 在 4 核以上的 GIC 压力表现无记录；aarch64 `DONE_TIMEOUT_LOOPS = 100_000_000`（[smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L25-L25)）对应的实际超时时长未标定。
