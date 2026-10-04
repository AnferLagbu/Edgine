# munmap 拆除与 protect 目标页表修复工程

> **定位**：修复"**用户页表的建立侧与拆除/改权限侧作用在不同的表上**"这一结构性缺陷——`mmap` 后的数据帧由缺页路径建到**用户页表**，而 `munmap` 的拆除与 `mprotect` 的权限改写落到**全局单表**变体（`KERNEL_PML4`）。
>
> **来源**：`cr3-lifetime-ownership.md` §6 登记项「`munmap` 不拆除 PTE、不归还数据帧」与「`vmspace.rs::protect()` 的 UAF 形态」。立项调研中发现登记项描述不准确（拆除调用链**存在**，缺陷在目标表），故本工程重新定义问题。
>
> **关系**：与 `cr3-lifetime-ownership.md`（已完成 D3）**不重叠**——D3 收敛的是"计数面契约"，本工程收敛的是"契约作用在正确的页表上"。两者同在 privileged/mm，**不可与其它 privileged 工程并行施工**。
>
> **关联**：`docs/explain/explain-framekernel.md`（I2/I4 不变式）；`docs/plan/cr3-lifetime-ownership.md`（帧持有计数面契约 §8.1）；`docs/plan/kpti-complete-project.md`（KPTI 两表模型）；`docs/plan/smp-ipi-protocol.md`（TLB 失效协议）。

## 1. 立项依据

### 1.1 根因一句话

用户地址空间存在**两张顶级页表**（KPTI 模型下 `USER_PML4` 与 `KERNEL_PML4`），而 privileged 的拆除/改权限原语有**两个变体**：`*_in_table(pml4, ...)`（显式指定表）与全局单表变体（固定 `KERNEL_PML4`）。用户映射的**建立**走显式变体，**拆除与改权限**却走全局变体 ⇒ 两侧目标表不一致。

### 1.2 三处缺陷

| #      | 缺陷                                            | 静态链路                                                                                                                                                                                                                                                                                                                                                                                                                    | 现状                                                                                              |
| ------ | --------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| **P1** | **munmap 拆除目标表错位**                            | [mmap.rs:195-207](../../src/kernel/functions/mm/mmap.rs#L195-L207) `munmap_syscall` → [vma.rs:437](../../src/kernel/privileged/mm/vma.rs#L437) `remove_range` → [vma.rs:490-500](../../src/kernel/privileged/mm/vma.rs#L490-L500) `unmap_vma_pages` → [vmm\_x86\_64.rs:416](../../src/kernel/privileged/mm/vmm_x86_64.rs#L416) `vmm.unmap_page`（目标表 [L419](../../src/kernel/privileged/mm/vmm_x86_64.rs#L419) `KERNEL_PML4`） | **调用链存在**，登记项原描述「不拆除 PTE」**不准确**；真实缺陷是目标表可能不是用户表                                                |
| **P2** | **mprotect 权限改写目标表错位**                        | [mprotect.rs:57-81](../../src/kernel/functions/mm/mprotect.rs#L57-L81) `mprotect_syscall` → [vma.rs:536](../../src/kernel/privileged/mm/vma.rs#L536) `MmStruct::mprotect` → [L652](../../src/kernel/privileged/mm/vma.rs#L652) `vmm.protect_page` → [vmm\_x86\_64.rs:501-525](../../src/kernel/privileged/mm/vmm_x86_64.rs#L501-L525)（目标表 [L504](../../src/kernel/privileged/mm/vmm_x86_64.rs#L504) `KERNEL_PML4`）           | 同上；权限**可能根本没改到用户表**                                                                             |
| **P3** | **`VmSpace::protect()`** **的 unmap→remap 形态** | [vmspace.rs:156-171](../../src/kernel/privileged/vmspace.rs#L156-L171)：`get_physical_in_pml4` 取旧物理地址 → `unmap_page_in_table` → `map_page_in_table` 复用同一物理地址                                                                                                                                                                                                                                                              | D3 后 `unmap` 会 `frame_dec`：若 leaf 是唯一持有者 ⇒ 归零 ⇒ 延迟释放，随后 remap 把**待释放帧**重新挂回 = UAF。**零调用者**（脚手架） |

### 1.3 与 cr3-lifetime-ownership.md §6 登记项的关系

登记项写的是「`munmap` 不拆除 PTE、不归还数据帧」与「`vmspace.rs::protect()` 的 UAF 形态」。调研订正两点：

1. **P1 的前提订正**：拆除调用链**已存在**（`remove_range` → `unmap_vma_pages` → `unmap_page`），故不是"缺少拆除"，而是"**拆除作用在内核表上**"。
2. **P2 为调研新增**：`mprotect` 与 `VmSpace::protect` 属同一实现问题（pml4 参数化的就地改权限），原登记项未覆盖。

因此本工程**不重复** D3 已完成的计数面工作，只补"目标表正确性"。

## 2. 源码调研结论

### 2.1 建立侧（数据帧如何被映射）

| #  | 位置                                                                                                                                                                                           | 目标表                                                                                           |
| -- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| B1 | [page\_fault.rs:260](../../src/kernel/privileged/mm/page_fault.rs#L260) `handle_vma_fault_with_mm`                                                                                            | 用户表（`pml4` 来自 [L114](../../src/kernel/privileged/mm/page_fault.rs#L114) `read_user_cr3_asm()`） |
| B2 | [page\_fault.rs:196](../../src/kernel/privileged/mm/page_fault.rs#L196) / [L287](../../src/kernel/privileged/mm/page_fault.rs#L287) / [L455](../../src/kernel/privileged/mm/page_fault.rs#L455) | 用户表（同上，栈扩展 / uffd / swap-in）                                                                  |
| B3 | [dispatch.rs:834](../../src/kernel/privileged/syscall/dispatch.rs#L834) FB 映射路径                                                                                                               | 用户表（`cr3` 来自映射记录）                                                                             |
| B4 | [vmspace.rs:100](../../src/kernel/privileged/vmspace.rs#L100) `VmSpace::map`                                                                                                                  | `self.pt_root`（显式变体）                                                                          |

**结论**：用户数据帧一律建在**用户表**上。注意 `mmap` 本身只建 VMA、不建 PTE（[mmap.rs:76-167](../../src/kernel/functions/mm/mmap.rs#L76-L167)），PTE 由按需缺页产生。

### 2.2 拆除/改权限侧

| #  | 位置                                                                                              | 目标表                                                        |
| -- | ----------------------------------------------------------------------------------------------- | ---------------------------------------------------------- |
| T1 | [vmm\_x86\_64.rs:1207](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1207) `unmap_page_in_table` | **显式 pml4**（含 `is_user()` 过滤 + `frame_dec` + `defer_free`） |
| T2 | [vmm\_aarch64.rs:678](../../src/kernel/privileged/mm/vmm_aarch64.rs#L678) `unmap_page_in_table`  | 显式 root（D3b 已补 `frame_dec`）                                |
| T3 | [vmm\_x86\_64.rs:416](../../src/kernel/privileged/mm/vmm_x86_64.rs#L416) `unmap_page`            | **`KERNEL_PML4`**（全局变体）                                    |
| T4 | [vmm\_x86\_64.rs:501](../../src/kernel/privileged/mm/vmm_x86_64.rs#L501) `protect_page`          | **`KERNEL_PML4`**（全局变体）                                    |
| T5 | [vmm\_aarch64.rs:477](../../src/kernel/privileged/mm/vmm_aarch64.rs#L477) `protect_page`         | **`self.kernel_l0`**（全局变体，同构问题）                            |

**结论**：拆除/改权限侧存在两个变体，而**全局变体被用户地址空间路径使用**（T3 用于 munmap，T4/T5 用于 mprotect）。**全仓不存在** **`protect_page_in_table`**（pml4 参数化的改权限）。

### 2.3 既有事实订正（调研中核实，与先前认知不同）

| 项                     | 订正                                                                                                                                                                                                                                                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Process::cr3`        | **已存在**（[process.rs:145](../../src/kernel/privileged/proc/process.rs#L145) `AtomicU64`），且有既有安全读取口 `process_get_cr3(pid)`（[proc\_ops.rs:272-276](../../src/kernel/privileged/proc/proc_ops.rs#L272-L276)，经 [mechanism.rs:44](../../src/kernel/privileged/proc/mechanism.rs#L44) 导出）。**不需要新增字段** |
| `get_current_pml4()`  | KPTI 下 syscall 入口已 `mov cr3, r12` 切到内核表（[isr.asm:192-202](../../src/kernel/privileged/boot/isr.asm#L192-L202)）⇒ 在 syscall 上下文读 CR3 得到的是**内核表**，且在[延期释放路径](../../src/kernel/privileged/mm/vmm_x86_64.rs#L2346-L2352)中会被 `defer_free` 临时改写 ⇒ **不可用于取用户页表根**                                     |
| `read_user_cr3_asm()` | 读 `USER_CR3_SAVE`（per-CPU 单槽，[isr.asm:193](../../src/kernel/privileged/boot/isr.asm#L193)），是**机制层缺页入口的临时槽**，非对象级句柄；对当前进程且处于用户态入口上下文才成立                                                                                                                                                       |
| `VmSpace` 调用者         | **零调用者**（仅 [usermode.rs:38](../../src/kernel/privileged/usermode.rs#L38) 形参、`prelude.rs` re-export、自身；`enter_user_mode` 亦无调用者）                                                                                                                                                               |

### 2.4 关键未决事实（须实证，不得假设）

**KPTI 下** **`KERNEL_PML4`** **低半区与** **`USER_PML4`** **低半区是否共享下层页表帧？**

* 证据 A（指向共享）：[sync\_user\_pml4\_entry](../../src/kernel/privileged/mm/kpti.rs#L228-L250) 把 `KERNEL_PML4` 的某一项**整项复制**到 `USER_PML4` ⇒ 项内的下层页表帧地址相同 ⇒ 若低半区也走此同步，则下层共享，T3/T4 改内核表**会**影响用户表。

* 证据 B（指向分叉）：缺页路径用 `user_cr3` 经 `map_page_in_table` **直接建在用户表**上，不经内核表。

* **两种证据互相冲突**，静态阅读一度无法判定 ⇒ 单独立为 D-5 实证条目。**D-5 已收口**：`create_user_page_table` 明示进程表低半区「保持全零」，两表**不同源**，故 P1/P2 判定为**真实缺陷**（证据见 D-5 详情）。

## 3. 用户裁定

| # | 议题                           | 裁定                                                                                                                                                         |
| - | ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1 | `munmap`/`mprotect` 的用户页表根来源 | **丙**：复用 `Process::cr3`——经 `privileged::proc::process_get_cr3(process_get_current_pid())` 取。理由：该字段是 cr3 的既有权威源（COW 克隆、销毁、上下文切换均以它为准），零新增字段、单一真相源、对任意 pid 成立 |
| 2 | 本轮范围                         | **并入 mprotect**：`VmSpace::protect` 与 `MmStruct::mprotect` 属同一「pml4 参数化就地改权限」实现，一次做对，避免同一实现做两遍                                                              |
| 3 | 实现强度（§12.3）                  | **B 相对完整实现**：含部分拆除语义核查、双架构覆盖、mprotect 全链路实证与回归、帧计数与 pcache 双持有者测试矩阵                                                                                        |

### 3.1 架构约束（F3 环依赖）

`mm` **不可**依赖 `proc`——[proc\_ops.rs:814](../../src/kernel/privileged/proc/proc_ops.rs#L814) 已依赖 `mm::cow` / vmm，`mm → proc` 即构成模块环（F3 违规）。

⇒ **cr3 不作为** **`MmStruct`** **的字段**，而是在 functions 侧取得后**作为参数**传入 privileged 的 mm 安全方法。functions 可同时使用 `privileged::proc` 与 `privileged::mm` 的顶层 re-export，不引入环。

（functions 侧取 cr3 有先例：[user\_driver.rs:146](../../src/kernel/privileged/egdf/user_driver.rs#L146) 等 3 处在 privileged 内使用 `process_get_cr3`；functions 侧 `process_get_current_pid` 用于 20+ 处。）

## 4. 施工条目

### D-1 新增 pml4 参数化的改权限入口

描述：privileged 侧补齐 `protect_page_in_table(pml4, virt, flags)`，使"就地改权限"可作用于指定页表（x86\_64 与 aarch64 各一份），与既有 `unmap_page_in_table` 对称。
方案：x86\_64 参照 [protect\_page:501-525](../../src/kernel/privileged/mm/vmm_x86_64.rs#L501-L525) 的四级遍历，把 `KERNEL_PML4` 替换为入参 `pml4`，保留内核半区安全门与 TLB 失效；aarch64 参照 [protect\_page:477](../../src/kernel/privileged/mm/vmm_aarch64.rs#L477)，把 `self.kernel_l0` 替换为入参。
状态：\[X]
详情：

* x86\_64（[vmm\_x86\_64.rs](../../src/kernel/privileged/mm/vmm_x86_64.rs)）：新增 `protect_page_in_table(pml4: u64, virt, new_flags)`，完整镜像 `protect_page` 的四级遍历（1G/2G/4K 三分支、只改写 `PRESENT|WRITABLE|USER|NX`、`flush_tlb`），差异仅三点：① 目标根取 `pml4` 入参；② `pml4 == 0` 提前返回（fail-closed）；③ 内核半区安全门（`pml4_idx() >= 256` ⇒ `klog_boot_info!` + return）。

* aarch64（[vmm\_aarch64.rs](../../src/kernel/privileged/mm/vmm_aarch64.rs)）：新增同名 `protect_page_in_table(root_paddr: u64, virt, new_flags)`，镜像 `protect_page` 的 L1 块(1GB)/L2 块(2MB)/L3 页(4KB) 三分支 + `tlbi vaae1is` 序列，`self.kernel_l0` 换为 `root_paddr`；**不含内核半区安全门**（与既有 `protect_page` 保持同构，不额外添加非本工程范围的门）。

* 未删原语：全局单表变体 `protect_page` 保留（见 §6 登记项 2）。

### D-2 `MmStruct` 拆除/改权限接口接收 cr3

描述：把 `MmStruct` 的用户地址空间操作改为以 cr3 为入参并走显式变体，使拆除侧与建立侧目标表一致。
方案：`unmap_vma_pages`（[vma.rs:490-500](../../src/kernel/privileged/mm/vma.rs#L490-L500)）由 `vmm.unmap_page` 改为 `vmm.unmap_page_in_table(cr3, ...)`；`MmStruct::mprotect`（[vma.rs:536](../../src/kernel/privileged/mm/vma.rs#L536)）由 `vmm.protect_page` 改为 D-1 的显式变体。cr3 经参数由 functions 传入（§3.1）。
状态：\[X]
详情：

**范围裁定（用户选定 A 全量收口）**：`remove_range` 是拆除侧唯一漏斗，签名一改则 7 处调用点连带；为避免出现"带 cr3 的平行变体"（违反单一实现），统一把 cr3 作为入参贯穿：

* `MmStruct::remove_range(start, end, cr3)`（[vma.rs:443](../../src/kernel/privileged/mm/vma.rs#L443)）：新增 cr3 参数 + 中文文档；四种 VMA 拆分分支的 4 处 `unmap_vma_pages(&x)` → `unmap_vma_pages(&x, cr3)`。

* `MmStruct::unmap_vma_pages(vma, cr3)`（[vma.rs:496](../../src/kernel/privileged/mm/vma.rs#L496)）：`vmm.unmap_page(VirtAddr)` → `vmm.unmap_page_in_table(cr3, VirtAddr)`，承担 §8.1 规则 3 的帧注销。

* `MmStruct::mprotect(start, len, new_flags, cr3)`（[vma.rs:547](../../src/kernel/privileged/mm/vma.rs#L547)）：两处 cfg 分支内的 `vmm.protect_page` → `vmm.protect_page_in_table(cr3, …)`（保留 x86\_64/aarch64 双 cfg 块结构，未合并）。

* 连带调用点（均已传 cr3）：`MmStruct::mremap`（3 处 `remove_range`）、`MmStruct::set_brk`（1 处）、[user\_driver.rs:167](../../src/kernel/privileged/egdf/user_driver.rs#L167) 与 [user\_driver.rs:343](../../src/kernel/privileged/egdf/user_driver.rs#L343)（cr3 已在作用域）、privileged 自测 [test\_new\_features.rs:374](../../src/kernel/privileged/tests/test_new_features.rs#L374)（该用例只校验 VMA 描述符增删、不建页表，故传 `0` 表达"不触及任何表"）。

**部分拆除语义核查结论**：`remove_range` 的四种 VMA 拆分分支产生的"待拆除区间"与逐页拆除范围**一致**——每分支都以 `Vma::new(<被删子区间>)` 为参数进入 `unmap_vma_pages`，后者按 `[start, end)` 逐页对齐遍历，与建立侧不对齐语义无冲突；拆除范围不含未覆盖页（不会误拆相邻 VMA 的 PTE）。

### D-3 functions 侧取 cr3 并传入

描述：`munmap_syscall` 与 `mprotect_syscall` 取本进程用户页表根并传给 privileged。
方案：取 `privileged::proc::process_get_cr3(process_get_current_pid())`；为 `None`（进程无 mm 或 cr3 为 0）时返回 `ENOMEM`/`EINVAL`（fail-closed：不静默退化为全局表）。
状态：\[X]
详情：

按裁定 1（丙）经既有安全读取口取 cr3，4 处 functions 侧入口全部 fail-closed（`None` ⇒ `ENOMEM`，不退化到全局表）：

* [mmap.rs](../../src/kernel/functions/mm/mmap.rs) `munmap_syscall`：取 cr3 后 `mm.remove_range(start, end, cr3)`；文档补 ENOMEM 语义。

* [mprotect.rs](../../src/kernel/functions/mm/mprotect.rs) `mprotect_syscall`：取 cr3 后 `mm.mprotect(addr, len, new_flags, cr3)`。

* [mremap.rs](../../src/kernel/functions/mm/mremap.rs) `mremap_syscall`（A 全量收口连带）：取 cr3 后委托 `mm.mremap(…, cr3)`。

* [brk.rs](../../src/kernel/functions/syscall/brk.rs) `sys_brk` 的 VMA 路径（A 全量收口连带）：取 cr3 后 `mm.set_brk(addr, cr3)`；注释说明"扩展方向虽不用 cr3，但同样要求合法页表根，避免静默退化"。

functions 层 0 unsafe（F1）：取 cr3 与传参均为 safe API（顶层 re-export），未新增 unsafe。

### D-4 `VmSpace::protect` 消除 unmap→remap

描述：修掉 P3 的 UAF 形态。
方案：`VmSpace::protect`（[vmspace.rs:156-171](../../src/kernel/privileged/vmspace.rs#L156-L171)）改为经 D-1 的 `protect_page_in_table(self.pt_root, virt, flags)` 就地改权限，删除 `get_physical_in_pml4` + unmap + map 三步。
状态：\[X]
详情：

`VmSpace::protect` 改为单条 `vmm.protect_page_in_table(self.pt_root.as_u64(), vaddr, new_flags)`；删除 `get_physical_in_pml4` 查旧物理地址 + `unmap_page_in_table` + `map_page_in_table` 三步。附注释记录 UAF 机理：原"先 unmap 再 map"在 unmap 侧 `frame_dec`，若该 leaf 是唯一持有者则计数归零进入延迟释放，随后又把**待释放帧**挂回 ⇒ UAF；就地改权限不拆除映射，故完全不触及帧持有计数面。

`VmSpace` 零调用者，本工程**只修语义、不接线**（接线属另议，见 §6 登记项 4）。

### D-5 KPTI 两表低半区共享性实证

描述：判定 §2.4 的未决事实，为 D-1/D-2 的定性（真实缺陷 vs 语义脆弱）提供依据。
方案：原计划临时埋点对比两表 PTE；**实际改由决定性静态证据 + D-6 负向行为测试判定**（省去临时插桩，避免与 F9 冲突，且行为验证强于读值对比）。
状态：\[X]（静态证据已决定性判定；行为确认已由 D-6 的负向验证闭合）
详情：

**结论：两表低半区不同源、不共享下层帧 ⇒ P1/P2 为真实缺陷。**

决定性证据（[vmm\_x86\_64.rs:800-836](../../src/kernel/privileged/mm/vmm_x86_64.rs#L800-L836) `create_user_page_table`）：

* L802 分配**全新** PML4 页并清零（L807）；

* L821 只复制**高半区** `PML4[256..511]`；

* [L823-825](../../src/kernel/privileged/mm/vmm_x86_64.rs#L823-L825) 明示「**低半部分 (PML4\[0..256]) 保持全零** …… 不应继承内核恒等映射」；

* 进程表低半区由 [L857-893](../../src/kernel/privileged/mm/vmm_x86_64.rs#L857-L893) **显式逐页构造**（trampoline 物理页恒等映射、GDT/IDT/TSS 页）。

⇒ `KERNEL_PML4` 低半区（boot 期恒等映射，见 [L1466](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1466)）与进程用户表低半区**互不相干**。故：

* `vmm.unmap_page` 在 munmap 中命中的是**内核表低半区**：若该 VA 索引在内核表无映射则 `!pml4e.is_present()` 静默早退（用户 PTE 悬留、帧不归还）；若有内核低半区映射则**破坏内核映射**。两者皆错。

* `vmm.protect_page` 同理：用户页权限**从未被改写**。

**附带发现（影响 D-2 方案）**：[unmap\_page:466-491](../../src/kernel/privileged/mm/vmm_x86_64.rs#L466-L491) 对 leaf 只做 `set_value(0)` + `flush_tlb`，**不含** **`frame_dec`、不含中间表释放** ⇒ 该全局变体**根本不参与帧持有计数面**，故 D-2 并非"参数替换"而是"改用 `unmap_page_in_table`"（后者含 `is_user()` 过滤 + `frame_dec` + `defer_free`）。

**行为确认载体**：并入 D-6——修复前「munmap 后访问该地址」应仍成功（复现 P1），修复后应 SIGSEGV。

### D-6 回归测试

描述：为拆除侧与权限侧补可验证的回归载体。
方案：

* munmap 后帧计数归零（`frame_ref_count` 读 0）且该地址不可访问（负向：访问应触发 SIGSEGV）；

* mprotect 后权限**确实生效**（负向：PROT\_READ 区写入应 SIGSEGV）；

* 双架构覆盖（x86\_64 走 kernel\_test，aarch64 以编译 0w0e + `qemu_boot_test.sh aarch64` 到 EL0 为最低载体）；

* 帧计数与 pcache 双持有者矩阵（文件映射页同时被 pcache 与 leaf 持有 ⇒ munmap 拆 leaf 后计数不得归零）。
  状态：\[X]
  详情：

新增 kernel 自测分组 `mm::vma_teardown`（[test\_mm.rs](../../src/kernel/privileged/tests/test_mm.rs)），两条用例均带 `#[cfg(feature = "host-test")]` Skip 变体：

* `remove_range_targets_user_table`：以既有 `cow_setup_mapped_page()`（返回 `(pml4, phys)`，映射于 `COW_TEST_VA = 0x40_0000`）建页表 + VMA。**负向控制** `remove_range(va, va+PAGE_SIZE, 0)` ⇒ 断言用户 PTE 悬留且 `frame_ref_count == 1`；**正向** 传 `pml4` ⇒ 断言 `vmm_get_physical_in_table == 0` 且 `frame_ref_count == 0`。

* `mprotect_targets_user_table`：读 `pte_before` → `mprotect(va, PAGE_SIZE, PRESENT|USER, pml4)` ⇒ 断言 PTE 值改变、仍 present、x86\_64 上 `Writable` 位清零、物理帧不变、`frame_ref_count == 1`（"改权限不得触碰计数面"= P3 UAF 判别）；改回 `rw` ⇒ 断言 `get_pte_value == Some(pte_before)`（往返精确还原）；`vmm_destroy_page_table` 后计数归零。

**未字面复现 P1 的理由**：不在 kernel 测试中直调全局变体 `vmm_unmap_page(0x400000)`。D-5 已证 `KERNEL_PML4` 低半区存在 boot 恒等映射，该调用会**清掉内核恒等映射**（危险且非被测语义）；改用 `cr3 == 0` 负向控制表达同一失效形态（"缺失/错误页表 ⇒ 用户 PTE 悬留 + 帧不归还"）。

**pcache 双持有者矩阵的处理**：不写"编码了错误期望"的测试。静态核查结论（原「`deref` 绕过帧持有计数」的表述**已订正**，见 §6 登记项 6）——`pcache::deref` 归零时调用的 `pmm.free_page` **本身即计数感知**（内部 `frame_counts_release`），故帧计数 2（pcache 自身一份 + 用户 leaf 一份）下 `munmap` 顺序为 3→2→1 递减，**不存在**"仍被映射即归还进 free list"的窗口；真实缺陷是 pcache 条目 `ref_count` 的**增减点不对称**（命中不登记、按区间无条件注销），属**与 §8.1 同族但不属本工程改动范围**的缺陷，登记为预存问题（§6 登记项 6），不为之造实现或造期望。

**双架构覆盖**：x86\_64 以 `make test-unit` 在 QEMU 内实跑（实测 512/512 全通过、0 skipped）；aarch64 以 `cargo build --features kernel_test` 0w0e + `qemu_boot_test.sh aarch64` 到 EL0 为最低载体（该架构的 kernel\_test 无 QEMU 运行配方）。

### D-7 文档同步

描述：代码落地后同步本文件与 `cr3-lifetime-ownership.md` §6 登记项状态（P1 描述订正）。
方案：回填 D-1..D-6 详情与实测数据；在 `cr3-lifetime-ownership.md` §6 对应登记项标注"已由 `munmap-protect-pml4.md` 承接"。
状态：\[X]
详情：

* 本文件回填 D-1..D-6 详情与实测数据（§4）；§5 补实测结果（§5.1）；§6 登记项由 5 项扩为 7 项（新增 pcache 绕过帧计数、`user_driver` 重复 unmap）。

* [cr3-lifetime-ownership.md](../../docs/plan/cr3-lifetime-ownership.md) §6：`vmspace.rs::protect()` UAF 隐患与「munmap 不拆除 PTE」两条登记项标注**已由本工程承接**，并在原条目内订正 P1 描述（拆除调用链已存在，缺陷在目标表）。

* 施工中修正的测试缺陷（已闭合，非预存问题）：`remove_range_targets_user_table` 初版让负向控制与正向复用同一 `MmStruct`，而 `remove_range` **无条件删除 VMA 描述符** ⇒ 正向找不到 VMA 而失去判别力（首轮实测 FAIL「用户 PTE 必须被拆除」）。修正为负向/正向各用独立 `MmStruct`。

* 未提交：本轮改动留在工作区，未创建 commit。

## 5. 验证门槛

每轮施工后串行复跑（主 agent 执行，取脚本自身退出码）：

1. `./ci/build.sh all` —— 双架构 0 error / 0 warning
2. `./ci/audit.sh quick` —— 退出码 0（含 clippy pedantic + 两 feature 维）
3. `make test-host` —— 退出码 0
4. `make test-unit` —— **判序列日志** `tests/reports/unit_test_<ts>.log`（该目标 fail-open，make 退出码不可信）
5. `./scripts/qemu_boot_test.sh x86_64` —— 到 Ring 3
6. `./scripts/qemu_boot_test.sh aarch64` —— 到 EL0
7. `make test-smp-multicore` —— 2/3/4 核全通过
8. **负向验证**（本工程专属）：munmap 后访问应立即 SIGSEGV（而非静默成功）；mprotect 后越权访问应立即 SIGSEGV

### 5.1 实测结果（全量收口 + D-6 落地后串行复跑）

| # | 门槛                                    | 实测                                                                                                             | 结果 |
| - | ------------------------------------- | -------------------------------------------------------------------------------------------------------------- | -- |
| 1 | `./ci/build.sh all`                   | `Passed: 5  Failed: 0`（exit 0；双架构 build 0w0e + host tests + 链接）                                                | ✅  |
| 2 | `./ci/audit.sh quick`                 | exit 0（0 unsafe 边界、6 不变式、SAFETY 100%、注释中文 100%、双架构 check、clippy pedantic + kernel\_test/host-test 两 feature 维） | ✅  |
| 3 | `make test-host`                      | exit 0（11 passed / 0 failed）                                                                                   | ✅  |
| 4 | `make test-unit`（判日志）                 | `RESULT: ALL 512 TESTS PASSED (0 skipped)`，QEMU exit 33                                                        | ✅  |
| 5 | `./scripts/qemu_boot_test.sh x86_64`  | 里程碑 `VFS ready`，进入 Ring 3                                                                                      | ✅  |
| 6 | `./scripts/qemu_boot_test.sh aarch64` | 里程碑 `VFS ready` + virtio-net NetOps 探测，进入 EL0                                                                  | ✅  |
| 7 | `make test-smp-multicore`             | 2/3/4 核全通过（用户态实际执行 + TLB shootdown 覆盖全核 + 延迟释放帧确有归还）                                                           | ✅  |
| 8 | 负向验证（本工程专属）                           | 见下                                                                                                             | ✅  |

**门槛 8 的载体与实测**：本工程的失效形态是"拆除/改权限作用在内核表 ⇒ 用户 PTE 不被改动"。Ring 3 侧的"访问即 SIGSEGV"依赖**用户态触发方**（现无任何用户程序调用 `mmap`/`munmap`/`mprotect`，须新增用户程序 + 接线，属独立工作面），故按 §12.3 简约路径改为**等价且更强**的载体：在 kernel 自测内**临时恢复修复前形态**（`unmap_vma_pages` → 全局 `vmm.unmap_page`；`mprotect` → 全局 `vmm.protect_page`），实测两条用例精确变红：

```
[154/512] mm::vma_teardown::remove_range_targets_user_table...FAIL: 用户 PTE 必须被拆除
[155/512] mm::vma_teardown::mprotect_targets_user_table...FAIL: 权限位应被改写
  RESULT: 510 passed, 2 FAILED, 0 skipped      (QEMU exit 35)
```

恢复修复形态后复跑：`ALL 512 TESTS PASSED`、全仓 `TEMP-NEG-VERIFY` 标记零残留。⇒ 两条用例如实具备"缺陷存在即变红"的判别力；"用户 PTE 不在 ⇒ 用户访问必然经缺页路径，而 `remove_range` 同时删除了 VMA 描述符 ⇒ 缺页路径走 no-VMA 分支返回 SIGSEGV"这一环由既有 host-test [demand\_paging\_test.rs](../../host-tests/tests/demand_paging_test.rs) 的 PfResult/no-VMA 语义覆盖。

## 6. 登记项（只报不动）

1. **重复实现**：[mmap.rs:244-261](../../src/kernel/functions/mm/mmap.rs#L244-L261) 的 `mprotect_syscall` 与 `functions/mm/mprotect.rs` 重复，且前者零调用者（属 `eliminate-parallel-implementations.md` 范畴）。
2. **全局单表变体的用途界定**：`map_page` / `unmap_page` / `protect_page` 三个固定 `KERNEL_PML4`（aarch64 固定 `kernel_l0`）的变体是否仍有合法用途（内核自身映射），或应整体废弃——本工程只改调用方，**不删原语**。
3. **`MmStruct`** **无 cr3 字段**的取舍：本工程按裁定 1 不改结构；若后续出现"非当前进程 mm 的拆除"需求（如 `process_vm_writev` 类），需重新评估是否把 cr3 纳入 mm 生命周期。
4. **`VmSpace`** **整体零调用者**：是否接线到生产路径（`allocate_user_space` / `destroy_user_page_table` 同为零调用者，见 `cr3-lifetime-ownership.md` §6）属独立议题。
5. **aarch64 大页/section 拆除**：`protect_page` 已处理 L1/L2 块映射，但拆除侧的块映射路径与帧计数面的关系未核查（与 x86\_64 的大页 leaf 不计数问题同源）。
6. **pcache** **`ref_count`** **增减不对称（本工程新增发现，只报不动；原描述已订正；已由** **[`pcache-frame-ownership.md`](pcache-frame-ownership.md)** **承接并修复）**：原描述「`pcache::deref` 绕过帧持有计数」**不成立**——`pmm.free_page` 内部即走 `frame_counts_release`（[pmm.rs:959-970](../../src/kernel/privileged/mm/pmm.rs#L959-L970)），不存在绕过；`munmap` 顺序（先 `release_file_pages` 后 `remove_range`）的帧计数亦为 3→2→1 递减，不会把仍被映射的帧归还进 free list。真实缺陷是 **pcache 条目** **`ref_count`** **的增减与「该 VMA 是否真的持有该缓存页」不对齐**：① 登记点不对称——`+1` 只在 `pcache_get`（miss 插入 / 显式 get），而 [page\_fault.rs:315](../../src/kernel/privileged/mm/page_fault.rs#L315) 的**命中**路径用 `pcache_lookup`（不 +1）却照样建映射 + `frame_inc`；② 注销点不对称——[mmap.rs:219-239](../../src/kernel/functions/mm/mmap.rs#L219-L239) 的 `release_file_pages` 按**地址区间**逐页**无条件** `pcache_put`（−1），不判该页是否曾被该 VMA 取用。后果：条目提前驱逐（缓存数据与 `dirty` 标记丢失、`MAP_SHARED` 一致性破裂）、未持有该页的 VMA 拆除时注销他人条目、`MAP_SHARED` 换叶不注销旧帧 ⇒ 帧泄漏。与 `cr3-lifetime-ownership.md` §8.1 帧持有契约同族，属**文件映射路径**，不属本工程改动范围。
7. **`user_driver.rs`** **重复拆除（本工程新增发现，只报不动）**：[user\_driver.rs:160-170](../../src/kernel/privileged/egdf/user_driver.rs#L160-L170) 与 [user\_driver.rs:335-345](../../src/kernel/privileged/egdf/user_driver.rs#L335-L345) 在调用 `remove_range` 之前/之后另有显式 `unmap_page_in_table(cr3, …)` 逐页循环，与 `remove_range` 的逐页拆除**重复**（属 `eliminate-parallel-implementations.md` 范畴）。

