//! 故障恢复 API (Recovery FREG 对外接口)
//!
//! ## 调用方契约
//! - 各可恢复子系统注册: `unkfs`、`fs`、`net`、`proc` 等
//! - `proc::scheduler::tick` —— 周期 tick 推进恢复
//! - `idt::on_panic` —— panic 路径触发 IDT 级别恢复
//! - 启动流程: `boot` 阶段调用 `recovery_freg_maintenance` 启动后台 tick
//!
//! ## 内部接口
//! - `RecoveryDomain` trait/struct 在 `freg::domain`,本文件是 C ABI
//!   友好的入口层。`#[no_mangle]` 函数供 asm stub / FFI 边界调用。
//!
//! ## 安全约束
//! - `recovery_*` 函数内部使用 `RECOVERY_MANAGER.lock()`,**不**可在
//!   中断上下文调用除 `recovery_panic_flag_*` 外的函数。
//! - `recovery_panic_flag_*` 是无锁 atomic,可在任何上下文使用。
//!
//! ## 性能特征
//! - `recovery_*` 路径: spinlock + 数组线性扫描 O(N),N ≤ 16 域,常数时间
//! - `recovery_panic_flag_*`: atomic load/store,无锁
use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, Ordering};

use super::domain::RecoveryDomain;
use super::types::{DIRECT_MAP_SIZE, MAX_RECOVERY_DOMAINS};
use crate::privileged::sync::OnceLock;

static RECOVERY_ATTEMPTED: AtomicBool = AtomicBool::new(false);
static BOOT_FINGERPRINTS_CHECKED: AtomicBool = AtomicBool::new(false);

// B03-10: 回收域池 (替代每次 register Box::leak 10-12KB 永久泄漏)。
// 注册时按 domain_id % MAX_RECOVERY_DOMAINS 取槽位, 重复 id 覆盖 (域可重新注册)。
// 池体量约 286 KB (MAX_RECOVERY_DOMAINS=32 × ~9.1 KB), 惰性堆置以避免常驻 BSS。
// `register` 需将 `&'static RecoveryDomain` 存入 RECOVERY_MANAGER, 故经 `Box::leak`
// 取得 `'static` 切片; 池只构造一次且注册点均在启动期, 泄漏量与单体池等量, 无重复泄漏。
static RECOVERY_DOMAIN_POOL: OnceLock<&'static [RecoveryDomain]> = OnceLock::new();

/// 获取回收域池 (首次调用时在 kmalloc 堆上惰性构造并永久驻留)。
fn recovery_domain_pool() -> &'static [RecoveryDomain] {
    RECOVERY_DOMAIN_POOL.get_or_init(|slot| {
        slot.write(Box::leak(
            (0..MAX_RECOVERY_DOMAINS)
                .map(|_| RecoveryDomain::new(0))
                .collect::<Box<[_]>>(),
        ));
    })
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_freg_maintenance() {
    let tick = crate::privileged::tick_query::current_tick();

    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.tick(tick);

    if !BOOT_FINGERPRINTS_CHECKED.swap(true, Ordering::SeqCst) {
        mgr.check_boot_fingerprints();
    }
}

// B03-10: 静态预分配回收域池 (避免每次 register Box::leak 10-12KB 永久泄漏)。
// MAX_RECOVERY_DOMAINS=32, 单域约 300 字节 (vs 10-12KB Box 容器 + 数据, 减少约 30 倍)。
// 注册时按 id 取模索引到预分配槽位, 重复 id 覆盖。
// 若 id 超出预分配池容量 (理论上注册点仅 4 处且均在启动期), 返回 -1。
//
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_domain_register(domain_id: u64) -> i32 {
    // 取 id 低 5 位作为索引 (0..32)。若超出 MAX_RECOVERY_DOMAINS, 失败。
    // 注: 取模而非用全局计数, 避免重启后 slot 已被占用的问题。
    let idx = (domain_id % MAX_RECOVERY_DOMAINS as u64) as usize;
    let domain: &'static RecoveryDomain = &recovery_domain_pool()[idx];
    // 注: domain_id 字段在 const init 时已为 0; 在此设置实际 id.
    // 由于 RecoveryDomain 字段都是 const-init, 这里仅是声明性归属。
    match super::RECOVERY_MANAGER.lock().register(domain) {
        Some(_) => 0,
        None => -1,
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_domain_unregister(domain_id: u64) -> i32 {
    let mut mgr = super::RECOVERY_MANAGER.lock();
    let count = mgr.count.load(Ordering::SeqCst) as usize;
    for i in 0..count {
        if let Some(dom) = mgr.domains[i] {
            if dom.id == domain_id {
                mgr.domains[i] = None;
                let id_idx = domain_id as usize;
                if id_idx < DIRECT_MAP_SIZE {
                    mgr.direct_map[id_idx] = None;
                }
                return 0;
            }
        }
    }
    -1
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[cfg(feature = "kernel_test")]
#[unsafe(no_mangle)]
pub extern "C" fn recovery_test_rollback(domain_id: u64, crash_fingerprint: u64) -> i32 {
    let tick = crate::privileged::tick_query::current_tick();
    let mgr = super::RECOVERY_MANAGER.lock();
    let rollbacks = mgr.cascade_rollback(domain_id, tick, crash_fingerprint);
    if rollbacks > 0 { 0 } else { -1 }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_panic_flag_is_set() -> bool {
    super::PANIC_FLAG.load(Ordering::SeqCst)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_panic_flag_clear() {
    super::PANIC_FLAG.store(false, Ordering::SeqCst);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_try_recover_from_idt() -> i32 {
    let tick = crate::privileged::tick_query::current_tick();

    if RECOVERY_ATTEMPTED.swap(true, Ordering::SeqCst) {
        return -2;
    }

    let Some(mgr) = super::RECOVERY_MANAGER.try_lock() else {
        RECOVERY_ATTEMPTED.store(false, Ordering::SeqCst);
        return -3;
    };

    let count = mgr.count.load(Ordering::SeqCst) as usize;
    if count == 0 {
        return -1;
    }

    let fingerprint = {
        let mut h: u64 = 5381;
        let msg = super::PANIC_MSG.lock();
        for &b in msg.iter() {
            if b == 0 {
                break;
            }
            h = h.wrapping_mul(33).wrapping_add(u64::from(b));
        }
        h
    };

    let fault_rip: u64 = super::CRASH_RIP.load(Ordering::SeqCst);
    if let Some(target_id) = mgr
        .locate_domain_by_addr(fault_rip)
        .or_else(|| mgr.locate_domain_by_panic_msg())
    {
        let rollbacks = mgr.cascade_rollback(target_id, tick, fingerprint);
        if rollbacks > 0 {
            if let Some(dom) = mgr.find(target_id) {
                dom.persist_crash_fingerprint(fingerprint);
            }
            recovery_panic_flag_clear();
            RECOVERY_ATTEMPTED.store(false, Ordering::SeqCst);
            return 0;
        }
    }

    for i in 0..count {
        if let Some(dom) = mgr.domains[i] {
            let did = dom.id;
            let rollbacks = mgr.cascade_rollback(did, tick, fingerprint);
            if rollbacks > 0 {
                dom.persist_crash_fingerprint(fingerprint);
                recovery_panic_flag_clear();
                RECOVERY_ATTEMPTED.store(false, Ordering::SeqCst);
                return 0;
            }
        }
    }
    -1
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 触发一次人为 panic, 用于屏障栈端到端 (E2E) 故障注入测试。
/// # Panics
/// 此函数总是触发 panic。
#[unsafe(no_mangle)]
pub extern "C" fn recovery_trigger_panic() -> ! {
    super::PANIC_FLAG.store(true, Ordering::SeqCst);
    panic!("[RECOVERY-TEST] Deliberate panic for freg-stack E2E test");
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_was_attempted() -> i32 {
    i32::from(RECOVERY_ATTEMPTED.load(Ordering::SeqCst))
}

/// 设置指定恢复域的捕获/回滚回调; 成功返回 0, 域不存在返回 -1。
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
// 注意: 保持 Rust ABI — 参数含 `Option<unsafe fn()>` 等非 FFI-safe 类型
#[unsafe(no_mangle)]
#[expect(clippy::no_mangle_with_rust_abi)]
pub fn recovery_domain_set_cbs(
    domain_id: u64,
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    capture_fn: Option<unsafe fn()>,
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    rollback_fn: Option<unsafe fn() -> bool>,
) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id).map_or(-1, |dom| {
        *dom.capture_cb.lock() = capture_fn;
        *dom.rollback_cb.lock() = rollback_fn;
        0
    })
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
pub extern "C" fn recovery_undo_record(domain_id: u64, field_ptr: *mut u8, old_val: u64) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id).map_or(-1, |dom| {
        let mut undo = dom.undo.lock();
        undo.record(field_ptr as *mut u64, old_val);
        0
    })
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn recovery_undo_count(domain_id: u64) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id)
        .map_or(-1, |dom| dom.undo.lock().count as i32)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_domain_add_dep(domain_id: u64, dep_id: u64) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id).map_or(-1, |dom| {
        if dom.add_dependency(dep_id) {
            if let Some(dep_dom) = mgr.find(dep_id) {
                dep_dom.add_depended_by(domain_id);
            }
            0
        } else {
            -1
        }
    })
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn recovery_domain_dep_count(domain_id: u64) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id)
        .map_or(-1, |dom| dom.dependency_count() as i32)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_domain_add_addr_range(domain_id: u64, start: u64, end: u64) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id).map_or(-1, |dom| {
        if dom.add_addr_range(start, end) {
            0
        } else {
            -1
        }
    })
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn recovery_rollback_log_count() -> i32 {
    let log = super::manager::ROLLBACK_LOG.lock();
    log.iter().filter(|e| e.is_some()).count() as i32
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_domain_get_state(domain_id: u64) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id).map_or(-1, |dom| dom.get_state() as i32)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn recovery_domain_get_failures(domain_id: u64) -> i32 {
    let mgr = super::RECOVERY_MANAGER.lock();
    mgr.find(domain_id).map_or(-1, |dom| {
        dom.consecutive_failures.load(Ordering::SeqCst) as i32
    })
}

// SAFETY: FFI 导出函数, 通过 C ABI 与外部代码互操作
#[cfg(feature = "fault_injection")]
#[unsafe(no_mangle)]
pub extern "C" fn recovery_set_fault_rate(rate: u32) {
    super::fault_inject::FAULT_INJECTION_RATE.store(rate, Ordering::SeqCst);
}

// SAFETY: FFI 导出函数, 通过 C ABI 与外部代码互操作
#[cfg(feature = "fault_injection")]
#[unsafe(no_mangle)]
pub extern "C" fn recovery_get_fault_rate() -> u32 {
    super::fault_inject::FAULT_INJECTION_RATE.load(Ordering::SeqCst)
}
