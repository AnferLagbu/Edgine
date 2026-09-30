# mm/vmm 双架构质量评估报告

> 总体判断：工程纪律和质量在同类自研内核中属第一梯队；架构方向符合现代化设计（KPTI、机制/策略分离、跨架构统一协议），但性能敏感特性存在"常量已定义、优化未接线"的半成品点，且个别模块文档与实现脱节。正确性/安全性维度现代化达标，性能维度未达现代生产基线。

本报告是双架构内存管理子系统质量评估的一次性快照，评估范围约 18.7K 行 mm 子系统源码 + 逐文件核查双架构 vmm/kpti，覆盖架构对称性、安全隔离、TLB 一致性、框内核分层纪律、测试矩阵与现代化缺口。作为后续制定 plan 与修复工程的输入依据。

## 一、做得好的（现代化证据）

**1. 双架构对称、边界清晰** —— `framework/mm/mod.rs` 用 `#[path]` 按 cfg 分发 `vmm_x86_64.rs`（2462 行，4 级页表）与 `vmm_aarch64.rs`（1683 行，L0→L3 + Block 描述符 2MB/1GB 巨页）。高半区内核映射双架构统一（aarch64 TTBR0/TTBR1 拆分，x86 PML4[256..511]），内核经高半区直射区在切 per-process 页表后仍可达——这是现代内核的标准做法。

**2. KPTI 两架构都是真隔离，不是摆设**

- **x86**：USER_PML4 逐页装配"入口依赖面"（KPTI-08 已移除高半区整段复制，等价 Linux KAISER 的精进形态），`invpcid` / `cr3_with_pcid` 原语齐备并有 CPUID 动态检测（`kpti.rs` L50 `PCID_KERNEL` / L71 `invpcid`）
- **aarch64**：实现得更彻底——"KPTI 方案 S3" 每进程配套 **EL1 视图**（TTBR0 用户半区 ∪ 内核恒等），入口汇编极薄（`ldr x4,[x2,#8]`），内核高半区经 TTBR1 恒可达

**3. 跨核 TLB 失效是全项目质量最高的路径之一** —— `framework/smp/mod.rs`：epoch/代协议，x86 `0xFD` IPI 与 aarch64 SGI13 **共用唯一接收实现**（消除了平行实现），"读代→flush→声明"三段次序由审计脚本 `audit_tlb_receive_order.py` 静态 fail-closed 强制，并有**运行期判别力探针**（不比对计数，要求远程核在接收路径内实测观测字节）——多数生产内核都没有这种自证机制。

**4. 框内核分层贯彻到位**

- 机制/策略 trait 注入：`alloc_trait` / `pmm_trait` / `slab_trait` / `swap_trait` + services 侧 `pmm_policy` / `slab_policy` / `swap_policy` / `memory_pressure`
- PMM buddy 用 `MetaStore` trait 抽象三类元数据载体（生产 `RawMetaStore` 裸指针 / host 测试 `VecMetaStore`），buddy 算法本体 safe Rust 单份代码无测试分叉
- `page_fault.rs` 路径完整：VMA 权限判定 / 栈扩展 / COW / 文件缺页 / **uffd**（696 行）；`copy_user.rs` 有异常表 fixup（`.exception_table` 段）
- unsafe 纪律：mm 层 453 处 unsafe / 444 条 SAFETY 注释，覆盖率≈100%；services/mm 0 unsafe

**5. 测试矩阵厚** —— kpti_x86_user_table / kpti_el0_fault_isolation / page_fault_vma_flags / mm_iomem_alias / pmm_buddy / demand_paging / copy_user_exception / munmap_pml4 等 host-tests 全覆盖。

## 二、质量问题与"现代化缺口"

| # | 问题 | 证据 | 定性 |
|---|---|---|---|
| 1 | **PCID 半接线**：`PCID_KERNEL/USER` 常量与 invpcid 原语齐全，但运行时切换仍是裸 `mov cr3`，上下文切换无 per-process PCID + NOFLUSH 位 → CR3 切换照常全刷 TLB | `mm/kpti.rs` L50/L71 常量与原语；`arch/x86_64/mod.rs` L429-430、L466 裸 `mov cr3` 切换，无 PCID 位编码 | 现代化欠账（性能） |
| 2 | **aarch64 无 ASID**：全 mm/arch 目录 grep 零命中，TTBR 切换即全 TLB 失效 | — | 现代化欠账（性能） |
| 3 | **Buddy 单全局自旋锁**：`PhysicalMemoryManager` 以 `AtomicBool` 自旋锁（`acquire_lock/release_lock`）串行化整棵 buddy，无 per-CPU pageset、无 zone/DMA 区分、无 min/low/high 水位线（碎片化只有策略 trait 接口） | `mm/pmm.rs` L629/L953-965 锁路径；L2066 单实例 `GLOBAL_PMM` | 简化设计（有意，但限制 SMP 扩展性） |
| 4 | **无 rmap 反向映射**：unmap 靠遍历该地址空间页表；跨进程共享页回收无精确性 | `cow.rs` 全局 BTreeMap（linux-compat-philosophy 明文的有意简化） | 立场内简化，代价是 O(N) |
| 5 | **MAX_USER_PAGE_TABLES=256 固定槽位** | `mm/vmm_x86_64.rs` L290 | 硬上限，fork 风暴下会耗尽 |
| 6 | **模块文档漂移**：`kpti.rs` 顶部"现状"仍写"汇编 trampoline 未完成、aarch64 双 TTBR 待实现"，但两者实际都已完成（arch/x86_64 L703 `mov cr3,rax` 入口已接、vmm_aarch64 全切换模型已实现） | 违反 AGENTS.md §9.2 文档同步 | 需订正，非代码缺陷 |
| 7 | shootdown 每次串口打一行日志（代码自标 SIMPLIFIED），高频 unmap 路径会被日志拖慢 | `smp/mod.rs` L184 | 已登记的简化项 |

## 三、结论与后续方向

- **正确性/安全性维度：现代化达标**——KPTI 真隔离双架构对称、TLB 代协议 + 静态审计 + 运行期探针、异常表 fixup、uffd、NUMA 感知 VMA 策略，这些已达到甚至超过多数教学型内核，形态上贴近 Asterinas/RedLeaf 一档。
- **性能维度：未达现代生产基线**——PCID/ASID 精细化切换、per-CPU 页缓存、rmap 三项是"接口/常量已备好、最后一公里未接"的状态；其中 **#1 PCID NOFLUSH 接线** 和 **#6 文档订正** 是当前性价比最高的两项改进，其余属于有意识的简化立场（linux-compat-philosophy 三层策略），不构成纪律违规。

后续据此制定 plan 时，建议以第二节的编号 # 为条目索引；#1/#2/#6 为优先候选。
