# pcache 帧持有计数与映射生命周期对齐

> **定位**：修复 pcache（文件页缓存）条目 `ref_count` 的**登记/注销点与「该 VMA 是否真的持有该缓存页」不对齐**这一结构性缺陷——命中路径不登记、拆除路径按地址区间无条件注销、换叶路径不注销旧帧。
>
> **来源**：`munmap-protect-pml4.md` §6 登记项 6。该登记项原描述「`pcache::deref` 绕过帧持有计数」经本工程调研**证伪**（`pmm.free_page` 本身即计数感知），真实根因由本工程重新定义。
>
> **关系**：与 `cr3-lifetime-ownership.md`（D3 帧持有计数**契约**）、`munmap-protect-pml4.md`（拆除/改权限的**目标表**正确性）**不重叠**——两者解决「计数面契约存在」与「作用于正确的表」，本工程解决「**登记与注销是否覆盖同一集合**」。三者同在 privileged/mm，**不可与其它 privileged 工程并行施工**。
>
> **关联**：`docs/explain/explain-framekernel.md`（I2/I4 不变式）；`docs/plan/cr3-lifetime-ownership.md`（帧持有计数面契约 §8.1）；`docs/plan/munmap-protect-pml4.md`（§6 登记项 6）。

## 1. 立项依据

### 1.1 根因一句话

`PageCacheEntry::ref_count` 的语义被声明为「多少个 VMA 映射了此页」（[pcache.rs:87](../../src/kernel/privileged/mm/pcache.rs#L87)），但**登记点**只有 `pcache_get`（未命中插入 / 显式 get），而**注销点**是「按地址区间逐页无条件 `pcache_put`」，两者覆盖的集合不同：命中路径建了映射却不登记，未映射的页却照样注销。

### 1.2 三处不对称

| # | 不对称 | 静态链路 | 后果 |
|---|---|---|---|
| **A1** | **登记点窄于注销点** | [page_fault.rs:315](../../src/kernel/privileged/mm/page_fault.rs#L315) 命中走 `pcache_lookup`（**不 +1**），随后 [L355](../../src/kernel/privileged/mm/page_fault.rs#L355) `frame_inc` + [L361/L370](../../src/kernel/privileged/mm/page_fault.rs#L361) 建映射；`+1` 只出现在 [L319](../../src/kernel/privileged/mm/page_fault.rs#L319) 的 miss 分支 | 同一条目被第二个 VMA 映射时计数不增 ⇒ 任一映射拆除即把 `ref_count` 减到 0 ⇒ **条目提前驱逐**（缓存副本与 `dirty` 标记丢失、`MAP_SHARED` 共享视图破裂） |
| **A2** | **注销点宽于登记点** | [mmap.rs:219-239](../../src/kernel/functions/mm/mmap.rs#L219-L239) `release_file_pages` 按 `[start, end)` **逐页无条件** `pcache_put`，不判该页是否曾被该 VMA 取用 | 从未缺页的 VMA 拆除时**注销他人条目**（同一文件页被另一 VMA/进程持有时被无端驱逐） |
| **A3** | **换叶路径不注销旧帧** | [page_fault.rs:354-361](../../src/kernel/privileged/mm/page_fault.rs#L354-L361)：`already_mapped == false` 时 `frame_inc` 新帧并 `map_page_in_table` **覆盖**旧 PTE，但 `frame_dec` 只存在于 [MAP_PRIVATE COW 分支 L402](../../src/kernel/privileged/mm/page_fault.rs#L402) | `MAP_SHARED` 下旧帧的映射持有者**永不注销** ⇒ 帧计数永不为 0 ⇒ **帧泄漏**（触发前置：条目已被驱逐 + 同 VA 重入缺页） |

> 注：上表行号指向**修复前**位置（立项依据的快照）。修复后建立侧判据已收敛至 [pcache.rs `pcache_acquire_for_va`](../../src/kernel/privileged/mm/pcache.rs)，拆除侧至 [mmap.rs `release_file_pages`](../../src/kernel/functions/mm/mmap.rs)。

### 1.3 与登记项原描述的关系（订正）

登记项原判据「`pcache.rs` 的 `deref` 在归零时**直接** `pmm.free_page`，不经帧持有计数面」**不成立**：

- [pmm.rs:959-970](../../src/kernel/privileged/mm/pmm.rs#L959-L970) `free_page` 内部即 `frame_counts_release`（计数 >= 2 时递减后**不归还**，== 1 才真归还）；
- 故帧计数 2（pcache 自身一份 + 用户 leaf 一份）下 `munmap` 顺序为 3→2→1 递减，**不存在**「仍被映射即归还进 free list」的窗口。

⇒ 本工程**不重复** D3 的计数面契约工作，只补「登记集合 == 注销集合」。

## 2. 源码调研结论

### 2.1 计数契约 vs 现状

| 项 | 应有语义 | 现状 |
|---|---|---|
| `entry.ref_count` | 映射持有该缓存帧的 VMA 页数 | 只统计「由 `pcache_get` 插入/显式 get 的那一份」⇒ 与真实持有集合偏离（A1/A2） |
| `pmm.frame_ref_count(phys)` | `entry.ref_count + 1`（+1 = 条目自身对帧的持有，由 `insert` 的 `alloc_page` 置 1） | 偏差来源：A1 少 +1 ⇒ `frame > ref + 1`；A3 未注销 ⇒ 帧计数虚高且永不归零 |
| 建立侧 frame 计数 | 每次「新建映射指向该帧」+1 | 已实现（[page_fault.rs:355](../../src/kernel/privileged/mm/page_fault.rs#L355)），含 `already_mapped` 幂等判据 |
| 拆除侧 frame 计数 | 每次「拆除指向该帧的用户 leaf」−1 | 已实现（`unmap_page_in_table` 内 `is_user()` 过滤 + `frame_dec` + 归零延迟释放） |

⇒ 帧计数面本身**正确**；缺陷全在 pcache 条目层。

### 2.2 判定范围的事实（调研核实）

| # | 事实 | 证据 | 影响 |
|---|---|---|---|
| F1 | **文件映射路径零用户调用者** | `VmaType::FileBacked` 仅由 `mmap_syscall(fd >= 0, 非 anonymous)` 产生（[mmap.rs:134-159](../../src/kernel/functions/mm/mmap.rs#L134-L159)）；全仓用户程序无文件映射 mmap，仅 [fbterm](../../src/user/fbterm/src/main.rs#L184) 走 `fb_mmap`→`sys_fb_mmap` 独立路径（不建 `FileBacked` VMA） | 整条路径当前**只有 kernel 自测可达**（同 `VmSpace`）⇒ 验证载体取 kernel 自测 |
| F2 | **`MAP_SHARED` 脏页写回未实装** | `pcache_mark_dirty` 只置 `dirty` 标志（[pcache.rs:209-216](../../src/kernel/privileged/mm/pcache.rs#L209-L216)）；全仓无「遍历脏页回写 inode/文件」的实现 | 当前「数据丢失」只损缓存副本，**尚未**造成文件内容错误 ⇒ 本工程只对齐引用模型，不实装写回（登记项 1） |
| F3 | **`invalidate_inode` 在文件关闭时无条件释放** | [handle.rs:178](../../src/kernel/privileged/fs/vfs/handle.rs#L178) `vfs_close_internal` → [pcache.rs:238-247](../../src/kernel/privileged/mm/pcache.rs#L238-L247) 逐条目 `free_page` + 抹除，**不判 `ref_count`** | 靠 PMM 帧计数兜底**不 UAF**，但语义上驱逐活跃映射的缓存页（POSIX：`munmap` 可在 `close` 之后）⇒ 纳入本工程（D-4） |
| F4 | **`pcache_get` 的 functions re-export 零调用者** | [functions/mm/pcache.rs](../../src/kernel/functions/mm/pcache.rs) 纯转发，无调用点 | 登记项 2 |

### 2.3 无纯缓存引用

条目**只**由缺页路径创建，创建时即同时产生一份映射持有；`vfs_read_internal` 的 pcache 快路径（[handle.rs:228-259](../../src/kernel/privileged/fs/vfs/handle.rs#L228-L259)）只 `pcache_lookup` 命中已有条目、**不取引用、不创建条目**。

⇒ 不变量：**已存在的条目恒有 `ref_count >= 1`**。这是 D-2 幂等判据与 D-4 语义的共同前提。

### 2.4 关键未决事实（须实证，不得假设）

**`handle_file_fault` 能否在 kernel 自测中被直接驱动？**

- 其唯一入口 `handle_page_fault(mm, info)`（[page_fault.rs:83](../../src/kernel/privileged/mm/page_fault.rs#L83)）内部用 `vmm::get_current_pml4()` 取表 ⇒ 测试无法把表替换为自建的 `pml4`；
- `handle_user_page_fault` 依赖 `USER_CR3_SAVE` 与用户态入口上下文。
- ⇒ **D-6 测试打在 D-2 的配对入口上**（而非打在 `handle_file_fault` 上）。该入口正是 A1/A3 的唯一判据实现处（D-3 后 `handle_file_fault` 亦调用它），故覆盖面等价；「调用点确实使用该入口」由 D-3 的静态收敛（删除内联判据）保证，不靠运行期证据。

## 3. 用户裁定

| # | 议题 | 裁定 |
|---|---|---|
| 1 | 实现强度（§12.3） | **B 相对完整实现**：把「该 VMA 是否持有该缓存页」的判据收敛为**单一实现**（`pcache_acquire_for_va` / `pcache_release_for_va` 配对入口），建立侧与拆除侧共用；建模 `close(fd)` 后映射仍存活的缓存语义；建立 `frame_ref_count == ref_count + 1` 不变式并以双持有者矩阵覆盖 |
| 2 | 回归测试载体 | **kernel 自测分组** `mm::pcache`（沿用前序工程 `cow_setup_mapped_page()` 风格 + `#[cfg(feature = "host-test")]` Skip 变体），含负向验证（临时恢复修复前形态 ⇒ 对应用例精确变红） |
| 3 | `close(fd)` 语义（D-4） | **不得驱逐仍有映射持有者的条目**：`invalidate_inode` 改为仅释放 `ref_count == 0` 的条目；依 §2.3 该集合当前恒为空 ⇒ 函数退化为守卫（保留调用点与实现，不删），若将来引入「无映射持有者的纯缓存页」需在此实装真正的驱逐（登记项 3） |

### 3.1 架构约束（锁序与层级）

- **判据只能有一份**：`pcache_acquire_for_va` / `pcache_release_for_va` 放在 privileged 的 [pcache.rs](../../src/kernel/privileged/mm/pcache.rs)（需读 PTE 与改帧计数，二者都是机制面）。functions 经 `privileged::mm::pcache` 调用，**不复制判据**（F1/F2 合规：本工程 functions 侧 0 新增 unsafe）。
- **锁序**：新增路径为 `VMA_LOCK → VMM_LOCK → pcache 桶锁 / PMM 锁`，与既有 `release_file_pages`（VMA → pcache 桶锁）与 `unmap_vma_pages`（VMA → VMM → PMM）**同向**；`acquire` 内先取桶锁并**释放**后再取 VMM 锁，不存在 `VMM → pcache 桶锁`的嵌套（无 ABBA 面）。须过 `audit_deadlock_matrix.py`。
- **旧帧归零后的延迟释放**：与建立侧既有形态一致——x86_64 走 `vmm.acquire_lock()` + `defer_free`（他核 TLB 可能仍缓存旧映射），aarch64 无该机制 ⇒ 立即 `free_page`。

## 4. 施工条目

### D-1 pcache 条目语义定死 + 观测口

描述：把 `ref_count` 的语义从「多少个 VMA 映射了此页」的**声明**落实为**不变量**，并提供只读观测口供断言使用。
方案：在 [pcache.rs](../../src/kernel/privileged/mm/pcache.rs) 的 `PageCacheEntry` / `insert` / `deref` / `lookup_and_ref` 上补齐中文语义注释，写死 `ref_count == 映射持有者数` 与 `frame_ref_count == ref_count + 1`（含 `insert` 的 `alloc_page` 置 1 = 条目自身持有）；新增 `pub fn pcache_ref_count(inode_id, page_index) -> Option<u32>`（`None` = 无条目），风格对齐 `pmm.frame_ref_count`（「仅用于断言/审计」）。
状态：[X]
详情：模块文档新增「## 计数契约」段；`insert` 改为返回 `(phys, 是否新插入)` 且新条目 `ref_count = 1`（首个映射持有者）；新增 `PageCacheBucket::ref_count_of` 与公共观测口 `pcache_ref_count`。

### D-2 新增「按 VA 判定持有」的单一配对入口

描述：把「该 VA 是否正持有该文件页的缓存帧」这一判据收敛为一份实现，登记与注销两侧共用。
方案：在 [pcache.rs](../../src/kernel/privileged/mm/pcache.rs) 新增两条 `pub fn`：

- `pcache_acquire_for_va(pml4, va, inode_id, page_index) -> Option<u64>`：① 读 `pml4` 中 `va` 的帧 `old`（`get_physical_in_pml4`）；② 若 `old` 非零且 `old == pcache_lookup(inode, page)` ⇒ **幂等**（该 VA 已持有，不重复登记），返回该帧；③ 否则 `pcache_get`（命中 +1 / 未命中插入）⇒ `frame_inc(phys)`（新建映射新增一份持有）；④ 若 `old` 非零且 `old != phys` ⇒ 注销旧帧（`frame_dec` + 归零则按 §3.1 释放）；返回 `phys`。
- `pcache_release_for_va(pml4, va, inode_id, page_index) -> Option<u64>`：仅当 `get_physical_in_pml4(pml4, va)` 的帧**恰为**该页缓存帧时 `pcache_put`（−1）并返回该帧，否则返回 `None`（**不注销**）。帧计数不在此处改（用户 leaf 的拆除由 `unmap_page_in_table` 承担），归零时 `deref` 释放条目自身那份。

`pml4 == 0` 两者均 fail-closed 返回 `None`（不过渡到全局单表，与 `munmap-protect-pml4.md` 裁定一致）。
状态：[X]
详情：实际签名为 `pcache_acquire_for_va(...) -> Option<(u64, bool)>`（第二位 = 是否新插入，供调用方决定是否回填文件数据）；命中登记由 `insert` 内的 `lookup_and_ref` 承担（满桶命中仍可登记）。`frame_inc` 失败 ⇒ `pcache_put` 回滚后 fail-closed。旧帧归零释放抽为私有 `release_frame_after_last_holder`（§3.1 的 x86_64 `defer_free` / aarch64 立即 `free_page`）。

### D-3 建立侧改用配对入口

描述：`handle_file_fault` 删掉内联的 `already_mapped` 判据与 `frame_inc`，改为调用 D-2 的 `pcache_acquire_for_va`，使 A1/A3 由单一实现消除。
方案：[page_fault.rs:311-356](../../src/kernel/privileged/mm/page_fault.rs#L311-L356) 先 `acquire`（含 miss 时的 `pcache_get` + `vfs_pread_inode` + `pcache_fill` 回填），再按 `vma.shared` 建映射；MAP_PRIVATE 写路径的 `pcache_put` + `frame_dec`（[L393-410](../../src/kernel/privileged/mm/page_fault.rs#L393-L410)）保持，与 acquire 的登记严格对称；A3（同 VA 悬留旧帧）的注销与归零释放由 acquire 内的实现承担（该处注销的是「被替换的旧缓存帧」，与 COW 换出语义不同，故两份并存，见登记项 4）。
状态：[X]
详情：内联 `pcache_lookup`/`pcache_get`/`already_mapped`/`frame_inc` 块整体删除，改为 `pcache_acquire_for_va(user_cr3, aligned, vma.inode_id, page_index)` 单点调用；`newly_inserted` 决定是否走 `vfs_pread_inode` + `pcache_fill`。函数上原 `#[expect(clippy::single_match_else)]` 因 match 形态改变成为 `unfulfilled_lint_expectations` ⇒ 删除该 expect（clippy 归零印证）。

### D-4 `close(fd)` 不驱逐活跃映射的缓存页

描述：修掉 F3——文件关闭时无条件释放条目会摧毁仍被映射的缓存页。
方案：[pcache.rs:238-247](../../src/kernel/privileged/mm/pcache.rs#L238-L247) `invalidate_inode` 改为仅当 `ref_count == 0` 时释放（含 `free_page` 与条目抹除）；补注释写明「仍被映射的条目由其最后一个映射的注销释放（`deref` 归零路径），符合 `munmap` 可晚于 `close` 的语义」，并注明「当前无纯缓存引用 ⇒ 该集合为空」（§2.3）。
状态：[X]
详情：守卫条件落在 `PageCacheBucket::invalidate_inode` 的 `entry.ref_count == 0`；未新增 `orphan` 字段（依 §2.3 该集合恒为空，加字段属无收益状态）。

### D-5 拆除侧改用配对入口

描述：修掉 A2——`munmap` 只注销「该 VMA 页确实持有」的条目。
方案：[mmap.rs:219-239](../../src/kernel/functions/mm/mmap.rs#L219-L239) `release_file_pages` 内 [L235](../../src/kernel/functions/mm/mmap.rs#L235) 的 `pcache_put` 换为 `pcache_release_for_va(cr3, addr, vma.inode_id, page_index)`（返回 `None` 即该页未被本 VMA 持有，跳过）；`cr3` 经参数由 `munmap_syscall` 传入（该函数已按前序工程 D-3 取得 cr3）。functions 层 0 unsafe 不变。
状态：[X]
详情：`release_file_pages` 增加 `cr3: u64` 形参（调用点 `munmap_syscall` 传已取得的 `cr3`）；循环内改为 `let _ = pcache_release_for_va(cr3, addr as u64, vma.inode_id, page_index);`（`None` = 未被本 VMA 持有 ⇒ 跳过，不再 `debug_assert` 失败）。functions 侧仍 0 unsafe（`audit.sh` 通过）。

### D-6 回归测试

描述：为 A1/A2/A3 与 F3 补可验证的回归载体。
方案：新增 kernel 自测分组 `mm::pcache`（[test_mm.rs](../../src/kernel/privileged/tests/test_mm.rs)），每条均带 `#[cfg(feature = "host-test")]` Skip 变体：

- `ref_count_matches_mapping_matrix`：同一 `(inode, page)` 在两个 VA 上依次 acquire + map ⇒ 断言 `(ref_count, frame_ref_count)` 为 `(1,2)` → `(2,3)`；逆序 release + unmap ⇒ `(1,2)` → 条目消失且帧计数 0（**A1 判别**：修复前第二次命中不登记 ⇒ 为 `(1,3)`）。
- `release_requires_actual_holding`：对**未映射**该缓存帧的 VA 调 `pcache_release_for_va` ⇒ 断言返回 `None` 且 `ref_count` 不变（**A2 判别**：修复前按区间无条件 −1）。
- `stale_leaf_replacement_releases_old_frame`：acquire+map ⇒ release 使条目归零（PTE 悬留旧帧）⇒ 再次 acquire 同 `(inode, page)` ⇒ 断言新帧 `frame_ref_count == ref_count + 1` 且**旧帧归零**（**A3 判别**：修复前不注销旧帧 ⇒ 旧帧计数永为 1）。
- `close_fd_keeps_mapped_entry`：acquire+map 后 `pcache_invalidate_inode(inode)` ⇒ 断言条目仍在、`ref_count`/帧计数不变；随后 release + unmap ⇒ 归零且条目消失（**F3 判别**：修复前条目被抹除）。
- 每条用例收尾 `vmm_destroy_page_table` + 断言相关帧计数归零（无跨用例残留）。

负向验证：临时把 D-3 的 acquire 调用退回修复前形态（命中用 `pcache_lookup` 不登记）并把 D-5 退回按区间无条件 `pcache_put`，断言前三条用例精确变红；还原后全绿、全仓 `TEMP-NEG-VERIFY` 标记零残留。
状态：[X]
详情：四条用例落在 [test_mm.rs](../../src/kernel/privileged/tests/test_mm.rs) 分组 `mm::pcache`，各用例使用互不相同的 inode 编号避免串扰。负向验证在 **A1/A2/A3 三处**同时临时回退（acquire 命中走 `lookup` 不登记 / release 去掉持有判据 / 移除旧帧注销），实测 `514 passed, 3 FAILED`，失败断言恰为「第二个映射持有者应 +1」「未持有该缓存帧的 VA 不得注销」「被替换的旧帧持有者必须注销 (归零)」，第四条 `close_fd_keeps_mapped_entry` 保持绿（与 A1/A2/A3 无关）；还原后 `ALL 517 TESTS PASSED (0 skipped)`，`TEMP-NEG-VERIFY` 全仓零残留。

**本工程连带修复（登记项 7）**：注册 4 条新用例后实测发现 `TestRegistry` 容量 `MAX_TESTS = 512` 已被占满，新增用例把表尾 `timerfd`/`syscall::splice` 用例**静默挤出**（且修复前期望总数实为 513，已有 1 条长期被静默丢弃）。已扩容至 640 并给 `TestRegistry::register` 加满容量 `assert!`（fail-closed，防复发——该措施本是归档审计的记录建议）。

### D-7 文档同步

描述：代码落地后同步本文件与前序工程登记项。
方案：回填 D-1..D-6 详情与 §5.1 实测数据；`munmap-protect-pml4.md` §6 登记项 6 标注「已由 `pcache-frame-ownership.md` 承接」（该条订正已随本工程立项完成）。
状态：[X]
详情：本文件 D-1..D-6 详情与 §5.1 已回填，登记项增至 7 条；`munmap-protect-pml4.md` §6 登记项 6 已标注承接（含订正说明）。

## 5. 验证门槛

每轮施工后串行复跑（主 agent 执行，取脚本自身退出码）：

1. `./ci/build.sh all` —— 双架构 0 error / 0 warning
2. `./ci/audit.sh quick` —— 退出码 0（含 clippy pedantic + 两 feature 维 + 锁序/边界/中文注释审计）
3. `make test-host` —— exit 0
4. `make test-unit`（**判日志** `tests/reports/unit_test_<ts>.log`，该目标 fail-open）—— `ALL 517 TESTS PASSED (0 skipped)`（= 修复前期望 513 + D-6 新增 4；注册表容量已由 512 扩至 640，见登记项 7）
5. `./scripts/qemu_boot_test.sh x86_64` —— 里程碑 `VFS ready` + 进入 Ring 3
6. `./scripts/qemu_boot_test.sh aarch64` —— 里程碑 `VFS ready` + 进入 EL0
7. `make test-smp-multicore` —— 2/3/4 核全通过
8. 负向验证（本工程专属，见 D-6）

### 5.1 实测结果

| # | 门槛 | 结果 |
|---|---|---|
| 1 | `./ci/build.sh all` | `Passed: 5  Failed: 0`（双架构 build+link、host tests、forbidden asm） |
| 2 | `./ci/audit.sh quick` | `audit (quick) 完成`，clippy pedantic + kernel_test 维 + host-test 维全过 |
| 3 | `make test-host` | 全部 `test result: ok`，0 failed |
| 4 | `make test-unit` | `ALL 517 TESTS PASSED (0 skipped)`，`mm::pcache` 四条于 `[156/517]`–`[159/517]` 全绿 |
| 5 | `qemu_boot_test.sh x86_64` | `1/1 通过`，`VFS ready` + Ring 3 |
| 6 | `qemu_boot_test.sh aarch64` | `1/1 通过`，`VFS ready` + EL0 |
| 7 | `make test-smp-multicore` | `SMP MULTICORE TEST PASSED: 核数 2 3 4 全部通过`（含 `deferred-free admitted/released` 非零且 `pending=false`，印证 §3.1 旧帧延迟释放路径被真实走到） |
| 8 | 负向验证 | `514 passed, 3 FAILED`（前三条用例精确变红）⇒ 还原后 `ALL 517 TESTS PASSED` |

施工中被门槛拦下并当轮修复的两处：

- **门槛 2 首跑失败**：`audit_comment_language.py` 报出 test_mm.rs 内一处纯英文注释 ⇒ 改为中文（F7）。
- **门槛 2 次跑失败**：clippy `unfulfilled_lint_expectations`（`handle_file_fault` 的 `#[expect(clippy::single_match_else)]` 因 D-3 改了 match 形态而失效）⇒ 删除该 expect。

## 6. 登记项（只报不动）

1. **`MAP_SHARED` 脏页写回未实装**（§2.2 F2）：`dirty` 只置标志，无回写 inode/文件的实现；本工程只保证引用模型正确，不实装写回。属独立工作面（需 fs 侧 `writeback` 入口 + 回写时机策略）。
2. **`pcache_get` 的 functions re-export 零调用者**：`src/kernel/functions/mm/pcache.rs` 的**五个**转发函数（`pcache_get` / `pcache_lookup` / `pcache_mark_dirty` / `pcache_put` / `pcache_invalidate_inode`）在 functions 侧无调用点（`pcache_invalidate_inode` 的实际调用者是 privileged 的 [handle.rs:178](../../src/kernel/privileged/fs/vfs/handle.rs#L178)）。属 `eliminate-parallel-implementations.md` 范畴。**已由 [duplicate-impl-convergence.md](./duplicate-impl-convergence.md) D-5 承接处置**：该文件与其 `pub mod pcache;` 声明已删除（零调用者 + 能力等价入口存在 + 非 API/FFI/feature/硬件原语面），functions 对 pcache 的唯一路径为 `privileged::mm::pcache`。原文「六个」系笔误，实测为五个。
3. **无「纯缓存引用」⇒ 无驱逐策略**（§2.3）：条目一旦创建即被映射持有，故 pcache 无独立容量上限与 LRU/内存压力驱逐；`invalidate_inode` 在修复后退化为守卫。若将来引入「无映射持有者的缓存页」（如为 `vfs_read` 快路径预取），须同时引入驱逐策略与条目自持引用语义。
4. **旧帧归零释放的形态重复**：`frame_dec` 归零后的「x86_64 `defer_free` / aarch64 立即 `free_page`」形态在 [page_fault.rs:392-400](../../src/kernel/privileged/mm/page_fault.rs#L392-L400)（MAP_PRIVATE COW 换叶）、[cow.rs:457-481](../../src/kernel/privileged/mm/cow.rs#L457-L481)、[vmm_x86_64.rs:1443-1495](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1443-L1495) 与 [vmm_aarch64.rs:1248-1265](../../src/kernel/privileged/mm/vmm_aarch64.rs#L1248-L1265) 各有一份。本工程按 D-2 口径**新增一份**（[pcache.rs `release_frame_after_last_holder`](../../src/kernel/privileged/mm/pcache.rs)）——原因：该处被替换的是「同 VA 悬留的旧缓存帧」，与 acquire 的登记点同属 pcache 的持有模型，放在 pcache 内可保持「判据与释放同处一份」；D-3 **未删除** COW 路径的副本（那里注销的是 MAP_PRIVATE 换出时的映射持有，语义不同）。故形态重复由 4 处增至 5 处，收敛属 `eliminate-parallel-implementations.md` 范畴。**已由 [duplicate-impl-convergence.md](./duplicate-impl-convergence.md) D-1..D-4 承接收敛**：实测站点为 8 处（比本条目统计的 5 处更多），已全部收敛到 `privileged/mm/mod.rs` 的双入口 `release_frame_locked`（要求已持 VMM_LOCK）/ `release_frame`（自持锁），本工程新增的 `release_frame_after_last_holder` 亦随之删除；同时修掉 `pcache::deref` 归零释放遗漏延迟释放的缺陷（该缺陷与本条目的形态重复同源，属预存问题）。
5. **`deref` 与 `invalidate_inode` 的锁粒度**：[pcache.rs:322-327](../../src/kernel/privileged/mm/pcache.rs#L322-L327) `pcache_invalidate_inode` 遍历全部 64 桶、每桶单独加解锁，单次关闭无原子快照语义（并发缺页可能在桶间穿插）。当前单核启动路径下不可观测，属独立议题。
6. **`VmaType::FileBacked` 路径整体零用户调用者**（§2.2 F1）：接线到用户态（用户程序 + 构建配方）属独立工作面；接线后 D-6 的 kernel 自测应升级为 Ring 3/EL0 端到端载体。
7. **测试注册表容量已满导致的静默丢弃（本工程发现并当轮修复，留档备查）**：[tests/mod.rs](../../src/kernel/privileged/tests/mod.rs) 的 `MAX_TESTS` 原为 512，而修复前**期望**注册总数实为 513 ⇒ 长期有 1 条用例被静默丢弃（表尾 `syscall::splice::einval_bad_flags`）；本工程 +4 条后又把 `timerfd::settime_disarm` / `timerfd::read_empty` / `syscall::sendfile::ebadf` / `syscall::splice::einval_no_pipe` 挤出。已扩容至 640 并给 `register` 加满容量 `assert!`（fail-closed）。后续若注册总数逼近 640，须先上调容量而非依赖 assert 拦下。