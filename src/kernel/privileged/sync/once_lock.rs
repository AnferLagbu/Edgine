//! OnceLock — TCB 一次性值容器 (safe 公共 API)
//!
//! 等价于 `std::sync::OnceLock<T>` 的内核版。
//!
//! ## 设计
//!
//! - 内部: `Once` (串行化) + `UnsafeCell<MaybeUninit<T>>` (存储)
//! - 公共 API: `set` / `get` / `get_or_init` 全部 safe
//! - `unsafe` 块隐藏在 `Once` 互斥保证之后
//!
//! ## SAFETY 契约
//!
//! 所有 unsafe 块都遵循统一模式:
//! - `write` 之前必须由 `Once` 串行化, 确认是唯一的写者
//! - `read` 必须确认 `Once::is_completed()` 为真 (cell 已初始化)
//! - `drop` 必须确认 `Once::is_completed()` 为真 (避免 drop uninit)
//!
//! 内部用 `AtomicU8` 状态机 (`UNINITIALIZED / IN_PROGRESS / DONE`)
//! 替代 `Once`, 避免循环依赖 (Once 在 functions::sync::once)。
//!
//! ## 与 `functions::sync::once` 的关系
//!
//! ```text
//! privileged::sync::once_lock::OnceLock    ← 本模块 (safe 公共 API, unsafe 内部)
//!   ↑
//! functions::sync::once::OnceCell          ← 纯 thin wrapper, 转调
//! functions::sync::once::Once              ← 纯 safe, 简单闭包一次性
//! ```

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};

// ============================================================================
// 内嵌 Once — 仅在本模块使用, 避免循环依赖
// ============================================================================

const UNINITIALIZED: u8 = 0;
const IN_PROGRESS: u8 = 1;
const DONE: u8 = 2;

struct InnerOnce {
    state: AtomicU8,
}

impl InnerOnce {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(UNINITIALIZED),
        }
    }

    /// 仅当尚未完成时执行闭包 (执行后可重入调用, 但闭包本身只跑一次)。
    ///
    /// ## panic 安全性
    ///
    /// 若 `f()` panic, 状态机从 `IN_PROGRESS` 重置为 `UNINITIALIZED`,
    /// 允许后续调用者重试初始化. 通过 `PanicGuard` 在 drop 时自动重置.
    fn call_once<F: FnOnce()>(&self, f: F) {
        // 快速路径
        if self.state.load(Ordering::Acquire) == DONE {
            return;
        }
        // 简单自旋等待 (无 Mutex 依赖, 纯原子操作)
        // 状态机: UNINITIALIZED → IN_PROGRESS → DONE
        // 多个线程同时进入时, CAS 保证只有一个把 UNINITIALIZED 翻成 IN_PROGRESS。
        let prev = self
            .state
            .compare_exchange(
                UNINITIALIZED,
                IN_PROGRESS,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .unwrap_or_else(|p| p);
        match prev {
            DONE => {}
            UNINITIALIZED => {
                // 我们赢得了执行权.
                // 守卫: 若 f() panic, drop 时将状态重置为 UNINITIALIZED,
                // 避免 OnceLock 永久毒化.
                struct PanicGuard<'a> {
                    state: &'a AtomicU8,
                }
                impl Drop for PanicGuard<'_> {
                    fn drop(&mut self) {
                        self.state.store(UNINITIALIZED, Ordering::Release);
                    }
                }
                let guard = PanicGuard { state: &self.state };
                f();
                // f() 成功 → 解除守卫, 设置 DONE
                core::mem::forget(guard);
                self.state.store(DONE, Ordering::Release);
            }
            IN_PROGRESS => {
                // 别的线程正在执行, 自旋等待完成.
                // 注: 若执行线程 panic, PanicGuard 将状态重置为 UNINITIALIZED,
                // 本线程可能看到 UNINITIALIZED 后再次竞争执行权.
                while self.state.load(Ordering::Acquire) == IN_PROGRESS {
                    core::hint::spin_loop();
                }
                // 退出循环后状态可能是 DONE (成功) 或 UNINITIALIZED (panic 后重置).
                // 若为 UNINITIALIZED, 递归调用自身重试一次.
                if self.state.load(Ordering::Acquire) == UNINITIALIZED {
                    self.call_once(f);
                }
            }
            _ => unreachable!("Once: unknown state"),
        }
    }

    #[inline]
    fn is_completed(&self) -> bool {
        self.state.load(Ordering::Acquire) == DONE
    }

    /// 返回原始状态值 (仅用于调试诊断).
    #[inline]
    fn debug_state(&self) -> u8 {
        self.state.load(Ordering::Relaxed)
    }
}

// ============================================================================
// OnceLock<T> — safe 公共 API
// ============================================================================

/// 一次性值容器 (safe)。
///
/// ## 用法
///
/// ```ignore
/// let lock: OnceLock<u32> = OnceLock::new();
/// assert!(lock.get().is_none());
/// let v = lock.get_or_init(|slot| slot.write(42));
/// assert_eq!(*v, 42);
/// ```
pub struct OnceLock<T> {
    once: InnerOnce,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: `InnerOnce` 串行化所有访问; `T: Send` 即可跨线程移动 (与 std::OnceLock 一致)。
unsafe impl<T: Send> Send for OnceLock<T> {}
// SAFETY: 共享引用跨线程安全 (访问经 Once 互斥)。
unsafe impl<T: Send + Sync> Sync for OnceLock<T> {}

impl<T> OnceLock<T> {
    /// 创建未初始化的 `OnceLock`。
    pub const fn new() -> Self {
        Self {
            once: InnerOnce::new(),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// 若未初始化, 用 `f` 在 cell 上写入并返回 `&T` 引用。
    ///
    /// 2026-06-29 修复: 闭包签名改为 `FnOnce(&mut MaybeUninit<T>)`, 强制
    /// 调用方在 `&mut MaybeUninit<T>` 上**就地构造** T, 避免返回值通过
    /// `x86_64` `SysV` ABI 大对象返回约定 (隐藏指针 + 调用方栈帧分配) 产生的
    /// **栈溢出**. 之前签名 `FnOnce() -> T` 在 T 较大时 (如
    /// `IdentityTable` ≈ 78 KB) 即使闭包体内只 `unsafe { cell.write(f()) }`,
    /// 编译器仍会为 `f()` 返回值在调用方栈帧 (此处为 64 KB
    /// `KERNEL_STACK_SIZE`) 分配 78 KB 槽位, 直接踩栈破坏后续函数 (如
    /// `pmm_alloc_pages` 的 `spin_lock` 标志) 导致 QEMU 120s hang.
    /// 新签名下, `f` 必须显式 `slot.write(value)`, 整个生命周期都在 BSS
    /// `value` cell 上, 零额外栈开销. SAFETY 不变: `call_once` 互斥保证
    /// cell 只被写一次.
    pub fn get_or_init(&self, f: impl FnOnce(&mut MaybeUninit<T>)) -> &T {
        self.once.call_once(|| {
            // SAFETY: call_once 互斥保证本闭包是唯一的 cell 写者. cell 之前是
            // uninit, 闭包必须确保 `f` 调用后 cell 是 init 状态. 后续
            // `assume_init_ref` 看到完整有效值.
            f(unsafe { &mut *self.value.get() });
        });
        // SAFETY: 此刻 `self.once.is_completed()` 必为真 (call_once 已返回),
        // 因此 cell 已被初始化.
        unsafe { (*self.value.get()).assume_init_ref() }
    }

    /// 直接设置值 (若未初始化)。
    ///
    /// 返回 `Ok(())` 表示首次设置成功, `Err(value)` 表示已初始化, 值被退回。
    ///
    /// 2026-06-29 同步修复: 用 stack-local 持有 value (仅在 set 路径上).
    /// 适用 T 通常较小 (指针/Option/Box), 栈分配可接受. 若 T 较大且需要
    /// 避免栈分配, 改用 `get_or_init(|slot| slot.write(value))` 配合 cell
    /// 已有 UNINIT 状态的预检.
    ///
    /// # Errors
    /// 当值已被初始化时, 返回 `Err(value)`, 将本次传入的 `value` 原样退回给调用方.
    ///
    /// # Panics
    /// 唯一潜在 panic 点是 `slot.take().expect("OnceLock: set slot empty")`;
    /// 由于 `call_once` 首次执行闭包时 `slot` 必然为 `Some`, 实际不会触发.
    pub fn set(&self, value: T) -> Result<(), T> {
        let mut slot: Option<T> = Some(value);
        self.once.call_once(|| {
            let v = slot.take().expect("OnceLock: set slot empty");
            // SAFETY: call_once 互斥保证此写独占 cell.
            unsafe { (*self.value.get()).write(v) };
        });
        slot.map_or_else(|| Ok(()), |v| Err(v))
    }

    /// 获取值 (若已初始化)。
    #[inline]
    pub fn get(&self) -> Option<&T> {
        if self.once.is_completed() {
            // SAFETY: is_completed 保证 cell 已初始化。
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }

    /// 获取值, 若未初始化则 panic 并给出诊断信息.
    ///
    /// 与 `get()` 不同, 本方法区分三种状态:
    /// - `DONE` → 返回值
    /// - `IN_PROGRESS` → panic 提示重入 (初始化过程中被递归调用)
    /// - `UNINITIALIZED` → panic 提示未初始化
    ///
    /// `name` 参数用于 panic 消息标识子系统 (如 "VMM", "PMM").
    ///
    /// # Panics
    /// 当值尚未初始化 (UNINITIALIZED) 时 panic, 提示对应的 `name::init()` 未调用或失败;
    /// 当初始化正在进行 (`IN_PROGRESS`) 时 panic, 提示初始化过程中发生了重入调用
    /// (可能是页表损坏、初始化完成前开中断或栈溢出).
    #[inline]
    pub fn get_or_panic(&self, name: &str) -> &T {
        let state = self.once.debug_state();
        match state {
            DONE => {
                // SAFETY: state == DONE 保证 cell 已初始化
                unsafe { (*self.value.get()).assume_init_ref() }
            }
            IN_PROGRESS => {
                panic!(
                    "[{}] accessed during initialization (reentrant call). \
                     A page fault or interrupt handler called get_{}() \
                     while {}::init() is still running. \
                     Check: 1) page table corruption during init, \
                     2) interrupt enabled before init complete, \
                     3) stack overflow corrupting init state.",
                    name,
                    name.to_lowercase(),
                    name
                );
            }
            _ => {
                panic!(
                    "[{name}] accessed before initialization. \
                     {name}::init() was never called or failed."
                );
            }
        }
    }

    /// 返回内部状态机的原始值 (仅用于调试诊断).
    /// 返回值: 0=未初始化, 1=初始化中, 2=已完成.
    #[inline]
    pub fn debug_state(&self) -> u8 {
        self.once.debug_state()
    }
}

impl<T> Default for OnceLock<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for OnceLock<T> {
    fn drop(&mut self) {
        if self.once.is_completed() {
            // SAFETY: is_completed 保证 cell 已初始化, drop 是唯一一次访问。
            unsafe { (*self.value.get()).assume_init_drop() };
        }
    }
}
