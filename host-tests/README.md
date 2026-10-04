# host-tests 测试框架指南

> Edgine host 端测试规范. 所有 `host-tests/tests/*.rs` 集成测试 + `host-tests/src/*.rs` 单元测试 + `host-tests/benches/*.rs` 性能基准均适用.
>
> **新建测试前必读本文**; **修改测试组织前必读本文**; **评审 PR 测试代码时必读本文**.

## 仓库定位

`host-tests` 是 Edgine 的 host 端验证 crate, 承担三类内容:

| 类型 | 位置 | 入口 |
|------|------|------|
| 单元测试 | `host-tests/src/<module>.rs` 内联 `#[cfg(test)] mod tests` | `cargo test -p edgine-host-tests --lib` |
| 集成测试 | `host-tests/tests/<scope>_<feature>_<kind>_test.rs` (70 个文件) | `cargo test -p edgine-host-tests --test <name>` |
| 性能基准 | `host-tests/benches/<scope>_bench.rs` (Cargo 官方, 待迁移) | `cargo bench -p edgine-host-tests` |

> **不要**把单元测试 + 集成测试 + 性能基准混放在一个 `tests/*.rs` 文件, Cargo 自动发现会让 `tests/*.rs` 编译为独立 binary, 极慢.

## 命名规范 (硬约束)

### 1. 集成测试文件

**格式**: `<scope>_<feature>_<kind>_test.rs` (snake_case, 全小写)

| 段 | 含义 | 示例 |
|----|------|------|
| `<scope>` | 子系统或模块缩写 | `vfs` / `net` / `mm` / `driver` / `proc` / `signal` / `audit` / `klog` / `elf` / `unkfs` |
| `<feature>` | 功能点 | `close_atomic` / `socket_wait_queue` / `iomem_alias` / `e1000_eeprom` |
| `<kind>` | 测试类型 (可选) | `test` (静态契约) / `e2e` (端到端) / `persist` (持久化) / `stress` (压力) |

**示例**:
- `vfs_close_atomic_test.rs` — VFS 关闭路径原子化
- `net_socket_wait_queue_test.rs` — socket 等待队列
- `driver_display_test.rs` — 显示器驱动
- `driver_e1000_eeprom_test.rs` — e1000 EEPROM 读取
- `mm_iomem_alias_test.rs` — IoMem 别名检测
- `unkfs_stress_test.rs` — UNKFS 压力
- `unkfs_persist_test.rs` — UNKFS 持久化往返
- `audit_comment_language_test.rs` — 注释语言审计契约

**禁用**:
- ❌ 无 `<scope>` (如 `display.rs`)
- ❌ 多 TD 编号塞一文件 (如 `td11_12_13_klog_cleanup_test.rs`)
- ❌ V1/V2 后缀 (如 `td09_v2_*.rs`, 新需求新建文件不写 v2)
- ❌ `_retired` 后缀 (退役测试直接删除, 不留空壳)
- ❌ `_unify` 与 `_unified` 重复 (选一个)

### 2. 单元测试模块

内联在 `host-tests/src/<module>.rs`:

```rust
//! <模块简述>
#![allow(...)]  // 仅在测试模块内 allow clippy lint

#[cfg(test)]
mod tests {
    use super::*;

    /// <测试名> — <一句话说明>
    #[test]
    fn <feature>_<expectation>() {
        // ...
    }
}
```

### 3. 性能基准

放在 `host-tests/benches/<scope>_bench.rs`, 使用 `criterion` 或等效库:

```rust
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_<feature>(c: &mut Criterion) {
    c.bench_function("<scope>::<feature>", |b| b.iter(|| { /* ... */ }));
}

criterion_group!(benches, bench_<feature>);
criterion_main!(benches);
```

**不要**用 `src/bin/framekernel_bench.rs` 跑性能测试, Cargo 不将其视为 `bench`, 会被 CI 跳过.

## 头文件规范 (硬约束)

每个测试文件首行必须满足 (按顺序):

```rust
//! <scope>: <简述>
//!
//! 验收:
//!   - <验收点 1>
//!   - <验收点 2>
//!   - ...
//!
//! 追踪: <追踪编号> (I-XX / P0-I-XX / P1-I-XX / P2-I-XX / P3-I-XX / TD-XX / B2.1 / W6 / DECISION-NNN)
//! SPDX-License-Identifier: MPL-2.0

#![allow(dead_code)]  // 仅当有 helper 函数但非测试路径时
```

**硬要求**:
- ✅ 全部用 `//!` (内联文档注释), 不用 `//`
- ✅ 全部带 `SPDX-License-Identifier: MPL-2.0` (auto-format by CI)
- ✅ 显式列"验收点", 评审时按点对
- ✅ "追踪"链接到具体维护条目, 便于历史回溯

## 测试分类索引

### 单元测试 (1 模块, 内联)

| 模块 | 内容 | 位置 |
|------|------|------|
| `dma_stream` | DMA 状态机 + 校验 | `src/dma_stream.rs` |

> H-04 (2026-09-09): `buddy` 平行实现已删除 — buddy 测试移至集成测试
> `pmm_buddy_host_test.rs` (经 `MetaStore` 注入 `VecMetaStore` 驱动内核真实 pmm).

### 集成测试 (按 scope 分类)

#### VFS / 文件系统

| 文件 | 内容 | 追踪 |
|------|------|------|
| `vfs_close_atomic_test.rs` | VFS 关闭路径原子化 | TD-03 |
| `fs_sync_trait_test.rs` | FileSystem trait 同步方法 | P3-I-18 |
| `mmap_pwm_test.rs` | Vma.file_pwm 桥接 | B2.1 |
| `i43_block_bridge_test.rs` | 块设备单一桥接入口 | I-43 |
| `td21_early_vfs_eacces_test.rs` | early VFS EACCES | TD-21 |
| `unkfs_test.rs` | UNKFS 综合集成 | I-05 |
| `unkfs_e2e_test.rs` | UNKFS 端到端 (host 端) | I-05 |
| `unkfs_persist_test.rs` | UNKFS 持久化往返 | I-05 |
| `unkfs_stress_test.rs` | UNKFS 压力 (CAS/ZAP/ZIL) | I-05 |
| `unkfs_trait_abstract_test.rs` | UNKFS 18 文件强耦合 trait 化 | I-04 |
| `memory_pressure_extraction_test.rs` | Memory Pressure 策略提取 | P1-I-01 D9 |
| `fd_table_extraction_test.rs` | FdTable 策略提取 | P1-I-01 |
| `fd_allocator_unified_test.rs` | 统一 FdAllocator (含 I-51 UDS/smoltcp 不重叠验收, 已合入) | TD-02 |
| `td04_stale_fd_test.rs` | 过期 FD 检测 | TD-04 |
| `td05_cache_align_test.rs` | 缓存对齐 | TD-05 |
| `td06_max_sm_fd_test.rs` | smoltcp FD 上限 | TD-06 |
| `td07_slab_buf_test.rs` | Slab 缓冲区 | TD-07 |
| `td15_fd_idx_of_test.rs` | fd_alloc::idx_of 集中反查 | TD-15 |
| `td18_fs_kernel_error_test.rs` | FS KernelError 迁移 | TD-18 |
| `fs_permissions_regression_test.rs` | B06-02/03/07 权限与句柄回归 | B06-02/03/07 |

#### 网络

| 文件 | 内容 | 追踪 |
|------|------|------|
| `dhcp_fallback_const_test.rs` | DHCP fallback 静态 IP 集中常量 | I-46 |
| `dhcp_policy_test.rs` | DHCP 策略 trait 抽象 | W6 |
| `driver_e1000_eeprom_test.rs` | e1000 EEPROM 读取 (QEMU + 真硬件双路径) | I-40 |
| `net_socket_wait_queue_test.rs` | socket 等待队列 | I-50 |
| `socket_max_sockets_test.rs` | socket 上限 | I-50 |
| `smoltcp_vendored_test.rs` | smoltcp 0.13 vendored 锁定 | REVAL-W |
| `smoltcp_transmute_test.rs` | smoltcp 0 transmute 验证 | REVAL-W |
| `nic_probe_arch_neutral_test.rs` | NIC 探测架构无关 | I-37 |
| `virtio_net_arch_unify_test.rs` | virtio-net 架构统一 | I-37 |
| `net_snapshot_test.rs` | 网络快照 (net_save/net_restore) | P2-I-44 |
| `net_ipv6_addr_test.rs` | IPv6 地址抽象类型契约 (Ipv6Addr/IpAddr/Ipv6Cidr) | DECISION-032 |
| `net_sockaddr_in6_test.rs` | sockaddr_in6 结构 + FFI 翻译层契约 | DECISION-032 |
| `net_dual_stack_socket_test.rs` | V4/V6 双栈 socket 契约 (sm_socket/分流/28B 缓冲) | DECISION-032 |
| `driver_e1000_eeprom_test.rs` | e1000 EEPROM 读取 | I-40 |

#### 进程 / 调度

| 文件 | 内容 | 追踪 |
|------|------|------|
| `scheduler_mlfq_retired_test.rs` | 调度器无 MLFQ/CFS 冗余 (已退役 MLFQ) | I-35 |
| `session_per_process_test.rs` | session 每进程 | I-29 |
| `usermode_ring3_test.rs` | usermode ring3 切换 | I-32 |
| `eash_cmd_parser_test.rs` | eash 用户态 Shell | I-10 |
| `cfs_btreemap_bench_test.rs` | CFS BTreeMap 性能基准 | I-34 |
| `td14_ipc_full_lifecycle_test.rs` | IPC 全生命周期 | TD-14 |
| `services_ipc_complete_test.rs` | services IPC 完整性 | I-14 |

#### 信号

| 文件 | 内容 | 追踪 |
|------|------|------|
| `execve_signal_state_test.rs` | execve 后信号状态重置 | I-48 |
| `sigaltstack_test.rs` | sigaltstack | I-31 |
| `sigreturn_trampoline_test.rs` | sigreturn trampoline | I-31 |
| `td22_sigill_delivery_test.rs` | SIGILL 投递 | TD-22 |
| `td23_sigaltstack_syscall_test.rs` | sigaltstack 系统调用 | TD-23 |
| `zombie_signal_boundary_test.rs` | 僵尸进程信号边界 | I-31 |

#### 内存管理

| 文件 | 内容 | 追踪 |
|------|------|------|
| `demand_paging_test.rs` | Demand Paging 模型 | P0-I-26 / B13-FL-01 |
| `page_fault_vma_flags_test.rs` | 缺页 VMA flags | I-26 |
| `copy_user_exception_test.rs` | exception table 缺失修复 | P0-I-36/37/38 |

#### 驱动 / 中断

| 文件 | 内容 | 追踪 |
|------|------|------|
| `driver_display_test.rs` | 显示器驱动 (PixelFormat/HDMI/DP) | I-22 |
| `nvme_ahci_activation_test.rs` | NVMe/AHCI 激活 | DRIVER-2 |
| `pic_spurious_irq_test.rs` | PIC spurious IRQ | I-23 |
| `idt_ist_validation_test.rs` | IDT IST 验证 | I-24 |
| `kmalloc_irq_save_test.rs` | kmalloc/slab 中断安全 | P1-I-28 |
| `mm_iomem_alias_test.rs` | IoMem 别名检测 (16 用例) | I-25 |

#### ELF / 进程加载

| 文件 | 内容 | 追踪 |
|------|------|------|
| `elf_loader_racy_cell_test.rs` | ELF loader RacyCell 静态分配器 | P1-I-32 |
| `elf_verify_unification_test.rs` | ELF 验证双份复制修复 | P1-I-33 |
| `exec_rollback_test.rs` | execve transactional | P0-I-31 |
| `test_runner_init_test.rs` | test_runner_init init_global | I-45 |
| `feature_attr_minimal_test.rs` | nightly API 最小化 | I-09 |

#### Klog

| 文件 | 内容 | 追踪 |
|------|------|------|
| `td09_log_sink_test.rs` | klog 多 sink 抽象 + 注册表 | TD-09 |
| `td09_v2_klog_sinks_procfs_test.rs` | (V2 已合入 v1, 退役) | TD-09 |
| `td10_utime_stime_test.rs` | klog uptime/startup time | TD-10 |
| `td11_12_13_klog_cleanup_test.rs` | idt/timer klog 清理 | TD-11/12/13 |
| `td24_netclock_hrtimer_test.rs` | netclock hrtimer | TD-24 |

#### 错误处理

| 文件 | 内容 | 追踪 |
|------|------|------|
| `td08_kernel_error_test.rs` | KernelError 迁移 (顶层) | TD-08 |
| `td16_signal_kernel_error_test.rs` | signal KernelError | TD-16 |
| `td17_table_kernel_error_test.rs` | table KernelError | TD-17 |
| `td19_proc_kernel_error_test.rs` | proc KernelError | TD-19 |
| `td20_services_kernel_error_test.rs` | services KernelError | TD-20 |

#### 审计契约

| 文件 | 内容 | 追踪 |
|------|------|------|
| `audit_comment_language_test.rs` | 注释语言一致性 audit | TD-24 / TD-25 |
| `safety_boilerplate_test.rs` | SAFETY 注释 boilerplate | I-04 |

#### 杂项

| 文件 | 内容 | 追踪 |
|------|------|------|
| `ioctl_enosys_test.rs` | sys_ioctl 行为契约 | P1-I-39 |
| `framework_spinlock_migration_test.rs` | framework spin::Mutex 迁移 | P1-I-17 |

## 当前规模

| 类别 | 数量 | 行数 |
|------|------|------|
| 集成测试 | 70 文件 (从 73 - 3) | ~11,200 |
| 单元测试模块 | 5 | ~800 |
| 公共库代码 | unkfs + unkfs_mock + framekernel_bench | ~6,000 |
| 性能基准 | 1 bin (待迁 benches/) | - |

**对比规范化前**:
- 73 → 70 文件 (-3 重复/退役)
- 4 无后缀 → 0 无后缀 (重命名 driver_/mm_/unkfs_ scope)
- 新增 README.md 索引
- 5 文件补 SPDX + 追踪号

## 运行命令

```bash
# 跑全部测试
make test-host

# 跑单个集成测试
cargo test -p edgine-host-tests --test vfs_close_atomic_test

# 跑单个单元测试
cargo test -p edgine-host-tests --lib dma_stream

# 跑 PMM buddy host 集成测试 (H-04)
cargo test -p edgine-host-tests --test pmm_buddy_host_test

# 跑特定测试函数
cargo test -p edgine-host-tests --test net_socket_wait_queue_test socket_wait_queue_basic

# 性能基准
cargo bench -p edgine-host-tests

# 静态契约测试 (依赖源文件存在)
cargo test -p edgine-host-tests --test audit_comment_language_test
```

## 添加新测试流程

1. **确定 scope**: 选好子模块缩写 (`vfs`/`net`/`mm`/...)
2. **命名文件**: `<scope>_<feature>_<kind>_test.rs`
3. **写头注释**: 按 [头文件规范](#头文件规范-硬约束)
4. **加追踪号**: 关联到 `maintenance-cycle-*.md` / `engineering-discipline.md` 维护条目
5. **写测试函数**: 3 段式 (arrange/act/assert) + 中文注释
6. **本地跑通**: `cargo test --test <name>`
7. **更新本 README 索引**: 在对应 scope 段加一行
8. **提交 PR**: 标题格式 `test(<scope>): <feature> <action>`

## 退役测试流程

**禁止** 留 `_retired_*` 空壳测试. 流程:
1. 把过期测试的 `#[test]` 函数加 `#[ignore]` 并加注释"已退役, 保留以防回滚"——不推荐
2. **推荐**: 直接删除, 在 commit message 写明废弃原因
3. 如果有真实回归风险, 把测试函数改名为 `<feature>_regression_<bug_id>`, 持续验证
4. 退役原因写入 commit message (Edgine 不维护独立 `CHANGELOG.md`, 见 DECISION-038)

## 重复测试处理

发现两份测试覆盖同一特性:
1. 选最新 (覆盖更全) 的那份保留
2. 把旧版的独有用例合并到新版 (避免测试缺失)
3. 删除旧版
4. 合并原因写入 commit message (Edgine 不维护独立 `CHANGELOG.md`, 见 DECISION-038)

## 测试架构原则

- **Arrange-Act-Assert**: 每个测试 3 段式
- **一测一断言**: 一个 `#[test]` 函数验证一个明确行为
- **测试公共 API**: 通过 `pub fn` 验证, 不依赖内部字段
- **测用户可见行为**: 用户视角, 不测实现细节
- **清理资源**: fd / 临时文件 / 子进程 / 全局状态 必须清理 (`defer` / `Drop`)
- **不要 `unwrap()`** 在可失败路径 (生产代码规则同样适用)
- **中文注释**强制 (与 src/kernel 同步, `audit_comment_language.py` 检测)

## 关联文档

- [AGENTS.md](../AGENTS.md) §8 测试规范
- [docs/plan/archive/engineering-discipline.md](../docs/plan/archive/engineering-discipline.md) 工程纪律 (已归档)
- [docs/plan/archive/maintenance-cycle-2026-06-19.md](../docs/plan/archive/maintenance-cycle-2026-06-19.md) 维护条目索引 (已归档)

## 变更历史

- **初始版本**: 索引 72 集成测试 + 5 单元测试 + 1 基准 + 命名/头文件/分类/退役/重复处理 5 项规范 (当前)
