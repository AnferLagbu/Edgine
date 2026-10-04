# 重复实现收敛工程（帧归还语义单点化 + functions 冗余代理清理）

> **定位**：把「物理帧的归还时机」这一条语义从 8 处复制收敛为 privileged/mm 的**两个入口**（自持锁 / 要求持锁），并清理 functions 层对 `privileged::mm::pcache` 的零调用者转发壳；同时把 `eliminate-parallel-implementations.md` 的 9 处失实状态归位。
>
> **来源**：`pcache-frame-ownership.md` §6 登记项 2（`pcache_get` 的 functions re-export 零调用者）与登记项 4（旧帧归零释放的形态重复，4→5 处），两项均标注「属 `eliminate-parallel-implementations.md` 范畴」。
>
> **关系**：与 `eliminate-parallel-implementations.md`（host-tests 平行实现，已实质完成）**不重叠**——本工程收敛的是**内核内部**的重复形态，该文档的收尾（状态归位）并入本工程 D-7。
>
> **关联**：`docs/plan/cr3-lifetime-ownership.md` §8.1（帧持有计数面契约）；`docs/plan/tlb-shootdown-epoch.md` §7.1（延迟释放与 TLB 代）；`docs/explain/explain-framekernel.md`（I2/I4）。

## 1. 立项依据

### 1.1 根因一句话

「帧计数归零 ⇒ 何时真正归还物理页」的决策被复制到 8 个站点，每处各自写一遍 `#[cfg(x86_64)] defer_free / #[cfg(aarch64)] free_page`（或架构专属文件里直接调本架构原语），**且其中一处（`pcache::deref`）连延迟释放都漏了**。语义分散 ⇒ 任一处新增/调整归还策略都必须记住全部 8 处，漏改即产生「他核 TLB 仍缓存旧映射而帧已重分配」的 UAF 前提缺口。

### 1.2 形态清单（调研实测）

| # | 站点 | 锁契约 | 释放对象 | 现状 |
|---|---|---|---|---|
| 1 | `page_fault.rs` MAP_PRIVATE 换叶 | 自行 acquire | 缓存帧（旧映射被替换） | cfg 分派 |
| 2 | `cow.rs` `cow_handle_fault` | 自行 acquire | COW 旧帧 | cfg 分派 |
| 3 | `cow.rs` `release_child_table_frame` | 调用方持锁 | 子页表帧（无 `frame_dec`） | cfg 分派 |
| 4 | `pcache.rs` `release_frame_after_last_holder` | 自行 acquire | 悬留旧缓存帧 | cfg 分派 |
| 5 | `vmm_x86_64.rs` `unmap_page_in_table` | 已持锁 | 用户数据帧 | 直接 `defer_free` |
| 6 | `vmm_x86_64.rs` `destroy_page_table` | 已持锁 | 数据帧 + 页表帧 | 直接 `defer_free` |
| 7 | `vmm_aarch64.rs` `destroy_l3_table` | 路径内 | 用户数据帧 | 直接 `free_page` |
| 8 | `vmm_aarch64.rs` unmap 路径 | 路径内 | 用户数据帧 | 直接 `free_page` |

**未纳入**（非本形态，理由见 §6 登记项）：`frame.rs:182`（`Frame` 句柄 RAII 归还，走 `api::pmm_free_page_phys`）、`swap.rs` / `pmm.rs` 内部直接 `free_page`（无跨架构分派语义）、`pcache.rs::invalidate_inode`（守卫集合恒空，见登记项）。

### 1.3 关键缺陷：`pcache::deref` 的最后一次释放不延迟

按本工程既定的计数契约（`pmm.frame_ref_count(phys) == ref_count + 1`），`pcache::deref` 在 `ref_count` 归零时移除条目，这**正是该帧的最后一次引用注销**；但 `deref` 直接调 `pmm.free_page(phys)` **立即归还**，既无 `defer_free` 也无架构分派。真实 `munmap` 序列（`release_file_pages` → `remove_range`）下，`unmap_page_in_table` 的 `frame_dec` 因条目自身那份持有而不归零，故 `defer` 不被触发，真正归零发生在随后的 `deref` ⇒ **他核 TLB 追平前提在最后一步被绕过**。属预存代码缺陷（非上一轮引入），但与本次收敛对象同源，用户裁定纳入本轮。

## 2. 源码调研结论

- **锁序事实**：`release_file_pages`（functions）持 `VMM_LOCK` 前的 `mm.vmas.lock()`（VMA_LOCK）逐页调 `pcache_release_for_va`；`get_physical_in_pml4` **不取锁**（只读遍历）。既定锁序为 `VMA_LOCK → VMM_LOCK → pcache 桶锁 / PMM 锁`。故归还动作（需 VMM_LOCK）**必须在桶锁外**执行，否则构成 `桶锁 → VMM_LOCK` 反向嵌套。
- **`defer_free` 语义**（`vmm_x86_64.rs:2390`）：`pub(crate)`，纯头插入链 + `DEFERRED_FREE_ADMITTED` 计数，要求持 VMM_LOCK 作批次链单写者；真正归还在 TLB 代追平的 `release_lock` 路径。
- **`frame_dec` 语义**：`cur == 0` ⇒ `false`（不动）；`cur == 1` ⇒ 写 0 且 `true`（唯一持有者）；否则递减且 `false`。⇒ **`true` 即「我是最后持有者」**，可作归还前置判据（比现行 `free_page` 的 "cur<=1 即真释放" 更严格、更 fail-closed）。
- **测试断言约束**：kernel 自测 `test_pcache_ref_count_matches_mapping_matrix` 末段断言「末个映射注销后条目释放 + 帧归零（`frame_ref_count == 0`）」。`frame_dec` 写 0 后**立即**满足该断言，延迟释放只推迟 PMM 真正回收 ⇒ 该用例在改造后仍应通过（这是设计正确性的判别点之一）。
- **判别力来源**：`DEFERRED_FREE_ADMITTED` 无公共访问器，仅经 klog 埋点输出（`vmm_x86_64.rs:2307`）。故延迟释放的**负向验证**以 `make test-smp-multicore` 解析 `admitted/released/pending` 承载，而不为测试新增访问器（避免为测试开 API 面）。
- **functions 转发壳判据（三合一）**：`src/kernel/functions/mm/pcache.rs` 的 **5 个**（原文写「六个」）公开函数 `pcache_get` / `pcache_lookup` / `pcache_mark_dirty` / `pcache_put` / `pcache_invalidate_inode` 全仓零调用者；能力等价入口已存在（`privileged::mm::pcache::*`）；非 API/FFI/feature/硬件原语面。且 `privileged::mm::pcache` **不在** `audit_functions_boundary.py` 的 `FORBIDDEN_PRIVILEGED_MODULES`（functions 可直接调用，`functions/mm/mmap.rs` 已如此），亦**不在** `PROXY_ALLOWANCE`（该壳不属被豁免的转发设计）。

## 3. 用户裁定

| 项 | 裁定 |
|---|---|
| 工程范围 | **C + D + 文档订正**：C = functions 冗余代理处置；D = 帧归还形态收敛；文档订正 = 两份 plan 状态归位 |
| D 实现路径 | **B 相对完整**：双入口（`release_frame_locked` 要求已持锁 + `release_frame` 自持锁）+ 架构语义注释单点化 |
| `pcache::deref` | **纳入本轮**：改走统一入口（x86 延迟 / aarch64 立即） |

## 4. 任务分解

- **D-1. privileged/mm 双入口建立（语义单点化）**
  - 描述：在 `privileged/mm/mod.rs`（与既有唯一判据 `is_user_leaf` 同处）新增两个 `pub(crate)` 入口：`release_frame_locked(PhysAddr)`（要求调用方已持 VMM_LOCK）与 `release_frame(PhysAddr)`（自持锁包装）。语义注释一并收拢：为何 x86 必须延迟（他核 TLB 陈旧映射 ⇒ 帧重分配 ⇒ UAF）、为何 aarch64 可立即（TLB 代协议仅覆盖 x86_64，广播失效即追平）、以及「调用方不得在持有 pcache 桶锁时调用」的锁序约束。
  - 方案：`release_frame_locked` 内 `#[cfg(target_arch = "x86_64")] vmm::get_vmm().defer_free(phys.0)` / `#[cfg(target_arch = "aarch64")] pmm::get_pmm().free_page(phys)`；`release_frame` = `acquire_lock` → `release_frame_locked` → `release_lock`。`pub(crate)` 非 `pub`（无 functions / 跨子系统调用者）。
  - 状态：[X]

- **D-2. 跨架构分派点收敛（站点 1/2/3）**
  - 描述：`page_fault.rs`（换叶旧缓存帧）、`cow.rs`（COW 旧帧 + `release_child_table_frame`）三处 cfg 分派替换为入口调用；站点 3 用 `release_frame_locked`（调用方已持 VMM_LOCK），站点 1/2 用 `release_frame`。
  - 方案：逐处 1:1 替换 + 删除就地 cfg 分支与重复注释，指向 D-1 的语义注释。
  - 状态：[X]

- **D-3. 架构专属文件的归零归还收敛（站点 5/6/7/8）**
  - 描述：`vmm_x86_64.rs` 与 `vmm_aarch64.rs` 内的帧归还（数据帧 + 页表帧）统一改经 `release_frame_locked`，使「归还一个帧」在该文件内只有一种写法。
  - 方案：`self.defer_free(x)` → `super::release_frame_locked(PhysAddr(x))`（x86 侧已持 VMM_LOCK）；aarch64 侧 `get_pmm().free_page(...)` → `super::release_frame_locked(...)`。`free_table` 内部与 pmm/swap 的直接释放不动（无分派语义）。
  - 状态：[X]

- **D-4. `pcache` 走统一入口（含 `deref` 缺陷修复）**
  - 描述：删除 `release_frame_after_last_holder`（唯一调用点在 acquire 换叶路径），acquire 侧改调 `super::release_frame`；`deref` 归零释放由「立即 `free_page`」改为「`frame_dec`（取回"最后持有者"真值）⇒ 归零才交 `release_frame`」，并把归还动作**移到桶锁外**（`deref` 返回 `Option<PhysAddr>`，`pcache_put` 出锁后归还）。
  - 方案：`deref` 内 `let last = pmm::get_pmm().frame_dec(phys); ... return if last { Some(phys) } else { None };`（`frame_dec == false` 表示仍有其他持有者或契约违反 ⇒ 不归还，fail-closed）。`pcache_put` 在 `guard` 作用域结束后归还。锁序注释就近写明「桶锁内只改计数/条目，归还需 VMM_LOCK 故必须在出锁后」。
  - 状态：[X]

- **D-5. functions 冗余代理清理（C）**
  - 描述：删除 `src/kernel/functions/mm/pcache.rs`（5 个零调用者转发函数）与 `functions/mm/mod.rs` 的 `pub mod pcache;` 声明。
  - 方案：删除后 grep 复核 `functions::mm::pcache` / `functions/mm/pcache.rs` 零残留（archive 文档引用不动）；能力等价面为 `privileged::mm::pcache::*`（functions 已可直接调用，且边界审计未将其列为禁止）。
  - 状态：[X]

- **D-6. 验证门槛与负向验证**
  - 描述：串行复跑 §5 全部门槛；负向验证以「延迟释放判别力」为核心。
  - 方案：负向验证 = 临时把 D-1 `release_frame_locked` 的 x86 分支改为立即 `free_page`（等价于退回"处处立即归还"）⇒ 预期 `test-smp-multicore` 的 `deferred-free admitted/released` 归零或显著下降而使既有断言变红；还原后复跑须全绿。另：`pcache::deref` 侧以「末个映射注销后帧归零」既有断言不变红，证明改造未破坏计数契约。
  - 状态：[X]

- **D-7. 文档订正**
  - 描述：`eliminate-parallel-implementations.md` 的 9 处 `状态：[]` 按实证归位；`pcache-frame-ownership.md` §6 登记项 2/4 标注由本工程承接并处置；本工程 §5 回填实测。
  - 方案：逐条以本轮 grep / 实测为据——A 背景两条（已解决）、B 三条（范围修正已裁定，按需补桩）、C 平行实现清单（7/7，buddy 由 H-04 删除）、删除完成标准与归零门槛（grep 实证）、DECISION-052（决策已落地）。原文「六个转发函数」订正为 5 个。
  - 状态：[X]

## 5. 验证门槛与实测

| # | 门槛 | 结果 |
|---|---|---|
| 1 | `./ci/build.sh all`（双架构 0 warning 0 error） | 通过（末尾 `Passed: 5  Failed: 0`；双架构 `build passed` + link passed，无 warning） |
| 2 | `./ci/audit.sh quick`（含 14 审计脚本 + clippy pedantic） | 通过（`EXIT=0`，23 项 `✓`，0 处 `✗/FAILED`；含双语注释 TD-22 0 违规、C 命名 I-07 0 残留） |
| 3 | `make test-host` | 通过（7 / 2 / 10 / 8 / 11 / 0 六组全 0 failed） |
| 4 | `make test-unit`（判 `tests/reports/unit_test_*.log`，`make` 目标 fail-open） | 通过（`unit_test_20260922_002107.log`：`ALL 517 TESTS PASSED (0 skipped)`；`unit_test_.log` 为空时间戳旧名，按 `ls -t` 取实际日志。`mm::pcache` 四条于 `[156/517]`–`[159/517]` 全绿 —— 含「末个映射注销后条目释放 + 帧计数归零」断言 ⇒ 计数契约未被 D-4 破坏；且该四条执行期间 `deferred-free admitted/released` 由 59 递增至 86 且 `pending=false` ⇒ `pcache::deref` 的归零释放确已改走延迟路径并被追平归还，D-4 修复在自测路径可见） |
| 5 | `./scripts/qemu_boot_test.sh x86_64`（至 Ring 3） | 通过（252 行串口，里程碑 `VFS ready`，进入 Ring 3 启动 init） |
| 6 | `./scripts/qemu_boot_test.sh aarch64`（至 EL0） | 通过（696 行串口，`VFS ready` + virtio-net 经 NetOps 安全桥探测成功，进入 EL0） |
| 7 | `make test-smp-multicore`（2/3/4 核，含 `deferred-free` 计数断言） | 通过（2/3/4 全绿；`admitted_total=11 released_total=11 pending=false`，即延迟释放入链的帧全部真正归还） |
| 8 | 负向验证（D-6） | 通过（把 D-1 的 x86 分支临时改为立即 `free_page` ⇒ `make test-smp` 在门槛 7 的 `deferred-free` 断言处判红：`未找到 '[VMM] deferred-free admitted_total=N ...'`，证明该断言对「归还时机」有判别力；还原后 2/3/4 核复跑全绿） |

补充说明（不影响结论，记录实际观测）：门槛 7 首轮 `-smp 3` 曾出现 AP 未能全部上线（日志仅 `online CPUs: 2`），单独重跑 3 核即通过，随后全核数复跑亦通过 ⇒ 判为 AP 启动竞态抖动，与本工程改动无关（本工程 x86 侧改动经 diff 核对为「同一调用语义改名」，未触及 AP bring-up 路径）。

终态复认证：门槛 8 的临时改动已逐字还原（`git diff` 核对 `mod.rs` 仅剩 D-1 新增的两个入口），还原后在最终工作树上重跑门槛 1/2 ⇒ `BUILD_EXIT=0`（`Passed: 5  Failed: 0`）+ `AUDIT_EXIT=0`；门槛 3–6 的实测日志与最终工作树代码逐字一致。

## 6. 登记项（只报不动）

1. **`frame.rs` 的 `Frame` 句柄归还未经延迟释放**：`frame_dec` 归零后直接 `api::pmm_free_page_phys`。若该句柄将来用于「曾建映射」的帧，同样存在他核 TLB 陈旧访问前提；当前调用面（pcache 条目 / DMA）不属映射帧，故不动。
2. **`pcache::invalidate_inode` 的条目释放仍是立即 `free_page`**：该集合在「无纯缓存引用」前提下恒空（`deref` 归零即移除条目），实际为守卫语义。若将来引入纯缓存引用，该路径须一并纳入统一入口，且归还仍需出桶锁执行（需批量收集）。
3. **`DEFERRED_FREE_ADMITTED` 无公共访问器**：延迟释放的可观测性仅靠 klog 埋点，故内核自测无法直接断言「某次归零走了延迟」；本工程以 SMP 计数埋点承载判别力。若后续要求自测级判别，需为 VMM 增加只读统计入口（届时属独立小工程）。
4. **`functions/mm/pcache.rs` 删除后，functions 对 pcache 的唯一路径是 `privileged::mm::pcache`**：该模块在 `audit_coupling.py` 的 `INTERNAL_PATTERNS` 中（约束 privileged 子系统间引用需经 `mm/mod.rs`），但不在 functions 边界审计的禁止名单；若将来收紧 functions 侧禁止面，须同步迁移到 `mm/mod.rs` 顶层 re-export。
5. **`host-tests/src/hvfs/` 为空目录**（0 文件，git 不跟踪空目录 ⇒ 本地迁移残留）：属 §12.5 工程外预存问题，只报不动。