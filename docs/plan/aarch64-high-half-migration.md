# aarch64 高半区内核迁移（L1，独立工程）

> 来源：KPTI 完整化工程（[kpti-complete-project.md](./kpti-complete-project.md)）Phase 1 收口时擢升为独立工程。
> 决策依据：aarch64 内核当前驻留低半区恒等映射，迫使 KPTI 的 per-process EL1 视图以 DRAM 1 GiB 块把内核镜像/数据/栈纳入 `TTBR0` 视图；迁移到高半区后 EL1 视图回归"只承载用户页"的自然形态。

## 背景与动机

aarch64 侧 `KERNEL_BASE` 取 [mm/mod.rs:184](../../src/kernel/framework/mm/mod.rs#L184) 的 `0`，即内核**恒等映射**在低半区（镜像 @ PA `0x40080000` ⇒ VA `0x40080000`）。而 aarch64 用两条 TTBR 划分地址空间：

- `TTBR0_EL1` → 低半区 `0x0000_0000_0000_0000 - 0x0000_FFFF_FFFF_FFFF`
- `TTBR1_EL1` → 高半区 `0xFFFF_0000_0000_0000 - 0xFFFF_FFFF_FFFF_FFFF`

低半区 VA 由 `TTBR0` 承载。KPTI 全切换模型下 EL0 与 EL1 使用**不同的** `TTBR0`（EL0 视图 vs EL1 视图），因此内核代码/数据既然住在低半区，EL1 视图就必须把它一并纳入（[vmm_aarch64.rs](../../src/kernel/framework/mm/vmm_aarch64.rs) 的 `L1_el1[1]` = 内核 `L1_IDMAP[1]` DRAM 1 GiB 块）。

**针对 x86_64 的对照**：x86 内核住高半区（`KERNEL_BASE = 0xFFFF800000000000`），低半区只承载用户映射；切 CR3 时高半区由同一套内核页表条目覆盖，内核不需要出现在"用户 CR3 的低半区"。aarch64 的等价形态要求内核住 `TTBR1` 领地。

## 现状盘点

`KERNEL_BASE` 在 `src/` 下共 **154 处 / 36 文件**（`grep -c KERNEL_BASE src/` 实测）。引用面分布（Top）：

| 文件 | 处数 | 备注 |
|---|---|---|
| [proc/user_proc.rs](../../src/kernel/framework/proc/user_proc.rs) | 24 | 用户态装载 / 栈地址换算 |
| [mm/vmm_x86_64.rs](../../src/kernel/framework/mm/vmm_x86_64.rs) | 21 | x86_64 专属，aarch64 迁移不需改动 |
| [mm/pmm.rs](../../src/kernel/framework/mm/pmm.rs) | 16 | `phys_to_virt` 族 |
| [proc/process.rs](../../src/kernel/framework/proc/process.rs) | 8 | 内核栈地址 |
| [mm/kpti.rs](../../src/kernel/framework/mm/kpti.rs) | 6 | x86_64 专属 |
| [memory_allocator.rs](../../src/kernel/memory_allocator.rs) | 7 | aarch64 取 0，非 aarch64 取高半区 |
| 其余 30 文件 | ≤5 各 | — |

关键收敛点：
- [memory_allocator.rs:13](../../src/kernel/memory_allocator.rs#L13) `#[cfg(aarch64)] KERNEL_BASE = 0` —— 迁移的核心开关
- [mm/mod.rs:184](../../src/kernel/framework/mm/mod.rs#L184) `#[cfg(aarch64)] pub const KERNEL_BASE = 0` —— 与上者同源
- `phys_to_virt` 族（pmm.rs / kpti_aarch64.rs `HIGH_ALIAS_BASE`）—— 迁移后低半区恒等假设失效，需改为"物理地址 → 高半区别名"

> 说明：`vmm_x86_64.rs`、`mm/kpti.rs` 等 x86_64 专属文件的引用**不计入** aarch64 迁移改动面，实际待评估面向量小于 154。

## 任务分解

- **L1-01. 迁移面精确盘点**
  - 描述：把 154 处引用按"编译目标"筛分：aarch64 参与编译的实改动面 vs x86_64 专属（`#[cfg(target_arch = "x86_64")]` 或 x86 文件）虚改动面。
  - 方案：逐文件核对 cfg 门控，产出"实改动清单"（文件 + 行 + 语义分类：地址换算 / 链接脚本 / 常量定义 / 断言）。
  - 状态：[X]
  - 详情：实测 `KERNEL_BASE` 165 处、`KERNEL_TEXT_BASE` 16 处、`HIGH_ALIAS_BASE` 10 处。按编译目标筛分结果：
    - **常量定义（3 处，必改）**：[mm/mod.rs:187](../../src/kernel/framework/mm/mod.rs#L187) `KERNEL_BASE`、[mm/mod.rs:226](../../src/kernel/framework/mm/mod.rs#L226) `KERNEL_TEXT_BASE`、[memory_allocator.rs:13](../../src/kernel/memory_allocator.rs#L13) `KERNEL_BASE`（aarch64 分支）。
    - **结构面（必改）**：[link/aarch64.ld](../../src/kernel/framework/link/aarch64.ld) VMA/LMA 拆分；[boot/aarch64/start.S](../../src/kernel/framework/boot/aarch64/start.S)、[entry.rs](../../src/kernel/framework/boot/aarch64/entry.rs)、[arch/aarch64/mmu.rs](../../src/kernel/framework/arch/aarch64/mmu.rs) 启动跳板；[exception.rs:874-887](../../src/kernel/framework/arch/aarch64/exception.rs#L874-L887) VBAR 与 [:496](../../src/kernel/framework/arch/aarch64/exception.rs#L496) trampoline 的 `+HIGH_ALIAS_BASE`（迁移后符号已是高 VA，会双重叠加，须清理）。
    - **隐式恒等假设破裂（须逐点确认）**：[kpti_aarch64.rs:129-136](../../src/kernel/framework/mm/kpti_aarch64.rs#L129-L136) `map_kernel_stack_top_page`（显式依赖 `VA==PA`）；[process.rs:66,98](../../src/kernel/framework/proc/process.rs#L66-L98) boot 栈 canary 的 `KERNEL_BASE + &stack_bottom`（取决于 boot 栈落低 VMA 还是高 VMA）。
    - **本盘点遗漏、由 L1-05 施工期暴露并闭合**：[lib.rs](../../src/kernel/lib.rs#L698) aarch64 专属 `heap_start = boot_info.kernel_end + 0x200000`（直取物理地址）。迁移前 `KERNEL_BASE = 0` 故与 x86_64 分支同值而未显形，L1-02 改基址后成为残项 ⇒ kmalloc 堆滞留低半区恒等区。闭合方式与实证见 L1-05 详情。
    - **已自动适配（无需改）**：[idt/mod.rs:342-354](../../src/kernel/framework/idt/mod.rs#L342-L354) 的 `+KERNEL_BASE` 已被 `#[cfg(target_arch = "x86_64")]` 门控，aarch64 走 `lo`；[context.rs:239](../../src/kernel/framework/arch/aarch64/context.rs#L239) 已写「`bit63==0` 才加别名」条件式。
    - **真实物理→虚拟换算（改动后自然成立，逻辑不动）**：`mm/pmm.rs`(16) / `slab.rs` / `cow.rs` / `swap.rs` / `pcache.rs` / `frame.rs` / `iobuf.rs` / `dma/*` / `driver/virtio/queue.rs` / `driver/net/e1000.rs` / `lib.rs` / `page_table.rs` / `proc/user_proc.rs` / `arch/shadow_stack.rs` / `vmm_aarch64.rs`。
    - **x86_64 专属（虚改动面，不触碰）**：`vmm_x86_64.rs`(21) / `kpti.rs`(6) / `arch/x86_64/gdt.rs` / `arch/x86_64/mod.rs` / `boot/isr.asm` / `idt/idt.rs` / `cpu/mod.rs:776`（已确认在 x86 SYSCALL 分支内）。
    - **测试面（随新 `KERNEL_BASE` 复核）**：`tests/test_mm.rs` / `tests/net.rs` / `tests/idt.rs` / `tests/mod.rs` / `mm/frame.rs` 断言。

- **L1-02. 内核 VA 基址决策**
  - 描述：确定 aarch64 高半区基址（候选：`0xFFFF_0000_4000_0000`，即现有 DRAM 别名的自然延伸；或独立于别名的 `0xFFFF_8000_0000_0000`，与 x86 `KERNEL_BASE` 对齐）。二者取舍影响 EL1 视图、`phys_to_virt` 族与链接脚本。
  - 方案：**决策灰色地带**（§9.1）——须由用户裁定，不在本工程自行选择。列候选 + 各自对 EL1 视图/别名体系的影响后再施工。
  - 状态：[X]
  - 详情：**裁定为「A 数值 + 合一布局」** —— `KERNEL_BASE = 0xFFFF_0000_0000_0000`（= 现有 `HIGH_ALIAS_BASE` = TTBR1 窗口基址），镜像落点 = `KERNEL_BASE + PA 0x4008_0000 = 0xFFFF_0000_4008_0000`，`KERNEL_TEXT_BASE` 同步改为 `0xFFFF_0000_4008_0000`。
    - **数值血统**：该值同时等于 Linux arm64 `PAGE_OFFSET = -(1<<48)`、FreeBSD arm64 `VM_MIN_KERNEL_ADDRESS`/`KERNBASE`、T1SZ=16 的 TTBR1 窗口基址、Edgine 现有 `HIGH_ALIAS_BASE`（四重血统）。
    - **角色定位**：FreeBSD 将 `KERNBASE`（镜像落点）与 `DMAP_MIN_ADDRESS`（直射区 `0xFFFF_A000_0000_0000`）分离；Linux arm64 将 `PAGE_OFFSET`（直射起点）与 `KIMAGE_VADDR`（镜像 `0xFFFF_8800_0000_0000`）分离。本方案**合一**——`KERNEL_BASE` 既作直射偏移（`phys_to_virt = phys + KERNEL_BASE`）又作镜像落点基准，省去两家都有的分离区。合一可行前提：Edgine 当前无 KASLR、无模块区、无 vmemmap 需求，无需为镜像预留独立高 VA 区段。
    - **布局形态裁定**：**一次性迁移**（照搬 FreeBSD `locore.S` / Linux `__primary_switch` 的「高半区链接 + 开机恒等跳板 + MMU 开启后一次性 `br` 切换」行业先例；无内核采用渐进双映射）。
    - **附带收敛**：迁移后 aarch64 的 `KERNEL_BASE == HIGH_ALIAS_BASE`（同值），MMIO 与 DRAM 访问同走高半区别名，与 Linux arm64 语义一致（`ioremap` 与 `PAGE_OFFSET` 同处高半区）。

- **L1-03. 链接脚本与启动路径迁移**
  - 描述：[link/aarch64.ld](../../src/kernel/framework/link/aarch64.ld) 当前按低半区地址布局（`. = 0x40080000` 级别）；`entry.rs`/bootloader 以恒等地址取指。迁移须改链接脚本 LMA/VMA 与早期 MMU 开启前的恒等窗口。
  - 方案：保留 boot 早期恒等映射作为跳板，MMU 开启后跳转高半区；`_vectors` 段的高别名/新基址映射同步调整。
  - 状态：[X]
  - 详情：**裁定为「镜像 x86_64 结构：asm 建引导表」**。核心结构：
    - **低 VMA 引导区**（VMA == LMA == PA，恒等可取指）：`.text.boot`（`start.S`）+ `.bootbss`（boot 栈、`_fdt_addr`、3 条目引导页表）。位于 PA `0x40080000` 起。
    - **高 VMA 内核区**：`.text` / `.vectors` / `.rodata` / `.data` / `.bss`，VMA = `KERNEL_BASE + LMA`（镜像落 `0xFFFF_0000_4008_0000` 起）。
    - **引导流程**：`start.S` 完成 EL3→EL2→EL1 后，用「运行时 delta」（`_start` 实际地址）做物理寻址设置 boot 栈、清 BSS；asm 就地构建 3 条目引导表（`L0[0]→L1`、`L1[0]→L2_DEVICE`(Device 0-1GB 2MB 块)、`L1[1]`=Normal 1GB 块）；`TTBR0=TTBR1=引导表物理地址`；开 MMU；`br` 高 VMA `entry`。
    - **职责边界**：引导表 = **临时表**（`.bootbss`，生命周期止于 `mmu::init()`），运行时内核根表 `L0_TABLE` 仍由 [mmu.rs](../../src/kernel/framework/arch/aarch64/mmu.rs) 单一实现构建（持久，`.bss` 高 VMA）。二者分工与 Linux `__create_page_tables`→`swapper_pg_dir` / FreeBSD `create_pagetables`→`pmap_bootstrap` 同构；asm 侧仅 3 条目、不含运行时页表逻辑，不构成并行实现。
    - **随之必改**：`mmu.rs` 内传入 TTBR 与写入表内条目的地址须改为 **PA**（`virt_to_phys`），因表迁至高 VMA `.bss` 后指针不再是 PA。
    - **随之必改（异常入口保留槽读取）**：`exception.rs` 的 `handle_el0_sync` / `handle_el0_irq` 在切内核表后读用户表**保留槽**（`ldr x4, [x2, #8]`，`x2` = 用户表 PA）。迁移后 `PA != VA` 且 `kernel_ttbr0` 不再提供低半区恒等，该读须改走**高半区别名**：`ldr x2, [x3, #24]` 后取 `x2 + KERNEL_BASE`（迁移后 `kernel_ttbr1` 覆盖高半区，别名可达）。L1-03b 已把该读取改为经 `[x3, #24]` 重载 `user_ttbr0`，仅差这一步别名化。
    - **选型理由**（长期最优）：① 双架构同构，维护单一心智模型；② 引导表/运行时表职责边界清晰；③ 规避 `#[link_section]` 标注 Rust 函数/静态量在 LLVM 下的脆弱性（本仓库零先例）；④ 引导表 3 条目极小且稳定，长期漂移风险低。
    - **施工落点（8 文件）**：[aarch64.ld](../../src/kernel/framework/link/aarch64.ld)（双区布局 + `stack_bottom = _kernel_base + _boot_stack_bottom`）、[start.S](../../src/kernel/framework/boot/aarch64/start.S)（引导表 + MAIR/TCR + `br` 高 VA）、[mmu.rs](../../src/kernel/framework/arch/aarch64/mmu.rs)（5 处表地址 PA 化）、[exception.rs](../../src/kernel/framework/arch/aarch64/exception.rs)（向量表/跳板符号不再叠加别名；入口保留槽读取走高半区别名）、[kpti_aarch64.rs](../../src/kernel/framework/mm/kpti_aarch64.rs)（3 处 PA 化）、[mm/mod.rs](../../src/kernel/framework/mm/mod.rs)（`KERNEL_BASE`/`KERNEL_TEXT_BASE` 取值）、[memory_allocator.rs](../../src/kernel/memory_allocator.rs)、[boot/mod.rs](../../src/kernel/framework/boot/mod.rs)（`kernel_end` 物理化）。
    - **`stack_bottom` 符号语义分叉**：内核区真高 VMA 后，`.bootbss` 的低半区符号距内核代码超出 `adrp` 的 ±4GB 可达范围（实测 `R_AARCH64_ADR_PREL_PG_HI21` 截断错误）；故改由链接脚本把 `stack_bottom` 直接定义为高别名，`process.rs` 侧 aarch64 分支不再加 `KERNEL_BASE`（x86_64 语义不变）。
    - **施工期缺陷修复（本轮 QEMU 暴露）**：引导表 `L1[1]` DRAM 1GB 块原写 `0x811`（缺 AF 位 bit10、误置 nG 位 bit11）⇒ MMU 开启后取指即 Access Flag fault（Prefetch Abort `FAR=0x4008018c`，`VBAR=0` 导致递归异常、串口零输出）。修正为 `0x411`，与 [mmu.rs](../../src/kernel/framework/arch/aarch64/mmu.rs#L118) 运行时表取值一致。
    - **运行期实证**（QEMU aarch64）：`Boot stack canary verified`（高别名 `stack_bottom` 校验通过）、`kernel_end=0x41E50000`（物理化换算正确）、`VFS ready`、`Entering EL0 (init pid=4)`、`[KPTI] EL0 kernel high-half access denied (pid=7)`；双架构 build / 双架构 clippy / `./ci/audit.sh quick` / `make test-host` / QEMU 双架构全通过。

- **L1-03b. KPTI 内核栈可达性改造（L1-03 施工期衍生项）**
  - 描述：迁移后内核栈变高 VA，`TTBR0` 不再能翻译它；而 EL0→EL1 入口在切表**之前**就要向内核栈压入 280 字节异常帧，出口 `el0_return` 也在切回 tramp 表**之后**从内核栈读帧。原 [map_kernel_stack_top_page](../../src/kernel/framework/mm/kpti_aarch64.rs#L125)（把栈顶页映射进**用户**页表，依赖 `VA==PA`）随之失效。
  - 方案：**裁定为方案 B（用户授权）** —— 入口/出口改为「先切 `TTBR1`，再压/读帧」：用 `KPTI_GLOBALS` 新增的暂存槽保住切换序列占用的 x3/x4，切 `TTBR1` 到完整内核表后栈页（高 VA）即可达；出口对称（切回 tramp 前先读回帧内活跃寄存器、暂存待恢复的 x3/x4，切表后取回）。**删除** `map_kernel_stack_top_page` 及 [user_proc.rs](../../src/kernel/framework/proc/user_proc.rs#L1093) / [proc_ops.rs](../../src/kernel/framework/proc/proc_ops.rs#L950) / [clone.rs](../../src/kernel/framework/syscall/clone.rs#L246) 三处调用。
  - 状态：[X]
  - 详情：选型理由：① 内核栈页不进任何 EL0 可见页表，隔离面严格优于现状（现状把栈顶页暴露在用户表）；② tramp 表维持「页级最小」（`.vectors` + `KPTI_GLOBALS` 两页），不随进程数增长；③ 净减代码（删三处调用 + 一个函数）；④ 与 arm64 Linux「trampoline 先切 `TTBR1`、不触任务栈」机制同源。
    - **施工期决定 ①（入口改为「先切两条 TTBR」而非仅切 TTBR1）**：内核栈在高 VA（迁移后），但迁移前内核栈仍在低 VA（`KERNEL_BASE=0`）。若入口仅切 `TTBR1`，迁移前压帧会 Data Abort。故入口切 **`TTBR0 → kernel_ttbr0` 且 `TTBR1 → kernel_ttbr1`**：迁移前栈经 `kernel_ttbr0` 的 DRAM 恒等块可达，迁移后栈经 `kernel_ttbr1` 高半区可达 —— 该改动**迁移前后同时成立**，因此 `map_kernel_stack_top_page` 可**立即删除**而不必推迟到 L1-03。
    - **施工期决定 ②（寄存器自举借 `TPIDRRO_EL0` 中转）**：入口时刻 `x0-x30` 全是用户态活跃值且帧尚未落栈，而切表序列需 2 个 scratch 寄存器、写暂存槽又需基址寄存器 —— 形成自举死结。逐一排除 `stp` 自引用 / `swp` / literal pool（`ldr =sym` 取的是**链接地址**，不经 PC 相对别名化，在 tramp 表下不可达）/ `x18` / SP 作基址后，裁定借一个 **sysreg 中转**（`msr` 不消耗 GPR）：选定 `TPIDRRO_EL0`（EL1 可写 / EL0 只读、内核无用途、与 Linux trampoline 用 `tpidrro_el0` 作 scratch 同源），用后清零防泄漏。
    - **`adrp` 在 `.vectors` 内自动别名化**（本轮关键认知）：`.vectors` 内所有 `adrp sym` 是 **PC 相对**（`page(PC) + (page(sym) - page(PC_link))`），而 `exception::init()` 令 `VBAR = 高别名 + vbar_low`，故运行期 PC 已在高别名 ⇒ `adrp` 得到符号的**高别名**，正好被 tramp 表映射。这是入口在切表前即可读写 `KPTI_GLOBALS` 的原因，也是迁移后（VBAR 为真实高 VA）同段代码自然成立的原因。
    - **出口对称**：先把帧内 `x3/x4` 搬进暂存槽（切表后内核栈不可达）→ 恢复除 `x3/x4` 外全部寄存器（含 `sp_el0`/`elr`/`spsr`）→ `add sp, sp, #(8*35)` → 用单个 scratch（`x4`）切回 `tramp_ttbr1`/`user_ttbr0` → 从（tramp 表映射的）全局量页取回 `x3/x4` → `eret`。出口全程**只用 `x3/x4` 作 scratch**，避免误 clobber 已恢复的 `x5` 等用户值。
    - **运行期实证**（QEMU aarch64）：`Entering EL0 (init pid=4)` 后 pid=5/6 打印 `X`/`Y` 并正常 `exit` ⇒ EL0→SVC→`el0_return`→EL0 往返成立；pid=7 的 KPTI-09 越权访问被 `handle_el0_sync` 正确解码并终止（ESR/FAR/ELR 均可信）⇒ 入口路径成立。双架构 build / aarch64 clippy / host-tests / QEMU 双架构 / `./ci/audit.sh quick` 全通过。
    - **剩余未运行期覆盖**：`handle_el0_irq`（EL0 期中断入口）本轮日志未见 `TIMER IRQ (EL0)`，即该入口在本次用例中未触发；其代码与 `handle_el0_sync` 入口段同构，待 L1-06 回归时一并复核。**机制订正（后续复验）**：该入口并非"用例负载不足"未触发，而是 aarch64 EL0 全程 `PSTATE.I=1`（`SPSR_EL1=0x3C0`，见 [mod.rs](../../src/kernel/framework/arch/aarch64/mod.rs#L326-L328)）⇒ IRQ 在 EL0 被 PSTATE 屏蔽，EL0 IRQ 入口**运行期不可达**；已登记 [ISSUE-RT-005](./unresolved-issues-2026-08-09.md)。
    - **顺手修复的既有缺陷（经用户确认「本轮一并修」）**：`handle_el0_irq` 压帧只覆盖 x0-x19/x30，而共用的 `el0_return` 会恢复 x0-x29 ⇒ EL0 期中断返回后用户 x20-x29 被内核栈残留值覆盖（HEAD 已存在，该路径在既有用例中未触发）。已补齐 x20-x29 压帧，与 `handle_el0_sync` 同集。
    - **回归测试**：新增 [host-tests/tests/aarch64_el0_frame_symmetry_test.rs](../../host-tests/tests/aarch64_el0_frame_symmetry_test.rs) 三项静态断言 —— ① 两入口压帧集合一致（x0-x29 + x30，共 16 条 `[sp, #(8*N)]`）；② 入口借 `TPIDRRO_EL0` 中转并经暂存槽 40/48 保住 x3/x4，且**切 TTBR1 早于压帧**；③ `el0_return` 先暂存再恢复、且不从帧内直接 `ldp` x3/x4。
    - 与 x86_64 的差异：x86 的 per-process 用户 PML4 能映射高 VA，故 [map_rsp0_page](../../src/kernel/framework/mm/kpti.rs#L563) 仍是有效形态（L1 不改 x86）；aarch64 的 `TTBR0` 只覆盖低半区，无法照抄，不构成双架构架构不一致。
    - 暂存槽实现：扩展 `KptiGlobals`，新增 2 个 u64 暂存字段（偏移 40/48），与现有 5 字段同法由 `offset_of!` 静态断言锁定。
    - 遗留：`kpti_aarch64.rs::phys_to_virt` 保留（`kpti_init` 清零 tramp 根表页仍用）；`HIGH_ALIAS_BASE` 与迁移后 `KERNEL_BASE` 同值（L1-04 收敛）。

- **L1-04. `phys_to_virt` 族收敛**
  - 描述：迁移后"PA 与低半区 VA 相同"的假设失效。`pmm.rs`、`kpti_aarch64.rs` 的 `HIGH_ALIAS_BASE`、`iomem.rs::mmio_virt` 需统一到单一"PA → 内核 VA"换算入口。
  - 方案：以 `KERNEL_BASE`/`HIGH_ALIAS_BASE` 为唯一来源，消除分散的隐式恒等假设；audit 侧补断言。
  - 状态：[X]
  - 详情：收敛到唯一入口 [mm/mod.rs](../../src/kernel/framework/mm/mod.rs) 的 `framework::mm::{phys_to_virt, virt_to_phys}`（`KERNEL_BASE` 为唯一数值来源）。
    - **施工落点**：[pmm.rs](../../src/kernel/framework/mm/pmm.rs)（4 处 `phys + KERNEL_BASE` → `phys_to_virt(phys)`，含 3 处 klog 参数同化）；[kpti_aarch64.rs](../../src/kernel/framework/mm/kpti_aarch64.rs)（删独立常量 `HIGH_ALIAS_BASE` 与本地换算副本）；[vmm_aarch64.rs](../../src/kernel/framework/mm/vmm_aarch64.rs)（删本地 `fn phys_to_virt`，改 `super::{..., phys_to_virt}` 导入）；[iomem.rs](../../src/kernel/framework/iomem.rs)（`mmio_virt` 折叠架构分派）；[arch/aarch64/context.rs](../../src/kernel/framework/arch/aarch64/context.rs)（注释精确性修正）。
    - **回归测试**：新增 [aarch64_pa_va_conversion_unify_test.rs](../../host-tests/tests/aarch64_pa_va_conversion_unify_test.rs) 6 项静态断言 —— ① 基址锚点（`KERNEL_BASE` / `KERNEL_TEXT_BASE` 取值与派生关系，防回退为 `0`）；② `framework/mm/` 子树内 `phys_to_virt`/`virt_to_phys` 仅 `mod.rs` 一处定义；③ `HIGH_ALIAS_BASE` 常量与裸算术不得回归；④ `iomem::mmio_virt` 不得按架构分派；⑤ `pmm.rs` 不得出现裸 `KERNEL_BASE` 算术；⑥ `lib.rs` 两处 `heap_start` 均须经 `KERNEL_BASE` 换算（L1-05 回归项）。

- **L1-05. KPTI EL1 视图简化**
  - 描述：迁移完成后，EL1 视图不再需要 DRAM 1 GiB 块（内核经 `TTBR1` 高半区可达）。`L1_el1[1]` 条目可移除，EL1 视图缩为"用户半区 + `L0_el1[0]→L1_el1`"。
  - 方案：改 [vmm_aarch64.rs](../../src/kernel/framework/mm/vmm_aarch64.rs) `build_el1_view`；同步 §KPTI-13 描述与 host-tests 断言。
  - 状态：[X]
  - 详情：**本项是 L1 的收益兑现点**（完成前 S3 结构含 DRAM 块，曾为有效形态，非缺陷）。
    - **主体施工**：[vmm_aarch64.rs](../../src/kernel/framework/mm/vmm_aarch64.rs) `build_el1_view` 删除 `L1_el1[1]` DRAM 1 GiB 块及其前置校验（`l1_idmap` / `dram_desc` 取用），视图缩为「用户半区 + `L0_el1[0]→L1_el1`」；同步 `build_el1_view` doc、`mirror_l0_slot_to_el1_view` doc、[exception.rs](../../src/kernel/framework/arch/aarch64/exception.rs) `handle_el0_sync` 第三步注释（内核镜像/栈经 `TTBR1` 可达，不在本视图内）、[iomem.rs](../../src/kernel/framework/iomem.rs) 视图构成说明、[kpti-complete-project.md](./kpti-complete-project.md) §KPTI-13 与依赖表。
    - **收益断言**：新增 [aarch64_el1_view_minimal_test.rs](../../host-tests/tests/aarch64_el1_view_minimal_test.rs) 2 项 —— ① `build_el1_view` 源码不含 `l1_idmap` / `dram_desc` / `L1_IDMAP` / `el1_l1_ptr.add`，且保留 `write_volatile(el1_l1_ptr, l2_u_desc)`；② `handle_el0_sync` 入口段注释不再主张「内核 DRAM 块」。
    - **施工期暴露的迁移面残项（本轮 QEMU 回归根因）**：移除 DRAM 块后 aarch64 启动即 `SYNC! ESR=0x96000005 FAR=0x42052880 ELR=0xFFFF00004013E008`（EC=EL1 数据异常 + DFSC=level-1 翻译故障；`ELR` 经 `build/kernel.map` 解析为 `SpinLock::raw_lock`）——即内核**已在高 VA 取指**，但解引用了**低 VA 内核对象**（`FAR` 落在 `heap_start = kernel_end + 0x200000` 起的 kmalloc 堆内，与日志 `proc=0x420530F0` 同区）。
      - 根因：[lib.rs](../../src/kernel/lib.rs#L698) aarch64 专属 `heap_start = boot_info.kernel_end + 0x200000`（详见 L1-01 补遗）。此类残项无法由 `KERNEL_BASE` 引用面检索发现，只能由「低 VA 内核对象的解引用」运行期暴露。
      - 旁证（已排除）：PMM 元数据四处（`RawMetaStore` / `init_bitmap`）走 `phys_to_virt`；[iobuf.rs](../../src/kernel/framework/iobuf.rs#L70) 走 `phys_to_virt`；[process.rs:403](../../src/kernel/framework/proc/process.rs#L403) 内核栈 `+ KERNEL_BASE`；[brk.rs:58](../../src/kernel/services/syscall/brk.rs#L58) 仅判空不解引用。`make test-unit` 通过亦佐证高 VA 堆在 aarch64 可用（其测试路径本就无条件 `KERNEL_BASE + kernel_end`）。
    - **残项闭合（经用户裁定，方案 A「修前进」）**：删除 aarch64 分叉，与 x86_64 共用 `KERNEL_BASE + boot_info.kernel_end + 0x200000`。堆**物理布局零变化**（PA 仍自 `kernel_end + 0x200000` 起），仅访问别名由低半区恒等切至高半区直射区（落在 `L1_IDMAP[1]` 覆盖范围内）。回归断言见 L1-04 详情第 ⑥ 项。
    - **运行期实证**（QEMU aarch64）：`kmalloc initialized at 0xFFFF000042051000`、`[USER] got proc=0xFFFF0000420530F0`（低 32 位与故障态同区，前缀即 `KERNEL_BASE`）；`VFS ready` → `Entering EL0 (init pid=4)` → `exit: pid=5/6 code=0` → pid=7 越权被终止 → `✓ KPTI 隔离断言通过 (KPTI-09)`。

- **L1-06. 回归与验证**
  - 描述：迁移后双架构不回归。
  - 方案：§2.3 五门槛（`./ci/build.sh all` / `./ci/audit.sh quick` / `make test-host` / `make test-unit` / QEMU x86_64+aarch64 boot）；QEMU aarch64 需确认 `Entering EL0` 后 SVC 往返仍成立（KPTI-06 判据）。
  - 状态：[X]
  - 详情：§2.3 五门槛本轮实测全过：
    - `./ci/build.sh all` —— 双架构 build passed + host tests passed + link passed（`Passed: 5  Failed: 0`）。
    - clippy —— 由 `./ci/audit.sh quick` 覆盖（clippy pedantic lib 维 + `kernel_test` 维 + `host-test` 维，全 passed）。
    - 核心审计 —— `./ci/audit.sh quick` 全 ✓。
    - `make test-host` —— 106 个 test binary 全 `ok`，0 failed（含本轮新增 3 个 aarch64 静态断言文件）。
    - `make test-unit` —— `✅ ALL TESTS PASSED (QEMU exit: 33)`。
    - QEMU boot —— aarch64 与 x86_64 均 `✓ 完整启动成功` 且 `✓ KPTI 隔离断言通过 (KPTI-09)`。
    - **KPTI-06 判据（`Entering EL0` 后 SVC 往返）成立**（QEMU aarch64 日志）：`Entering EL0 (init pid=4)` → `SELF-CHECK: calling enter_user_asm...` → `exit: pid=5 code=0` / `exit: pid=6 code=0`（EL0→SVC→`el0_return`→EL0 往返）→ `exit: pid=7 code=7` + `[KPTI] EL0 kernel high-half access denied`（越权路径由 `handle_el0_sync` 正确解码）。
    - **未覆盖项（非回归）**：`handle_el0_irq`（EL0 期中断入口）本轮日志仍未见 `TIMER IRQ (EL0)`，即该入口在既有用例中未被触发；与 L1-03b 登记一致，待后续用例覆盖。**机制订正（后续复验）**：成因是 EL0 全程 `PSTATE.I=1`（`SPSR_EL1=0x3C0`），EL0 IRQ 入口**运行期不可达**，非用例覆盖不足；已登记 [ISSUE-RT-005](./unresolved-issues-2026-08-09.md)。
    - 施工期注意（工具链层面，非代码问题）：[qemu_boot_test.sh](../../scripts/qemu_boot_test.sh#L115) 会删除 `build/kernel.{bin,flat,map}` 与 `build/stage1.bin`，故 QEMU 测试后需先重建（`make build/stage1.bin` / `make user` / `./ci/build.sh <arch>`）再跑依赖这些产物的门槛。
    - **本轮一并修复的 HEAD 既存缺陷（经用户确认「本轮一并修复」）**：aarch64 侧 `clippy -D pedantic`（CI `clippy aarch64 -D pedantic` job，[ci-x86.yml:192-208](../../.github/workflows/ci-x86.yml#L192-L208)）在 HEAD 既有 **7 处 error**，该 job 处于红灯态；此为 HEAD 预存问题、**非 L1 引入**，随本轮一并闭合（闭合后该 job 转绿）。逐处处置：
      - [exception.rs](../../src/kernel/framework/arch/aarch64/exception.rs) `verbose_bit_mask`（`frame.spsr & 0xF == 0`）—— 该判据即 `SPSR_EL1.M[3:0] == 0` 的位域比较，与相邻注释同形；改 `trailing_zeros() >= 4` 反而偏离架构文档语义，故加 `#[expect]` 兜底而不改判据。
      - [vmm_aarch64.rs](../../src/kernel/framework/mm/vmm_aarch64.rs) `manual_let_else` + `single_match_else`（`alloc_table` 失败时经 `free_table(el1_l0)` 回滚）—— 改 `let-else`（回滚语句置于 else 块）。
      - [proc_ops.rs](../../src/kernel/framework/proc/proc_ops.rs) `manual_let_else` + `single_match_else`（子进程 EL1 视图 fail-closed 回滚：`drop_boxed_process` + `free_pid`）—— 改 `let-else`。
      - [proc_ops.rs](../../src/kernel/framework/proc/proc_ops.rs) `used_underscore_binding` ×2（`_fpu_pad` @136 历史填充槽名复用为用户页表槽位，被 `switch.asm` / `context.rs` 偏移表**按名**引用）—— 分别以 `proc_save_user_regs_aarch64` 的 fn 级 `#[expect]`、以及 fork 路径 `#[cfg(target_arch = "aarch64")]` 块内的内部属性 `#![expect]` 兜底。
      - 兜底形态约束（避免误伤另一架构）：两条 `#[expect]` 分别挂在 `#[cfg(target_arch = "aarch64")]` 的 fn 与块**内部**，故 x86_64 维不产生 `unfulfilled_lint_expectations`。实测：aarch64 与 x86_64 两侧 `cargo clippy ... -D clippy::pedantic` 均 `Finished`（0 error），且 `./ci/audit.sh quick` 的 clippy 三维全 passed。

## 验证标准

- §2.3 5 条门槛全过（双架构 build/clippy/test-host/test-unit/QEMU）
- QEMU aarch64：`Entering EL0` 之后 SVC 往返日志成立（沿用 KPTI-06 判据）
- `KERNEL_BASE` 引用面全部完成"目标口径"复核（L1-01 清单逐项勾销）
- L1-05 完成后，host-tests 断言 EL1 视图**不含**内核 DRAM 块
- L1-03b 完成后，内核栈页不出现在任何 EL0 可见页表（tramp 表仅 `.vectors` + `KPTI_GLOBALS` 两页）

## 风险与回退

- **启动期恒等窗口**：MMU 开启前指令取指依赖恒等映射，迁移需保留一段 boot 跳板窗口；漏处理 ⇒ 启动即 Prefetch Abort。缓解：分步（L1-03 先迁链接脚本、L1-04 后迁换算），每步 QEMU 验证。
- **分散恒等假设**：154 处引用中隐含"PA == VA"的部分未必显式写 `KERNEL_BASE`。缓解：L1-01 深挖 + audit 脚本兜底。
- **与 KPTI 交织**：迁移期间 KPTI 的 EL1 视图处于过渡态（含 DRAM 块），须避免两工程并行施工导致 QEMU 问题难以归因（同 §9.1 T1/T5 并行约束的先例）。
- **回退**：`KERNEL_BASE` 两处常量（memory_allocator.rs / mm/mod.rs）回退为 `0` 即可恢复低半区形态；L1-05 未做前 S3 结构可原样保留。

## 关联

- 上游：[kpti-complete-project.md](./kpti-complete-project.md)（KPTI-13 EL1 视图 S3 / 遗留登记项 L1）
- 重叠待办：**k3g**（`context_switch_asm` 用户表语义 + 目标 EL0 时切 TTBR1=TRAMP + aarch64 `set_kernel_stack` 空实现）——与 aarch64「fork 子进程零上下文」缺陷合并设计（见 KPTI-17）