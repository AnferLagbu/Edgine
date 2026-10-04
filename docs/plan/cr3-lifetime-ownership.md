# cr3（用户地址空间）所有权计数与生命周期修复工程

> **定位**：修复 `Process::cr3` 背后那条用户页表（PML4）**没有使用者计数**所导致的四类缺陷——二次销毁、失败回退别名共享、execve 转移未清源、退出路径无人回收。
>
> **来源**：B 档 `tlb-shootdown-epoch.md` §3 S-6「destroy 路径处置」的裁定过程中，原"方案 A（destroy 路径保持现状即安全）"被核实**证伪**；用户裁定改走 **D 方向**：给 cr3 引入使用者计数，把该缺陷作为真实缺陷立项修复。
>
> **关系**：本文件与 B 档**并列、不重叠**。B 档只改 TLB 失效协议与延迟释放排空；本文件只改 cr3 的所有权记账。两者同在 privileged 子树，**不可并行施工**（同 privileged 改动并行会导致 QEMU 问题难以归因）。
>
> **关联**：`docs/explain/explain-framekernel.md`（I2/I4 不变式）；`docs/plan/smp-ipi-protocol.md`；`docs/plan/tlb-shootdown-epoch.md`。

## 1. 立项依据

### 1.1 根因一句话

`Process.cr3`（`AtomicU64`，[process.rs:145](../../src/kernel/privileged/proc/process.rs#L145)）是一个**裸物理地址**：分配它的人不增计数，共享它的人不建引用，转移它的人不清源，销毁它的人不查占用。页表使用者集合从未被表达。

### 1.2 四类缺陷（均已逐行核实）

| # | 缺陷 | 静态链路 | 现状是否可达 |
|---|---|---|---|
| **G1** | **CLONE_VM 双所有权** | [clone.rs:181](../../src/kernel/privileged/syscall/clone.rs#L181) 把 `parent_cr3` 直接写进子 `Process.cr3`，不新建表、不计数；父与子各持同一个值 | **可达**（`alloc_process` 走 `Process::new`，`ref_count = 1` ⇒ 两个 `Process::drop` 都会销毁同一 PML4） |
| **G2** | **fork 失败回退别名** | [proc_ops.rs:812-821](../../src/kernel/privileged/proc/proc_ops.rs#L812-L821) 原为 `clone_user_page_table_cow(parent_cr3).unwrap_or(parent_cr3)`：COW 克隆失败时**静默共享**父页表（该回退已由 D-2 移除，改为失败即回滚） | **可达**（仅 OOM 时；届时父子同样双销毁，且 COW 语义静默降级） |
| **G3** | **execve 转移未清源** | [user_proc.rs:962](../../src/kernel/privileged/proc/user_proc.rs#L962) `store_cr3(new_cr3)` 把新表移交给当前进程；临时进程对象仍留在 `PROCESS_TABLE`，其 `cr3` 未清空；随后 [proc_ops.rs:636](../../src/kernel/privileged/proc/proc_ops.rs#L636) `remove_and_free` → `Process::drop` → 销毁**当前进程正在使用**的页表 | **当前被 §2.5 的意外保护抑制**（见下）；一旦 `ref_count` 修复即变为在用地址空间被立即销毁 |
| **G4** | **退出路径销毁归属不明** | `process_exit` 处 [proc_ops.rs:395](../../src/kernel/privileged/proc/proc_ops.rs#L395) `destroy_by_pid_no_kstack` 在 `SCHEDULER.exit` **之前**调用，此时状态尚非 Zombie ⇒ `destroy()` 因 `is_exited()` 早退；reap 处 [scheduler.rs:1176](../../src/kernel/privileged/proc/scheduler.rs#L1176) `remove_and_free` 因 `ref_count = 0` 而不 free（§2.5） | **未核实**（需独立复核：`USER_PROC_MANAGER.create` 创建的进程退出后其 cr3 由谁销毁） |

### 1.3 与 B 档 S-6 的关系

B 档 S-6 要回答的是"`destroy_page_table` 的帧该不该走 deferred-free"。D 方向给出的事实是：**该问题的前提（destroy 时机正确）本身不成立**——页表可能在仍被使用时被销毁，也可能永不销毁。故：

- B 档 S-6 收敛为"**destroy 路径保持现状不动**"，把缺口交给本文件；
- 本文件不触碰 TLB 协议与延迟释放队列，只补所有权记账。

## 2. 源码调研结论（cr3 使用者全集）

### 2.1 创建点（谁分配 PML4）

| # | 位置 | 说明 |
|---|---|---|
| C1 | [user_proc.rs:985](../../src/kernel/privileged/proc/user_proc.rs#L985) `raw::create_user_page_table()` | `USER_PROC_MANAGER::create`——新用户进程（含 execve 的临时进程） |
| C2 | [proc_ops.rs:814](../../src/kernel/privileged/proc/proc_ops.rs#L814) `clone_user_page_table_cow(parent_cr3)` | fork：新建整棵页表树（PML4/PDPT/PD/PT 均为新帧，[cow.rs:60-232](../../src/kernel/privileged/mm/cow.rs#L60-L232)），只有**用户数据页**被共享并计数 |
| C3 | [clone.rs:181](../../src/kernel/privileged/syscall/clone.rs#L181) `child.cr3.store(parent_cr3)` | CLONE_VM：**不创建**，直接别名 |
| C4 | [process.rs:415](../../src/kernel/privileged/proc/process.rs#L415) `allocate_user_space` | **零调用者**（预存死代码，F9 面，见 §6） |

### 2.2 转移点

| # | 位置 | 说明 |
|---|---|---|
| T1 | [user_proc.rs:962](../../src/kernel/privileged/proc/user_proc.rs#L962) | execve：新表移交给当前进程，**源未清空**（G3） |
| T2 | [user_proc.rs:2064](../../src/kernel/privileged/proc/user_proc.rs#L2064) | `user_proc_clone`：镜像写入同值（非所有权转移） |
| T3 | [proc_ops.rs:755](../../src/kernel/privileged/proc/proc_ops.rs#L755) | `proc_save_user_regs`：`ProcessContext.cr3` 快照（非所有权） |
| T4 | [thread.rs:264](../../src/kernel/privileged/proc/thread.rs#L264) | `Thread::create_thread` 写 `Thread.cr3`；该函数与 `Thread::new` 在 functions 层**零调用者**（仅定义 + tests） |

### 2.3 销毁点（`vmm_destroy_page_table` 全部调用）

| # | 位置 | 说明 |
|---|---|---|
| X1 | [process.rs:562](../../src/kernel/privileged/proc/process.rs#L562) `Drop for Process` | 无"mm 不驻留任何核"契约（B 档 §6 已登记） |
| X2 | [user_proc.rs:845](../../src/kernel/privileged/proc/user_proc.rs#L845) `USER_PROC_MANAGER::destroy` | 要求 `is_exited()` |
| X3 | [user_proc.rs:958](../../src/kernel/privileged/proc/user_proc.rs#L958) `replace_user_space`（`old_cr3`） | execve 旧空间 |
| X4 | [user_proc.rs:999](../../src/kernel/privileged/proc/user_proc.rs#L999) / [:1054](../../src/kernel/privileged/proc/user_proc.rs#L1054) | `create` 失败回滚 |
| X5 | [proc_ops.rs:107](../../src/kernel/privileged/proc/proc_ops.rs#L107) / [user_proc.rs:371](../../src/kernel/privileged/proc/user_proc.rs#L371) | 包装层，**零调用者**（预存死代码，F9 面，见 §6） |

### 2.4 计数缺口

- 现有的**唯一**手写帧计数面是 COW 专用的 [COW_REFS](../../src/kernel/privileged/mm/cow.rs#L28)（`IrqSpinLock<Option<BTreeMap<u64,u32>>>`），且：
  - 它只覆盖**用户数据页**（`destroy_page_table` 只在 leaf PTE 上 `cow_dec_ref`，[vmm_x86_64.rs:1229](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1229)）；
  - **不覆盖页表结构帧与 PML4 本身**（[vmm_x86_64.rs:1236-1248](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1236-L1248) 对 PT/PD/PDPT/PML4 一律无条件 `defer_free`）。
- PMM 无 per-frame 引用计数元数据（`pmm.rs` 全文件 `ref_count|refcount|RefCount` 零命中）。
- 存量 TCB 脚手架 [frame.rs](../../src/kernel/privileged/frame.rs#L30)（`Frame`，带 `ref_count` + `meta`，注释明言"等价于 OSTD 的 `Frame<M>`"）与 [vmspace.rs](../../src/kernel/privileged/vmspace.rs#L33)（`VmSpace`）/ [frame_alloc.rs](../../src/kernel/privileged/alloc/frame_alloc.rs#L24) 已存在，但**未接入生产路径**（[functions/proc/table.rs:16](../../src/kernel/functions/proc/table.rs#L16) 明记"进程创建/析构——留待 Phase 2.5.3（依赖 ELF 加载与 `VmSpace`）"；生产路径走裸 `cr3` + PMM）。

### 2.5 意外保护与次生问题（决定了 G3 当前的严重度）

- **P1（已核实）**：[alloc_kernel_process](../../src/kernel/privileged/proc/user_proc.rs#L585-L589) 用 `alloc_zeroed` 分配，[init_kernel_process_fields](../../src/kernel/privileged/proc/user_proc.rs#L655-L721) **未写 `ref_count`** ⇒ 经 `USER_PROC_MANAGER.create` 创建的 `Process` 其 `ref_count = 0`（`Process::new` 的 [process.rs:364](../../src/kernel/privileged/proc/process.rs#L364) `AtomicU32::new(1)` 不生效）。
- **P2（已核实）**：[remove_and_free](../../src/kernel/privileged/proc/process.rs#L709) 的 `dec_ref()` 在 0 上回绕（`fetch_sub` → `u32::MAX`），`prev = MAX-1 ≠ 0` ⇒ **`Box::from_raw` 不执行、`Process::drop` 不运行、`free_pid` 不执行**。
- **推论**：G3 的销毁被 P2 **意外抑制**，故 execve 静态链路"必崩"却未见崩溃；代价是这类 `Process` 对象与 PID **永不回收**（每进程泄漏 `size_of::<Process>()` 字节 + 1 个 PID 槽位）。
- **该推论标记为「未核实」**：静态链路逐行成立，但**未做运行时复现**。施工前须独立复核（否则 G3 的处置方向可能错）。
- **次序约束**：G1/G2/G3 的所有权修复**必须先于 P1**。若先修 P1 而 G3 未修，`Process::drop` 立即生效 ⇒ 在用地址空间被销毁的 UAF 由潜在变为现实。

### 2.6 Asterinas 0.18.1 的 fork/COW 共享机制（已逐行核实）

> 本节核实结论**推翻**了此前"帧是唯一所有权、无共享计数"的论断——那是 Edgine `frame.rs` 的形态，不是 Asterinas 的。

**Asterinas 的帧本身就是"pfn 索引槽 + 引用计数"的多所有者共享句柄**：

- `MetaSlot`（[meta.rs:107](../../other/asterinas-0.18.1/ostd/src/mm/frame/meta.rs#L107)）持有 `ref_count: AtomicU64`，按 paddr 索引（`get_from_in_use(paddr)`，[:265](../../other/asterinas-0.18.1/ostd/src/mm/frame/meta.rs#L265)）。
- `impl Clone for Frame<M>` → `self.slot().inc_ref_count()`，克隆即共享同一 phys（[mod.rs:237-247](../../other/asterinas-0.18.1/ostd/src/mm/frame/mod.rs#L237-L247)）。
- `impl Drop for Frame<M>` → `ref_count.fetch_sub(1)`；**归零才** `drop_last_in_place()` + `dealloc` 给全局帧分配器（[mod.rs:249-264](../../other/asterinas-0.18.1/ostd/src/mm/frame/mod.rs#L249-L264)）。
- 哨兵值 `REF_COUNT_UNIQUE`（单所有者快路径，避免原子竞争）/ `REF_COUNT_UNUSED`（未分配）。
- `UniqueFrame`（unique.rs）是单所有者变体；`FrameRef`（frame_ref.rs）是**借用**（非共享计数）。

**fork/COW 的共享点**（[vmar_impls/fork.rs:18-71](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/fork.rs#L18-L71)）：

1. `cow_copy_pt` 遍历父页表，命中 `MappedRam { frame, prop }` 时 `let frame = (*frame).clone();`（[:100](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/fork.rs#L100)）⇒ **引用计数 +1，父子映射同一物理帧**；
2. 父 PTE 去 `W`（`src.protect_next(..., *flags -= PageFlags::W)`，[:102](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/fork.rs#L102)），子 PTE 以同帧同属性 `dst.map(frame, prop)`（[:106](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/fork.rs#L106)）；
3. 最后一个 mapping 结束后做一次**全量 TLB flush**（`issue_tlb_flush(TlbFlushOp::for_all())` + `dispatch` + `sync`，[:60-64](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/fork.rs#L60-L64)）。

**页表页自身也是 `Frame`**：`activate_page_table(self.clone().into_raw())`（[page_table/node/mod.rs:105](../../other/asterinas-0.18.1/ostd/src/mm/page_table/node/mod.rs#L105)）——页表页"激活于多个核"时用 `clone` 计数，与 TLB 激活集合同构。

**整表共享的边界**：`PageTable::shallow_copy`（[page_table/mod.rs:454-461](../../other/asterinas-0.18.1/ostd/src/mm/page_table/mod.rs#L454-L461)）确实以 `root: self.root.clone()` 共享整棵页表，但注释明确限定"**仅对 IOMMU 页表有用，其他场景三思**" ⇒ **进程间共享不走这条**（正解见下：走 `Arc<Vmar>`）。

**`CLONE_VM` 的承载：`Arc<Vmar>` 句柄，而非共享页表帧**（已逐行核实）：

- 共享单位是 **`VmarHandle(Arc<Vmar>)`**（[handle.rs:13](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/handle.rs#L13)）；`Vmar` 内含 `vm_space: Arc<VmSpace>`（[vmar_impls/mod.rs:47](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/mod.rs#L47)），自身以 `Arc::new_cyclic` 创建并另持 `num_handles: AtomicUsize`（[:53](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/mod.rs#L53)/[:71](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/mod.rs#L71)）。
- `clone_vmar`（[clone.rs:672-680](../../other/asterinas-0.18.1/kernel/core/src/process/clone.rs#L672-L680)）：`CLONE_VM` ⇒ `parent_vmar.clone_handle()`（`Arc::clone` + `inc_num_handles`）；否则 ⇒ `Vmar::fork_from`（COW）。**两条路径完全不同**。
- `VmarHandle::drop` ⇒ `dec_num_handles()`；注释明言"**最后一个句柄 drop 时，全部映射与页表才被清除**"（[handle.rs:10-12](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/handle.rs#L10-L12)/[:23-27](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/handle.rs#L23-L27)）。
- `has_multiple_handles()` = `num_handles > 1`（[vmar_impls/mod.rs:97-99](../../other/asterinas-0.18.1/kernel/core/src/vm/vmar/vmar_impls/mod.rs#L97-L99)），被 `unshare` 用作前置校验：已共享的 VMAR 不允许 `CLONE_VM`（[unshare.rs:79-83](../../other/asterinas-0.18.1/kernel/core/src/syscall/unshare.rs#L79-L83)）。

⇒ **两层共享各司其职，且互相正交**：

| 层 | 机制 | 承载什么 |
|---|---|---|
| 地址空间对象 | `Arc<Vmar>` + `num_handles` | **`CLONE_VM`**（多进程共享同一 mm：VMA 集合 + 页表根 + mm 级状态） |
| 物理帧 | `Frame` 的 pfn-indexed `ref_count` | **COW**（多地址空间共享同一数据页）+ 页表页的**多核激活** |

⇒ **PML4/页表根帧本身仍只有唯一持有者**（那个 `Arc<VmSpace>`）。这修正了 §3.1 的推论：**D2+（帧计数）只能承载 COW，不足以承载 `CLONE_VM`**——G1 的本质是"**缺少地址空间对象**"，帧计数只能使其"不双销毁"，无法表达"两个进程共享 VMA 集合与 mm 级状态"。

### 2.7 `sys_fork` 对 `MmStruct` 的实际处置（已逐行核实）——地址空间对象缺失

**结论：Edgine 不存在"属于某个进程的地址空间对象"。**

- **`sys_fork` 完全不处置 `MmStruct`**（[proc_ops.rs:786-880](../../src/kernel/privileged/proc/proc_ops.rs#L786-L880)）：全函数无 `MmStruct`/`vmas`/`set_current_mm` 的任何操作；处理项只有 cr3（COW 克隆，[:806-808](../../src/kernel/privileged/proc/proc_ops.rs#L806-L808)）、rlimit、session/pgid/canary、seccomp、namespace、cgroup、kstack、context ⇒ **子进程继承父的 mm**。
- **载体是全局单例指针**：`static CURRENT_MM: AtomicPtr<MmStruct>`（[vma.rs:1271-1272](../../src/kernel/privileged/mm/vma.rs#L1271-L1272)），非 per-CPU、非进程字段。唯一写入点是 ELF 装载末尾 `vma_set_current_mm(mm)`（[elf/mod.rs:297](../../src/kernel/privileged/proc/elf/mod.rs#L297)）。
- **注释失实**：[vma.rs:1279](../../src/kernel/privileged/mm/vma.rs#L1279) 称"CURRENT_MM 是当前 CPU 的 per-CPU 状态指针"，实为**单个** `AtomicPtr`，非按 CPU 索引的数组。
- **生产路径无创建点**：全仓 `MmStruct::new()` 仅命中 [tests/test_mm.rs:145/364](../../src/kernel/privileged/tests/test_mm.rs#L145)、[tests/test_new_features.rs:361](../../src/kernel/privileged/tests/test_new_features.rs#L361)；所有生产 `&MmStruct` 均为参数（[elf/mod.rs:116/150](../../src/kernel/privileged/proc/elf/mod.rs#L116)、[mmap.rs:77/171](../../src/kernel/functions/mm/mmap.rs#L77)、[remap.rs:30](../../src/kernel/functions/mm/mremap.rs#L30)、[user_driver.rs:115](../../src/kernel/privileged/egdf/user_driver.rs#L115)）或经 `vma_get_current_mm()` 取得。
- **所有消费者都经全局入口取 mm**：`mmap`（[mmap.rs:314/341](../../src/kernel/functions/mm/mmap.rs#L314)）、`mprotect`（[mprotect.rs:76](../../src/kernel/functions/mm/mprotect.rs#L76)）、`brk`（[brk.rs:27/38](../../src/kernel/functions/syscall/brk.rs#L27)）、`numa`（[numa.rs:461](../../src/kernel/privileged/mm/numa.rs#L461)）、`uffd`（[uffd.rs:376](../../src/kernel/privileged/mm/uffd.rs#L376)）、`madvise_mlock`（[madvise_mlock.rs:72-216](../../src/kernel/privileged/proc/madvise_mlock.rs#L72)）、`coredump`（[coredump.rs:346](../../src/kernel/privileged/proc/coredump.rs#L346)）⇒ **不存在"进程 → mm"的归属边**。
- **`Process` 结构体无 mm 字段（已逐字段核实）**：`Process` 字段全集为 [process.rs:139-296](../../src/kernel/privileged/proc/process.rs#L139-L296)，无 `MmStruct`/`mm`/`mm_ptr` 任何形态的字段；`privileged/proc/` 全目录对 `MmStruct|mm_ptr|current_mm` 的 grep 仅命中原子的 `mm::` **模块路径**，无字段级命中。⇒ §2.7 结论（"不存在进程 → mm 归属边"）**反面验证通过**，D-γ 成立。

**推导出的缺陷（比 G1 更广）**：

- **D-α 多进程串台**：`CURRENT_MM` 在每次 ELF 装载时被**覆盖**。第二个进程 exec 后，第一个进程的 `mmap`/`brk`/`mprotect`/`madvise`/`coredump` 会操作**后来者的 mm**。单进程单核测试不可见。
- **D-β fork 后父子共享 VMA 集合**：fork 不复制 ⇒ 子进程的 `mmap`/`brk` 直接改变父进程的 VMA 视图，`start_brk`/`brk`/`mmap_base`/`locked_vm`/`mlock_all_flags` 亦共享。
- **D-γ `CLONE_VM` 无从表达**：页表可共享（`child.cr3 = parent_cr3`），但"共享同一 mm"**没有载体**——因为 mm 本就不是进程字段。
- [vma.rs:280](../../src/kernel/privileged/mm/vma.rs#L280) 注释"`MmStruct` 在 fork 中不复制"**不是有意设计，而是没有载体可复制**。

⇒ **修正工程定位**：G1（`CLONE_VM` 双所有权）只是表层；根因是**"地址空间对象缺失"**。故 **D3（引入地址空间对象 `Mm`，引用计数持有）不是后续工程，而是本工程的核心**；D2+（帧表 / pfn 持有计数）退为其**前置**——负责 COW 的数据页保鲜。

## 3. 路线对比（待用户裁定）

> 属 §12.3「架构关键路径」⇒ 必须由用户显式授权后再施工。

| 维度 | **D1 窄：cr3 专用计数面** | **D2 中：统一帧计数面** | **D3 大：下沉到 Frame/VmSpace** |
|---|---|---|---|
| 做法 | 新增 `CR3_REFS: IrqSpinLock<BTreeMap<u64,u32>>`，仅服务 cr3 | 把 `COW_REFS` 泛化为统一物理帧计数面，cr3 与页表结构帧一并纳入 | 激活 `frame.rs`/`vmspace.rs`/`frame_alloc.rs` 脚手架，`cr3` 变为 `VmSpace` 句柄 |
| 改动面 | 3 个接入函数 + 6 个销毁点 + 4 个创建/转移点 | D1 + `cow.rs` 全部 21 个调用点 + `destroy_page_table` 页表结构帧路径 | D2 + PMM 元数据 + `alloc/free` 全部 53 处调用点 + functions 层 API 形态 |
| 行数估算 | ~150–250 | ~350–500 | ~1000+ |
| 形态评价 | 与 `COW_REFS` 形成**第二张平行表**（形态劣），但可用"接入点唯一化 + 禁止直写 `Process.cr3`"约束约束住 | 消除平行表，与 Asterinas「帧引用计数」同构 | 最终形态最优，是 (c) 路线的完整切片 |
| 主要风险 | 双表并存的长期维护成本 | 重构**没有坏掉**的 COW 路径（§12.2 边界） | 触及 TCB 架构与全部内存分配路径，回归面覆盖整个 mm |
| 与 §2.3 五门槛契合 | 高（可加 host-test 直接测计数语义） | 中（COW 回归需新测试） | 低（需全量内存回归 + 长时间 QEMU） |
| 与 B 档关系 | 互不干扰，可先 B 后 D | 同上 | 同上，但 D3 会改变 B 档的帧表示前提 |

### 3.1 推荐（已于 2026-09-19 修正）

**原推荐 D1 已撤回**。原理由为"§12.2 不重构没坏的部分 ⇒ `COW_REFS` 工作正常不应迁移"，该前提被用户驳回：**当整条链路是病态时，"没坏"只是局部的**。核实证据如下（均已逐行核实）：

- `COW_REFS` 的"没坏"是**局部**的：它只覆盖用户数据页，其覆盖边界（不计页表结构帧与 PML4，[vmm_x86_64.rs:1236-1248](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1236-L1248)）**正是 G1–G3 无保护的直接原因**。
- `Process.ref_count` 本应承担"何时销毁页表"，但对 `alloc_kernel_process` 路径恒为 0（§2.5 P1）⇒ 该职责**从未生效**。
- `Frame`/`FrameAlloc` 并非空脚手架——`BuddyFrameAlloc` **已接线真 PMM**（[frame_alloc.rs:52-99](../../src/kernel/privileged/alloc/frame_alloc.rs#L52-L99)），但**Edgine 的** `Frame` 不变式写死为"**同一 phys 至多一个实例**"（[frame.rs:26-27](../../src/kernel/privileged/frame.rs#L26-L27)），**无 `Clone`**，其 `ref_count` 语义是"**被映射次数**"（[:88](../../src/kernel/privileged/frame.rs#L88)）而非"**持有者数**" ⇒ 结构上**无法表达**"两个进程共享同一 PML4"。（对照：Asterinas 的 `Frame` 恰好相反——`Clone` 即持有计数 +1，见 §2.6。）
- 生产页表路径（`cr3` 裸 u64、`vmm_create_user_page_table`、`destroy_page_table`）**完全绕过以上三者**，直接走裸 phys + PMM（PMM 无 per-frame 计数）。

⇒ 现状是**三条各自不完整的所有权机制并存，且生产路径一条都不用**。D1 会造出**第四条**并行机制，加重病态，故撤回。

**修正后推荐（含 §2.6 二次修正）：D2+ 为第一段，D3 才是目标形态**

§2.6 核实表明 `CLONE_VM` 的承载是**地址空间对象**（Asterinas 用 `Arc<Vmar>`），与帧计数**正交**。故 D2+ 单独**不构成**长期最优，只能视为 D3 的必要前置：

- **第一段 · D2+**（帧表 / pfn 索引持有计数）：承载 COW 与页表页多核激活；消灭 `COW_REFS` 与 `Frame.ref_count` 两份重复。
- **第二段 · D3**（地址空间对象 `Mm`，引用计数持有）：承载 `CLONE_VM`——G1 的本质是"缺少地址空间对象"，帧计数只能使其"不双销毁"，无法表达"两个进程共享 VMA 集合与 mm 级状态"。

下表"D2+"一列的形态仍适用，但须并入上述两段式理解：

| 项 | 内容 |
|---|---|
| 做法 | 在 PMM 建立 **phys-indexed 所有权计数**（复用既有 `buddy_meta`/bitmap 的元数据载体模式，[pmm.rs:146-202](../../src/kernel/privileged/mm/pmm.rs#L146-L202)）；`alloc_page` 置 1、`free_page` 改为"dec，归零才归还"；新增 `frame_inc(phys)` / `frame_dec(phys) -> bool` 供共享方使用；`cr3` 的所有权由该计数表达 |
| 消灭的重复 | `COW_REFS` 删除并由其吸收；`Frame.ref_count` 改为查询 PMM 计数（消除第二份计数） |
| 签名不变 | `alloc_page`/`free_page`/`alloc_pages`/`free_pages` **签名与语义入口不变** ⇒ §2 统计的 53 处调用点**零改动** |
| `Process.cr3` | **保持 `AtomicU64`**（上下文切换路径要求 Copy 值；类型化承载属 D3） |
| 行数估算 | ~300–450（PMM 元数据 + 三处计数接入 + `cow.rs` 委托改写 + 删除 `COW_REFS`） |
| 与 D2/D3 关系 | 吸收 D2 的目标（单一计数面）而不必先造旁表；D3（`Frame` 演化为可共享句柄、`Process.mm` 类型化）成为其上的独立后续工程 |

### 3.2 大页（连续多帧）计数语义（裁定 2，已定）

**裁定依据（三项均已核实）**：

1. **实际调用面**：Edgine 的连续多帧入口是 `pmm_alloc_pages(count)` / `pmm_free_pages(addr, count)`（[api.rs:113-150](../../src/kernel/privileged/mm/api.rs#L113-L150)）。生产调用者全集为：
   - [memory_allocator.rs:61/99/111](../../src/kernel/memory_allocator.rs#L61)（Rust 全局分配器后端）
   - [slab.rs:632/661](../../src/kernel/privileged/mm/slab.rs#L632)（slab 页）
   - [iobuf.rs:65/117](../../src/kernel/privileged/iobuf.rs#L65)（IO 缓冲）
   - [dynamic.rs:139/170](../../src/kernel/privileged/ipc/dynamic.rs#L139)、[shm.rs:49/161](../../src/kernel/functions/ipc/shm.rs#L49)（共享内存区）
   - [brk.rs:51](../../src/kernel/functions/syscall/brk.rs#L51)（brk 扩展）
   ⇒ **全部为独占用途，无一方需要"部分共享同一连续块"**。
2. **需计数的帧只有两类**：用户数据页（COW 共享）与页表结构帧（PML4/PDPT/PD/PT，CLONE_VM 共享）。两者都经单页 `alloc_page`/`free_page` 产生与销毁，与连续多帧入口**不相交**。
3. **其他内核的处理**：
   - **Asterinas**：逐页计数。`Segment`（连续帧句柄）在 `Drop`/`Clone`/`slice` 中一律 `step_by(PAGE_SIZE)` 逐页 inc/dec（[segment.rs:48-63](../../other/asterinas-0.18.1/ostd/src/mm/frame/segment.rs#L48-L63)/[:177-182](../../other/asterinas-0.18.1/ostd/src/mm/frame/segment.rs#L177-L182)），不做 head 归一。其可行前提是元数据本就是**每页一槽**的 `MetaSlot` 数组（[meta.rs:107](../../other/asterinas-0.18.1/ostd/src/mm/frame/meta.rs#L107)）。
   - **Linux**：compound page —— `order > 0` 分配只维护 **head page** 的 `_refcount`，tail page 以 `PageTail` 归一到 head，`get_page`/`put_page` 一律作用于 head。这是为"整块不可分割"语义（hugetlb 等）服务的，代价是 tail 页无独立计数。
4. **Edgine 的元数据现实**：`buddy_meta` 已存在且是**按页 1 字节**，但该字节已被 buddy 阶数语义占用（`0xFF` = 已分配，`0..=MAX_BUDDY_ORDER` = 空闲块头阶数，[pmm.rs:177-180](../../src/kernel/privileged/mm/pmm.rs#L177-L180)），**无法复用为计数槽** ⇒ 计数面须独立按 pfn 索引的数组。

**裁定语义（本轮采用）**：

- 计数面**只覆盖单页帧**（`alloc_page`/`free_page` 产生的 `order == 0` 帧）；`alloc_page` 置 1，`free_page` 改为 dec-归零才归还。
- **连续多帧块（`alloc_pages(count > 1)`）视为单一 holder**：块内不逐页计数，块首的计数代表整块；块整体归还时一次性 dec。
- `frame_inc` / `frame_dec` **只接受单页帧**：传入连续块的非首帧按契约违反处理（文档契约 + `debug_assert`）。
- 共享方（COW 用户页、`CLONE_VM` 的页表帧）经 `frame_inc` 表达持有；**不存在"共享连续块"的调用点**（依据第 1 项核实结论）。

**与 Asterinas 的收敛路径**：差异仅在"连续块是否逐页计数"，而 Edgine 当前无该需求。若未来出现大页共享（如透明大页拆分共享），把语义扩展为逐页即可，**契约层（`frame_inc`/`frame_dec` 入口）不变**，属局部扩展。

**与 buddy 合并/分裂的交互**：因连续块不逐页计数，buddy 在块内合并/分裂时**不触碰计数**（块首计数的存在即保证该块在 `free_pages` 前不会被 buddy 视为完全空闲并合并走）——这与 Linux 的 head 计数在语义上一致，差别只是 Edgine 不做 tail 归一。

## 4. 施工条目（已裁定，按序开工）

- **D-1. cr3 计数面（架构无关）**
  - 描述：cr3（PML4 物理地址）需要"使用者计数"，且计数归零才是唯一销毁依据。
  - 方案：新增帧持有计数面（形态依 §3 裁定），提供 `frame_inc(phys) -> bool` / `frame_dec(phys) -> bool`（true = 归零可销毁）两个语义入口；"转移"语义为"不计数只移动"（由调用方清空源，见 D-4）。
  - 状态：[X]
  - 详情：**命名对齐（本轮实施后回写）**：入口按 §3.1 表格命名为 `frame_inc` / `frame_dec`（PMM 的方法），**不新建 `Cr3Refs` 类型**——计数面本就是"pfn 索引的帧持有计数"，cr3 只是其中一个使用者，包装成 cr3 专用类型会与 §3.1 表格"`Frame.ref_count` 改为查询 PMM 计数"的收敛方向相反。另提供 `frame_ref_count(phys) -> u8` 只读观测入口（仅供断言/审计）。
  - 详情：**落地形态（源码）**：`MetaStore` trait 新增 `setup_counts` / `counts_read` / `counts_write` 三方法（生产载体 `RawMetaStore` 增 `counts: Option<NonNull<u8>>`，host 载体 `VecMetaStore` 增 `counts: RefCell<Option<Vec<u8>>>`）；`init_bitmap` 在 FREE_LINKS 之后追加**按页 1 字节**的 FRAME_COUNTS 区并 `set_bit` 标记，klog 输出 `[PMM] FRAME_COUNTS: N B at 0x… (N pages)`。**不复用 `buddy_meta`**（其字节已被 buddy 阶数占用，§3.2 第 4 项）。
  - 详情：**语义分工（三入口有意不同，非冗余）**：① `frame_inc` 拒绝 0/`u8::MAX`（0 = 未计数帧，`u8::MAX` = 饱和防护）；② `frame_dec` 在计数 0 上 **fail-closed 返回 `false`**（未计数帧不报告归零 ⇒ 调用方不得据此销毁，同时防同一 PML4 被二次销毁）；③ `frame_counts_release`（`free_page` 专用私有入口）对 `cur <= 1` 均**写 0 并返回 `true`**——因 `free_page` 须保持"未计数帧（连续块内页 / reserved 页）直接归还"的既有语义，若在此处也 fail-closed 将造成存量归还路径回归。
  - 详情：**计数面初始化次序**：FRAME_COUNTS 区在 `buddy_init_free_lists` **之前**完成布局与 `set_bit`，与 PMM 初始化同序；载体未就绪时读写走 `meta_store().map_or(...)` / `if let Some(...)` 静默降级（读 0 = 未计数），不 panic。

- **D-2. 创建/转移点接入**
  - 描述：C1/C2 须 `acquire`；C3（CLONE_VM）须 `acquire`（子共享父表）；T1（execve）须"转移"并**清空源**；C2 的 `unwrap_or(parent_cr3)` 回退须删除。
  - 方案：C1/C2/C3 各自加计数；[proc_ops.rs:812](../../src/kernel/privileged/proc/proc_ops.rs#L812) 的 `.unwrap_or(parent_cr3)` 改为失败即回滚返回 0（ENOMEM），消除静默别名。
  - 状态：[X]
  - 详情：CLONE_VM 的 `child.cr3` 与 `child_ctx.cr3`（[clone.rs:234](../../src/kernel/privileged/syscall/clone.rs#L234)）两处写同值，属同一所有权，计数只能一次——接入点须选在 `child.cr3.store` 处，`ProcessContext.cr3` 是快照不计数。
  - 详情：**C1/C2 无需显式接入（零改动）**：两条路径的 cr3 均由 `vmm_create_user_page_table` → `alloc_page` 产生，计数已由 `alloc_page` 置 1（D-1），"创建者即唯一初始持有者"天然成立 ⇒ §3.1 表格"53 处调用点零改动"的口径在本轮得到保持。
  - 详情：**C3（CLONE_VM）接入点（[clone.rs:188](../../src/kernel/privileged/syscall/clone.rs#L188)）**：`child.cr3.store(parent_cr3)` **之后**紧接 `frame_inc(PhysAddr(parent_cr3))`；`frame_inc` 返回 `false`（父表不处于计数态 = 契约违反）时 `klog_error!` 记录、把 `child.cr3` 回退置 0 并 `drop_boxed_process(child_ptr)` 后返回 `ENOMEM`——即"共享登记失败"必须整体回滚，不得留下持有裸 cr3 却未计数的子进程。
  - 详情：**G2 修复落地（[proc_ops.rs:812-821](../../src/kernel/privileged/proc/proc_ops.rs#L812-L821)）**：`clone_user_page_table_cow(parent_cr3).unwrap_or(parent_cr3)` 改为 `let Some(child_cr3) = … else { … }`（clippy `manual_let_else` 形态）：COW 克隆失败时 `raw::drop_boxed_process(child_ptr)` + `PROCESS_TABLE.free_pid(child_pid)` 后 `return 0`。**此刻子进程尚无内核栈**（`allocate_kernel_stack` 在其后），故可直接释放描述符；回退 pid 是补项——否则失败 fork 会泄漏 pid 位图位。原静默别名（失败时子进程"继承"父 cr3 而不计数）消除。本修复的**判别载体** = `Proc::fork_cow_failure_rolls_back`（§5.1 门槛 7 / §5.2 注入 2），并配套把 `cow.rs` 克隆改为两阶段（§6.1）。
  - 详情：**克隆中途失败的页表帧泄漏（本轮一并修复，[cow.rs](../../src/kernel/privileged/mm/cow.rs)）**：原实现 4 处 `pmm.alloc_page()?` 失败即返回 `None`，**已建子树页表帧不归还**（每次失败泄漏已建的 PML4/PDPT/PD 帧）。改为 `let … else { free_child_page_table_tree(child_pml4_phys); return None; }`——依据两阶段改造（§6.1），此刻父页表未被改动、未登记任何持有者，故只需释放已建结构帧。

- **D-3. 销毁点接入**
  - 描述：X1–X4 须统一改为 `if frame_dec(cr3) { vmm_destroy_page_table(cr3) }`；X5 两处零调用者包装按 §6 处置。
  - 状态：[X]
  - 详情：**X1（[process.rs:617-635](../../src/kernel/privileged/proc/process.rs#L617-L635) `Process::drop`）**：`cr3 != 0` 且 `frame_dec(cr3)` 为真才 `unsafe { vmm_destroy_page_table(cr3) }`。这是 G1 的**正解**——`CLONE_VM` 双所有者下先退出的一方 `frame_dec` 得 `false`（仍有持有者），共享表存活；同时 `frame_dec` 对未计数帧返回 `false` 覆盖了"同一 PML4 被二次销毁"。
  - 详情：**X2（[user_proc.rs:843-851](../../src/kernel/privileged/proc/user_proc.rs#L843-L851) `USER_PROC_MANAGER::destroy`）与 X3（[user_proc.rs:950-969](../../src/kernel/privileged/proc/user_proc.rs#L950-L969) `replace_user_space` 旧用户栈释放后）**：同形闸门。X2 与 X1 作用于同一 cr3 时，先到者取走唯一一次"归零"，后到者 `frame_dec` 返回 `false` ⇒ **恰好销毁一次**。
  - 详情：**X4（[user_proc.rs:1010-1014](../../src/kernel/privileged/proc/user_proc.rs#L1010-L1014) 用户栈分配失败 / [user_proc.rs:1068-1072](../../src/kernel/privileged/proc/user_proc.rs#L1068-L1072) 内核栈分配失败）**：两处创建回滚路径原为无条件 `destroy_user_page_table(cr3_val)`，现加同一 `frame_dec` 闸门，避免与 `Process::drop` 形成二次销毁。
  - 详情：**X5（两处零调用者包装）处置：仅计调用者变化，不删符号**。`user_proc::raw::destroy_user_page_table`（[user_proc.rs:369](../../src/kernel/privileged/proc/user_proc.rs#L369)）本轮由 X2/X3/X4 **从零调用者变为 4 个调用点**（855 / 972 / 1018 / 1076），其"零调用者"登记项随之闭合；`proc_ops::raw::destroy_user_page_table`（[proc_ops.rs:105](../../src/kernel/privileged/proc/proc_ops.rs#L105)）仍零调用者，但它是 `mechanism.rs:25` 的**顶层 re-export 出口**（F2 边界面），删之会破坏 re-export 契约 ⇒ 按 §6 保留登记、不删。

- **D-4. execve 转移收口**
  - 描述：G3——`replace_user_space` 移交后源未清空。
  - 方案：`replace_user_space` 内对源进程 `store_cr3(0)`（或改由调用方在 `detach_by_pid` 前清空），使临时进程的 `drop` 不再持有该表。
  - 状态：[X]
  - 详情：**落地位置取"改由调用方清空"（[proc_ops.rs:636-642](../../src/kernel/privileged/proc/proc_ops.rs#L636-L642)，`proc_exec_replace` 阶段 4）**：`PROCESS_TABLE.with_process(new_pid_u32, |p| p.cr3.store(0, SeqCst))` 置于 `detach_by_pid` + `remove_and_free` **之前**。依据：G3 的语义是"所有权随 cr3 **移动**"（execve 不产生新地址空间，只是把临时进程刚建的表交给当前进程），移动 = **不调用 inc/dec**，故必须在源被释放前把源指针清空；否则临时进程的 `Process::drop` 会对**当前进程正在使用**的 PML4 执行 `frame_dec` → 计数 1→0 → 销毁在用页表（UAF）。

- **D-5. `ref_count` 初始化（P1）【需用户确认是否纳入本轮】**
  - 描述：`alloc_kernel_process` 分配出的 `Process` `ref_count = 0`，使 `remove_and_free` 永不释放（P2）。
  - 方案：`init_kernel_process_fields` 显式初始化 `ref_count = 1`。
  - 状态：[X]
  - 详情：**次序约束**——必须在 D-2/D-3/D-4 落地**之后**（§2.5）。纳入本轮会使 cr3 销毁路径首次真正生效，属行为面变更，需用户确认；不纳入则本工程只消除"错误销毁"，不清除"永不销毁"的泄漏。**用户裁定纳入本轮（§7 裁定 3）**，实施次序符合约束：D-2/D-3/D-4 先落地，D-5 后落地。
  - 详情：**落地（[user_proc.rs:682-686](../../src/kernel/privileged/proc/user_proc.rs#L682-L686)）**：`init_kernel_process_fields` 内 `ptr::write(&mut (*kproc_ptr).ref_count, AtomicU32::new(1))`，注明该路径经 `alloc_zeroed` 分配（不走 `Process::new`），不写此字段则 `ref_count` 恒 0、`dec_ref()` 在 0 上回绕到 `u32::MAX` ⇒ `Box::from_raw` 不执行、`Process::drop` 不运行、PID 不回收（P2）。
  - 详情：**行为面实测后果（正向，非回归）**：D-5 生效后 `make test-smp-multicore` 日志**首次**出现 `[VMM] destroy_page_table: cr3=0x6355000 deferred_in_call=26` 与 `[VMM] deferred-free admitted_total=26 released_total=26 pending=false`——即 B 档 S-10 登记的"帧释放路径零覆盖"被实际打通，销毁路径由"永不运行"变为"运行且帧确有归还"。B 档对应条目的闭合依据即本项（见 [tlb-shootdown-epoch.md](./tlb-shootdown-epoch.md) S-10 / D4 详情）。

- **D-6. 文档回写**
  - 描述：B 档 S-6/D8/§6 需指向本文件；本文件的裁定结果需回写 B 档。
  - 状态：[X]
  - 详情：① **本文件回写**：D-1~D-7 状态与详情、命名对齐（`cr3_acquire`/`cr3_release` → `frame_inc`/`frame_dec`，见 D-1 详情）、§5.1 门槛 6/7 的载体落点、§5.2 注入 1 的实测与注入 2 的缺口登记、§6 登记项更新、SIMPLIFIED 记录（`alloc_pages` 的 `order == 0` 分支）。② **B 档回写**（[tlb-shootdown-epoch.md](./tlb-shootdown-epoch.md)）：S-6（`destroy_page_table` 路径移交）、S-10（闭合依据指向 D-5）、§6 的 `Process::drop` 缺契约项（指向 D-3）、D8（移交说明）。

- **D-7. 验收门槛与 fail-closed 复验**
  - 描述：本工程为 TCB 所有权变更，须由**非实施者**独立复跑。
  - 方案：§5 全部条目；注入式反向验证见 §5.2。
  - 状态：[X]
  - 详情：**门槛 1–7 复跑结果（主 agent 串行执行，取脚本自身退出码，非终端截断）**：① `./ci/build.sh all` **EXIT=0**（双架构 0 error/0 warning；`Passed: 5  Failed: 0`；host tests 全通过）；② `./ci/audit.sh quick` **EXIT=0**（functions 零 unsafe/I1–I6/SAFETY 缺漏 0/F7 注释 0 违规/双架构 check/clippy pedantic lib + `kernel_test`/`host-test` 双 feature 维，六段全绿）；③ `make test-host` **EXIT=0**，各套件均 `test result: ok`、**0 failed**（含 `pmm_buddy_host_test` 10 例，其中 6 例为本轮新增计数语义）；④ `make test-unit` **EXIT=0**，`Registered 506 test cases` / `RESULT: ALL 506 TESTS PASSED (0 skipped)`，本轮 3 例位于序号 **225/226/227** 且 PASS；⑤ `./scripts/qemu_boot_test.sh x86_64` **EXIT=0**（里程碑 `VFS ready`，1/1 通过）；⑥ `make test-smp-multicore`（2/3/4 核）**EXIT=0**，各核日志序列一致（`admitted=27 released=0 pending=true` → `admitted=27 released=27 pending=false` → `admitted=39 released=27 pending=true`）且 `SMP TEST PASSED`；⑦ 门槛 7 的 host 维 `e04_shared_runner_test`（原红，见下）**367 passed / 12 skipped、0 failed**。
  - 详情：**过程中修复的门槛红（本轮）**：新增 3 例位于 `tests/test_proc.rs`，而 [mod.rs:48](../../src/kernel/privileged/tests/mod.rs#L48) 的 `pub mod test_proc;` **无 cfg 门控** ⇒ 亦被 `feature = "host-test"` 编译。host 维下 `create_user_page_table()` 真正被调用 → `get_vmm()` 未初始化 panic（`once_lock.rs "[VMM] accessed before initialization"`）且该 panic **不可 unwind** ⇒ `SIGABRT`，`./ci/build.sh all` 报 `error: test failed, to rerun pass --test e04_shared_runner_test`（`Passed: 4  Failed: 1`）。**修复**：按同套件既有 E-04 先例（`test_mm.rs:502-506` 的 `SKIP: E-04: host 无 PMM 初始化, 跳过 (依赖裸机页表分配)`），为三例各加 `#[cfg(feature = "host-test")]` Skip 占位 + `#[cfg(not(feature = "host-test"))]` 真实实现。修后复跑绿（见门槛 ⑦）。
  - 详情：**fail-closed 复验**见 §5.2：注入 1 **有判别力且已复验**（注入态 `make test-unit` FAIL / 还原态全绿，`git diff` 复核无残留——本轮复核 `grep -rn "INJECTION|TEMP-INJECT|TEMP-DBG"` 于 `mm/`/`proc/`/`syscall/` 子树返回空）；**注入 2 已建立载体并复验**（注入态 `508 passed / 1 FAILED`，唯一失败即载体用例；还原态 `509/509`，实测见 §5.2）。
  - 详情：**登记项 1/2 载体补齐后的门槛复跑（主 agent 串行，取脚本自身退出码）**：① `./ci/build.sh all` **EXIT=0**；② `./ci/audit.sh quick` **EXIT=0**（`grep -c "✗"` 为 0，clippy lib + `kernel_test` 维 + `host-test` 维均 passed）；③ `make test-host` **EXIT=0**，且**负向验证** `CARGO_TARGET_DIR=/proc/nonexistent_target_dir make test-host` 得 **EXIT=2**（旧 `; true` 形态恒为 0）⇒ fail-open 确已闭合；④ `make test-unit` **EXIT=0**，`Registered 509 test cases` / `RESULT: ALL 509 TESTS PASSED (0 skipped)`，新用例序号 **231/509** `Proc::fork_cow_failure_rolls_back` 且 PASS；⑤ `./scripts/qemu_boot_test.sh x86_64` **EXIT=0**（里程碑 `VFS ready`）；⑥ `make test-smp-multicore`（2/3/4 核）**EXIT=0**，`SMP MULTICORE TEST PASSED: 核数 2 3 4 全部通过`，含 `deferred-free admitted_total=12 released_total=12 pending=false`；⑦ 注入 2 复验见下条与 §5.2。
  - 详情：**注入 2 复验（fail-closed，本轮）**：临时恢复 `.unwrap_or(parent_cr3)` ⇒ `make test-unit` 报 `508 passed, 1 FAILED`，**唯一失败为本轮新增载体**（失败行 `FAIL: 克隆失败时 sys_fork 必须返回 0 (不得静默共享父 cr3)`）⇒ 判别力成立；完全还原后复跑 `509/509` 全绿，`grep -rn "TEMP-INJECT"` 于 `src/` 无残留（仅余说明注入语义的注释文本）。`Makefile.ci` 同源负向验证：`make -f Makefile.ci ci-test-host` 配坏 target dir 得 **EXIT=2**。

## 5. 验证门槛

### 5.1 门槛（§2.3 五条底线 + 本工程附加）

1. `./ci/build.sh all`：双架构 0 error / 0 warning。
2. `./ci/audit.sh quick`：核心审计全部通过（F1/F2/F4/F7/F9）。
3. `make test-host`：**fail-open 已闭合**（原末尾 `; true` 使测试失败也返回 0）。配方改为"日志落盘 → 取 `cargo test` 自身 `status` → `cat` 日志 → `exit $status`"（[Makefile:424](../../Makefile#L424)）；[Makefile.ci](../../Makefile.ci) 的 `ci-test-host` 同源修复（原 `| tee` 无 pipefail）。负向验证见 D-7 详情。
4. `make test-unit`（**该配方自身仍不传播 QEMU 退出码，属独立 fail-open，见 §6**：判读须结合串口日志的 `RESULT: ALL N TESTS PASSED` 行）。
5. `scripts/qemu_boot_test.sh x86_64`。
6. **附加**：新增计数语义单元测试（`frame_inc`/`frame_dec` 配对、归零恰好一次、转移不改变计数）。
   - **载体（host 维）**：[pmm_buddy_host_test.rs](../../host-tests/tests/pmm_buddy_host_test.rs) 新增 6 例（`pmm_frame_count_alloc_starts_at_one` / `pmm_frame_inc_dec_paired` / `pmm_frame_zero_reported_exactly_once` / `pmm_frame_release_only_when_zero` / `pmm_frame_transfer_keeps_single_owner` / `pmm_frame_uncounted_block_rejected`）。该文件宿主于 `VecMetaStore`（host 计数载体），故 host 维可完整覆盖计数语义。
7. **附加**：CLONE_VM / fork / exit 三条路径的回归测试（须覆盖"共享者仍在运行时所有者退出"）。
   - **载体（内核维）**：[test_proc.rs](../../src/kernel/privileged/tests/test_proc.rs) 新增 3 例（`cr3_shared_owner_exit_keeps_table` / `cr3_single_owner_exit_zeroes_once` / `cr3_transfer_source_cleared`），以 `user_proc::raw::create_user_page_table` 取真实 PML4、并借 `raw::{alloc_process, process_ref_mut, drop_boxed_process}` 复现 CLONE_VM 双持有与 execve 转移两种形态。
   - **载体裁定依据**：三例依赖裸机页表分配（`create_user_page_table` → `get_vmm()`），host 维无 VMM/PMM 初始化且该 panic 不可 unwind（会 SIGABRT 打断整个 `run_all`）。故按既有 E-04 先例，三例在 `feature = "host-test"` 下以 `TestResult::Skip("E-04: host 无 VMM/PMM 初始化, 跳过 (依赖裸机页表分配)")` 占位、仅 `kernel_test` 维执行原实现——**不得**声称 host 维已覆盖门槛 7。
   - **载体（内核维，注入 2 回归）**：[test_proc.rs](../../src/kernel/privileged/tests/test_proc.rs) `fork_cow_failure_rolls_back`（`Proc` 组）——`sys_fork` 在 COW 克隆失败时必须返回 0、不得为数据帧登记第二持有者、已建子树页表帧必须全部归还。以 `pmm.arm_alloc_failure(0..3)` 做 4 轮迭代，逐一命中克隆内部 PML4 → PDPT → PD → PT 四个分配点。实现与注册同为 `#[cfg(all(feature = "kernel_test", not(feature = "host-test")))]` 门控（注入面仅在该配置编译），故不设 host 维 Skip 占位。

> 门槛 1–7 全部由主 agent 串行复跑；子智能体不得执行 `cargo`/`make`/`ci`/`qemu`。

### 5.2 fail-closed 注入复验（必做）

- **注入 1**：使 `frame_inc` 空转（不计数，即让 CLONE_VM 共享登记失效），确认门槛 7 中"共享者仍运行"用例**必须 FAIL**；验后完全还原。
  - **实测（有判别力）**：注入后 `make test-unit` 报 `❌ TESTS FAILED (QEMU exit: 35)`，`505 passed / 1 FAILED`，失败行为 `FAIL: two holders expected after CLONE_VM-style registration`（即 225 号用例 `Proc::cr3_shared_owner_exit_keeps_table`）⇒ 判别力成立。还原后复跑全绿。该轮总用例数为 505（加入本轮注入 2 载体后为 509）；本项未随本轮复跑，判别力结论不变。
- **注入 2**：恢复 `unwrap_or(parent_cr3)` 回退，确认门槛 7 的 COW 失败用例**必须 FAIL**；验后完全还原。
  - **载体**：注入面 = PMM 内 `#[cfg(any(test, feature = "kernel_test"))]` 门控的**确定性失败开关**（[pmm.rs:906-932](../../src/kernel/privileged/mm/pmm.rs#L906-L932) `arm_alloc_failure(skip)` / `disarm_alloc_failure`，生产二进制不含该路径；**未接 `freg/fault_inject`**，简化依据见 §6.1）。该开关只拦 `alloc_page`，故 `sys_fork` 内子进程描述符分配（走 `alloc_pages`/slab）不消耗注入计数，4 轮 `skip=0..3` 精确命中克隆的四个页表帧分配点。回归用例 = [test_proc.rs:392-486](../../src/kernel/privileged/tests/test_proc.rs#L392-L486) `Proc::fork_cow_failure_rolls_back`。
  - **实测（有判别力且已复验）**：注入态（临时恢复 `unwrap_or(parent_cr3)`）→ `make test-unit` 报 `508 passed, 1 FAILED`，**唯一失败即本用例**，失败行 `FAIL: 克隆失败时 sys_fork 必须返回 0 (不得静默共享父 cr3)`；还原态 → `Registered 509 test cases` / `ALL 509 TESTS PASSED (0 skipped)`；`grep -rn "TEMP-INJECT"` 于 `src/` 返回空（其余命中仅为说明注入语义的注释文本）。依据：**载体未建立前不得标注为通过**；本轮载体建立并复验后登记为通过（§6 对应登记项已闭合）。
- 两项注入的还原状态须以 `git diff` 复核，禁止残留。

## 6. 登记项（§12.5，只报不动）

- `allocate_user_space`（[process.rs:474](../../src/kernel/privileged/proc/process.rs#L474)）**零调用者**：F9 面死代码（本轮核实仍为唯一调用点为空）。
- `proc_ops::raw::destroy_user_page_table`（[proc_ops.rs:105-108](../../src/kernel/privileged/proc/proc_ops.rs#L105-L108)）**零调用者**：F9 面死代码，但作为 `mechanism.rs:25` 顶层 re-export 出口保留（见 D-3 详情 X5）。
- ~~`user_proc::destroy_user_page_table` 零调用者~~：**本轮闭合**——X2/X3/X4 使其获得 4 个调用点（见 D-3 详情 X5）。
- `Thread::create_thread`（[thread.rs:240](../../src/kernel/privileged/proc/thread.rs#L240)）与 `Thread::cr3` 生产路径**零使用者**。
- 存量 TCB 脚手架 `frame.rs` / `vmspace.rs` / `frame_alloc.rs` **未接入生产路径**（`usermode.rs::enter_user_mode` 的调用面未核实）。**本轮更新（D2+ 段已落地）**：`Frame.ref_count` / `COW_REFS` 的"两份重复计数"中，**`COW_REFS` 与计数重复已消除**——`COW_REFS` 整表（含 `frame_key` / `cow_init` / 三个 wrapper）删除，帧持有计数收敛为 PMM 单一计数面（`frame_inc` / `frame_dec` / `frame_ref_count`，契约见 §8.1）；`Frame.ref_count` 按 §8 裁定 ③ **本轮不动**，**转 D3 前置**（依据：`Frame` 无生产使用者，且 host 无 PMM、`order > 0` 帧未计数，强行改造须加降级分支）。
- **~~`vmspace.rs::protect()` 在新 dec 语义下的隐患~~（已由 `munmap-protect-pml4.md` 承接）**：[vmspace.rs:156-171](../../src/kernel/privileged/vmspace.rs#L156-L171) 以"unmap → 同帧 remap"实现改权限。D-9④ 落地后 `unmap` 会 `frame_dec`（归零即 `defer_free`），故接入生产路径将出现"在用帧被注销并延迟释放后又被 remap"的 UAF 形态。当前该文件为零调用者脚手架（见上条），**未接入即无现网影响**；接入前须改为"就地改 PTE 权限"（同 `vmm::protect_page` 形态）。**承接结果**：该形态已消除——改为 `protect_page_in_table(self.pt_root, …)` 就地改权限（`munmap-protect-pml4.md` D-4），判别力由该文件 D-6 用例 `mm::vma_teardown::mprotect_targets_user_table` 承担（修复前形态实测变红）。
- **~~`munmap` 不拆除 PTE、不归还数据帧~~（已由 `munmap-protect-pml4.md` 承接，描述已订正）**：[mmap.rs](../../src/kernel/functions/mm/mmap.rs) 的 `munmap_syscall` 仅 `remove_range` + `release_file_pages`（`pcache_put`），**不调用 `unmap_page_in_table`** ⇒ 数据帧的"拆除侧 dec"在 `munmap` 路径上不发生；当前仅 `destroy_page_table`（进程销毁）承担该侧注销。属既有缺口，与 D-9 的规则 3 无关（未拆除即无 dec，不产生双重注销）。**承接结果**：调研订正——拆除调用链**已存在**（`remove_range` → `unmap_vma_pages` → `vmm.unmap_page`），真实缺陷是**目标表错位**（落到 `KERNEL_PML4`，用户 PTE 悬留且帧不归还）；该工程已改为 cr3 显式变体（D-2/D-3），判别力由 `mm::vma_teardown::remove_range_targets_user_table` 承担。
- **`user_proc_load_elf` 失败回滚路径的调用顺序（本轮核实，只报不动）**：[proc_ops.rs:546](../../src/kernel/privileged/proc/proc_ops.rs#L546) 先 `remove_and_free`（触发 `Process::drop` ⇒ 销毁页表），再于 [proc_ops.rs:550](../../src/kernel/privileged/proc/proc_ops.rs#L550) 调 `destroy_by_pid`（其内 `virt_to_phys` 作用于**已销毁**的页表）。D-5 生效后该路径顺序的行为面首次真正被执行；因 `destroy_page_table` **不清空 PTE 条目**（仅遍历 defer 释放帧），行为与 D-5 之前等价 ⇒ **非本轮引入的缺陷**，属既有脆弱点（依赖"销毁后 PTE 未被覆写"这一未成文假设）。
- P1/P2 的完整影响面（其他 `alloc_zeroed` 路径是否同样漏初始化 Atomic 字段）——**未核实**。
- ~~**注入 2 的载体缺口**~~：**本轮闭合**——注入面取"PMM 内 `#[cfg(any(test, feature = "kernel_test"))]` 门控的确定性失败开关"（`arm_alloc_failure` / `disarm_alloc_failure`），**未接 `freg/fault_inject`**；回归载体 [test_proc.rs](../../src/kernel/privileged/tests/test_proc.rs) `Proc::fork_cow_failure_rolls_back` 已建立，并完成注入态 FAIL / 还原态全绿的双向复验（实测见 §5.2）。
- ~~`Makefile` `test-host` 末尾 `; true`（fail-open，D-9-4）~~：**本轮闭合**，含同源项 [Makefile.ci](../../Makefile.ci) 的 `ci-test-host`（原 `| tee` 无 pipefail）。两处配方均改为"日志落盘 → 取 `cargo test` 自身 `status` → `cat` 日志 → `exit $status`"；负向验证（故意使 `cargo` 失败：坏 `CARGO_TARGET_DIR`）均得 `EXIT=2`，修复前形态恒为 0。**未采用 `set -o pipefail`**：`SHELL` 未设置 ⇒ make 默认 `/bin/sh`（dash）不支持该选项。
- **`make test-unit` 自身不传播 QEMU 退出码（本轮新发现，只报不动）**：[Makefile:449-483](../../Makefile#L449-L483) 的 `test-unit` recipe 以 `if/elif … echo` 仅打印 `✅/❌`，**末尾命令是 `tail`/`if`** ⇒ 测试失败（`exit_code=35`）时 make 仍返回 0。与上一条同源但**独立**，且与 cr3 工程无关，故本轮仅登记不修；判读须结合串口日志的 `RESULT: ALL N TESTS PASSED` 行。
- **`make test-unit` 打印空时间戳路径（本轮新发现，只报不动）**：[Makefile:479](../../Makefile#L479) 的 `@echo "  Report: tests/reports/unit_test_$${timestamp}.log"` 为独立 recipe 行（每行独立 shell），`timestamp` 在该行未定义 ⇒ 输出 `unit_test_.log`。
- 扩展项（后续独立机制工程）：D2 统一帧计数面；D3 `Frame`/`VmSpace` 下沉；PMM per-frame 元数据。

### 6.1 本轮 SIMPLIFIED 简化点（逐条可追溯）

- [pmm.rs:1053-1055](../../src/kernel/privileged/mm/pmm.rs#L1053-L1055)（`alloc_pages`）：**连续多帧块的块首计数未实装**——影响面为 `order > 0` 的块内页计数恒 0、其归还/共享不经计数面（与既有语义一致）；需扩展时机为出现大页共享时按逐页计数展开，契约入口 `frame_inc`/`frame_dec` 不变。依据 §3.2 裁定 2。
- [vmm_x86_64.rs:2180-2182](../../src/kernel/privileged/mm/vmm_x86_64.rs#L2180-L2182)（`release_lock` 埋点）：只统计"释放帧总数"聚合量，不区分批次/来源；需扩展时机为排查需定位滞留来源时改分路径计数。
- **拆除侧的大页 leaf 不参与帧计数（D-9 段，契约推论非实现简化）**：`unmap_page_in_table`（[vmm_x86_64.rs:1253-1270](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1253-L1270) 的 1GB/2MB 分支）与 `destroy_page_table`（仅遍历 4KB USER leaf）对 huge leaf **不 `frame_dec`**。影响面：大页映射的拆除不递减计数（与"大页未计数"对称，不产生"帧永不归零"的泄漏）；需扩展时机为大页共享场景落地时按 §3.2 裁定 2 逐页展开——依据同上条 `alloc_pages` 简化点，仅计数侧对称，本处以契约推论记录、不再重复置代码标记。
- **分配失败注入实现为 PMM 内的确定性进程内开关（登记项 1 载体段）**：[pmm.rs:901-932](../../src/kernel/privileged/mm/pmm.rs#L901-L932) 的 `arm_alloc_failure(skip)` / `disarm_alloc_failure` / `alloc_fail_should_fail`，以 `#[cfg(any(test, feature = "kernel_test"))]` 门控使生产二进制不含该路径；**未接入 `freg/fault_inject`**（其 `maybe_inject_fault` 仅被 `freg/recoverable.rs` 消费，且 `fault_injection` feature 仅在 [Makefile:222](../../Makefile#L222) 的 chaos 构建面启用）。影响面：只能构造 `alloc_page` 侧失败，`alloc_pages`/slab 侧失败不可注入——对本判别目标充分（克隆中途四个失败点全部走 `alloc_page`）；需扩展时机为需要在 chaos/生产构建下做可配置故障注入时，改走 `freg` 域并与 chaos 构建面合并。
- **`cow.rs` 克隆两阶段化（登记项 1 载体段，契约推论非实现简化）**：[cow.rs:60-232](../../src/kernel/privileged/mm/cow.rs#L60-L232) 把"分配 + 改父页表 + 登记持有者"重排为"**阶段 1 只建结构**（页表帧分配 + leaf PTE 原值复制，不改父页表、不 `frame_inc`）/ **阶段 2 只改 PTE 与计数**（清 WRITABLE + `frame_inc`，无分配）"，使阶段 2 不可能失败 ⇒ 失败回滚只需 [free_child_page_table_tree](../../src/kernel/privileged/mm/cow.rs#L348) 释放已建**页表结构帧**。影响面：回滚面**不含 leaf 数据帧**（阶段 1 既未改父页表也未 `frame_inc`，无数据帧需回滚）；需扩展时机为阶段 2 若引入可失败操作（如惰性叶子复制）时，须补"已 `frame_inc` 的计数回退 + PTE 内容回退"。

## 7. 裁定结果（用户已裁定）

| # | 议题 | 裁定 |
|---|---|---|
| 1 | §3 路线 | **采纳两段式**：D2+（帧持有计数，前置）→ D3（地址空间对象 `Mm`，目标形态）。D1 维持撤回 |
| 2 | 大页计数语义 | **按实际调用面收敛**：只对单页帧计数；连续多帧块视为单一 holder。依据与对照见 §3.2 |
| 3 | D-5（`ref_count` 初始化） | **纳入本轮**，但受 §2.5 次序约束——必须在 D-2/D-3/D-4 落地**之后** |
| 4 | 串并行与次序 | **按序推进**：B 档 → D2+ → D3（约束不变：同在 privileged 子树，不可并行） |

### 7.1 规划修正（本轮核实，推翻此前文档论断）

**修正项：D2+ 不吸收 B 档 S-4，两者正交。** 此前 §7.4（已删）推断"D2+ 落地后，S-4 的 pending 表与代判据会被 `dec 归零即归还` 吸收"，**该推断错误**。核实依据：

- defer 的动机是"**TLB 追平先于释放**"，非"所有权未清"：[release_lock:2013-2019](../../src/kernel/privileged/mm/vmm_x86_64.rs#L2013-L2019) 的注释与结构（等 shootdown 完成后才 `free_page`，[L2027-2032](../../src/kernel/privileged/mm/vmm_x86_64.rs#L2027-L2032)）。
- `ref_count == 0`（无持有者）与"TLB 已追平"（无核仍缓存旧映射）是**两个独立充分必要条件**，必须 **AND**：一个未被任何进程共享的页（`ref_count` 1→0）在远端核 TLB 仍缓存其映射时若立即归还 PMM，该帧可被重分配 → 远端经旧映射访问 → UAF。
- ⇒ S-4 在 D2+ 落地后**代码形态不变**（`free_page` 内部自动做 dec-归零判断），仅"归还"的最终语义收紧。**B 档先行不产生返工**。

### 7.2 次序依据（补充核实事实）

- HEAD **不含** `defer_free`/`DEFERRED_FREE`（`git show HEAD:...vmm_x86_64.rs` 对 `defer_free|DEFERRED_FREE` 零命中）⇒ B 档 §2.2 的 L1/L2/L3 **全部由 A 档未提交改动引入**，非 HEAD 既有漏洞。
- ⇒ B 档前置的收益：A 档骨架已在工作区（[tlb-shootdown-epoch.md](./tlb-shootdown-epoch.md) §2.1），改动面集中且能就地收口活跃缺陷；且因 §7.1 的正交性，B 档不因 D2+ 返工。
- D-5 置于 D-2/D-3/D-4 之后的**原因不变**（§2.5）：若先修 P1 而 G1/G2/G3 未修，`Process::drop` 立即生效 ⇒ 在用地址空间被销毁的 UAF 由潜在变为现实。

## 8. D2+ 剩余段：COW_REFS 消除与帧计数改造（用户已裁定）

> **裁定（§12.3 前置询问，用户已选）**：① **统一持有者数语义**——不采用"机械等价移植"（后者会用 PMM 计数表达旧"额外注册数"以保持行为不变，代价是保留单次 fork 后隔离失效与"所有者先退出即 free"的 UAF）；② **完整「映射即持有者」模型**；③ **`Frame.ref_count` 本轮不动**，登记为 D3 前置（Frame 无生产使用者，且 host 无 PMM、`order > 0` 帧未计数，强行改造须加降级分支）。

### 8.1 计数契约（本轮确立，取代 COW_REFS 的局部记账）

**计数 = 该帧的引用数**，四条规则：

1. `alloc_page` 的 +1 代表"创建者把该帧交付给**紧随其后的第一个映射**" ⇒ 各缺页路径、`vmm_map_user_page`、`load_elf`、`map_kpage` 的首次映射**不额外 `frame_inc`**。
2. 任何在此之后**新增的引用**必须 `frame_inc`：fork 的 COW 共享（每 leaf PTE 一次，D-2 已落地）、**页缓存帧被映射**（缓存自身已持有一份持有者）。
3. 对应拆除必须 `frame_dec`，**归零才** `defer_free`：`destroy_page_table`（每 USER leaf）、`unmap_page_in_table`（每 USER leaf）、**映射被替换**的路径（MAP_PRIVATE 文件 COW、`cow_handle_fault` 复制分支）。
4. **豁免面（不计不拆）**：非 USER 映射（KPTI 的 GDT/IDT/TSS 为 `PRESENT|WRITABLE`、内核高半区）、设备/MMIO 映射（`CACHE_DISABLE` / `WRITE_THROUGH`，物理地址不由 PMM 分配）、连续多帧块内页（§3.2 裁定 2）。`frame_inc`/`frame_dec`/`frame_ref_count` 须增 **pfn 越界 fail-closed**（MMIO 物理地址的 pfn 远超 `total_pages`，现状会越界读写计数区）。

**`should_reuse` 判据**：`frame_ref_count(phys) <= 1` ⇒ 本映射是该帧唯一引用 ⇒ 就地清 WRITABLE；`>= 2` ⇒ 必须复制 + `frame_dec` 旧帧。旧 `COW_REFS` 的 `or_insert(0) += 1` 是"额外注册数"，与 `alloc_page` 置 1 差 1，故机械移植会保留"单次 fork 后父子写同一页"的隔离缺陷。

### 8.2 施工条目

- **D-8. `cow.rs` 去表接入**
  - 描述：删除 `COW_REFS`（`IrqSpinLock<Option<BTreeMap<u64,u32>>>`）与 `frame_key`，计数委托 PMM 面。
  - 方案：`cow_inc_ref` → `frame_inc`；`cow_dec_ref` → `frame_dec`；`cow_ref_count` → `frame_ref_count`（`u32` → `u8`）；`cow_init` 删除（无状态）并从 [vmm_x86_64.rs](../../src/kernel/privileged/mm/vmm_x86_64.rs#L2308) 与 [mm/mod.rs:149](../../src/kernel/privileged/mm/mod.rs#L149) 的 re-export 移除；`should_reuse` 改按 §8.1 判据；复制分支的 `free_page` 改 `defer_free`（帧可能仍被他人映射且远端核 TLB 未追平，见 B 档 §7.1）。
  - 状态：[X]
  - 详情：已按方案落地。① 删除项：`static COW_REFS`、`frame_key()`、`cow_init()`、三个 wrapper、`use alloc::collections::BTreeMap`、单元测试 `test_frame_key_alignment`，以及 [mm/mod.rs](../../src/kernel/privileged/mm/mod.rs) 的 4 个 re-export。② 判据改为 `frame_ref_count <= 1`（持有者语义，非旧"额外注册数"）；③ 复制分支 `frame_dec` 归零后走 `defer_free`，并**显式 `acquire_lock()` 包裹**——`defer_free` 写 `BATCH_HEAD` 要求持 `VMM_LOCK`（单写者），而本函数不持锁（`map_page_in_table` 内部自持锁，不可嵌套）；④ **aarch64 分支**：`defer_free` 是 x86_64 专有（TLB 代协议仅覆盖 x86_64），故以 `#[cfg(target_arch = "x86_64")]` / `#[cfg(target_arch = "aarch64")]` 双分支保持 aarch64 既有"立即 `free_page`"语义，避免破坏 F5 双架构编译。
- **D-9. 映射/拆除侧接入**
  - 描述：按 §8.1 规则 2/3 补齐"新增引用"与"拆除"两侧。
  - 方案：① 页缓存映射处（[page_fault.rs:350](../../src/kernel/privileged/mm/page_fault.rs#L350) / [:359](../../src/kernel/privileged/mm/page_fault.rs#L359)）在**该 VA 尚未映射该帧**时 `frame_inc`（同 VA 第二次缺页会重入 `handle_file_fault`，须幂等）；② MAP_PRIVATE 文件 COW 替换映射处（[:386](../../src/kernel/privileged/mm/page_fault.rs#L386)）`frame_dec(cache_phys)`；③ `destroy_page_table` leaf 循环由 `cow_dec_ref` 改为 `frame_dec` + **USER 位过滤**（KPTI/supervisor 页不得参与）；④ `unmap_page_in_table` 对 USER leaf `frame_dec` + 归零 `defer_free`；⑤ [page_fault.rs:442-443](../../src/kernel/privileged/mm/page_fault.rs#L442-L443) 的"unmap + 显式 `free_page`"改为只 unmap（unmap 已含 dec 与归还）。
  - 状态：[X]
  - 详情：五项全部落地，**③④ 合批实施不可拆**——`destroy_page_table` 原先依赖 `cow_dec_ref` 的"仅注册页返回 true"充当隐性闸门；改为 `frame_dec` 后，KPTI 以 `PRESENT|WRITABLE`（无 USER）映射进每个用户页表的 GDT/IDT/TSS/IST 页会被计入零 ⇒ 误释放内核页，故 `pte.is_user()` 过滤是该替换的**必要同批项**（`unmap` 侧同理）。⑥ **pfn 越界 fail-closed（规则 4）**：`frame_inc`/`frame_dec`/`frame_ref_count` 三入口统一经 `frame_counts_pfn` 校验，设备/MMIO 帧（pfn 远超 `total_pages`）一律"不计数、不报告归零"。[page_fault.rs](../../src/kernel/privileged/mm/page_fault.rs) 的回滚路径改为"只 unmap"后由 unmap 统一承担 dec 与归还，消除同帧被注销两次。
  - 详情（aarch64）：`unmap_page_in_table` / `destroy_page_table` 的帧计数接入目前仅存在于 x86_64 实现；aarch64 侧 `unmap_page_in_table`/`destroy_page_table` 未做帧计数（既有语义），且其 `free_table` 直调 `free_page`。该差异不影响 F5（两架均 0 error/0 warning），归 D3 统一。
- **D-10. 测试载体与门槛**
  - 描述：统一语义是行为面变更，须有 COW 隔离与持有者配对载体。
  - 方案：host 维补 `should_reuse` 判据（`frame_ref_count<=1` 就地可写 / `>=2` 复制且旧帧计数递减）与"私有页 destroy 归零恰好一次 / 页缓存帧不被销毁误释放"用例；内核维补 fork 后子写不污染父页、以及"共享者仍在运行时所有者退出"的 COW 形态；主 agent 串行复跑 §5.1 门槛 1–7 并取脚本自身退出码。
  - 状态：[X]
  - 详情（载体）：**内核维 3 例**（[test_mm.rs](../../src/kernel/privileged/tests/test_mm.rs) 组 `mm::cow`，host 维按 E-04 先例 Skip、仅 `kernel_test` 维执行）：`child_write_isolated_from_parent`（fork 后计数 2 ⇒ `cow_handle_fault` 必须复制新帧、旧帧递减为 1、父子内容互不污染）、`shared_frame_survives_owner_exit`（销毁父页表后共享帧计数 1、内容不变、连续分配不得重发该帧；子销毁后归零）、`unique_mapping_fault_reuses_frame`（计数 1 ⇒ 就地恢复可写、返回同一帧且计数不变）。**host 维 2 例**（[pmm_buddy_host_test.rs](../../host-tests/tests/pmm_buddy_host_test.rs)）：`pmm_should_reuse_predicate_boundary`（判据取值边界 + 复制分支对旧帧的净效果）、`pmm_frame_out_of_range_pfn_rejected`（MMIO 地址三入口 fail-closed）。方案中"私有页 destroy 归零恰好一次 / 页缓存帧不被销毁误释放"两项**未另立用例**——其计数形态与既有 `pmm_frame_zero_reported_exactly_once` / `pmm_frame_release_only_when_zero` 逐条同形，重复用例不增加判别力；`should_reuse` 的完整分支行为由内核维 3 例覆盖（host 维只覆盖判据输入面）。
  - 详情（门槛复跑，主 agent 串行，取脚本自身退出码）：① `./ci/build.sh all` RC=0（`Passed: 5 Failed: 0`，双架构 0 error/0 warning）；② `./ci/audit.sh quick` RC=0（TD-22 / clippy pedantic / 双架构 check / SAFETY 覆盖 / I1-I6 全绿）；③ `make test-host` RC=0（该轮此配方仍为 fail-open 形态——末行 `; true`，已逐段核对 0 failed；本轮已修复，见 §6）；④ `make test-unit` RC=0，**508/508 通过 0 skipped**（原 505 例 + 本轮 3 例；加入注入 2 载体后为 509）；⑤ `scripts/qemu_boot_test.sh x86_64` RC=0（里程碑 `VFS ready`，进入 Ring 3）；⑥ `make test-smp-multicore` RC=0（2/3/4 核全通过，且 `deferred-free admitted_total=11 released_total=11 pending=false` 佐证 B 档延迟释放与计数归零的 AND 关系）。
  - 详情（fail-closed 注入 1 复验）：注入 `frame_inc` 空转（不计数）后 `make test-unit` 报 `❌ TESTS FAILED (QEMU exit: 35)`，`504 passed / 4 FAILED`，失败行为**本轮新增的两个判别载体**（`FAIL: fork 共享后持有者数应为 2` ×2）+ 既有两例（`shared holder registration must succeed` / `共享方登记应成功`）⇒ 判别力由 1 例提升到 4 例。还原后复跑 508/508 全绿，`grep -rn "TEMP-INJECT|INJECTION|u64::MAX"` 复核无新增残留（命中仅为既有 `SENTINEL` 常量）。~~**注入 2 仍无载体**~~：**本轮已补载体**（PMM 注入面 + `Proc::fork_cow_failure_rolls_back`）并完成双向复验，见 §5.2 与 §6。
- **D-11. 文档回写**
  - 描述：§6 登记项"`Frame.ref_count` / `COW_REFS` 两份重复计数仍未做"须收敛；B 档指针同步。
  - 状态：[X]
  - 详情：① §6 该条已收敛——`COW_REFS` 与计数重复消除、`Frame.ref_count` 转 D3 前置；② §6 新增三项登记（`vmspace.rs::protect()` 在新 dec 语义下的 UAF 形态、`munmap` 不拆 PTE/不归还数据帧的既有缺口、aarch64 计数面差异归 D3）；③ §6.1 新增"拆除侧大页 leaf 不参与计数"的可追溯条目；④ §5.1 门槛 7 与 §5.2 注入 1 的实测数据按本轮载体更新（未新增门槛条目）；⑤ B 档指针同步（均在 [tlb-shootdown-epoch.md](./tlb-shootdown-epoch.md)）：§7.4 A7-1/A7-2——"唯一现成的引用计数面是 `COW_REFS`"的前提已失效，改为"帧持有计数面已存在（PMM，按 pfn 索引）"；§7.3 对照表"帧表示"行——由"裸 `u64`（无引用计数）"改为"帧类型仍是裸 `u64`，持有计数面在 PMM 侧表"；§7.5"对 S-6 的直接意义"——原判"两条前提都缺"，其中"地址空间使用者计数"一条已由 cr3 计数闸门补齐（仅剩 per-mm 激活集合一条）；§2.2 L4 与 S-6 描述、§6 登记项内的 `Process::drop` 断言补注已被计数闸门取代（"无核驻留"契约仍缺，计数不等于无核运行）；同时修正 `Process::drop` 的失效行号锚点（原 `process.rs#L556-L566` 已随 cr3 工程漂移，实为 `#L617-L635`，全处 3 个引用已更正）。

### 8.3 D3 段：`Frame` 句柄化与 aarch64 计数/释放面统一（用户已裁定）

> **裁定（§12.3 前置询问，用户已选）**：
>
> 1. **范围**：D3a 与 D3b 两段都做，但**串行**（D3a → 门槛 1–7 复跑 → D3b → 门槛复跑），不合成一批——两段都动 privileged 子树，合批会让 QEMU 现象无法归因。
> 2. **路径**：**相对完整** —— `Frame` 演化为**可共享句柄**（`Clone` ⇒ 持有者 +1；`Drop` ⇒ 持有者 −1；**仅归零才归还**），而不是"带计数器的地址包装"。
> 3. **接线面**：**甲** —— driver DMA 缓冲（现返回裸 `(vaddr, phys, size)` 元组）改为**持有 `DmaStream`（内含 `Frame`）的 RAII 句柄**，为 `Frame` / `DmaStream` 提供真实生产调用点（就地闭合 §6 "存量 TCB 脚手架 `frame.rs` / `frame_alloc.rs` / `dma_buf.rs` 未接入生产路径"）。
> 4. **aarch64 释放时机**：**甲（广播失效即追平，归零即释放）** —— aarch64 拆除序列是 `tlbi vaae1is`（All-ASID / Inner-Shareable）+ `dsb ish` + `isb`，TLB 在该点**系统级追平**，不存在 x86_64 那种"远端核 TLB 未追平"窗口 ⇒ **不移植 `defer_free`**；约束是"释放严格晚于失效序列"，且 `destroy_page_table` 需先补一次系统级失效（该路径当前无 per-VA 失效）。
> 5. **D3b 载体**：双架构 `./ci/build.sh all` 0 error / 0 warning + `scripts/qemu_boot_test.sh aarch64` 启动到 EL0 日志（aarch64 无 ISA-debug-exit ⇒ `make test-unit` 的退出码机制不适用），登记"aarch64 计数语义无单元级判别载体"。
> 6. **D3b 施工顺序（补裁，用户已选）**：**甲 —— 先修 inc 面再落 dec 面**（分两个串行可归因段）。理由：fork 的 +1 侧判据原是 `x86_64` PTE 语义（`(flags & 2) && (flags & 4)`），在 aarch64 描述符上恒不选中用户 leaf ⇒ aarch64 从不 `frame_inc`；若直接补 dec 面，会把"父子两侧映射、计数仅 1"的帧在任一侧拆除时计入零而误释放（UAF）。故先使 +1 侧架构中性并落地，门槛复跑后再落 −1 侧。
> 7. **aarch64 COW 语义（补裁，用户已选）**：**甲 —— 同集计数、不置只读**。aarch64 无 page-fault 处理器（EL0 非 SVC 同步异常一律停机，见 [exception.rs:563-611](../../src/kernel/privileged/arch/aarch64/exception.rs#L563-L611)），置只读会让 fork 后首次写入挂死系统，故 aarch64 分支 `mark_cow_readonly` 恒返回原值（保持既有"共享写"语义），**只登记帧持有计数**。**补齐 aarch64 缺页 / COW 恢复路径是内核工程必需项（乙′），本轮不做、仅登记**（见 §8.3.3）。

#### 8.3.1 §10 调研结论（D3a/D3b 施工依据，逐行核实）

- **现有 driver DMA 分配路径**：[driver/storage/mod.rs:283](../../src/kernel/privileged/driver/storage/mod.rs#L283)（`nvme_alloc_dma_buffer`）与 [:609](../../src/kernel/privileged/driver/storage/mod.rs#L609)（`ahci_alloc_dma_buffer`）经 `get_dma().alloc_coherent(size)`（[engine.rs:95-141](../../src/kernel/privileged/dma/engine.rs#L95-L141)）→ `pmm_alloc_pages_phys(ceil(size/4K))` + 直映射 VA（`phys + KERNEL_BASE`，与 `phys_to_virt` 同式）+ 清零 + `cache_flush`（[engine.rs:430](../../src/kernel/privileged/dma/engine.rs#L430)），并登记进 `DmaEngine.mappings`；释放侧靠 `free_coherent` 以双键（`cpu_addr` + `size`）反查物理地址后 `pmm_free_pages_phys`（[:151-178](../../src/kernel/privileged/dma/engine.rs#L151-L178)）。
- **调用点全集（9 分配 / 9 释放，全在 functions；两架构均编译）**：`functions/driver/storage/mod.rs` 99/112/142；`storage/nvme.rs` 926/965、977/1023、1132/1154、1186/1203；`storage/ahci.rs` 881/900、938/955、979/995；`usb/xhci.rs` 801/821（xHCI 复用 NVMe 分配器，并把 `vaddr/paddr/size` 存进 `TransferRing` 字段、由 `free(&self)` 显式释放）。
- **服务侧可用性**：`Frame` / `DmaStream` / `BuddyFrameAlloc` / `FrameAlloc` 均在 [prelude.rs:4-23](../../src/kernel/privileged/prelude.rs#L4-L23) 顶层 re-export（functions 可直接用，不触 F2）。
- **`Frame` 现语义**（[frame.rs:25-109](../../src/kernel/privileged/frame.rs#L25-L109)）：持 `phys` / `ref_count: AtomicU32` / `order` / `meta`；`order` 决定 `size() = PAGE_SIZE << order`；`as_virt_ptr()` = `phys_to_virt`；**零生产调用者**，唯一使用者为 host 维 [dma_stream.rs](../../host-tests/src/dma_stream.rs)（以 `from_raw` 构造"纯算术载体"帧，含 order 17 / MMIO 地址 / 1000 次循环，依赖"drop 不触碰 PMM"）。
- **`BuddyFrameAlloc` 阶数缺陷（D3a 必须一并修正）**：[frame_alloc.rs:64-76](../../src/kernel/privileged/alloc/frame_alloc.rs#L64-L76) 的 `alloc_pages(count)` 把 `order` 硬编码为 `9`（`count <= 512`），而 `pmm::free_pages(addr, count)` 用 `count_to_order(count)`（[pmm.rs:1143-1150](../../src/kernel/privileged/mm/pmm.rs#L1143-L1150)）⇒ 非 512 页的块将按错误阶释放（Frame 成为真实属主后即"释放面阶数不一致"）。修法：以 `count.next_power_of_two()` 作为请求页数传给 `pmm_alloc_pages_phys`（使 pmm 内部 `count_to_order` 与 `Frame.order` 严格同值），`DMA_MAX` 面维持既有上限判据。
- **host 维门控先例**：`sync/spinlock.rs:317-325` 已用 `#[cfg(feature = "host-test")]` 把"host 无中断语义"的中断禁用降为 no-op。D3a 的 `Drop` 归还路径按同型处理（host 构建不承担物理归还，测试帧为算术载体）——**不是平行实现**，与既有先例同构。
- **旁路影响（登记，不改）**：新路径不再经 `alloc_coherent` ⇒ 这些缓冲不再计入 `DmaEngine.stats`，也不再由 `DmaEngine::shutdown()` 释放（改由句柄 RAII 接管）；`nvme_alloc_io_queues`（[mod.rs:264](../../src/kernel/privileged/driver/storage/mod.rs#L264)）、`ahci_alloc_cmd_lists`（[mod.rs:585-606](../../src/kernel/privileged/driver/storage/mod.rs#L585-L606)）与 e1000 / virtio / UNKFS 仍走 `alloc_coherent`，本轮不动。
- **同语义面的第三份实现（登记，不合并）**：[virtio/queue.rs:283](../../src/kernel/privileged/driver/virtio/queue.rs#L283) 的 `DmaBuffer`（自有 `Drop`，virtio blk/net 生产在用）与 `DmaStream` 属同一语义面；本轮按 §12.2 不做合并，登记为预存问题。
- **aarch64 现状**（D3b 依据）：[vmm_aarch64.rs:677-721](../../src/kernel/privileged/mm/vmm_aarch64.rs#L677-L721) 的 `unmap_page_in_table` 已执行 `dsb ishst` + `tlbi vaae1is` + `dsb ish` + `isb`（且注释明确"必须在释放页表页前执行"），但 leaf 不 `frame_dec`；[:1075-1124](../../src/kernel/privileged/mm/vmm_aarch64.rs#L1075-L1124) 的 `destroy_page_table` 只走 L0→L1→L2、在 L2 处把 `0b11` 当表指针交给 `free_table`（[:345-349](../../src/kernel/privileged/mm/vmm_aarch64.rs#L345-L349) 直调 `get_pmm().free_page`），**完全不遍历 L3 leaf ⇒ 用户数据帧在销毁路径上零释放**（既有缺口，非本轮引入）。
- **aarch64 +1 侧失效（D3b 补调研，裁定 6 的实测依据）**：[cow.rs](../../src/kernel/privileged/mm/cow.rs) 的 `clone_user_page_table_cow_inner` 阶段 2 原用 `let flags = parent_pte & 0xFFF; if (flags & 2) == 0 || (flags & 4) == 0 { return; }` 过滤 leaf —— 该判据是 `x86_64` PTE 语义（bit1 = WRITABLE、bit2 = USER）。aarch64 描述符 bits[1:0] = `0b11`（页描述符）且 bit2 为 MAIR 属性索引位（用户页用索引 4 ⇒ bit2 = 0），故 `flags & 4` **恒为 0** ⇒ aarch64 从不 `frame_inc`、也不清写位（fork 实为"无 COW 的共享写"）。两面同集前若直接补 dec，必然 UAF。
- **aarch64 无缺页处理器（D3b 决定性约束，裁定 7 的实测依据）**：[exception.rs:236-244](../../src/kernel/privileged/arch/aarch64/exception.rs#L236-L244) 的 EL0 同步异常中非 SVC 一律进 `sync_exception_handler`，该 handler 打印 `SYNC! ESR= FAR= ELR=` 后 `loop { wfi }` 停机（[:563-611](../../src/kernel/privileged/arch/aarch64/exception.rs#L563-L611)）；全仓唯一 COW 缺页入口 [page_fault.rs:153](../../src/kernel/privileged/mm/page_fault.rs#L153) 只被 `x86_64` 的 `idt/handlers.rs:217` 调用 ⇒ **aarch64 不能承受"置只读 + 依靠缺页复制"**。
- **判据落点约束（F3）**：[cow.rs](../../src/kernel/privileged/mm/cow.rs) 已 `use super::vmm`，故 `vmm_aarch64.rs` 不可反向依赖 `super::cow`（会构成模块环依赖）；裸 `u64` 用户 leaf 判据只能置于两处共同父模块 [mm/mod.rs](../../src/kernel/privileged/mm/mod.rs)。

#### 8.3.2 施工条目

- **D-12. D3a：`Frame` 句柄化 + driver DMA 接线（甲）**
  - 描述：`Frame` 由"本地计数包装"改为"PMM 计数面之上的可共享句柄"，并把 driver DMA 缓冲改为持有它的 RAII 句柄，使该句柄首次进入生产路径。
  - 方案：① [frame.rs](../../src/kernel/privileged/frame.rs) 删除 `ref_count: AtomicU32`，`ref_count()` 委托 `api::frame_ref_count`，`inc_ref()` 改为返回 `bool` 的 `api::frame_inc` 委托，删除 `dec_ref()`（改由 `Drop` 承担）；新增 `impl Clone`（持有者 +1，帧不处于计数态即**硬失败**，不静默产生未计数句柄）与 `impl Drop`（持有者 −1，仅归零才归还；`order == 0` 走 `free_page`，`order > 0` 走 `free_pages(phys, 1 << order)`），`Drop` 的归还调用按 `#[cfg(not(feature = "host-test"))]` 门控（先例同 `spinlock.rs`）。② [frame_alloc.rs](../../src/kernel/privileged/alloc/frame_alloc.rs) 修正 `alloc_pages` 阶数推导（见 §8.3.1），`free(frame)` 收敛为 RAII 语义（等价 `drop`）。③ [driver/storage/mod.rs](../../src/kernel/privileged/driver/storage/mod.rs) 的 `nvme_alloc_dma_buffer` / `ahci_alloc_dma_buffer` 改为返回 `Option<DmaStream>`（`BuddyFrameAlloc::alloc_pages` → `frame.zero()` → 保持既有的 `cache_flush` 语义 → `DmaStream::from_frame`），删除 `nvme_free_dma_buffer` / `ahci_free_dma_buffer`（调用点改为作用域结束自动释放）。④ functions 侧 9 个调用点机械改写（取 `cpu_addr().as_ptr() as u64` / `dma_addr().as_u64()`，删掉显式 free 调用；xHCI `TransferRing` 改为持有句柄并用 `Option::take` 实现既有 `free()` 语义）。
  - 状态：[X]
  - 详情（已落地）：① [frame.rs](../../src/kernel/privileged/frame.rs) 删除 `ref_count: AtomicU32` 字段与 `core::sync::atomic` import；`ref_count()` 委托 `api::frame_ref_count`，`inc_ref()` 改为返回 `bool` 的 `api::frame_inc` 委托，删除 `dec_ref()`；新增 `impl Clone`（`assert!(frame_inc)` 硬失败——静默产生未计数句柄会让后续 `Drop` 把仍在使用的帧计入零）与 `impl Drop`（`order == 0` ⇒ `frame_dec` 归零才 `pmm_free_page_phys`；`order ∈ [1, MAX_BUDDY_ORDER]` ⇒ `pmm_free_pages_phys(phys, 1 << order)`；`order > 9`（1GB 大页，零调用点）⇒ fail-closed 不归还 + SIMPLIFIED 注释）；归还体按 `#[cfg(not(feature = "host-test"))]` 门控（先例同 `sync/spinlock.rs`）。`MAX_BUDDY_ORDER` 由 [pmm.rs](../../src/kernel/privileged/mm/pmm.rs) 提为 `pub(crate)`，避免该界在 privileged 内出现第二份硬编码。② [api.rs](../../src/kernel/privileged/mm/api.rs) 新增 `frame_inc` / `frame_dec` / `frame_ref_count` 三个类型安全委托（`u8` → `u32` 收口）。③ [frame_alloc.rs](../../src/kernel/privileged/alloc/frame_alloc.rs) `alloc_pages` 阶数同值修正：`npages = count.checked_next_power_of_two().filter(|&n| n <= 512)?` + `order = u8::try_from(npages.trailing_zeros())?`，请求 `pmm_alloc_pages_phys(npages)`；`free(frame)` 收敛为 `drop(frame)`。④ [driver/storage/mod.rs](../../src/kernel/privileged/driver/storage/mod.rs) 新增私有 `alloc_dma_buffer(size) -> Option<DmaStream>`（`BuddyFrameAlloc.alloc_pages(ceil(size/4K))` → `frame.zero()` → `DmaStream::from_frame(Bidirectional)` → `get_dma().cache_flush(dma_addr().to_virt(), size())`），两个 `*_alloc_dma_buffer` 委托之并改返回 `Option<DmaStream>`，删除 `nvme_free_dma_buffer` / `ahci_free_dma_buffer`；[engine.rs](../../src/kernel/privileged/dma/engine.rs) 的 `cache_flush` 由私有提为 `pub(crate)` 以复用（避免第二份缓存刷新实现），故"清零对设备可见"语义与 `alloc_coherent` 完全同源。⑤ functions 9 个调用点改写完成：`storage/mod.rs` 1 点（MSIX-03 自测）、`storage/nvme.rs` 4 点、`storage/ahci.rs` 3 点、`usb/xhci.rs` 1 点。
  - 详情（偏离记录，按实测裁定）：a) xHCI `TransferRing` 原方案"用 `Option::take` 实现既有 `free()` 语义"**失去对象**——实测 `free()` 与 `TransferRing` 全仓零调用点（仅同文件 `EndpointTransfer` 按值持有），故**删除 `free()`**（F9 零死代码），字段改为 `dma: DmaStream`，`vaddr/paddr/buf_size` 全由句柄派生，`depth` 由 `dma.size() / size_of::<Trb>()` 派生（与原 `actual_size` 同值 ⇒ 环深语义不变）；b) `ahci.rs::identify()` 的 `buf_size` 随三元组解构一并消失，其上方 `#[expect(clippy::similar_names)]`（判据仍由 `buf_vaddr`/`buf_paddr` 触发）的 reason 文案同步收窄为"虚拟/物理地址对"；c) `nvme_zero_dma` / `*_copy_*_dma` 等按原生地址的填充/拷贝 wrapper 保留（仅换入参来源），未新增抽象。
  - 详情（语义变化，登记面）：新路径不再经 `alloc_coherent` ⇒ 不计入 `DmaEngine.stats`，不由 `DmaEngine::shutdown()` 释放（RAII 接管）；不再有 `dma.is_initialized()` 闸门；句柄 `size()` 由"请求值"变为"2 的幂页取整值"（各调用点均以 `byte_count` 为拷贝长度 ⇒ 安全）。
  - 详情（载体）：内核维 2 例，新增于 [test_mm.rs](../../src/kernel/privileged/tests/test_mm.rs) 组 `mm::frame`（host 维按 E-04 先例 Skip）——`handle_clone_drop_pairing`（分配后计数 1 且从 PMM 取走一页 → `Clone` 计数 2 → 克隆析构回 1 且**页不归还**、64 次探测不得重发该帧 → 末句柄析构计数归零且页归还）、`dma_buffer_raii_release`（走生产接线面 `nvme_alloc_dma_buffer(2 页)`：`size()` 页取整、取走 2 页、`cpu_addr` 为 `phys_to_virt` 同式、初值为零 → 析构后**页数守恒**且计数面无残留）。
  - 详情（门槛复跑，主 agent 串行，取脚本自身退出码）：① `./ci/build.sh all` RC=0（`Passed: 5 Failed: 0`，双架构 0 error / 0 warning）；② `./ci/audit.sh quick` RC=0（双架构 check + clippy pedantic + `kernel_test`/`host-test` 两 feature 维 clippy 全绿）；③ `make test-host` RC=0（全部 `test result: ok`，0 failed）；④ `make test-unit` ⇒ 序列日志 `[TEST] ALL TESTS PASSED (511/511)`（原 509 例 + 本轮 2 例），QEMU exit 33；⑤ `scripts/qemu_boot_test.sh x86_64` RC=0（里程碑 `VFS ready`，进入 Ring 3）；⑥ `make test-smp-multicore` RC=0（2/3/4 核全通过，`deferred-free admitted_total=11 released_total=11 pending=false`）。
  - 详情（门槛 7 注入复验，双向）：**注入 A**（`Frame::drop` 两归还分支均置空）⇒ `❌ TESTS FAILED (QEMU exit: 35)`，496 PASS / **2 FAILED，恰为本轮 2 个新载体**（`handle_clone_drop_pairing: 计数归零即归还物理帧`、`dma_buffer_raii_release: 句柄析构应按帧阶数整块归还 (页数守恒)`）；**注入 B**（`alloc_pages` 复现修复前的 `order = 9` 硬编码）⇒ 497 PASS / **1 FAILED**，失败行为 `dma_buffer_raii_release: 2 页请求应得到 2 页缓冲 (页取整且阶数同值)`——即阶数同值修正确有判别载体。还原后复跑 511/511 全绿，`grep -rn "TEMP-INJECT"` 复核无残留。
  - 详情（§12.5 另记，只报不动）：`make test-unit` 配方为 **fail-open 形态**（失败时打印 `❌ TESTS FAILED`，但配方以 `fi` 收尾 ⇒ `make` 仍返回 0），故门槛 4 的判据取序列日志而非 `$?`；如需与已修复的 `test-host` 同型处理须另立任务。
- **D-13. D3b：aarch64 计数面与释放面统一**
  - 描述：把 aarch64 的映射/拆除两侧接入 PMM 计数面，并按裁定 4（广播失效即追平）确定释放时机。
  - 方案：① [vmm_aarch64.rs](../../src/kernel/privileged/mm/vmm_aarch64.rs) 的 `unmap_page_in_table` 对 `0b11` leaf 执行 `frame_dec`，归零即释放（**严格晚于** `tlbi vaae1is` + `dsb ish` 序列）；② `destroy_page_table` 补 L3 leaf 遍历（闭合"销毁不释放用户数据帧"缺口）并在释放前补一次系统级失效（复用既有 `tlbi vmalle1is` 广播形态）；③ 不移植 `defer_free`（aarch64 无 x86 式延迟面，属**设计差异非缺口**，写入 `docs/explain` 或本条详情）；④ 登记：aarch64 无 cr3 式"无核驻留"闸门。
  - 状态：[X]
  - 详情（分段与判据统一）：按裁定 6 分两个串行段施工。**段 1（+1 侧，先落地并复跑门槛）**：① 裸 `u64` 用户 leaf 判据收敛为单一实现 `pub(crate) fn is_user_leaf(entry: u64) -> bool`（[mm/mod.rs](../../src/kernel/privileged/mm/mod.rs)，`PAGE_NX` 之后），按架构分派 —— `x86_64` 用 `entry & PAGE_USER != 0`，aarch64 用 `entry & 0b11 == 0b11 && entry & (1 << 6) != 0`（bit6 = AP[1]）；置于父模块而非 `cow.rs` 是因为 `cow.rs` 已 `use super::vmm`，若 `vmm_aarch64` 反向依赖 `cow` 即构成模块环依赖（F3）。② [cow.rs](../../src/kernel/privileged/mm/cow.rs) 删除局部同名私有函数并改为 `use super::is_user_leaf`；阶段 2 闭包重写为"`is_user_leaf` 过滤 → **每个用户 leaf** `pmm.frame_inc` → 仅当 `mark_cow_readonly` 返回值与原值不同时才写回两槽位"。③ 新增 `mark_cow_readonly(entry: u64) -> u64`：`x86_64` 分支对"用户可写页"清 bit1，aarch64 分支恒返回原值（裁定 7；带 `SIMPLIFIED` 标记说明扩展时机）。**段 2（−1 侧）**：④ `unmap_page_in_table` 改为 `ptr::read_volatile` 保留旧值后再清零，并在 `tlbi vaae1is` + `dsb ish` + `isb` **之后**执行 `is_user_leaf(old_entry)` 判定 → `frame_dec` → 归零即 `free_page`（释放严格晚于失效，裁定 4）；⑤ `destroy_l2_table` 原把 `0b11` L2 项直接交给 `free_table`（L3 leaf 零遍历），改为调用新增的 `destroy_l3_table(paddr)`：逐 L3 项按 `is_user_leaf` 判定 `frame_dec` → 归零即 `free_page`，随后再 `free_table(l3_paddr)` 释放 L3 页表页本身；⑥ `destroy_page_table` 在 `root_paddr == 0` 早退之后补一次系统级失效 `dsb ishst` + `tlbi vmalle1is` + `dsb ish` + `isb`（复用 `switch_page_table` 的广播形态），确保"立即归还数据帧/页表帧"前所有核已停止使用该页表。**未移植 `defer_free`**（裁定 4：aarch64 广播失效即系统级追平，属设计差异非缺口）。
  - 详情（语义变化面，登记）：① `x86_64` 的 +1 集由"用户**可写** leaf"扩为"用户 leaf"，与拆除侧 `is_user()` 严格同集 —— 顺带闭合"共享的只读用户页计数未 +1"的潜在 UAF（该页被拆除时 `frame_dec` 会把计数 1 误判为归零而释放）；② aarch64 的 +1 由"从不生效"变为"每个用户 leaf +1 生效"，且 fork 后父子仍为"共享写"（不置只读，裁定 7）；③ aarch64 的 −1 面从"零释放"变为"逐用户 leaf `frame_dec`、归零即归还"，销毁路径的用户数据帧不再泄漏。
  - 详情（aarch64 边界判据）：① **无缺页/COW 恢复路径** —— EL0 非 SVC 同步异常一律停机（[exception.rs:563-611](../../src/kernel/privileged/arch/aarch64/exception.rs#L563-L611)），故只同集计数、不置只读（裁定 7），补齐该路径为内核工程必需项（乙′，见 §8.3.3）；② **大页 leaf 不参与计数** —— aarch64 用户低地址（`virt >> 48 == 0`）下 `map_page` / `map_huge_page` 直接 no-op，用户页表无 L2 块描述符（`0b01`），`destroy_l2_table` 的 `entry & 0b11 == 0b11` 判据亦不匹配块描述符，与 `x86_64` 侧一致；③ **`destroy_page_table` 未持 `VMM_LOCK`** —— 与 `x86_64` 不对称（既有状态，本轮不改、仅登记），本轮新增的 `frame_dec` / `free_page` 均在 PMM 锁内自洽，未引入新的锁序（VMM → PMM 单向不变）；④ **`tlb_flush_all` 在 aarch64 为本地 `tlbi vmalle1`** —— COW 置只读的刷新面在 aarch64 上属"从未生效"路径（裁定 7 下 aarch64 不置只读，故无实际影响），登记待独立工程。
  - 详情（门槛复跑，主 agent 串行，取脚本自身退出码）：① `./ci/build.sh all` RC=0（`Passed: 5 Failed: 0`，双架构 0 error / 0 warning）；② `./ci/audit.sh quick` RC=0（双架构 check + clippy pedantic + `kernel_test`/`host-test` 两 feature 维 clippy 全绿，含 SAFETY 覆盖与注释中文化 0 违规）；③ `make test-host` RC=0；④ `make test-unit` ⇒ 序列日志 `RESULT: ALL 511 TESTS PASSED (0 skipped)` / `[TEST] ALL TESTS PASSED (511/511)`，QEMU exit 33（配方 fail-open，判据取日志）；⑤ `scripts/qemu_boot_test.sh x86_64` RC=0（里程碑 `VFS ready`，进入 Ring 3）；⑥ `scripts/qemu_boot_test.sh aarch64` RC=0（里程碑 `VFS ready`，进入 EL0 启动 init，virtio-net 经 NetOps 安全桥探测成功）；⑦ `make test-smp-multicore` RC=0（2/3/4 核全通过，`deferred-free admitted_total=11 released_total=11 pending=false`）。段 1 与段 2 各跑一轮完整门槛，均全绿。

#### 8.3.3 本轮登记项（§12.5，只报不动）

- `DmaEngine::shutdown()` 与 DMA 统计不再覆盖新路径缓冲（D3a 引出，句柄 RAII 接管；队列/其它设备仍走 `alloc_coherent`，统一需后续工程）。
- [virtio/queue.rs:283](../../src/kernel/privileged/driver/virtio/queue.rs#L283) `DmaBuffer` 与 `DmaStream` 同语义面第三份实现，未合并。
- aarch64 计数语义无单元级判别载体（仅有"双架构 0w0e + aarch64 启动到 EL0"作为载体）。
- aarch64 无 x86_64 cr3 式"无核驻留"闸门（D3b 后仍需独立工程）。
- **aarch64 无缺页 / COW 恢复路径（D3b 引出，内核工程必需项）**：EL0 非 SVC 同步异常一律 `wfi` 停机（[exception.rs:563-611](../../src/kernel/privileged/arch/aarch64/exception.rs#L563-L611)），全仓唯一 COW 缺页入口只被 `x86_64` 的 `idt/handlers.rs:217` 调用 ⇒ aarch64 目前既不能承受"置只读 + 缺页复制"的真 COW，也不能承受任何用户态合法缺页（栈增长、按需分页等）。本轮按裁定 7 只做"同集计数、不置只读"，补齐该路径（乙′）需独立立项。
- aarch64 `mm::arch::tlb_flush_all` 为本地 `tlbi vmalle1`（非 IS 广播），与 `vmm_aarch64` 内部使用的 `tlbi vaae1is` / `vmalle1is` 广播面不一致；在裁定 7 下 COW 置只读在 aarch64 属"从未生效"路径，故无实际影响，登记待独立工程。
- aarch64 `Aarch64Vmm::destroy_page_table` 未持 `VMM_LOCK`（与 `x86_64` 对称面缺失，既有状态）；本轮新增的 `frame_dec` / `free_page` 未改变锁序（VMM → PMM 单向），但该不对称本身需独立工程评估。
- 大页 leaf 不参与帧持有计数（`x86_64` 与 aarch64 一致）：`cow.rs` 的 2MB skip 判据与 aarch64 `destroy_l2_table` 的 `0b11` 表描述符判据均不命中块描述符，属既有语义，登记可追溯。
- `make test-unit` 配方 fail-open（失败时 `make` 仍返回 0，判据须取序列日志）；与已修复的 `test-host` 属同一形态，修复需另立任务（D3a 门槛复跑时实测）。