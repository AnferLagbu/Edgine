# 决策 D 预研：x86_64 kview 低半区恒等窗口退役

> 状态：预研中，未施工。原决策见 [userspace-decoupling.md](userspace-decoupling.md) 的「决策 D」，仍为 `[]`；本文件只做设计、路径盘点和验收定义，不改行为。

## 预研结论

- **描述**：K-08 的直接来源不是“内核镜像体积预算”，而是 x86_64 kview 低半区 `[0, ceil(kernel_end/2MiB))` 的 boot 恒等窗口。该窗口与用户 VA 下界 `USER_CODE_BASE=0x400000` 在 4 MiB 处重合，才产生 `kernel_end <= USER_CODE_BASE` 的硬约束。
- **方案**：预研把决策 D 拆成两条路线，避免把“退役恒等窗口”和“退役 kview 用户镜像”混成一个批次。
  - **D-1（最小路线）**：kview 保留用户映射镜像，但**不再复制 boot 恒等窗口**；内核镜像只经高半区直射区访问。改动集中在 kview 构建/同步、`user_va_lower_bound` 与 K-08 断言。它直接消除 K-08，不拆 kview 本身。
  - **D-2（完整路线）**：`copy_*_user` / `UserReadPtr` / `UserWritePtr` 也改走页表遍历 + `phys_to_virt`，不再依赖 kview 用户镜像。它触及所有用户指针包装和调用面，风险与跨度显著大于 D-1。
- **状态**：`[]`。建议先评审 D-1；只有 D-1 被证明不可行（存在必须经低 VMA 访问内核映像的路径），或后续明确要整体退役 kview 时，才启动 D-2。
- **详情**：K-08 的根因是恒等窗口，不是 `copy_*_user` 的访问方式。`copy_*_user` 当前依赖的是 kview 的**用户映射镜像**（`boundary..512` 的 PDE），不是 `[0,boundary)` 的恒等映射。把这两件事分开，D-1 可以用小得多的改动面完成决策 D 的核心目标。

## 路径清单

| 路径 | 当前行为 | D-1 影响 | D-2 影响 |
|---|---|---|---|
| [`copy_from_user`](../../src/kernel/privileged/mm/copy_user.rs#L143) / [`copy_to_user`](../../src/kernel/privileged/mm/copy_user.rs#L189) | 先 `validate_user_*`，再直接按用户 VA `copy_nonoverlapping` | 不变 | 改为逐页翻译 + 直射区拷贝 |
| [`copy_string_from_user`](../../src/kernel/privileged/mm/copy_user.rs#L224) / [`strlen_user`](../../src/kernel/privileged/mm/copy_user.rs#L305) | 逐字节读用户 VA | 不变 | 改为逐页翻译后读直射区 |
| [`clear_user`](../../src/kernel/privileged/mm/copy_user.rs#L275) | 先写权限预检，再按用户 VA 清零 | 不变 | 改为逐页翻译 + 直射区清零 |
| [`UserReadPtr` / `UserWritePtr` / `UserRefMut`](../../src/kernel/privileged/userptr.rs#L45) | 封装用户 VA 为 `&[u8]` / `&mut [u8]` | 不变 | 不能继续直接暴露用户 VA 切片；需要 bounce buffer 或新的拷贝型 API |
| [`validate_user_buf`](../../src/kernel/privileged/userptr.rs#L269) / [`validate_user_writable`](../../src/kernel/privileged/userptr.rs#L302) | 当前进程用户根表逐页 walk | 可复用 | 可作为翻译前的映射/可写预检 |
| [`write_struct_to_user`](../../src/kernel/privileged/userptr.rs#L359) / [`read_struct_from_user`](../../src/kernel/privileged/userptr.rs#L382) | 预检后 `write_unaligned` / `read_unaligned` 用户 VA | 不变 | 改为翻译 + 直射区访问 |
| [`page_fault::validate_user_writable`](../../src/kernel/privileged/mm/page_fault.rs#L792) | 逐页确认用户 leaf 可写，COW 只读页先破 COW | 不变 | 必须保留，并补翻译后的页生命周期约束 |
| [`Vmm::translate_in_pml4`](../../src/kernel/privileged/mm/vmm_x86_64.rs#L553) | 已支持 4 KiB / 2 MiB / 1 GiB 叶子翻译，返回 PA 与可写位 | 可复用 | D-2 的直接构建块 |
| [`boot_identity_tables`](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1166) | 从 boot PML4/PDPT/PD 取 `[0,boundary)` 恒等链 | 删除或收窄为 boot-only | 删除 kview 侧使用 |
| [`build_kernel_view_for_user_root_locked`](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1218) | 私有 PD 复制 boot 全量恒等，再镜像用户 PDE `boundary..` | 改为零初始化 + 全量镜像用户 PDE | 进一步可缩减用户镜像 |
| [`sync_kernel_view_locked`](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1333) | 用户条目拆除时恢复 boot 恒等条目 | 改为写 0，不再恢复恒等 | 同步策略随 kview 形态调整 |
| [`user_va_lower_bound`](../../src/kernel/privileged/mm/mod.rs#L272) | x86 返回 `max(恒等窗口上界, USER_CODE_BASE)` | x86 只返回 `USER_CODE_BASE` | 不变 |
| [K-08 启动断言](../../src/kernel/privileged/boot/mod.rs#L638) | `kernel_end <= USER_CODE_BASE`，越界 fail-closed | 删除；改由用户 VA 下界和映射测试兜底 | 删除 |
| [`DIRECT_MAP_LIMIT`](../../src/kernel/privileged/mm/mod.rs#L250) | x86 直射区覆盖 `[0,4GiB)` | 不变 | 超过 4 GiB 的 PA 需拒绝或扩展直射区 |

## D-1 设计草案

- **描述**：保留 kview 作为“本进程在内核态运行时的 CR3”，但低半区不再保留任何 boot 恒等映射；kview 低半区只镜像当前用户页表。
- **方案**：
  - `build_kernel_view_for_user_root_locked`：私有 PD/PDPT 先清零；`PDPT[0]` 指向私有 PD；用户 PML4E `0..256`、用户 PDPTE/PDE 全部按 present 镜像，不再复制 `boot_pd` / `boot_pdpt[1..]`。
  - `sync_kernel_view_locked`：用户条目拆除时写 0，不恢复 boot 恒等；不再需要 `boot_pd` / `boot_pdpt` 参数。
  - `boot_identity_tables`：只保留 boot 期使用；若 kview 是唯一消费者，可整体删除。
  - `user_va_lower_bound`：x86_64 返回 `USER_CODE_BASE`，与 aarch64 同口径。
  - K-08 启动断言：删除；`kernel_end` 仍可作为 PMM 预留和镜像起止的单一读数保留。
- **状态**：`[]`，待设计评审。
- **详情**：D-1 成立的前提是“内核运行期不存在必须经低 VMA 访问内核映像的路径”。现有 x86 内核文本有高半区别名（`KERNEL_BASE` 直射区，见 [mm/mod.rs](../../src/kernel/privileged/mm/mod.rs#L234)），但必须用一次全仓审计证明没有残留的低地址取指/取数；这是 D-1 的第一道 go/no-go 门。

### D-1 的开放问题

- 用户 1 GiB 大页：当前 `build_kernel_view` 在用户 `PDPT[0]` 为 1 GiB 大页时保留 boot 恒等（`SIMPLIFIED`）。D-1 要么把用户 1 GiB 大页按 PDPTE 整体镜像，要么明确拒绝用户 1 GiB 大页；不能继续用 boot 恒等兜底。
- 低 VMA 残留：启动 trampoline、`boot.asm`、IDT/GDT 早期路径、`_kernel_end` 物理读数、PMM 预留表都要逐项确认只在 boot CR3 使用，不在 kview 下解引用。
- KPTI 交互：kview 仍是非用户 CR3；KPTI 用户根表不复制 kview 的低半区用户镜像，需确认 D-1 不改变 `PML4[511]` 互指与 CR3 切换契约。
- 同步与 TLB：用户 unmap 后 kview 对应 PDE 必须立即写 0，并按现有 remote TLB 失效路径处理；不能留下指向已释放用户页表的悬垂项。

## D-2 设计草案

- **描述**：把内核访问用户内存的方式从“kview 低半区用户 VA 直读”改为“当前用户页表 walk 得到 PA，再经 `phys_to_virt(PA)` 访问”。
- **方案**：
  - 单页翻译复用 `Vmm::translate_in_pml4(get_current_pml4(), VirtAddr(va))`；按 4 KiB / 2 MiB / 1 GiB 叶子返回 PA 与写位。
  - 区间访问按页循环；跨页、跨大页边界拆分；PA 必须小于 `DIRECT_MAP_LIMIT`，否则返回 `EFAULT` 或走受限慢路径。
  - 写路径先复用 `validate_user_writable` 的 COW 预检，再翻译；读路径复用 `validate_user_buf`。
  - `copy_string_from_user` / `strlen_user` 逐页翻译后在直射区读字节；`clear_user` 逐页翻译后清零。
  - `UserReadPtr` / `UserWritePtr` 不能再把用户 VA 直接暴露为切片；改为拷贝型 API（bounce buffer 或 `read_*` / `write_*` 迭代器），这是一次跨文件系统、网络、IPC 的 API 迁移。
- **状态**：`[]`，仅在 D-1 不可行或明确要整体退役 kview 时启动。
- **详情**：D-2 的核心风险不是页表 walk 本身，而是 **PA 生命周期**。翻译得到 PA 后，另一线程可能 unmap/COW/释放该页；直射区访问仍会读写已释放或复用帧。D-2 必须引入页 pin/refcount 或等价的“翻译-访问”原子化契约，不能只做 `translate + copy_nonoverlapping`。

### D-2 的开放问题

- 页 pin：现有 PMM/COW 是否支持临时引用计数？若没有，先补 `get_page` / `put_page` 语义，再接 uaccess。
- 并发 unmap / COW：`validate_user_writable` 与真正拷贝之间仍有窗口；D-2 需要锁、pin 或重试协议。
- SMAP：直射区页是 supervisor 映射，SMAP 不拦；但仍要保证不把用户可控 VA 当内核 VA 解引用。
- 性能：逐页 walk 会放大 syscall 热路径成本；需要 baseline 与绝对预算，而不是只看正确性。
- 4 GiB 直射区上限：x86 当前 `DIRECT_MAP_LIMIT=4GiB`；大内存机器的 PA 可能超出，D-2 必须定义拒绝或扩展策略。

## 风险登记

| 风险 | 路线 | 后果 | 前置防线 |
|---|---|---|---|
| 低 VMA 内核访问残留 | D-1 | kview 下取指/取数缺页，启动或 syscall 崩溃 | 全仓低地址审计 + QEMU 全腿 + `boot_identity_tables` 删除后编译 |
| 用户 1 GiB 大页 | D-1 | kview 低半区无法按 PDE 镜像，可能回退恒等或漏映射 | 明确拒绝或按 PDPTE 镜像，补形状测试 |
| 用户条目拆除后 kview 悬垂 | D-1 | 内核访问已释放用户页表 | `sync_kernel_view` 写 0 + remote TLB 失效回归 |
| PA 生命周期竞态 | D-2 | 直射区读写已释放/复用帧 | 页 pin/refcount + 并发 unmap/COW 压力腿 |
| 直射区上限 | D-2 | 高物理地址不可访问 | `PA < DIRECT_MAP_LIMIT` 硬判据 + 边界测试 |
| 用户切片 API 迁移 | D-2 | `UserReadPtr` 调用面大面积改动 | 先做 API 盘点，不直接改调用方 |

## 验收矩阵

### D-1

- x86_64 默认镜像启动：`./scripts/qemu_boot_test.sh x86_64` 全绿，K-08 断言删除后仍能进入 Ring 3。
- `boot_identity_tables` 不再被 kview 构建/同步引用；`sync_kernel_view` 拆除用户条目后 kview 对应项为 0。
- `user_va_lower_bound()` 在 x86_64 上只返回 `USER_CODE_BASE`；补 host 形状锁。
- 用户映射/缺页/COW/fork/signal/线程压力：`make test-host`、`make test-kernel-host`、`qemu_boot_test.sh all`、full audit 全绿。
- glibc G0–G4 与 musl 动态装载腿不回退；`set_tid_address`、`prlimit64`、`clone3`、`dlopen` 不回归。
- 低 VMA 审计清单为零命中；若发现残留，D-1 停在设计阶段，不进入施工。

### D-2

- 在 D-1 全部验收之上，增加 `copy_*_user` 坏指针、跨页、跨 2 MiB/1 GiB 边界、COW、并发 unmap、页释放复用压力腿。
- `UserReadPtr` / `UserWritePtr` 调用面全部迁移并有编译期/运行期覆盖。
- 性能绝对预算不回归；直射区上限边界有正向/反向用例。
- 任何 PA 生命周期竞态用例失败即阻断 D-2，不允许以“概率低”降级。

## 决策点

- **待用户裁定 A**：是否接受 D-1 作为决策 D 的最小闭环；若接受，D-2 只作为后续可选形态，不阻塞 K-08 解除。
- **待用户裁定 B**：D-1 的施工窗口。当前 K-08 余量已被 klog 环切片拉回 64 KiB，可先继续 D7 剩余项；D-1 建议在 D7 收口后单开批次，除非余量再次低于 32 KiB。
- **待用户裁定 C**：用户 1 GiB 大页的处置——拒绝，还是按 PDPTE 镜像。该项不做，D-1 不能开工。
- **待用户裁定 D**：是否保留 `boot_identity_tables` 作为 boot-only 只读诊断接口；若保留，必须证明 kview 不再消费。
