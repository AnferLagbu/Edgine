//! Framekernel 微基准测试 (性能回归基线)
//!
//! ## 目标
//! 测量 Edgine 框内核关键路径的纯算法性能, 建立可重复的回归基线.
//! 所有实现都 host-runnable (std 可用), 与内核版本位一致, 便于:
//! - CI 跑回归检查 (vs. baseline.json)
//! - 优化前后对比
//! - 论文性能数据采集
//!
//! ## 覆盖的热点路径
//! 1. `page_flags_bench`: PageFlags 位运算 (PRESENT/WRITABLE/USER/NX)
//! 2. `pte_set_flags_bench`: PageTableEntry.set_flags 原子位操作
//! 3. `iomem_alias_bench`: IoMem 别名区间注册 (重叠检测, 内核 `IoMem::new`)
//! 4. `capability_check_bench`: 能力矩阵域位检查 (内核 `PolicyEngine::check`)
//! 5. `dma_state_machine_bench`: DmaStream 状态机迁移
//! 6. `sha256_block_bench`: SHA-256 哈希 (sgeg 身份, 内核 `sha256`)
//! 7. `attribution_classify_bench`: 故障归属分类 (freg)
//! 8. `recovery_decide_bench`: 恢复策略决策 (freg)
//! 9. `bitmap_scan_bench`: PMM 物理页分配 (内核 buddy alloc/free)
//!
//! ## 输出
//! stdout 单行 JSON:
//!   `{"version": 1, "results": [{"name": "...", "ns_per_op": ...}, ...]}`
//!
//! ## 集成
//! - `scripts/record_bench_baseline.py` 生成 baseline.json
//! - `scripts/check_bench_regression.py` 对比并报告 > 15% 退化
//! - `make -f Makefile.ci bench-baseline` / `bench-check`

// G-07 (2026-09-06): 原 `#![allow(dead_code)]` (F9 违规) 删除 — 实测移除后 0 个
// dead_code 警告 (全部 23 个 bench 均被 run_all() 与 tests 使用, 属防御性历史残留).

use std::time::Instant;

// ====== A 类: 内核真实实现直引 (G-07 消除 host 侧平行实现) ======
//
// 以下 bench 组不再本地复刻算法, 改为直接引用内核真实源码 (经 edgine 壳 crate 的
// host-test feature 暴露面), 与 `src/kernel/**` 位一致. 各 bench 仅保留计时骨架,
// 计算本身完全由内核实现承担 — 平行复刻体已删除.
use edgine::kernel::framework::debug::{
    BpfInsn, BpfProg, BpfProgType, BpfVerifier, VerifyResult, opcode,
};
// `BpfSubsystem` 仅单测使用 (bench 体走 `&dyn BpfVerifier`), 故 cfg(test) 门控.
#[cfg(test)]
use edgine::kernel::framework::debug::BpfSubsystem;
use edgine::kernel::framework::dma_buf::{DmaDirection, DmaStream, SyncState};
use edgine::kernel::framework::frame::Frame;
use edgine::kernel::framework::mm::{PageFlags, PageTableEntry, PhysAddr};
use edgine::kernel::framework::net::wait_queue::{SocketWaitQueue, WakeReason};
use edgine::kernel::services::config::sysctl::{
    SysctlKind, SysctlValue, sysctl_register, sysctl_write,
};
use edgine::kernel::services::freg::attribution::{FaultAttribution, FaultAttributor, TcbModule};
use edgine::kernel::services::freg::recovery_policy::{
    FaultSignal, RecoveryAction, RecoveryPolicy,
};
// `sysctl_read`/`SysctlError` 仅单测使用 (bench 体只写), 故 cfg(test) 门控.
#[cfg(test)]
use edgine::kernel::services::config::sysctl::{SysctlError, sysctl_read};
use edgine::kernel::services::debug::ebpf_verifier::STANDARD_VERIFIER;

// ====== B 类: 内核真实实现直引 + 机制层载体注入 (G-07 消除 host 侧平行实现) ======
//
// 与 A 类同口径, 但以下热点需宿主载体 (host 无裸机 PMM/直映射) 才能运行内核真实实现:
// - `PhysicalMemoryManager` + `VecMetaStore`: buddy 分配/合并唯一实现 (载体注入模式,
//   与 `tests/pmm_buddy_host_test.rs` 一致)
// - `IoMem`: 别名注册表唯一公共入口 (与 `tests/mm_iomem_alias_test.rs` 一致)
// - `VirtQueue`: 描述符/环区操作用宿主堆块作 DMA 后备 (host 无 PMM, 见 §12 段注释)
use edgine::kernel::framework::driver::virtio::queue::{
    VQ_SIZE, VirtQueue, VqAvail, VqDesc, VqUsed, VqUsedElem,
};
use edgine::kernel::framework::sgeg::sha256::sha256;
// 描述符标志位仅单测断言使用 (bench 体经 `prepare_desc` 的 write 参数间接设置)
#[cfg(test)]
use edgine::kernel::framework::driver::virtio::queue::{VQ_DESC_F_NEXT, VQ_DESC_F_WRITE};
use edgine::kernel::framework::iomem::IoMem;
use edgine::kernel::framework::mm::pmm::{PhysicalMemoryManager, VecMetaStore};
use edgine::kernel::services::sgeg::policy::{
    CapBits, CapDomain, CapabilityMatrix, InMemoryMatrix, PolicyEngine, PolicyResult,
};

// ====== C 类: 内核真实实现直引 (G-07 遗留项: unkfs / egdf / epoll 策略面) ======
//
// 与 A/B 类同口径, 覆盖 G-07 遗留的三处平行实现:
// - `egdf::BlockDevice` + `EGDF_DEVICES` 注册表: 块设备边界检查与 dispatch
//   的唯一实现 (host 侧仅提供扇区存储载体 `BenchBlockDevice`)
// - `framework::fs::vfs_poll_trait` (机制) + services `StandardVfsPollPolicy` (策略):
//   epoll `check_fd_ready` 事件位决策的唯一实现
// - `unkfs::*`: Zap / TXG / DMU / SPA / RAID-Z / ARC / ZIL / ZIL-persist 八大子模块
//   的唯一实现, 本地 `HostXxx` trait + `StandardHostXxx` 复刻体已全部删除
use edgine::kernel::framework::egdf::{
    BlockDevice, egdf_blk_read, egdf_blk_write, egdf_register_block_dev,
};
// `egdf_blk_is_present` 仅单测断言使用 (bench 体只做读写)
#[cfg(test)]
use edgine::kernel::framework::egdf::egdf_blk_is_present;
use edgine::kernel::framework::fs::vfs_poll_trait::{
    EPOLLERR, EPOLLHUP, EPOLLIN, EPOLLOUT, VfsPollContext, VfsPollPolicyRef,
};
// `VfsPollPolicy` trait 仅单测直接调用策略方法时需在作用域
use edgine::kernel::framework::error::KernelError;
use edgine::kernel::framework::fs::VfsFileType;
#[cfg(test)]
use edgine::kernel::framework::fs::vfs_poll_trait::VfsPollPolicy;
use edgine::kernel::services::fs::unkfs::arc::{NestArcBufType, NestArcKey};
use edgine::kernel::services::fs::unkfs::arc_trait::{ArcCache, StandardArc};
use edgine::kernel::services::fs::unkfs::bp::NestBlockPointer;
use edgine::kernel::services::fs::unkfs::dmu::{NestObjSet, NestObjType};
use edgine::kernel::services::fs::unkfs::raidz::{NestRaidzLevel, NestRaidzMap};
// RAID-Z 列数上下限仅单测断言 clamp 行为时使用
#[cfg(test)]
use edgine::kernel::services::fs::unkfs::raidz::{HV_RAIDZ_MAX_COLS, HV_RAIDZ_MIN_COLS};
use edgine::kernel::services::fs::unkfs::spa::NestSpa;
use edgine::kernel::services::fs::unkfs::txg::NestTxgGroup;
use edgine::kernel::services::fs::unkfs::vdev::NestVdevConfig;
use edgine::kernel::services::fs::unkfs::zap::NestZap;
use edgine::kernel::services::fs::unkfs::zil::{NestZil, NestZilRecord};
use edgine::kernel::services::fs::unkfs::zil_persist::NestZilPersist;
use edgine::kernel::services::fs::vfs_poll_policy::StandardVfsPollPolicy;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

// ====== 1. PageFlags 位运算 (来自 framework/mm/mod.rs) ======

// G-07: 本地 `bitflags::bitflags!` 复刻已删除, 直引内核 `framework::mm::PageFlags`.

/// 每轮执行 64 个位运算, 使每轮有可测量的耗时
const PAGE_FLAGS_BATCH: u64 = 64;

pub fn page_flags_bench(iters: u64) -> u128 {
    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        let mut f = flags;
        for j in 0..PAGE_FLAGS_BATCH {
            if (i + j) & 1 == 0 {
                f |= PageFlags::NX;
            }
            if (i + j) & 3 == 0 {
                f |= PageFlags::GLOBAL;
            }
            if (i + j) & 7 == 0 {
                f |= PageFlags::ACCESSED;
            }
            sink ^= f.bits();
        }
    }
    std::hint::black_box(sink);
    // 归一化到 "单操作时间": 总耗时 (ns) / (iters * BATCH), 转 ps 避免精度损失
    let elapsed = start.elapsed().as_nanos();
    let total_ops = (iters as u128) * (PAGE_FLAGS_BATCH as u128);
    elapsed.saturating_mul(1_000) / total_ops
}

// ====== 2. PTE set_flags (来自 framework/mm/mod.rs PageTableEntry) ======

// G-07: 本地 `MockPte` 复刻已删除, 直引内核 `framework::mm::PageTableEntry`.
// 注: 内核实现以 `AtomicU64` + Acquire/Release 承载位域, 且 `set_flags` 取 `&self`
// 而非旧 mock 的裸 `u64` 写入 — 语义与性能特征以内核为准, 基线随实现重录.

pub fn pte_set_flags_bench(iters: u64) -> u128 {
    let pte = PageTableEntry::from_value(0x0);
    let flags = PageFlags::PRESENT | PageFlags::WRITABLE;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        pte.set_flags(flags);
        if i & 1 == 0 {
            pte.set_flags(flags | PageFlags::USER);
        }
        if pte.is_present() {
            sink ^= 1;
        }
    }
    std::hint::black_box(sink);
    // 归一化到 "单操作时间" (1 轮 = 1 次 PTE 位域更新), 转 ps 避免精度损失
    let elapsed = start.elapsed().as_nanos();
    elapsed.saturating_mul(1_000) / iters as u128
}

// ====== 3. IoMem 别名区间注册 (来自 framework/iomem.rs) ======

// G-07: 本地 `AliasEntry`/`AliasRegistry`/`MAX_MMIO_MAPPINGS` 复刻已删除, 直引内核
// `framework::iomem::IoMem` — 别名注册表 (`ALIAS_REGISTRY`) 为私有全局态, 唯一公共
// 入口是 `IoMem::new` (注册) / `Drop` (注销), 用法与 `tests/mm_iomem_alias_test.rs` 一致.
//
// 语义与基线变更 (来源同 A 类): 旧 mock 只计时 `check_conflict` 单次扫描, 内核入口每次
// 往返含 1 次 `IrqSpinLock` 加解锁 + 注册表写入 + 注销, 故 1 op = 一次「扫描 + 注册 + 注销」.

/// bench 预置的已注册 MMIO 区段数 (与旧 mock 的 30 条基线对齐)
const IOMEM_BASELINE_ENTRIES: u64 = 30;

pub fn iomem_alias_bench(iters: u64) -> u128 {
    // 预置基线区段: 句柄须存活至计时结束 (Drop 即注销, 表回退到 0 条)
    let mut baseline = Vec::with_capacity(IOMEM_BASELINE_ENTRIES as usize);
    for i in 0..IOMEM_BASELINE_ENTRIES {
        // SAFETY: phys 仅为纯算术载体 — `IoMem::new` 只做对齐/溢出/别名冲突校验
        // (`mmio_virt` 为 `phys_to_virt` 纯换算), 不解引用该地址, 无 MMIO 访问.
        let m = unsafe { IoMem::new(PhysAddr(0x1000 + i * 0x1000), 0x800, "bench.iomem") };
        baseline.push(m.expect("bench 基线 MMIO 区段注册失败"));
    }
    const BATCH: u64 = 32;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        for j in 0..BATCH {
            let phys = 0x50000 + ((i * BATCH + j) & 0xFFFF) * 0x100;
            {
                // SAFETY: 同基线注册 (纯算术载体, 不触碰映射内存)
                let m = unsafe { IoMem::new(PhysAddr(phys), 0x800, "bench.iomem") };
                if m.is_err() {
                    sink ^= 1;
                }
                // 句柄随本作用域结束 Drop → 注销, 注册表回到 30 条基线
            }
        }
    }
    sink ^= baseline.len() as u64;
    std::hint::black_box(sink);
    let elapsed = start.elapsed().as_nanos();
    let total_ops = (iters as u128) * (BATCH as u128);
    elapsed.saturating_mul(1_000) / total_ops
}

// ====== 4. 能力矩阵域位检查 (来自 services/sgeg/policy.rs) ======

// G-07: 本地 `CapabilityMatrix`/`CAP_DOMAINS` 复刻已删除, 直引内核
// `services::sgeg::policy` 的 `InMemoryMatrix` (16×AtomicU64) + `PolicyEngine::check`
// (域合法性 → 原子读 → 包含判定 → 可行下界保护).
//
// bench 域表按内核 16 域常量构造 (避免字面量映射).
const BENCH_CAP_DOMAINS: [CapDomain; 16] = [
    CapDomain::SYSTEM,
    CapDomain::FS,
    CapDomain::NET,
    CapDomain::PROC,
    CapDomain::DEVICE,
    CapDomain::USER_MGMT,
    CapDomain::IPC,
    CapDomain::MEM,
    CapDomain::TIME,
    CapDomain::FREG,
    CapDomain::SIGNAL,
    CapDomain::SHM,
    CapDomain::SEM,
    CapDomain::MSGQ,
    CapDomain::DMA,
    CapDomain::RESERVED,
];

pub fn capability_check_bench(iters: u64) -> u128 {
    let m = InMemoryMatrix::new();
    // 预置各域能力位 (域下标与旧 mock 的 grant 序列对齐: 1/3/2/5)
    let _ = m.set(CapDomain::FS, CapBits(0b11));
    let _ = m.set(CapDomain::PROC, CapBits(0b10101));
    let _ = m.set(CapDomain::NET, CapBits(0b1111));
    let _ = m.set(CapDomain::USER_MGMT, CapBits(0b1));
    let engine = PolicyEngine::new();
    const BATCH: u64 = 64;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        for j in 0..BATCH {
            let dom = BENCH_CAP_DOMAINS[((i * BATCH + j) as usize) & 0xF];
            let bits = CapBits(1u64 << ((i + j) & 0x1F));
            if engine.check(&m, dom, bits) == PolicyResult::Allow {
                sink ^= 1;
            }
        }
    }
    std::hint::black_box(sink);
    let elapsed = start.elapsed().as_nanos();
    let total_ops = (iters as u128) * (BATCH as u128);
    elapsed.saturating_mul(1_000) / total_ops
}

// ====== 5. DmaStream 状态机 (来自 framework/dma_buf.rs) ======

// G-07: 本地 `DmaStream`/`SyncState`/`transition` 复刻已删除, 直引内核
// `framework::dma_buf::DmaStream`. 内核状态机不暴露 `transition`, 仅提供
// `sync_for_device`/`sync_for_cpu`; 二者在 `Bidirectional` 流上可无限 ping-pong
// (CpuReady ↔ DeviceReady), 故 bench 用单一双向流承载全部迁移操作.

/// 构造 bench 用 Frame (host 无真实物理页).
///
/// # SAFETY
/// phys 仅为纯算术载体: 内核 `DmaStream::from_frame` 只做对齐/溢出/大小校验与
/// 状态机迁移, 不解引用 `as_virt_ptr()`; `Frame` 无 Drop 实现 (不释放物理页).
unsafe fn bench_frame(paddr: u64, order: u8) -> Frame {
    // SAFETY: 见函数文档 (前置条件与调用点一致)
    unsafe { Frame::from_raw(PhysAddr(paddr), order) }
}

pub fn dma_state_machine_bench(iters: u64) -> u128 {
    // SAFETY: 0x10000 页对齐, 见 bench_frame() 说明
    let frame = unsafe { bench_frame(0x10000, 0) };
    let mut s = DmaStream::from_frame(frame, DmaDirection::Bidirectional)
        .expect("bidirectional DmaStream 构造失败");
    const BATCH: u64 = 64;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for _ in 0..iters {
        // black_box 阻断状态机折叠: 否则编译器证明状态迁移确定性后可将整个循环消除
        // (实测未阻断时 12.8M 次迁移仅耗时 90ns, 记录值恒为 0)
        let s = std::hint::black_box(&mut s);
        for _ in 0..BATCH {
            if s.sync_for_device().is_ok() {
                sink ^= 1;
            }
            if s.sync_for_cpu().is_ok() {
                sink ^= 2;
            }
        }
    }
    sink ^= u64::from(s.sync_state() == SyncState::CpuReady);
    std::hint::black_box(sink);
    let elapsed = start.elapsed().as_nanos();
    // 1 op = 1 次状态机迁移 (每 BATCH 轮 = 2 次迁移)
    let total_ops = (iters as u128) * (BATCH as u128) * 2;
    elapsed.saturating_mul(1_000) / total_ops
}

// ====== 6. SHA-256 哈希 (来自 framework/sgeg/sha256.rs) ======

// G-07: 本地 `K`/`rotr`/`sha256_transform` 复刻已删除, 直引内核
// `framework::sgeg::sha256::sha256` — 消息填充 + 压缩函数 + 输出编码的唯一公共入口.
//
// 语义与基线变更: 旧 mock 只做单 block 压缩 (无填充); 内核公共入口对 64B 输入做
// 2 次压缩 (数据块 + 填充块) 并编码输出, 故 1 op 口径改为 1 次完整 `sha256` 调用.
// 输入经 `black_box` 屏蔽常量传播, 避免编译器把整轮折叠为一次调用.

pub fn sha256_block_bench(iters: u64) -> u128 {
    let block = [0u8; 64];
    let start = Instant::now();
    let mut sink: u8 = 0;
    for _ in 0..iters {
        sink ^= sha256(std::hint::black_box(&block))[0];
    }
    std::hint::black_box(sink);
    // 归一化到 "单操作时间" (1 轮 = 1 次完整 `sha256` 调用), 转 ps 避免精度损失
    let elapsed = start.elapsed().as_nanos();
    elapsed.saturating_mul(1_000) / iters as u128
}

// ====== 7. Attribution classify (来自 services/freg/attribution.rs) ======

// G-07: 本地 `FaultAttribution`/`FaultRecord`/`classify` 复刻已删除, 直引内核
// `services::freg::attribution::FaultAttributor::attribute(panic_rip)`.
//
// 语义对齐说明 (基线变更来源): 旧 mock 按 `FaultRecord` 的 in_interrupt /
// holding_lock / in_services 标志位做规则判定; 内核真实入口的唯一入参是
// `panic_rip`, 按落入 `TCB_RANGES` / `SERVICE_RANGES` 静态地址区间判定归属,
// 两者输入面不同. bench 现按内核契约以伪 RIP 序列驱动归属判定.

pub fn attribution_classify_bench(iters: u64) -> u128 {
    // 三类伪 RIP: TCB 区间 / Services 区间 / 两区间外 (Unknown)
    let rips: Vec<u64> = (0..256u64)
        .map(|i| match i % 3 {
            0 => 0xFFFF_FFFF_8000_0000 + i * 0x40, // TCB 区间起始段
            1 => 0xFFFF_FFFF_0000_0000 + i * 0x40, // Services 区间起始段
            _ => 0x0000_1000_0000_0000 + i * 0x40, // 两区间外 → Unknown
        })
        .collect();
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        match FaultAttributor::attribute(rips[(i as usize) & 0xFF]) {
            FaultAttribution::Tcb { .. } => sink ^= 1,
            FaultAttribution::Service { domain_id, .. } => sink ^= domain_id,
            FaultAttribution::CrossLayer { .. } => sink ^= 0xCAFE,
            FaultAttribution::Unknown => sink ^= 0xF00D,
        }
    }
    std::hint::black_box(sink);
    // 归一化到 "单操作时间" (1 轮 = 1 次归属判定), 转 ps 避免精度损失
    let elapsed = start.elapsed().as_nanos();
    elapsed.saturating_mul(1_000) / iters as u128
}

// ====== 8. Recovery decide (来自 services/freg/recovery_policy.rs) ======

// G-07: 本地 `FaultSignal`/`decide` 复刻已删除, 直引内核
// `services::freg::recovery_policy::{FaultSignal, RecoveryAction, RecoveryPolicy}`.
// 入参构造改用内核 `FaultSignal::tcb` 与 `FaultAttribution::Service` 结构体字面量
// (旧 mock 的 `is_tcb`/`retry` 字段名映射为 `attribution`/`retry_count`).

/// 构造第 i 个 bench 用故障信号 (奇偶交替 TCB / Service 两类归属).
fn bench_signal(i: u64) -> FaultSignal {
    if i & 1 == 0 {
        FaultSignal::tcb(TcbModule::FREG, i)
    } else {
        FaultSignal {
            attribution: FaultAttribution::Service {
                domain_id: i & 0xF,
                recoverable: i & 2 != 0,
            },
            retry_count: (i % 8) as u32,
            heartbeat_gap: i * 30,
            dependents: (i % 4) as u32,
            tick: i,
        }
    }
}

pub fn recovery_decide_bench(iters: u64) -> u128 {
    let signals: Vec<FaultSignal> = (0..64).map(bench_signal).collect();
    const BATCH: u64 = 64;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        for j in 0..BATCH {
            let a = RecoveryPolicy::decide(&signals[((i * BATCH + j) as usize) & 0x3F]);
            sink ^= match a {
                RecoveryAction::Noop => 0,
                RecoveryAction::FREGBaseRecovery => 1,
                RecoveryAction::FREGSoftReset => 2,
                RecoveryAction::FREGHardReset => 3,
                RecoveryAction::Quarantine => 4,
            };
        }
    }
    std::hint::black_box(sink);
    let elapsed = start.elapsed().as_nanos();
    elapsed.saturating_mul(1_000) / (iters as u128)
}

// ====== 9. PMM 物理页分配 (来自 framework/mm/pmm.rs) ======

// G-07: 本地 `Bitmap` 复刻已删除, 直引内核 `framework::mm::pmm::PhysicalMemoryManager`
// — 经 `MetaStore` 载体注入宿主 `VecMetaStore`, buddy 分配/合并走内核唯一实现
// (装配方式与 `tests/pmm_buddy_host_test.rs` 一致, 无测试/生产分叉).

/// 模拟物理内存 64MB (buddy 完整覆盖 order-0..9)
const BENCH_MEM_SIZE: u64 = 64 * 1024 * 1024;
/// 模拟内核镜像末尾 16MB (init_bitmap 前的内核保留区)
const BENCH_KERNEL_END: u64 = 16 * 1024 * 1024;
/// bench 预分配页数 (与旧 mock 的 512 位基线对齐)
const BENCH_PREALLOC_PAGES: usize = 512;

pub fn bitmap_scan_bench(iters: u64) -> u128 {
    let pmm = PhysicalMemoryManager::new();
    pmm.inject_meta_store(VecMetaStore::new());
    pmm.init(BENCH_MEM_SIZE, BENCH_KERNEL_END);
    pmm.init_bitmap(0);
    // 预分配基线页, 使后续 alloc 走非空空闲链路径
    let mut baseline = Vec::with_capacity(BENCH_PREALLOC_PAGES);
    for _ in 0..BENCH_PREALLOC_PAGES {
        match pmm.alloc_page() {
            Some(addr) => baseline.push(addr),
            None => break,
        }
    }
    const BATCH: u64 = 32;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        for j in 0..BATCH {
            if let Some(addr) = pmm.alloc_page() {
                sink ^= addr.0;
                if (i + j) & 1 == 0 {
                    pmm.free_page(addr);
                }
            }
        }
    }
    sink ^= baseline.len() as u64;
    std::hint::black_box(sink);
    let elapsed = start.elapsed().as_nanos();
    elapsed.saturating_mul(1_000) / (iters as u128)
}

// ====== 11. Socket WaitQueue (来自 framework/net/wait_queue.rs) ======
//
// 16 个 fd (MAX_SM_FD) 上的 mark_waiting → try_wake 循环.
// 单次循环 = 1 个 fd 上的 1 次 send/wake 对应操作.
// 验收: 1000 个并发 send 路径平均延迟 < 1μs (QEMU 环境 1000 < 1ms 目标换算).
//
// G-07: 本地 `MockSocketWaitQueue` 复刻已删除, 直引内核
// `framework::net::wait_queue::SocketWaitQueue`. 原注释所写
// services/net/wait_queue.rs 为失效路径 (DECISION-J 已将该基础设施归位 framework).
//
// 注: 内核版以 `IrqSpinLock` 保护 pending 状态 (host-test 下禁中断为 no-op),
// 并以 `is_pending`/`wake_count`/`last_reason` 暴露观测面.

// G-07 收口: 本地 mock 的 `StdMutex` 依赖已随 `MockBlockDevice` 等复刻体删除而移除,
// 全部同步原语由内核实现承担 (host-test 下 `IrqSpinLock` 的禁中断为 no-op).

/// MAX_SM_FD: 16 (与 services/net/socket.rs 的 fd 空间 [0, 16) 对齐)
const MAX_SM_FD: usize = 16;

pub fn socket_wait_queue_bench(iters: u64) -> u128 {
    let queues: Vec<SocketWaitQueue> = (0..MAX_SM_FD).map(|_| SocketWaitQueue::new()).collect();
    // 1 轮 (BATCH) = 1000 次并发 send/wake 路径 = 验收目标
    const BATCH: u64 = 1000;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for i in 0..iters {
        for j in 0..BATCH {
            // 轮询 16 个 fd, 每个 fd 上做 mark_waiting + try_wake
            let fd = ((i * BATCH + j) as usize) % MAX_SM_FD;
            queues[fd].mark_waiting();
            if queues[fd].try_wake(WakeReason::Readable) {
                sink ^= 1;
            }
        }
    }
    // 读取内核观测面, 防止编译器优化掉 pending 状态迁移
    sink ^= u64::from(queues[0].is_pending()) | u64::from(queues[0].wake_count());
    std::hint::black_box(sink);
    let elapsed = start.elapsed().as_nanos();
    let total_ops = (iters as u128) * (BATCH as u128);
    elapsed.saturating_mul(1_000) / total_ops
}

// ====== 12. virtio-blk I/O 路径 (来自 framework/driver/virtio/queue.rs) ======
//
// G-07: 本地 `MockVqDesc`/`MockVirtQueue` 复刻已删除, 直引内核
// `framework::driver::virtio::queue::VirtQueue` — 描述符准备/链接/提交/回收与已用环
// 弹出全部走内核实现. host 侧仅保留两处「装配与设备模拟」(非内核算法复刻):
//   1. `bench_virtqueue`: 以宿主堆块充当环区后备 (host 无 PMM, `VirtQueue::new` 经
//      extern `pmm_alloc_pages` + `phys_to_virt` 直写内核直映射, 在 host 不可运行)
//   2. `bench_device_complete`: 设备侧写 used ring (设备行为, 内核不含此逻辑)
//
// 4K 写请求 = 3 段描述符链: header (16B, 设备读) + data (4096B, 设备读) + status (1B, 设备写).
// 验收目标: 4K 写延迟 < 100μs (QEMU virtio-blk 设备实测),
//          host 端算法路径应远低于此 (<< 1μs).

/// 4K 写请求的数据段长度
const BLK_4K_BYTES: u32 = 4096;
/// 环区宿主后备块字节数 (desc 512 + avail 68 + used 260, 分段放置于 4096 内)
const VQ_BACKING_BYTES: usize = 4096;
/// 环区分段偏移 (互不重叠且满足各自对齐: desc @0 / avail @1024 / used @2048)
const VQ_AVAIL_OFFSET: usize = 1024;
const VQ_USED_OFFSET: usize = 2048;

/// 用宿主内存装配 bench 用 `VirtQueue`.
///
/// 空闲描述符链初始化与内核 `VirtQueue::new` 一致 (`desc[i].next = i + 1`,
/// 末项 `0xFFFF`); 队列状态字段按内核构造的初值设置.
///
/// # SAFETY
/// `backing` 为 8 字节对齐的宿主堆块且长度 ≥ `VQ_BACKING_BYTES`, 其生命周期必须
/// 覆盖返回的 `VirtQueue` (调用方需在更外层作用域持有该后备块).
unsafe fn bench_virtqueue(backing: &mut [u64]) -> VirtQueue {
    let base = backing.as_mut_ptr().cast::<u8>();
    // SAFETY: 见函数文档 — 后备块对齐/大小/存活性由调用方保证, 分段偏移在块内.
    unsafe {
        let desc = base.cast::<VqDesc>();
        let avail = base.add(VQ_AVAIL_OFFSET).cast::<VqAvail>();
        let used = base.add(VQ_USED_OFFSET).cast::<VqUsed>();
        for i in 0..VQ_SIZE {
            (*desc.add(i as usize)).next = if i + 1 < VQ_SIZE { i + 1 } else { 0xFFFF };
        }
        VirtQueue {
            desc,
            avail,
            used,
            queue_size: VQ_SIZE,
            free_head: 0,
            last_used_idx: 0,
            next_avail_idx: 0,
            // host 无真实物理地址, 三者为 DMA 描述用物理地址 (bench 不使用)
            desc_phys: 0,
            avail_phys: 0,
            used_phys: 0,
        }
    }
}

/// 设备侧完成 (host 模拟设备行为, 非内核逻辑): 写 used ring 并推进 `idx`.
///
/// # SAFETY
/// `vq` 的 used 环必须指向有效后备块 (见 `bench_virtqueue`), 且调用方独占访问.
unsafe fn bench_device_complete(vq: &mut VirtQueue, head: u16, len: u32) {
    // SAFETY: 见函数文档.
    unsafe {
        let used = vq.used;
        let idx = (*used).idx;
        (*used).ring[(idx % VQ_SIZE) as usize] = VqUsedElem {
            id: u32::from(head),
            len,
        };
        (*used).idx = idx.wrapping_add(1);
    }
}

pub fn virtio_blk_io_bench(iters: u64) -> u128 {
    let mut backing = vec![0u64; VQ_BACKING_BYTES / 8];
    // SAFETY: backing 在本函数作用域内存活, 覆盖 vq 全部使用期; Vec<u64> 为 8 字节对齐
    let mut vq = unsafe { bench_virtqueue(&mut backing) };
    // 1 轮 (BATCH) = 32 次 4K 写 (覆盖整个 virtqueue 一次)
    const BATCH: u64 = 32;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for _ in 0..iters {
        for _ in 0..BATCH {
            // 3 段描述符链: header → data → status (末段设备写)
            let h1 = vq.prepare_desc(0, 16, false);
            let h2 = vq.prepare_desc(0, BLK_4K_BYTES, false);
            let h3 = vq.prepare_desc(0, 1, true);
            vq.link_desc(h1, h2);
            vq.link_desc(h2, h3);
            vq.submit(h1);
            vq.commit_and_kick();
            // SAFETY: vq.used 指向本函数作用域内的 backing
            unsafe { bench_device_complete(&mut vq, h1, BLK_4K_BYTES) };
            if let Some((id, len)) = vq.pop_used() {
                sink ^= u64::from(id) ^ u64::from(len);
            }
            vq.reclaim_desc(h1);
            vq.reclaim_desc(h2);
            vq.reclaim_desc(h3);
        }
    }
    std::hint::black_box(sink);
    let elapsed = start.elapsed().as_nanos();
    // 归一化: 1 op = 1 个 4K 写请求 (prepare+link+submit+kick+complete+pop+reclaim)
    let total_ops = (iters as u128) * (BATCH as u128);
    elapsed.saturating_mul(1_000) / total_ops
}

// ============================================================================
// EBPF-3: BpfVerifier trait dispatch bench (G-07: 直引内核真实实现)
// ============================================================================
//
// G-07: 本地 `MockBpfProg`/`VerifyResult`/`BpfVerifier`/`MockBpfVerifier`/
// `MockBpfSubsystem` 复刻已全部删除, 直引内核真实类型:
//   - framework (机制): `framework::debug::{BpfProg, BpfInsn, BpfProgType,
//     BpfVerifier, VerifyResult}`
//   - services (策略): `services::debug::ebpf_verifier::STANDARD_VERIFIER`
//     (7 条验证规则的 services 实现)
//
// bench 测量 `&dyn BpfVerifier::verify` 动态分派 + 7 条规则全路径吞吐.

/// bench: 测量 `&dyn BpfVerifier::verify` 动态分派 throughput
///
/// 1 op = 1 次 `verify` 调用. 1 op 包含:
/// - 通过 `&dyn BpfVerifier` 间接调用 `StandardBpfVerifier::verify`
/// - 匹配 `VerifyResult`
pub fn bpf_verifier_dispatch_bench(iters: u64) -> u128 {
    // 最小合法程序: ALU64 MOV r0,0 + EXIT — 通过内核 7 条规则的全部检查路径
    let insns = vec![
        BpfInsn::new(opcode::ALU64 | opcode::MOV, 0, 0, 0, 0),
        BpfInsn::new(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
    ];
    let prog = BpfProg::new(BpfProgType::SocketFilter, insns);
    let v: &dyn BpfVerifier = &STANDARD_VERIFIER;
    let start = Instant::now();
    let mut sink: u32 = 0;
    for _ in 0..iters {
        let r = v.verify(&prog);
        // 读取结果, 防止编译器优化掉整条路径
        sink ^= match r {
            VerifyResult::Ok => 1,
            VerifyResult::Err(_) => 0,
        };
    }
    let elapsed = start.elapsed().as_nanos();
    // 防御性: 防止编译器优化掉 sink
    std::hint::black_box(sink);
    // 1 op = 1 次 verify 调用
    elapsed.saturating_mul(1_000) / (iters as u128)
}

// ============================================================================
// SYSCTL-2: sysctl register/write bench (G-07: 直引内核真实实现)
// ============================================================================
//
// G-07: 本地 `MockSysctlValue`/`MockSysctlKind`/`MockSysctlEntry`/`MockSysctlTable`
// 复刻已全部删除, 直引内核 `services::config::sysctl`:
//   - 注册表: 内核 `SYSCTL_TABLE` 全局静态 (32 槽 `IrqSpinLock` + 原子字段)
//   - API: `sysctl_register` / `sysctl_write` (host-test 下 `IrqSpinLock` 原子自旋
//     互斥, 禁中断为 no-op)
//
// 注: 内核注册表是进程内全局唯一且无注销面 — 故 16 个 bench 节点经 `Once` 只注册
// 一次, 计时主体仅覆盖 write 路径 (旧 mock 每轮新建本地表, 不受此约束).

/// bench sysctl 节点数 (内核 `MAX_SYSCTL_ENTRIES` = 32 槽位内)
const SYSCTL_NODES: usize = 16;

/// bench sysctl 节点名 (静态字符串池, 免去 `Box::leak`)
const SYSCTL_BENCH_NAMES: [&str; SYSCTL_NODES] = [
    "bench.sysctl.0",
    "bench.sysctl.1",
    "bench.sysctl.2",
    "bench.sysctl.3",
    "bench.sysctl.4",
    "bench.sysctl.5",
    "bench.sysctl.6",
    "bench.sysctl.7",
    "bench.sysctl.8",
    "bench.sysctl.9",
    "bench.sysctl.10",
    "bench.sysctl.11",
    "bench.sysctl.12",
    "bench.sysctl.13",
    "bench.sysctl.14",
    "bench.sysctl.15",
];

/// bench 节点的值类型 (按下标轮转 Int/UInt/Bool)
fn sysctl_bench_kind(i: usize) -> SysctlKind {
    match i % 3 {
        0 => SysctlKind::Int,
        1 => SysctlKind::UInt,
        _ => SysctlKind::Bool,
    }
}

/// 一次性注册 bench 节点 (内核注册表无注销面, 重复注册返回 `Duplicate`)
fn sysctl_bench_init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        for (i, name) in SYSCTL_BENCH_NAMES.iter().enumerate() {
            let kind = sysctl_bench_kind(i);
            let initial = match kind {
                SysctlKind::Int => SysctlValue::Int(i as i64),
                SysctlKind::UInt => SysctlValue::UInt(i as u64),
                SysctlKind::Bool => SysctlValue::Bool(i % 2 == 0),
            };
            let _ = sysctl_register(name, kind, initial);
        }
    });
}

/// bench: sysctl write 吞吐量
///
/// 1 轮 (BATCH) = 16 次 write. 节点注册在首次进入时一次性完成 (见 `sysctl_bench_init`).
pub fn sysctl_bench(iters: u64) -> u128 {
    sysctl_bench_init();
    const BATCH: u64 = 16;
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        for i in 0..BATCH {
            let idx = (i as usize) % SYSCTL_NODES;
            let val = match sysctl_bench_kind(idx) {
                SysctlKind::Int => SysctlValue::Int(i as i64 + (r as i64) * 1000),
                SysctlKind::UInt => SysctlValue::UInt(i + r * 1000),
                SysctlKind::Bool => SysctlValue::Bool(r % 2 == 0),
            };
            let _ = sysctl_write(SYSCTL_BENCH_NAMES[idx], val);
            sink ^= idx as u64;
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    // 1 op = 1 次 write
    elapsed.saturating_mul(1_000) / (iters as u128 * BATCH as u128)
}

// ============================================================================
// T-4.1 (LEGACY-4): BlockDevice trait dispatch bench
// ============================================================================
//
// G-07: 本地 `HostBlockDevice` / `MockEGDFDevice` 复刻已删除, 直引内核
// `framework::egdf` — 设备注册 (`egdf_register_block_dev`)、协议/状态/长度
// 边界检查与 trait dispatch (`egdf_blk_read`/`egdf_blk_write`) 全为内核唯一实现.
// host 侧仅保留「设备载体」`BenchBlockDevice`: 提供扇区存储, 实现内核 `BlockDevice` 契约.

/// 宿主块设备载体 (实现内核 `BlockDevice`, 供 `EGDF_DEVICES` 注册表 dispatch)
pub struct BenchBlockDevice {
    /// 内部存储 (按 sector 索引)
    storage: Vec<[u8; 512]>,
}

impl BenchBlockDevice {
    /// 构造 `capacity_sectors` 个扇区, 首 2 字节写入扇区号 (便于区分扇区)
    pub fn new(capacity_sectors: usize) -> Self {
        Self {
            storage: (0..capacity_sectors)
                .map(|i| {
                    let mut s = [0u8; 512];
                    s[0] = (i & 0xFF) as u8;
                    s[1] = ((i >> 8) & 0xFF) as u8;
                    s
                })
                .collect(),
        }
    }
}

impl BlockDevice for BenchBlockDevice {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
        let s = sector as usize;
        if s >= self.storage.len() {
            return KernelError::Io.as_i32();
        }
        buf.copy_from_slice(&self.storage[s]);
        0
    }
    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
        let s = sector as usize;
        if s >= self.storage.len() {
            return KernelError::Io.as_i32();
        }
        self.storage[s].copy_from_slice(&buf[..512]);
        0
    }
    fn blk_is_present(&self) -> bool {
        true
    }
    fn blk_total_sectors(&self) -> u64 {
        self.storage.len() as u64
    }
}

/// 注册 1024 扇区的 bench 块设备并返回 drive 索引
///
/// `box_leak` 只执行一次 (measure 会对同一 bench 多次取样), 避免重复注册泄漏.
fn bench_blk_drive() -> u8 {
    static SLOT: OnceLock<u8> = OnceLock::new();
    *SLOT.get_or_init(|| {
        let dev: &'static mut BenchBlockDevice = Box::leak(Box::new(BenchBlockDevice::new(1024)));
        egdf_register_block_dev("bench_blk", None, None, dev) as u8
    })
}

/// bench: T-4.1 块设备 I/O 路径 throughput (经内核 egdf dispatch)
pub fn blk_dev_dispatch_bench(iters: u64) -> u128 {
    let drive = bench_blk_drive();

    // 预热 (避免首次调用路径开销污染)
    let mut buf = [0u8; 512];
    for _ in 0..100 {
        let _ = egdf_blk_read(drive, 0, &mut buf);
    }

    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        // 1 轮: 1 读 + 1 写 (16 扇区 旋转)
        let sector = r & 0xF;
        let _ = egdf_blk_read(drive, sector, &mut buf);
        sink ^= u64::from(buf[0]) | (u64::from(buf[1]) << 8);
        let _ = egdf_blk_write(drive, sector, &buf);
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    // 1 op = 1 读 + 1 写 = 2 实际 ops
    elapsed.saturating_mul(1_000) / (iters as u128 * 2)
}

// ============================================================================
// REVAL-6.1: VfsPollPolicy trait dispatch bench
// ============================================================================
//
// G-07: 本地 `MockVfsFileType` / `MockVfsPollPolicy` / `MockEpollCheck` 复刻已删除,
// 直引内核 `framework::fs::vfs_poll_trait` (机制) + services `StandardVfsPollPolicy`
// (策略): `VfsPollPolicyRef::events_for` 即 epoll `check_fd_ready` 的唯一决策入口.

/// bench 用 epoll 事件掩码 (与内核 `check_fd_ready` 的 user mask 语义一致)
const BENCH_EPOLL_MASK: u32 = EPOLLIN | EPOLLOUT | EPOLLERR | EPOLLHUP;

/// 供 `VfsPollPolicyRef::Registered` 引用的内核默认策略
static BENCH_VFS_POLL_POLICY: StandardVfsPollPolicy = StandardVfsPollPolicy;

/// bench: REVAL-6.1 策略决策路径 throughput
pub fn vfs_poll_dispatch_bench(iters: u64) -> u128 {
    let policy = VfsPollPolicyRef::Registered(&BENCH_VFS_POLL_POLICY);

    // 预热
    for _ in 0..1000 {
        let ctx = VfsPollContext {
            valid: true,
            file_type: VfsFileType::File,
        };
        let _ = policy.events_for(ctx) & BENCH_EPOLL_MASK;
    }

    // 4 种 file_type 旋转
    let fts = [
        VfsFileType::File,
        VfsFileType::Dir,
        VfsFileType::Dev,
        VfsFileType::Symlink,
    ];
    let start = Instant::now();
    let mut sink: u32 = 0;
    for r in 0..iters {
        let ft = fts[(r & 0x3) as usize];
        let ctx = VfsPollContext {
            valid: true,
            file_type: ft,
        };
        sink ^= policy.events_for(ctx) & BENCH_EPOLL_MASK;
        // 偶尔插入 invalid fd
        if r & 0xFF == 0 {
            let inv_ctx = VfsPollContext {
                valid: false,
                file_type: VfsFileType::File,
            };
            sink ^= policy.events_for(inv_ctx) & BENCH_EPOLL_MASK;
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.1: ZAP (NestZap) dispatch bench
// ============================================================================
//
// G-07: 本地 `HostZapStore` / `StandardHostZap` (Mutex<HashMap>) 复刻已删除, 直引内核
// `services::fs::unkfs::zap::NestZap`. 注: 内核 ZAP 为线性扫描 (先比 hash 再比名字),
// 与内核真实行为位一致.

/// bench: ZAP insert / lookup / contains 路径 throughput
pub fn zap_dispatch_bench(iters: u64) -> u128 {
    // 容量 > 键空间, 保证 insert 分支始终走「查找已有键」真实路径
    let zap = NestZap::with_capacity(512);
    // 预热: 填满键空间
    for i in 0..256u64 {
        zap.insert_u64(&format!("k_{}", i), i);
    }
    // bench: insert + lookup + contains 旋转
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        let key = format!("k_{}", r & 0xFF);
        if r & 0x3 == 0 {
            // insert (已有键 → 原地更新)
            let _ = zap.insert_u64(&key, r);
        } else if r & 0x3 == 1 {
            // lookup_u64
            if let Some(v) = zap.lookup_u64(&key) {
                sink = sink.wrapping_add(v);
            }
        } else if r & 0x3 == 2 {
            // insert raw
            let _ = zap.insert(&key, &r.to_le_bytes());
        } else {
            // contains
            if zap.contains(&key) {
                sink = sink.wrapping_add(1);
            }
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.2: TXG (NestTxgGroup) dispatch bench
// ============================================================================
//
// G-07: 本地 `MockTxgState` / `HostTxgManager` / `StandardHostTxg` 复刻已删除, 直引内核
// `services::fs::unkfs::txg::NestTxgGroup` — `init`/`transition`/`add_dirty_to_open`/
// `current_txg` 的唯一实现. 事务组三态 (open/quiescing/syncing) 迁移与脏块登记
// 均由内核承担.

/// bench: TXG 事务组迁移 + 脏块登记 路径 throughput
pub fn txg_dispatch_bench(iters: u64) -> u128 {
    let mut txg = NestTxgGroup::new();
    txg.init(1);
    // 预热
    for _ in 0..1000 {
        txg.add_dirty_to_open(NestBlockPointer::null());
    }
    // bench: add_dirty + current_txg + transition 旋转
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        if r & 0x7 == 0 {
            // transition (稀有)
            let id = txg.transition();
            sink = sink.wrapping_add(id);
        } else if r & 0x3 == 1 {
            // current_txg
            sink = sink.wrapping_add(txg.current_txg());
        } else {
            // add_dirty_to_open
            txg.add_dirty_to_open(NestBlockPointer::null());
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.4: DMU (NestObjSet) dispatch bench
// ============================================================================
//
// G-07: 本地 `MockDmuObject` / `HostDmuManager` / `StandardHostDmu` (Mutex<HashMap>)
// 复刻已删除, 直引内核 `services::fs::unkfs::dmu::NestObjSet` — 对象分配/释放/查询/
// 计数唯一实现 (内核为 `Mutex<Vec<NestDmuObject>>`, 查询与计数为线性扫描).

/// bench: DMU 对象分配 / 查询 路径 throughput
pub fn dmu_dispatch_bench(iters: u64) -> u128 {
    let dmu = NestObjSet::new();
    dmu.init(0x100);
    // 预热: alloc 1000 个 File 对象
    for _ in 0..1000 {
        dmu.alloc_obj(NestObjType::File, 0x100);
    }
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        if r & 0x3 == 0 {
            // alloc File
            if let Some(id) = dmu.alloc_obj(NestObjType::File, 0x100) {
                sink = sink.wrapping_add(id);
            }
        } else if r & 0x3 == 1 {
            // get_obj (线性扫描)
            if let Some(obj) = dmu.get_obj(2) {
                sink = sink.wrapping_add(obj.obj_id);
            }
        } else if r & 0x3 == 2 {
            // obj_count (全表扫描)
            sink = sink.wrapping_add(dmu.obj_count());
        } else {
            // get_root
            if let Some(root) = dmu.get_root() {
                sink = sink.wrapping_add(root.obj_id);
            }
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.5: SPA (NestSpa) dispatch bench
// ============================================================================
//
// G-07: 本地 `SpaState` / `HostSpaManager` / `StandardHostSpa` 复刻已删除, 直引内核
// `services::fs::unkfs::spa::NestSpa` — 池初始化 / vdev 装配 / 事务组推进 / 统计读取
// 唯一实现. 注: 内核 vdev 上限为 `NestSpaConfig::max_vdevs` (默认 8).

/// bench: SPA 池状态读 + 事务组推进 路径 throughput
pub fn spa_dispatch_bench(iters: u64) -> u128 {
    let spa = NestSpa::new();
    spa.init("bench");
    // 预热: 装配 vdev 至内核上限 (max_vdevs = 8)
    for i in 0..8u16 {
        spa.add_vdev(NestVdevConfig::new_disk(i, "bench_disk", 9));
    }
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        if r & 0x7 == 0 {
            // advance_txg (稀有)
            sink = sink.wrapping_add(spa.advance_txg());
        } else if r & 0x3 == 1 {
            // current_txg
            sink = sink.wrapping_add(spa.current_txg());
        } else if r & 0x3 == 2 {
            // vdev_count
            sink = sink.wrapping_add(spa.vdevs.lock().len() as u64);
        } else {
            // guid
            sink = sink.wrapping_add(spa.config.lock().guid);
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.7: RAID-Z 几何查询 dispatch bench
// ============================================================================
//
// G-07: 本地 `MockRaidzLevel` / `HostRaidzEngine` / `StandardHostRaidz` 复刻已删除,
// 直引内核 `services::fs::unkfs::raidz::NestRaidzMap`. 注: 内核以 struct 字段
// (`ncols` / `nparity` / `ashift`) + `level` 枚举方法表达几何, 无 `is_single` /
// `is_mirror` 谓词, 故此处按内核真实访问面测量.

/// bench: RAID-Z 几何查询 (ncols / nparity / max_failures / ashift) throughput
pub fn raidz_dispatch_bench(iters: u64) -> u128 {
    let map = NestRaidzMap::new(NestRaidzLevel::RaidZ1, 3, 9);
    // 预热
    for _ in 0..1000 {
        let _ = map.ncols;
    }
    let start = Instant::now();
    let mut sink: u64 = 0;
    for it in 0..iters {
        match it & 0x3 {
            0 => sink = sink.wrapping_add(map.ncols as u64),
            1 => sink = sink.wrapping_add(map.nparity as u64),
            2 => sink = sink.wrapping_add(map.level.max_failures() as u64),
            _ => sink = sink.wrapping_add(u64::from(map.ashift)),
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.8: ARC 缓存 dispatch bench
// ============================================================================
//
// G-07: 本地 `MockArcKey` / `HostArcCache` / `StandardHostArc` / `ArcState`
// (Mutex<HashMap>) 复刻已删除, 直引内核 `services::fs::unkfs::arc_trait::StandardArc`.
// 注: 内核 `ArcCache::hit_rate()` 返回千分比 (u64), 且 `insert` 额外带
// `NestArcBufType` 参数.

/// ARC 初始容量 (内核 `HV_ARC_DEFAULT_SIZE` 量级, 保证预热后仍有淘汰余量)
const BENCH_ARC_MAX_SIZE: usize = 100;

/// bench: ARC trait dispatch (lookup / insert / hit_count / current_size) throughput
pub fn arc_dispatch_bench(iters: u64) -> u128 {
    let arc: Box<dyn ArcCache> = Box::new(StandardArc::new());
    arc.init(BENCH_ARC_MAX_SIZE);
    // 预热: 填满容量上限, 使 lookup 分支可命中
    for i in 0..BENCH_ARC_MAX_SIZE as u64 {
        let k = NestArcKey::new(0, i, 0);
        arc.insert(k, &[0u8; 16], NestArcBufType::Data);
    }
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        let k = NestArcKey::new(0, r & 0xFF, 0);
        if r & 0x3 == 0 {
            // lookup
            if arc.lookup(&k) {
                sink = sink.wrapping_add(1);
            }
        } else if r & 0x3 == 1 {
            // insert
            arc.insert(k, &[0u8; 16], NestArcBufType::Data);
        } else if r & 0x3 == 2 {
            // hit_count
            sink = sink.wrapping_add(arc.hit_count());
        } else {
            // current_size
            sink = sink.wrapping_add(arc.current_size());
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.10: ZIL 日志 dispatch bench
// ============================================================================
//
// G-07: 本地 `MockZilRecord` / `HostZilLog` / `StandardHostZil` / `ZilLogState`
// 复刻已删除, 直引内核 `services::fs::unkfs::zil::NestZil`. 注: 内核无
// `is_enabled` / `set_enabled` / `current_seq()` / `committed_seq()` 访问器,
// 序列号域为 `AtomicU64` 直读.

/// bench: ZIL 日志 (add_record / current_seq / pending_count / commit) throughput
pub fn zil_log_dispatch_bench(iters: u64) -> u128 {
    let zil = NestZil::new();
    zil.init();
    // 预热
    for i in 0..1000 {
        zil.add_record(NestZilRecord::new_write(1, 100, i, 4096));
    }
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        if r & 0x7 == 0 {
            // commit (稀有)
            zil.commit((r & 0x3) + 1);
        } else if r & 0x3 == 1 {
            // current_seq
            sink = sink.wrapping_add(zil.current_seq.load(Ordering::Acquire));
        } else if r & 0x3 == 2 {
            // pending_count
            sink = sink.wrapping_add(zil.pending_count() as u64);
        } else {
            // add_record
            zil.add_record(NestZilRecord::new_write(1, 100, r, 4096));
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ============================================================================
// LEGACY-5.11: ZIL 持久化 dispatch bench
// ============================================================================
//
// G-07: 本地 `HostZilPersist` / `StandardHostZilPersist` / `MockZilPersistState`
// 复刻已删除, 直引内核 `services::fs::unkfs::zil_persist::NestZilPersist`.
// 注: 内核 serialize/deserialize 为关联函数, 输入为真实 `NestZil` 记录集
// (每块上限 `ZIL_MAX_RECORDS_PER_BLOCK` = 15 条), 含 CRC32 逐位计算.

/// 预热用 ZIL 记录数 (等于内核单块记录上限 15, 使 serialize 走满块路径)
const BENCH_ZIL_PERSIST_RECORDS: u64 = 15;

/// bench: ZIL 持久化 (serialize / deserialize / mark_written) throughput
pub fn zil_persist_dispatch_bench(iters: u64) -> u128 {
    let persist = NestZilPersist::new();
    let zil = NestZil::new();
    zil.init();
    for i in 0..BENCH_ZIL_PERSIST_RECORDS {
        zil.add_record(NestZilRecord::new_write(1, 100, i, 4096));
    }
    // 预热: serialize + deserialize
    let warm_block = NestZilPersist::serialize_zil_to_block(&zil, 1);
    if let Some(b) = &warm_block {
        let _ = NestZilPersist::deserialize_zil_from_block(b);
    }
    let start = Instant::now();
    let mut sink: u64 = 0;
    for r in 0..iters {
        if r & 0x3 == 0 {
            // serialize
            if let Some(b) = NestZilPersist::serialize_zil_to_block(&zil, 1) {
                sink = sink.wrapping_add(b.len() as u64);
            }
        } else if r & 0x3 == 1 {
            // deserialize
            if let Some(b) = &warm_block {
                sink =
                    sink.wrapping_add(NestZilPersist::deserialize_zil_from_block(b).len() as u64);
            }
        } else {
            // mark_written
            persist.mark_written();
        }
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed.saturating_mul(1_000) / iters as u128
}

// ====== 编排器 ======

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct BenchEntry {
    pub name: String,
    pub category: String,
    pub iterations: u64,
    pub total_ns: u128,
    /// 整数纳秒 (向下取整)
    pub ns_per_op: u128,
    /// 浮点纳秒 (保留亚纳秒精度, 用于跨运行对比)
    pub ns_per_op_frac: f64,
    /// 整数皮秒 (精确)
    pub ps_per_op: u128,
    pub ops_per_sec: u128,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct BenchReport {
    pub version: u32,
    pub results: Vec<BenchEntry>,
}

/// 约定: `f(iters)` 执行 `iters` 轮工作, 并返回该轮工作量的**单操作皮秒数**
/// (bench 内已按自身总操作数归一化 —— 含 BATCH 倍数, 故归一化只能留在 bench 内,
/// measure 不再二次归一化). 闭包/函数接收 `iters`, 与本函数记账的 `iterations` 同源.
///
/// 修复 (G-07 收尾轮, 详见 docs/plan/framekernel-bench-measure-fix.md): 历史上闭包
/// 捕获字面量 iters, 自适应放大值从未传入 bench; 且 measure 把 bench 返回的
/// ps_per_op 当作"总耗时"再除以放大后的 iters, 二次归一化使记录值 = 实际/10⁴
/// (单操作 < 10ns 者直接折叠为 0). 现为「一次预热 + 一次计时」, 三项字段自洽:
/// `iterations` = 实际轮数, `total_ns` = 计时轮实测墙钟, `ps_per_op` = bench 自报单操作值.
fn measure<F: Fn(u64) -> u128>(name: &str, category: &str, iters: u64, f: F) -> BenchEntry {
    // 预热一次消除首次调用的冷 cache / 惰性初始化偏差, 只对第二次计时
    let _ = f(iters);
    let start = Instant::now();
    let ps_per_op = f(iters);
    let total_ns = start.elapsed().as_nanos();
    let ns_per_op = ps_per_op / 1_000;
    let ns_per_op_frac = (ps_per_op as f64) / 1_000.0;
    let ops_per_sec = if ns_per_op_frac > 0.0 {
        (1_000_000_000.0 / ns_per_op_frac) as u128
    } else {
        0
    };
    BenchEntry {
        name: name.to_string(),
        category: category.to_string(),
        iterations: iters,
        total_ns,
        ns_per_op,
        ns_per_op_frac,
        ps_per_op,
        ops_per_sec,
    }
}

#[allow(clippy::vec_init_then_push)] // 23 项基准测试, vec![] 宏可读性差
pub fn run_all() -> BenchReport {
    let mut results = Vec::new();
    // 第三参数 = 该 bench 的执行轮数 (measure 记账用, 同时传给 bench 本体, 单一来源)
    results.push(measure("page_flags_bits", "mm", 100_000, page_flags_bench));
    results.push(measure("pte_set_flags", "mm", 100_000, pte_set_flags_bench));
    results.push(measure(
        "iomem_alias_check",
        "iomem",
        100_000,
        iomem_alias_bench,
    ));
    results.push(measure(
        "capability_check",
        "sgeg",
        100_000,
        capability_check_bench,
    ));
    results.push(measure(
        "dma_state_machine",
        "dma",
        100_000,
        dma_state_machine_bench,
    ));
    results.push(measure("sha256_block", "sgeg", 1_000, sha256_block_bench));
    results.push(measure(
        "attribution_classify",
        "freg",
        100_000,
        attribution_classify_bench,
    ));
    results.push(measure(
        "recovery_decide",
        "freg",
        100_000,
        recovery_decide_bench,
    ));
    results.push(measure("bitmap_scan", "pmm", 100_000, bitmap_scan_bench));
    results.push(measure(
        "socket_wait_queue",
        "net",
        10_000,
        socket_wait_queue_bench,
    ));
    results.push(measure(
        "virtio_blk_io",
        "storage",
        10_000,
        virtio_blk_io_bench,
    ));
    // EBPF-3: eBPF verifier trait dispatch bench
    results.push(measure(
        "bpf_verifier_dispatch",
        "ebpf",
        100_000,
        bpf_verifier_dispatch_bench,
    ));
    // SYSCTL-2: sysctl register/write bench
    results.push(measure("sysctl_rw", "config", 10_000, sysctl_bench));
    // T-4.1: BlockDevice trait dispatch bench (LEGACY-4 验证)
    results.push(measure(
        "blk_dev_dispatch",
        "block",
        100_000,
        blk_dev_dispatch_bench,
    ));
    // REVAL-6.1: VfsPollPolicy dispatch bench
    results.push(measure(
        "vfs_poll_dispatch",
        "epoll",
        100_000,
        vfs_poll_dispatch_bench,
    ));
    // LEGACY-5.1: ZAP dispatch bench (线性扫描 + 键名 format, 故缩小 iters)
    results.push(measure("zap_dispatch", "unkfs", 10_000, zap_dispatch_bench));
    // LEGACY-5.2: TXG dispatch bench (脏块 Vec 累积, 故缩小 iters)
    results.push(measure("txg_dispatch", "unkfs", 10_000, txg_dispatch_bench));
    // LEGACY-5.4: DMU dispatch bench (get_obj/obj_count 为 O(n) 线性扫描, 故缩小 iters)
    results.push(measure("dmu_dispatch", "unkfs", 1_000, dmu_dispatch_bench));
    // LEGACY-5.5: SPA dispatch bench
    results.push(measure(
        "spa_dispatch",
        "unkfs",
        100_000,
        spa_dispatch_bench,
    ));
    // LEGACY-5.7: RAID-Z 几何查询 dispatch bench
    results.push(measure(
        "raidz_dispatch",
        "unkfs",
        100_000,
        raidz_dispatch_bench,
    ));
    // LEGACY-5.8: ARC 缓存 dispatch bench
    results.push(measure(
        "arc_dispatch",
        "unkfs",
        100_000,
        arc_dispatch_bench,
    ));
    // LEGACY-5.10: ZIL 日志 dispatch bench
    results.push(measure(
        "zil_log_dispatch",
        "unkfs",
        100_000,
        zil_log_dispatch_bench,
    ));
    // LEGACY-5.11: ZIL 持久化 dispatch bench (含 CRC32 逐位计算, 故缩小 iters)
    results.push(measure(
        "zil_persist_dispatch",
        "unkfs",
        1_000,
        zil_persist_dispatch_bench,
    ));

    BenchReport {
        version: 1,
        results,
    }
}

// ====== 单元测试 (验证算法正确性, 不测时序) ======

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_flags_compose() {
        let f = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
        assert!(f.contains(PageFlags::PRESENT));
        assert!(f.contains(PageFlags::WRITABLE));
        assert!(f.contains(PageFlags::USER));
        assert!(!f.contains(PageFlags::NX));
    }

    #[test]
    fn test_pte_set_flags_round_trip() {
        // G-07: 直引内核 PageTableEntry (原子位域, set_flags 取 &self)
        let pte = PageTableEntry::from_value(0xFFFF_FFFF_FFFF_FFFF);
        pte.set_flags(PageFlags::PRESENT | PageFlags::WRITABLE);
        assert!(pte.is_present());
        let pte2 = PageTableEntry::from_value(0x0);
        assert!(!pte2.is_present());
    }

    #[test]
    fn test_iomem_alias_semantics() {
        // G-07: 直引内核 IoMem — 别名注册表为进程级全局态, 故单测合并为一个函数
        // 并使用独立地址段 (0x8000_1000 起), 避免与其他用例并发相互干扰.
        // SAFETY: phys 为纯算术载体 (IoMem::new 不解引用该地址), 同 bench 语义.
        let a = unsafe { IoMem::new(PhysAddr(0x8000_1000), 0x800, "bench.t1") };
        assert!(a.is_ok());
        // 完全不相邻 → 可注册
        assert!(unsafe { IoMem::new(PhysAddr(0x8000_2000), 0x800, "bench.t2") }.is_ok());
        // 起点在内 → 冲突
        assert!(unsafe { IoMem::new(PhysAddr(0x8000_1200), 0x800, "bench.t3") }.is_err());
        // 起点边界 (完全在内) → 冲突
        assert!(unsafe { IoMem::new(PhysAddr(0x8000_1000), 0x400, "bench.t4") }.is_err());
        // 起点在外, 末端在内 → 冲突
        assert!(unsafe { IoMem::new(PhysAddr(0x8000_0800), 0x900, "bench.t5") }.is_err());
        // 与 a 末端相接 (不重叠) → 可注册
        assert!(unsafe { IoMem::new(PhysAddr(0x8000_1800), 0x800, "bench.t6") }.is_ok());
    }

    #[test]
    fn test_capability_check() {
        // G-07: 直引内核 `PolicyEngine::check` + `InMemoryMatrix`
        let m = InMemoryMatrix::new();
        let _ = m.set(CapDomain::FS, CapBits(0b11));
        let engine = PolicyEngine::new();
        // 已授予且不含可行下界 (FS 下界 = READ|EXEC = 0b101) → 允许
        assert_eq!(
            engine.check(&m, CapDomain::FS, CapBits(0b01)),
            PolicyResult::Allow
        );
        assert_eq!(
            engine.check(&m, CapDomain::FS, CapBits(0b10)),
            PolicyResult::Allow
        );
        // 未授予位 → 无权限
        assert!(matches!(
            engine.check(&m, CapDomain::FS, CapBits(0b100)),
            PolicyResult::Deny(_)
        ));
    }

    #[test]
    fn test_dma_state_machine() {
        // G-07: 直引内核 DmaStream — 双向流 CpuReady ↔ DeviceReady ping-pong
        // SAFETY: 0x10000 页对齐, 见 bench_frame() 说明
        let frame = unsafe { bench_frame(0x10000, 0) };
        let mut s = DmaStream::from_frame(frame, DmaDirection::Bidirectional).unwrap();
        assert_eq!(s.sync_state(), SyncState::CpuReady);
        assert!(s.sync_for_device().is_ok());
        assert_eq!(s.sync_state(), SyncState::DeviceReady);
        assert!(s.sync_for_cpu().is_ok());
        assert_eq!(s.sync_state(), SyncState::CpuReady);
        // ToDevice 方向调用 sync_for_cpu → 状态机拒绝
        // SAFETY: 同上
        let frame2 = unsafe { bench_frame(0x20000, 0) };
        let mut t = DmaStream::from_frame(frame2, DmaDirection::ToDevice).unwrap();
        assert!(t.sync_for_cpu().is_err());
    }

    #[test]
    fn test_sha256_known_digest() {
        // G-07: 直引内核 sgeg `sha256` — 已知向量 "abc" (与 framework 侧单测同源)
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(sha256(b"abc"), expected);
    }

    #[test]
    fn test_attribution_tcb_path() {
        // G-07: 直引内核 FaultAttributor — 按 RIP 落入地址区间判定归属
        assert!(matches!(
            FaultAttributor::attribute(0xFFFF_FFFF_8000_0000),
            FaultAttribution::Tcb { .. }
        ));
        // Services 区间
        assert!(matches!(
            FaultAttributor::attribute(0xFFFF_FFFF_0000_0000),
            FaultAttribution::Service { .. }
        ));
        // 两区间外 → Unknown
        assert!(matches!(
            FaultAttributor::attribute(0x0000_1000_0000_0000),
            FaultAttribution::Unknown
        ));
    }

    #[test]
    fn test_recovery_tcb_is_bhr() {
        // G-07: 直引内核 RecoveryPolicy — TCB 故障不可恢复 → 硬重置
        let s = FaultSignal::tcb(TcbModule::FREG, 0);
        assert_eq!(RecoveryPolicy::decide(&s), RecoveryAction::FREGHardReset);
    }

    #[test]
    fn test_pmm_alloc_free_roundtrip() {
        // G-07: 直引内核 PMM (经 `VecMetaStore` 注入宿主载体, 同 pmm_buddy_host_test.rs)
        let pmm = PhysicalMemoryManager::new();
        pmm.inject_meta_store(VecMetaStore::new());
        pmm.init(BENCH_MEM_SIZE, BENCH_KERNEL_END);
        pmm.init_bitmap(0);
        let a = pmm.alloc_page().expect("首次分配应成功");
        assert!(a.0 >= BENCH_KERNEL_END, "分配不得落入内核保留区");
        pmm.free_page(a);
        // 释放后页回到池: 再次分配应成功
        let b = pmm.alloc_page().expect("释放后再分配应成功");
        pmm.free_page(b);
    }

    #[test]
    fn test_socket_wait_queue_mark_then_wake() {
        // G-07: 直引内核 SocketWaitQueue
        let q = SocketWaitQueue::new();
        // 首次 mark_waiting 返回 true (之前未标记)
        assert!(q.mark_waiting());
        // 重复 mark_waiting 返回 false (已经标记)
        assert!(!q.mark_waiting());
        // try_wake 成功清掉 pending, 返回 true
        assert!(q.try_wake(WakeReason::Readable));
        // 没有等待者时 try_wake 返回 false
        assert!(!q.try_wake(WakeReason::Readable));
        assert_eq!(q.wake_count(), 1);
        assert_eq!(q.last_reason(), Some(WakeReason::Readable));
    }

    #[test]
    fn test_socket_wait_queue_bench_runs() {
        // smoke: 调用一次小迭代 bench 确保不 panic
        let _ = socket_wait_queue_bench(10);
    }

    #[test]
    fn test_virtio_prepare_desc_chain() {
        let mut backing = vec![0u64; VQ_BACKING_BYTES / 8];
        // SAFETY: backing 在本作用域内存活, 覆盖 vq 全部使用期
        let mut vq = unsafe { bench_virtqueue(&mut backing) };
        // 描述符链: header → data → status (末段设备写)
        let h1 = vq.prepare_desc(0x1000, 16, false);
        let h2 = vq.prepare_desc(0x2000, BLK_4K_BYTES, false);
        let h3 = vq.prepare_desc(0x3000, 1, true);
        assert_eq!((h1, h2, h3), (0, 1, 2), "空闲描述符链应顺序分配");
        vq.link_desc(h1, h2);
        vq.link_desc(h2, h3);
        // SAFETY: vq.desc 指向本作用域内 backing
        unsafe {
            assert_eq!((*vq.desc.add(0)).next, 1);
            assert_eq!((*vq.desc.add(0)).flags & VQ_DESC_F_NEXT, VQ_DESC_F_NEXT);
            assert_eq!((*vq.desc.add(2)).flags & VQ_DESC_F_WRITE, VQ_DESC_F_WRITE);
        }
        // 提交 → avail 环写入 head
        assert_eq!(vq.submit(h1), 0);
        vq.commit_and_kick();
        // 设备侧完成 → pop_used 取回链头
        // SAFETY: vq.used 指向本作用域内 backing
        unsafe { bench_device_complete(&mut vq, h1, BLK_4K_BYTES) };
        assert_eq!(vq.pop_used(), Some((h1, BLK_4K_BYTES)));
        // 无新完成 → None
        assert_eq!(vq.pop_used(), None);
    }

    #[test]
    fn test_virtio_blk_bench_runs() {
        // smoke: 调用一次小迭代 bench 确保不 panic
        let _ = virtio_blk_io_bench(10);
    }

    // ====== EBPF-3: BpfVerifier trait dispatch + bench ======
    // G-07: 直引内核真实实现 — framework 机制 (`BpfProg`/`BpfVerifier` trait/
    // `BpfSubsystem`) + services 策略 (`STANDARD_VERIFIER`), 无本地 mock.

    /// 构造最小合法程序: ALU64 MOV r0,0 + EXIT (通过内核 7 条规则)
    fn bench_min_valid_insns() -> Vec<BpfInsn> {
        vec![
            BpfInsn::new(opcode::ALU64 | opcode::MOV, 0, 0, 0, 0),
            BpfInsn::new(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ]
    }

    #[test]
    fn test_bpf_verifier_trait_dispatch() {
        // 关键: 验证 `&dyn BpfVerifier` 动态分派机制可工作 (内核最小合法程序)
        let prog = BpfProg::new(BpfProgType::SocketFilter, bench_min_valid_insns());
        let v: &dyn BpfVerifier = &STANDARD_VERIFIER;
        assert!(matches!(v.verify(&prog), VerifyResult::Ok));
    }

    #[test]
    fn test_bpf_verifier_reject() {
        // 拒绝路径: 缺 EXIT 结尾的程序被内核验证器拒绝 (规则 5)
        let insns = vec![BpfInsn::new(opcode::ALU64 | opcode::MOV, 0, 0, 0, 0)];
        let prog = BpfProg::new(BpfProgType::SocketFilter, insns);
        let v: &dyn BpfVerifier = &STANDARD_VERIFIER;
        assert!(matches!(v.verify(&prog), VerifyResult::Err(_)));
    }

    #[test]
    fn test_bpf_subsystem_safety_default_no_verifier() {
        // 安全默认: 未注册 verifier 时, prog_load 拒绝所有
        // (framekernel 设计, 拒绝 = 安全默认). 用局部实例, 不扰动全局 BPF_SUBSYSTEM.
        let subsys = BpfSubsystem::new();
        let result = subsys.prog_load(BpfProgType::SocketFilter, bench_min_valid_insns());
        assert_eq!(result, -1); // EPERM: no verifier registered
    }

    #[test]
    fn test_bpf_subsystem_set_then_load() {
        // 注册后 prog_load 走 verifier (局部实例, 不扰动全局 BPF_SUBSYSTEM)
        let subsys = BpfSubsystem::new();
        subsys.set_verifier(&STANDARD_VERIFIER);
        let result = subsys.prog_load(BpfProgType::SocketFilter, bench_min_valid_insns());
        assert_eq!(result, 1); // fd = 1
    }

    #[test]
    fn test_bpf_verifier_bench_runs() {
        // smoke: 调用一次小迭代 bench
        let _ = bpf_verifier_dispatch_bench(100);
    }

    // ====== SYSCTL-2: 内核 sysctl 全局注册表单元测试 ======
    // G-07: 直引内核 `services::config::sysctl` 真实实现 (无本地 mock).
    // 命名空间用 `ut.sysctl.*` 与 bench 节点 `bench.sysctl.*` 隔离.

    #[test]
    fn test_sysctl_register_and_read() {
        assert_eq!(
            sysctl_register("ut.sysctl.reg", SysctlKind::Int, SysctlValue::Int(42)),
            Ok(())
        );
        assert_eq!(sysctl_read("ut.sysctl.reg"), Some(SysctlValue::Int(42)));
    }

    #[test]
    fn test_sysctl_write_type_mismatch() {
        assert_eq!(
            sysctl_register("ut.sysctl.tm", SysctlKind::Int, SysctlValue::Int(0)),
            Ok(())
        );
        // 写 Bool 到 Int 节点
        assert_eq!(
            sysctl_write("ut.sysctl.tm", SysctlValue::Bool(true)),
            Err(SysctlError::TypeMismatch)
        );
    }

    #[test]
    fn test_sysctl_read_not_found() {
        assert_eq!(sysctl_read("ut.sysctl.nonexistent"), None);
    }

    #[test]
    fn test_sysctl_duplicate_register() {
        assert_eq!(
            sysctl_register("ut.sysctl.dup", SysctlKind::UInt, SysctlValue::UInt(1)),
            Ok(())
        );
        assert_eq!(
            sysctl_register("ut.sysctl.dup", SysctlKind::UInt, SysctlValue::UInt(2)),
            Err(SysctlError::Duplicate)
        );
    }

    #[test]
    fn test_sysctl_bench_runs() {
        let _ = sysctl_bench(10);
    }

    // ====== T-4.1 (LEGACY-4): 块设备 I/O 路径单元测试 (经内核 egdf 注册表) ======

    /// 注册 4 扇区的独立块设备载具 (边界用例专用), 返回 drive 索引
    fn bench_blk_small_drive() -> u8 {
        static SLOT: OnceLock<u8> = OnceLock::new();
        *SLOT.get_or_init(|| {
            let dev: &'static mut BenchBlockDevice = Box::leak(Box::new(BenchBlockDevice::new(4)));
            egdf_register_block_dev("bench_blk_small", None, None, dev) as u8
        })
    }

    #[test]
    fn test_blk_dev_read_write_roundtrip() {
        let drive = bench_blk_drive();
        let wbuf = [0xAB; 512];
        assert_eq!(egdf_blk_write(drive, 1, &wbuf), 0);
        let mut rbuf = [0u8; 512];
        assert_eq!(egdf_blk_read(drive, 1, &mut rbuf), 0);
        assert_eq!(rbuf[0], 0xAB);
    }

    #[test]
    fn test_blk_dev_oob() {
        let drive = bench_blk_small_drive();
        let buf = [0u8; 512];
        let mut rbuf = [0u8; 512];
        // 越界 sector (容量 4) 应由载体返回 -EIO
        assert_eq!(egdf_blk_read(drive, 100, &mut rbuf), -5);
        assert_eq!(egdf_blk_write(drive, 100, &buf), -5);
    }

    #[test]
    fn test_blk_dev_buf_too_small() {
        let drive = bench_blk_small_drive();
        let mut small = [0u8; 256];
        // 内核 dispatch 层 buf.len() < 512 → -EINVAL
        assert_eq!(egdf_blk_read(drive, 0, &mut small), -22);
    }

    #[test]
    fn test_blk_dev_metadata() {
        let drive = bench_blk_small_drive();
        assert!(egdf_blk_is_present(drive));
        // 容量 4: 末扇区可读, 越界不可读
        let mut buf = [0u8; 512];
        assert_eq!(egdf_blk_read(drive, 3, &mut buf), 0);
        assert_eq!(egdf_blk_read(drive, 4, &mut buf), -5);
        // 未注册的 drive 索引 → 不存在
        assert!(!egdf_blk_is_present(200));
    }

    #[test]
    fn test_blk_dev_unregistered_drive() {
        let mut buf = [0u8; 512];
        assert_eq!(egdf_blk_read(200, 0, &mut buf), -5);
    }

    #[test]
    fn test_blk_dev_dispatch_bench_runs() {
        // smoke test
        let _ = blk_dev_dispatch_bench(100);
    }

    // ====== REVAL-6.1: VfsPollPolicy 事件位单元测试 ======

    #[test]
    fn test_vfs_poll_events_for_file_type() {
        let p = StandardVfsPollPolicy;
        assert_eq!(
            p.events_for_file_type(VfsFileType::File),
            EPOLLIN | EPOLLOUT
        );
        assert_eq!(p.events_for_file_type(VfsFileType::Dir), EPOLLIN);
        assert_eq!(p.events_for_file_type(VfsFileType::Dev), EPOLLHUP);
        assert_eq!(
            p.events_for_file_type(VfsFileType::Symlink),
            EPOLLIN | EPOLLHUP
        );
    }

    #[test]
    fn test_vfs_poll_events_for_invalid_fd() {
        let p = StandardVfsPollPolicy;
        assert_eq!(p.events_for_invalid_fd(), EPOLLERR | EPOLLHUP);
    }

    #[test]
    fn test_vfs_poll_ref_registered_valid_file() {
        let policy = VfsPollPolicyRef::Registered(&BENCH_VFS_POLL_POLICY);
        let ctx = VfsPollContext {
            valid: true,
            file_type: VfsFileType::File,
        };
        // File → IN|OUT, 与 user 掩码 AND 后按关心位报告
        assert_eq!(
            policy.events_for(ctx) & BENCH_EPOLL_MASK,
            EPOLLIN | EPOLLOUT
        );
        assert_eq!(policy.events_for(ctx) & EPOLLIN, EPOLLIN);
        assert_eq!(policy.events_for(ctx) & EPOLLOUT, EPOLLOUT);
        // File 不报告 ERR
        assert_eq!(policy.events_for(ctx) & EPOLLERR, 0);
    }

    #[test]
    fn test_vfs_poll_ref_registered_invalid_fd() {
        let policy = VfsPollPolicyRef::Registered(&BENCH_VFS_POLL_POLICY);
        let ctx = VfsPollContext {
            valid: false,
            file_type: VfsFileType::File,
        };
        // 无效 fd → ERR|HUP, 与 user 掩码 AND 后按关心位报告
        assert_eq!(
            policy.events_for(ctx) & BENCH_EPOLL_MASK,
            EPOLLERR | EPOLLHUP
        );
        assert_eq!(policy.events_for(ctx) & EPOLLIN, 0);
    }

    #[test]
    fn test_vfs_poll_ref_fallback_without_registered_policy() {
        // 未注册策略时 Fallback 分支仍给出事件位 (与原硬编码一致)
        let policy = VfsPollPolicyRef::Fallback;
        let ctx = VfsPollContext {
            valid: true,
            file_type: VfsFileType::File,
        };
        assert_ne!(policy.events_for(ctx) & BENCH_EPOLL_MASK, 0);
    }

    #[test]
    fn test_vfs_poll_bench_runs() {
        let _ = vfs_poll_dispatch_bench(100);
    }

    // ====== LEGACY-5.1: ZAP 单元测试 ======

    #[test]
    fn test_zap_insert_lookup() {
        let zap = NestZap::new();
        assert!(zap.insert("a", b"1"));
        assert_eq!(zap.lookup("a"), Some(b"1".to_vec()));
        assert_eq!(zap.lookup("nokey"), None);
    }

    #[test]
    fn test_zap_update() {
        let zap = NestZap::new();
        assert!(zap.insert("k", b"v1"));
        assert!(zap.insert("k", b"v2"));
        assert_eq!(zap.lookup("k"), Some(b"v2".to_vec()));
        // 原地覆盖, 不新增条目
        assert_eq!(zap.len(), 1);
    }

    #[test]
    fn test_zap_capacity_limit() {
        let zap = NestZap::with_capacity(2);
        assert!(zap.insert("a", b"1"));
        assert!(zap.insert("b", b"2"));
        // 内核实现: 容量满后一切 insert 均拒 (含已存在键的更新)
        assert!(!zap.insert("c", b"3"));
        assert!(!zap.insert("a", b"x"));
        assert_eq!(zap.len(), 2);
    }

    #[test]
    fn test_zap_u64() {
        let zap = NestZap::new();
        assert!(zap.insert_u64("count", 42));
        assert_eq!(zap.lookup_u64("count"), Some(42));
    }

    #[test]
    fn test_zap_remove() {
        let zap = NestZap::new();
        zap.insert("a", b"1");
        assert!(zap.contains("a"));
        assert!(zap.remove("a"));
        assert!(!zap.contains("a"));
        assert!(!zap.remove("a"));
    }

    #[test]
    fn test_zap_bench_runs() {
        let _ = zap_dispatch_bench(100);
    }

    // ====== LEGACY-5.2: TXG 单元测试 ======

    #[test]
    fn test_txg_init() {
        let mut txg = NestTxgGroup::new();
        txg.init(1);
        assert_eq!(txg.current_txg(), 1);
        // init 后 open/quiescing/syncing 槽位分别指向 txgs[0..3]
        assert_eq!(txg.get_open_txg().expect("open 槽位存在").txg_id, 1);
        assert_eq!(txg.get_syncing_txg().expect("syncing 槽位存在").txg_id, 3);
        assert_eq!(txg.total_syncs.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_txg_transition() {
        let mut txg = NestTxgGroup::new();
        txg.init(1);
        let old = txg.current_txg();
        let new = txg.transition();
        assert!(new > old);
        assert_eq!(txg.total_syncs.load(Ordering::Acquire), 1);
    }

    #[test]
    fn test_txg_dirty_accumulate() {
        let mut txg = NestTxgGroup::new();
        txg.init(1);
        for _ in 0..5 {
            txg.add_dirty_to_open(NestBlockPointer::null());
        }
        assert_eq!(txg.total_dirty.load(Ordering::Acquire), 5);
    }

    #[test]
    fn test_txg_bench_runs() {
        let _ = txg_dispatch_bench(100);
    }

    // ====== LEGACY-5.4: DMU 单元测试 ======

    #[test]
    fn test_dmu_uninitialized() {
        let dmu = NestObjSet::new();
        assert!(!dmu.initialized.load(Ordering::Acquire));
        assert_eq!(dmu.obj_count(), 0);
        // 未 init 时对象表为空 → root 不可得
        assert!(dmu.get_root().is_none());
    }

    #[test]
    fn test_dmu_init_creates_root() {
        let dmu = NestObjSet::new();
        dmu.init(0x100);
        assert!(dmu.initialized.load(Ordering::Acquire));
        // init 后有 root + meta 两个对象
        assert_eq!(dmu.obj_count(), 2);
        assert_eq!(
            dmu.get_root().expect("root 对象存在").obj_type,
            NestObjType::Dir
        );
    }

    #[test]
    fn test_dmu_alloc_obj() {
        let dmu = NestObjSet::new();
        dmu.init(0x100);
        let f = dmu
            .alloc_obj(NestObjType::File, 0x100)
            .expect("File 分配成功");
        // init 后 next_obj_id = root+2 = 4
        assert!(f >= 4);
        assert_eq!(
            dmu.get_obj(f).expect("已分配对象可查").obj_type,
            NestObjType::File
        );
    }

    #[test]
    fn test_dmu_free_link_count() {
        let dmu = NestObjSet::new();
        dmu.init(0x100);
        let f = dmu
            .alloc_obj(NestObjType::File, 0x100)
            .expect("File 分配成功");
        assert_eq!(dmu.get_obj(f).expect("对象可查").link_count, 1);
        assert!(dmu.free_obj(f));
        // link_count 归 0 → used=false, 查询不到
        assert!(dmu.get_obj(f).is_none());
        assert_eq!(dmu.obj_count(), 2);
    }

    #[test]
    fn test_dmu_unsupported_obj_type() {
        let dmu = NestObjSet::new();
        dmu.init(0x100);
        // 内核仅支持 File/Dir/Zap/ZapMicro/Symlink
        assert!(dmu.alloc_obj(NestObjType::None, 0x100).is_none());
        assert!(dmu.alloc_obj(NestObjType::Snapshot, 0x100).is_none());
    }

    #[test]
    fn test_dmu_bench_runs() {
        let _ = dmu_dispatch_bench(100);
    }

    // ====== LEGACY-5.5: SPA 单元测试 ======

    #[test]
    fn test_spa_uninitialized() {
        let spa = NestSpa::new();
        assert!(!spa.is_initialized());
        assert_eq!(spa.vdevs.lock().len(), 0);
    }

    #[test]
    fn test_spa_init() {
        let spa = NestSpa::new();
        spa.init("tank");
        assert!(spa.is_initialized());
        assert_eq!(spa.current_txg(), 1);
        assert_ne!(spa.config.lock().guid, 0);
    }

    #[test]
    fn test_spa_add_vdev() {
        let spa = NestSpa::new();
        spa.init("tank");
        assert!(spa.add_vdev(NestVdevConfig::new_disk(0, "d0", 9)));
        assert!(spa.add_vdev(NestVdevConfig::new_disk(1, "d1", 9)));
        assert_eq!(spa.vdevs.lock().len(), 2);
    }

    #[test]
    fn test_spa_vdev_limit() {
        let spa = NestSpa::new();
        spa.init("tank");
        // 内核上限 = config.max_vdevs (默认 8)
        for i in 0..8u16 {
            assert!(spa.add_vdev(NestVdevConfig::new_disk(i, "d", 9)));
        }
        assert!(!spa.add_vdev(NestVdevConfig::new_disk(8, "d", 9)));
    }

    #[test]
    fn test_spa_advance_txg() {
        let spa = NestSpa::new();
        spa.init("tank");
        let t1 = spa.advance_txg();
        let t2 = spa.advance_txg();
        assert!(t2 > t1);
        assert_eq!(spa.current_txg(), t2);
    }

    #[test]
    fn test_spa_bench_runs() {
        let _ = spa_dispatch_bench(100);
    }

    // ====== LEGACY-5.7: RAID-Z 几何单元测试 ======

    #[test]
    fn test_raidz_z1_geometry() {
        let map = NestRaidzMap::new(NestRaidzLevel::RaidZ1, 3, 9);
        assert_eq!(map.ncols, 3);
        assert_eq!(map.nparity, 1);
        assert_eq!(map.data_cols(), 2);
        assert_eq!(map.level.max_failures(), 1);
    }

    #[test]
    fn test_raidz_z2_geometry() {
        let map = NestRaidzMap::new(NestRaidzLevel::RaidZ2, 5, 12);
        assert_eq!(map.nparity, 2);
        assert_eq!(map.data_cols(), 3);
        assert_eq!(map.level.max_failures(), 2);
        assert_eq!(map.ashift, 12);
    }

    #[test]
    fn test_raidz_mirror_and_single_parity() {
        // Mirror 与 Single 均无校验列, 但 Mirror 容许 1 块盘故障
        let mirror = NestRaidzMap::new(NestRaidzLevel::Mirror, 2, 9);
        assert_eq!(mirror.nparity, 0);
        assert_eq!(mirror.level.max_failures(), 1);
        let single = NestRaidzMap::new(NestRaidzLevel::Single, 2, 9);
        assert_eq!(single.nparity, 0);
        assert_eq!(single.level.max_failures(), 0);
    }

    #[test]
    fn test_raidz_cols_clamped() {
        // 构造时 ncols 被 clamp 到 [HV_RAIDZ_MIN_COLS, HV_RAIDZ_MAX_COLS]
        let low = NestRaidzMap::new(NestRaidzLevel::Single, 1, 9);
        assert_eq!(low.ncols, HV_RAIDZ_MIN_COLS);
        let high = NestRaidzMap::new(NestRaidzLevel::Single, 64, 9);
        assert_eq!(high.ncols, HV_RAIDZ_MAX_COLS);
    }

    #[test]
    fn test_raidz_bench_runs() {
        let _ = raidz_dispatch_bench(100);
    }

    // ====== LEGACY-5.8: ARC 单元测试 ======

    #[test]
    fn test_arc_uninitialized() {
        let arc = StandardArc::new();
        assert!(!arc.is_initialized());
    }

    #[test]
    fn test_arc_lookup_miss_hit() {
        let arc = StandardArc::new();
        arc.init(10);
        let k = NestArcKey::new(0, 0, 0);
        // 首次 lookup → miss
        assert!(!arc.lookup(&k));
        assert_eq!(arc.miss_count(), 1);
        arc.insert(k, &[1u8; 16], NestArcBufType::Data);
        // 二次 lookup → hit
        assert!(arc.lookup(&k));
        assert_eq!(arc.hit_count(), 1);
    }

    #[test]
    fn test_arc_capacity_eviction() {
        let arc = StandardArc::new();
        // 内核淘汰以「条目数」为口径 (max_size 为条目上限)
        arc.init(3);
        for i in 0..5u64 {
            arc.insert(NestArcKey::new(0, i, 0), &[0u8; 16], NestArcBufType::Data);
        }
        assert!(arc.evict_count() > 0);
        // 存活条目数不超过 max_size
        assert!(arc.mru_size() + arc.mfu_size() <= 3);
    }

    #[test]
    fn test_arc_hit_rate() {
        let arc = StandardArc::new();
        arc.init(10);
        let k = NestArcKey::new(0, 0, 0);
        arc.insert(k, &[0u8; 16], NestArcBufType::Data);
        arc.lookup(&k);
        arc.lookup(&k);
        arc.lookup(&NestArcKey::new(0, 99, 0));
        arc.lookup(&NestArcKey::new(0, 100, 0));
        // 内核 `hit_rate()` 为千分比: 2 hit / 4 total → 500
        assert_eq!(arc.hit_rate(), 500);
    }

    #[test]
    fn test_arc_bench_runs() {
        let _ = arc_dispatch_bench(100);
    }

    // ====== LEGACY-5.10: ZIL 日志单元测试 ======

    #[test]
    fn test_zil_log_init() {
        let zil = NestZil::new();
        zil.init();
        assert!(zil.enabled.load(Ordering::Acquire));
        assert_eq!(zil.current_seq.load(Ordering::Acquire), 0);
        assert_eq!(zil.pending_count(), 0);
    }

    #[test]
    fn test_zil_log_add_record() {
        let zil = NestZil::new();
        zil.init();
        zil.add_record(NestZilRecord::new_write(1, 100, 0, 4096));
        assert_eq!(zil.current_seq.load(Ordering::Acquire), 1);
        assert_eq!(zil.pending_count(), 1);
        assert!(zil.has_uncommitted());
    }

    #[test]
    fn test_zil_log_commit() {
        let zil = NestZil::new();
        zil.init();
        zil.add_record(NestZilRecord::new_write(1, 100, 0, 4096));
        zil.add_record(NestZilRecord::new_write(2, 100, 0, 4096));
        zil.add_record(NestZilRecord::new_write(3, 100, 0, 4096));
        // commit txg=2 → 保留 txg=3
        zil.commit(2);
        assert_eq!(zil.pending_count(), 1);
        // committed_seq 是被移除记录的最大 seq (seq 1, 2 被移除 → 2)
        assert_eq!(zil.committed_seq.load(Ordering::Acquire), 2);
    }

    #[test]
    fn test_zil_log_disabled() {
        let zil = NestZil::new();
        zil.init();
        zil.enabled.store(false, Ordering::Release);
        zil.add_record(NestZilRecord::new_write(1, 100, 0, 4096));
        // disable 后 add_record 不分配 seq
        assert_eq!(zil.current_seq.load(Ordering::Acquire), 0);
        assert_eq!(zil.pending_count(), 0);
    }

    #[test]
    fn test_zil_bench_runs() {
        let _ = zil_log_dispatch_bench(100);
    }

    // ====== LEGACY-5.11: ZIL 持久化单元测试 ======

    /// 构造含 `count` 条 write 记录的 ZIL
    fn zil_with_records(count: u64) -> NestZil {
        let zil = NestZil::new();
        zil.init();
        for i in 0..count {
            zil.add_record(NestZilRecord::new_write(1, 100, i, 4096));
        }
        zil
    }

    #[test]
    fn test_zil_persist_serialize_empty() {
        let zil = NestZil::new();
        zil.init();
        // 无记录 → 无块可写
        assert!(NestZilPersist::serialize_zil_to_block(&zil, 1).is_none());
    }

    #[test]
    fn test_zil_persist_roundtrip() {
        let zil = zil_with_records(10);
        let block = NestZilPersist::serialize_zil_to_block(&zil, 1).expect("有记录时块生成成功");
        assert_eq!(block.len(), 4096);
        let records = NestZilPersist::deserialize_zil_from_block(&block);
        assert_eq!(records.len(), 10);
        // 反序列化按 seq 升序还原
        assert_eq!(records[0].seq, 1);
        assert_eq!(records[9].seq, 10);
    }

    #[test]
    fn test_zil_persist_short_block() {
        // 长度不足 4096 → 空记录 (整块拒绝)
        assert!(NestZilPersist::deserialize_zil_from_block(&[]).is_empty());
    }

    #[test]
    fn test_zil_persist_corrupt_block_rejected() {
        let zil = zil_with_records(10);
        let mut block = NestZilPersist::serialize_zil_to_block(&zil, 1).expect("块生成成功");
        // 翻转 record 区一个字节 → 块 CRC 失配 → 整块拒绝 (ZFS 块级校验语义)
        block[128] ^= 0xFF;
        assert!(NestZilPersist::deserialize_zil_from_block(&block).is_empty());
    }

    #[test]
    fn test_zil_persist_mark_written() {
        let persist = NestZilPersist::new();
        assert!(!persist.zil_blocks_written.load(Ordering::Acquire));
        persist.mark_written();
        assert!(persist.zil_blocks_written.load(Ordering::Acquire));
    }

    #[test]
    fn test_zil_persist_bench_runs() {
        let _ = zil_persist_dispatch_bench(100);
    }
}
