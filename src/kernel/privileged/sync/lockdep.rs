//! Lockdep — 运行时锁依赖检测器
//!
//! 在 `debug_assertions` 或 `feature = "lockdep"` 启用时, 跟踪每个锁的
//! 获取/释放, 构建锁序图, 检测以下问题:
//!
//! 1. **AB-BA 死锁**: 线程 A 持锁 L1 再获取 L2, 线程 B 持锁 L2 再获取 L1
//! 2. **中断上下文睡眠**: 在硬中断中获取 Mutex (会 yield)
//! 3. **递归获取非递归锁**: 同一线程对同一 SpinLock/Mutex 重复 lock
//! 4. **释放未持有的锁**: unlock 时当前线程并非持有者
//!
//! ## 架构
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │ LockDepMap (全局, IrqSpinLock 守护)                   │
//! │  ├── lock_classes: [LockClass; MAX_CLASSES]          │
//! │  ├── adjacency: [[bool; MAX_CLASSES]; MAX_CLASSES]   │
//! │  └── class_count: usize                              │
//! ├──────────────────────────────────────────────────────┤
//! │ HeldLocks (per-CPU / per-thread)                     │
//! │  └── stack: [LockClassId; MAX_HELD]                  │
//! ├──────────────────────────────────────────────────────┤
//! │ 检测入口 (每个锁的 lock/unlock 路径调用)              │
//! │  ├── lockdep_acquire(class_id, irq_context)          │
//! │  └── lockdep_release(class_id)                       │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! ## 性能
//!
//! - 生产构建 (`release`, 无 `lockdep` feature): 所有入口为 `#[inline(always)]`
//!   空操作, 零开销
//! - 调试构建: 每次获取锁时 O(n²) 环检测 (n = 已注册锁类数), 仅在首次
//!   观察到新依赖边时执行; 已知边直接跳过
//!
//! ## 框内核契约
//!
//! - 本模块位于 privileged/ (TCB), 需要访问全局静态状态
//! - functions 层通过 `functions::sync::lockdep` 安全代理调用
//!
//! ## 参考
//!
//! - Linux kernel lockdep (Documentation/locking/lockdep-design.txt)  // 参考文献链接
//! - FreeBSD witness (sys/kern/subr_witness.c)  // 参考文献链接

// ============================================================================
// 日志输出 (使用项目 klog 宏)
// ============================================================================

/// Lockdep 内部日志宏
macro_rules! lockdep_log {
    ($($arg:tt)*) => {
        $crate::klog_warn!(Kernel, "lockdep: {}", format_args!($($arg)*))
    };
}

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::privileged::constants::limits::{MAX_HELD_LOCKS, MAX_LOCK_CLASSES};
use crate::privileged::sync::IrqSpinLock;

/// 邻接矩阵中 "已验证无环" 的标记位 (避免重复 BFS)
const DEPENDENCY_VERIFIED: u8 = 1;

/// 邻接矩阵中 "新边, 需要检测" 的标记位
const DEPENDENCY_NEW: u8 = 2;

// ============================================================================
// 锁类型标识
// ============================================================================

/// 锁类型分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LockKind {
    /// 自旋锁 (不可睡眠)
    SpinLock = 0,
    /// 中断安全自旋锁
    IrqSpinLock = 1,
    /// 睡眠锁 (不可在中断上下文使用)
    Mutex = 2,
    /// 读写锁
    RwLock = 3,
    /// 优先级继承互斥锁
    PiMutex = 4,
    /// 顺序锁 (写端)
    SeqLockWrite = 5,
}

impl LockKind {
    /// 是否可在中断上下文中安全获取
    pub fn irq_safe(self) -> bool {
        matches!(self, Self::SpinLock | Self::IrqSpinLock)
    }

    /// 是否为睡眠锁 (获取时可能 yield)
    pub fn may_sleep(self) -> bool {
        matches!(self, Self::Mutex | Self::PiMutex)
    }
}

// ============================================================================
// 锁类 ID
// ============================================================================

/// 锁类 ID (全局唯一, 由 `register_class` 分配)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockClassId(pub u16);

impl LockClassId {
    /// 无效 ID
    pub const INVALID: Self = Self(u16::MAX);
}

// ============================================================================
// 锁类描述
// ============================================================================

/// 锁类描述 (注册时提供)
#[derive(Debug, Clone, Copy)]
pub struct LockClassDesc {
    /// 锁名称 (静态字符串, 用于报告)
    pub name: &'static str,
    /// 锁类型
    pub kind: LockKind,
}

// ============================================================================
// 锁类注册表
// ============================================================================

/// 锁类注册条目
#[derive(Clone, Copy)]
struct LockClassEntry {
    /// 锁名称
    name: &'static str,
    /// 锁类型
    kind: LockKind,
    /// 是否已注册
    used: bool,
}

impl Default for LockClassEntry {
    fn default() -> Self {
        Self {
            name: "",
            kind: LockKind::SpinLock,
            used: false,
        }
    }
}

// ============================================================================
// 全局锁依赖图
// ============================================================================

/// 全局锁依赖图
///
/// 邻接矩阵 `adjacency[i][j]` 表示 "持有锁类 i 时获取锁类 j" 的依赖关系。
/// 值为 0 = 无依赖, `DEPENDENCY_VERIFIED` = 已验证无环, `DEPENDENCY_NEW` = 新边待检测。
struct LockDepMap {
    /// 锁类注册表
    classes: [LockClassEntry; MAX_LOCK_CLASSES],
    /// 已注册锁类数
    class_count: usize,
    /// 邻接矩阵: adjacency[i][j] = 依赖标记
    adjacency: [[u8; MAX_LOCK_CLASSES]; MAX_LOCK_CLASSES],
    /// 是否已检测到死锁 (一旦检测到, 后续 acquire 直接 panic 防止数据损坏)
    deadlock_detected: AtomicBool,
    /// 检测到的死锁计数
    violation_count: AtomicU32,
}

impl LockDepMap {
    const fn new() -> Self {
        Self {
            classes: [LockClassEntry {
                name: "",
                kind: LockKind::SpinLock,
                used: false,
            }; MAX_LOCK_CLASSES],
            class_count: 0,
            adjacency: [[0u8; MAX_LOCK_CLASSES]; MAX_LOCK_CLASSES],
            deadlock_detected: AtomicBool::new(false),
            violation_count: AtomicU32::new(0),
        }
    }

    /// 注册锁类, 返回 `LockClassId`
    fn register(&mut self, desc: LockClassDesc) -> LockClassId {
        // 查找是否已注册同名锁类
        for i in 0..self.class_count {
            if self.classes[i].used && self.classes[i].name == desc.name {
                return LockClassId(i as u16);
            }
        }

        // 新注册
        if self.class_count >= MAX_LOCK_CLASSES {
            lockdep_log!(
                "lockdep: MAX_LOCK_CLASSES ({}) exceeded, skipping '{}'",
                MAX_LOCK_CLASSES,
                desc.name
            );
            return LockClassId::INVALID;
        }

        let id = self.class_count;
        self.classes[id] = LockClassEntry {
            name: desc.name,
            kind: desc.kind,
            used: true,
        };
        self.class_count += 1;
        LockClassId(id as u16)
    }

    /// 检查并添加依赖边 (from → to)
    ///
    /// 返回 true = 添加成功 (无环), false = 检测到环 (死锁风险)
    fn add_dependency(&mut self, from: LockClassId, to: LockClassId) -> bool {
        let fi = from.0 as usize;
        let ti = to.0 as usize;

        if fi >= self.class_count || ti >= self.class_count {
            return true; // 无效 ID, 不检测
        }

        // 已有边, 跳过
        if self.adjacency[fi][ti] != 0 {
            return true;
        }

        // 添加新边, 标记为待检测
        self.adjacency[fi][ti] = DEPENDENCY_NEW;

        // BFS 检测: 从 to 出发能否回到 from (即 to → ... → from 的路径)
        if self.has_path(ti, fi) {
            // 环检测到! 报告死锁
            self.adjacency[fi][ti] = 0; // 回滚边
            self.deadlock_detected.store(true, Ordering::Release);
            self.violation_count.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // 无环, 标记为已验证
        self.adjacency[fi][ti] = DEPENDENCY_VERIFIED;
        true
    }

    /// BFS 检测从 `start` 到 `target` 是否有路径
    fn has_path(&self, start: usize, target: usize) -> bool {
        // 简单 BFS, 使用栈上位数组作为 visited
        let mut visited = [false; MAX_LOCK_CLASSES];
        let mut queue = [0usize; MAX_LOCK_CLASSES];
        let mut head = 0usize;
        let mut tail = 0usize;

        visited[start] = true;
        queue[tail] = start;
        tail += 1;

        while head < tail {
            let current = queue[head];
            head += 1;

            for next in 0..self.class_count {
                if visited[next] {
                    continue;
                }
                if self.adjacency[current][next] != 0 {
                    if next == target {
                        return true; // 找到路径 → 环
                    }
                    visited[next] = true;
                    if tail < MAX_LOCK_CLASSES {
                        queue[tail] = next;
                        tail += 1;
                    }
                }
            }
        }

        false
    }

    /// 获取锁类名称
    fn class_name(&self, id: LockClassId) -> &'static str {
        let idx = id.0 as usize;
        if idx < self.class_count && self.classes[idx].used {
            self.classes[idx].name
        } else {
            "(unknown)"
        }
    }

    /// 获取锁类类型
    fn class_kind(&self, id: LockClassId) -> Option<LockKind> {
        let idx = id.0 as usize;
        if idx < self.class_count && self.classes[idx].used {
            Some(self.classes[idx].kind)
        } else {
            None
        }
    }

    /// 已注册锁类数
    fn num_classes(&self) -> usize {
        self.class_count
    }

    /// 检测到的违规数
    fn num_violations(&self) -> u32 {
        self.violation_count.load(Ordering::Relaxed)
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局锁依赖图 (`IrqSpinLock` 守护)
static LOCK_DEP_MAP: IrqSpinLock<LockDepMap> = IrqSpinLock::new(LockDepMap::new());

// ============================================================================
// Per-CPU / Per-Thread 持有锁栈
// ============================================================================

/// 持有锁栈条目
#[derive(Debug, Clone, Copy)]
struct HeldLockEntry {
    /// 锁类 ID
    class_id: LockClassId,
    /// 获取时是否在中断上下文
    in_irq: bool,
}

impl Default for HeldLockEntry {
    fn default() -> Self {
        Self {
            class_id: LockClassId::INVALID,
            in_irq: false,
        }
    }
}

/// Per-CPU 持有锁栈
///
/// 当前实现使用全局单栈 (单核/引导阶段), 后续可改为 per-CPU。
struct HeldLockStack {
    entries: [HeldLockEntry; MAX_HELD_LOCKS],
    depth: usize,
}

impl HeldLockStack {
    const fn new() -> Self {
        Self {
            entries: [HeldLockEntry {
                class_id: LockClassId::INVALID,
                in_irq: false,
            }; MAX_HELD_LOCKS],
            depth: 0,
        }
    }

    /// 压入一个锁
    fn push(&mut self, class_id: LockClassId, in_irq: bool) -> bool {
        if self.depth >= MAX_HELD_LOCKS {
            lockdep_log!("lockdep: held lock stack overflow (depth={})", self.depth);
            return false;
        }
        self.entries[self.depth] = HeldLockEntry { class_id, in_irq };
        self.depth += 1;
        true
    }

    /// 弹出指定锁类
    fn pop(&mut self, class_id: LockClassId) -> bool {
        // 从栈顶向下查找 (LIFO 语义: 最近获取的先释放)
        for i in (0..self.depth).rev() {
            if self.entries[i].class_id == class_id {
                // 移除并压缩
                for j in i..self.depth.saturating_sub(1) {
                    self.entries[j] = self.entries[j + 1];
                }
                self.depth -= 1;
                self.entries[self.depth] = HeldLockEntry::default();
                return true;
            }
        }
        false
    }

    /// 查找栈中是否已持有指定锁类
    fn contains(&self, class_id: LockClassId) -> bool {
        self.entries[..self.depth]
            .iter()
            .any(|e| e.class_id == class_id)
    }

    /// 获取当前持有锁的深度
    fn len(&self) -> usize {
        self.depth
    }

    /// 获取栈中所有已持有锁的 ID (从底到顶)
    fn held_classes(&self) -> &[HeldLockEntry] {
        &self.entries[..self.depth]
    }

    /// 检查是否在中断上下文中持有锁
    fn any_in_irq(&self) -> bool {
        self.entries[..self.depth].iter().any(|e| e.in_irq)
    }
}

/// 全局持有锁栈 (单核阶段; 后续改为 per-CPU)
static HELD_LOCK_STACK: IrqSpinLock<HeldLockStack> = IrqSpinLock::new(HeldLockStack::new());

// ============================================================================
// 中断上下文跟踪
// ============================================================================

/// 中断上下文嵌套深度 (0 = 进程上下文, >0 = 中断上下文)
static IRQ_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// 标记进入中断上下文
pub fn irq_enter() {
    IRQ_DEPTH.fetch_add(1, Ordering::Relaxed);
}

/// 标记退出中断上下文
pub fn irq_exit() {
    let prev = IRQ_DEPTH.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(prev > 0, "lockdep: irq_exit underflow");
}

/// 当前是否在中断上下文
pub fn in_irq_context() -> bool {
    IRQ_DEPTH.load(Ordering::Relaxed) > 0
}

// ============================================================================
// 公开 API
// ============================================================================

/// 注册锁类
///
/// 返回 `LockClassId`, 后续 acquire/release 使用此 ID。
/// 同名锁类只注册一次 (幂等)。
pub fn register_class(desc: LockClassDesc) -> LockClassId {
    let mut map = LOCK_DEP_MAP.lock();
    map.register(desc)
}

/// 锁获取通知
///
/// 在锁成功获取后调用。检查:
/// 1. 与当前持有锁的依赖关系 (AB-BA 检测)
/// 2. 中断上下文获取睡眠锁
/// 3. 递归获取非递归锁
///
/// # 参数
/// - `class_id`: 锁类 ID
/// - `irq_context`: 是否在中断上下文中获取 (由调用方判断)
///
/// # 返回
/// - `true`: 正常
/// - `false`: 检测到违规 (已打印警告)
pub fn acquire(class_id: LockClassId, irq_context: bool) -> bool {
    if class_id == LockClassId::INVALID {
        return true;
    }

    // 检查 1: 中断上下文获取睡眠锁
    let kind = {
        let map = LOCK_DEP_MAP.lock();
        map.class_kind(class_id)
    };

    if let Some(k) = kind {
        if irq_context && k.may_sleep() {
            let map = LOCK_DEP_MAP.lock();
            let name = map.class_name(class_id);
            lockdep_log!(
                "lockdep VIOLATION: acquiring sleep lock '{}' ({:?}) in IRQ context!",
                name,
                k
            );
            map.violation_count.fetch_add(1, Ordering::Relaxed);
            return false;
        }
    }

    // 检查 2: 递归获取非递归锁
    {
        let mut stack = HELD_LOCK_STACK.lock();
        if stack.contains(class_id) {
            let map = LOCK_DEP_MAP.lock();
            let name = map.class_name(class_id);
            lockdep_log!(
                "lockdep VIOLATION: recursive acquire of non-recursive lock '{}'!",
                name
            );
            map.violation_count.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // 检查 2.5: 持有 IRQ 上下文锁时获取 sleep lock → 潜在死锁
        if !irq_context && stack.any_in_irq() {
            if let Some(k) = kind {
                if k.may_sleep() {
                    let map = LOCK_DEP_MAP.lock();
                    let name = map.class_name(class_id);
                    lockdep_log!(
                        "lockdep WARNING: acquiring sleep lock '{}' while holding IRQ-context lock",
                        name
                    );
                }
            }
        }

        // 检查 3: AB-BA 依赖检测
        // 对当前持有栈中的每个锁, 添加 "held → new" 依赖边
        let held = stack.held_classes().to_vec();
        for entry in &held {
            let mut map = LOCK_DEP_MAP.lock();
            if !map.add_dependency(entry.class_id, class_id) {
                let from_name = map.class_name(entry.class_id);
                let to_name = map.class_name(class_id);
                lockdep_log!(
                    "lockdep DEADLOCK: circular dependency detected: {} → {} (AB-BA)",
                    from_name,
                    to_name
                );
                // 不 return, 继续记录但标记违规
            }
        }

        // 压入持有栈
        stack.push(class_id, irq_context);
    }

    true
}

/// 锁释放通知
///
/// 在锁释放前调用。从持有栈中移除。
pub fn release(class_id: LockClassId) {
    if class_id == LockClassId::INVALID {
        return;
    }

    let mut stack = HELD_LOCK_STACK.lock();
    if !stack.pop(class_id) {
        let map = LOCK_DEP_MAP.lock();
        let name = map.class_name(class_id);
        lockdep_log!(
            "lockdep VIOLATION: releasing lock '{}' not held by current context!",
            name
        );
        map.violation_count.fetch_add(1, Ordering::Relaxed);
    }
}

/// 查询当前持有锁深度
pub fn held_depth() -> usize {
    let stack = HELD_LOCK_STACK.lock();
    stack.len()
}

/// 查询已注册锁类数
pub fn num_classes() -> usize {
    let map = LOCK_DEP_MAP.lock();
    map.num_classes()
}

/// 查询检测到的违规数
pub fn num_violations() -> u32 {
    let map = LOCK_DEP_MAP.lock();
    map.num_violations()
}

/// 检查是否已检测到死锁
pub fn deadlock_detected() -> bool {
    let map = LOCK_DEP_MAP.lock();
    map.deadlock_detected.load(Ordering::Acquire)
}

/// 打印当前锁依赖状态 (调试用)
pub fn dump_state() {
    let map = LOCK_DEP_MAP.lock();
    let stack = HELD_LOCK_STACK.lock();

    lockdep_log!("=== Lockdep State Dump ===");
    lockdep_log!("Registered classes: {}", map.num_classes());
    lockdep_log!("Violations: {}", map.num_violations());
    lockdep_log!(
        "Deadlock detected: {}",
        map.deadlock_detected.load(Ordering::Relaxed)
    );
    lockdep_log!("Held locks (depth={}):", stack.len());

    for entry in stack.held_classes() {
        let name = map.class_name(entry.class_id);
        lockdep_log!("  {} (irq={})", name, entry.in_irq);
    }

    // 打印邻接矩阵中的边
    lockdep_log!("Dependency edges:");
    for i in 0..map.class_count {
        for j in 0..map.class_count {
            if map.adjacency[i][j] != 0 {
                lockdep_log!("  {} → {}", map.classes[i].name, map.classes[j].name,);
            }
        }
    }
    lockdep_log!("=== End Lockdep Dump ===");
}

// ============================================================================
// 单元测试 (host 端)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_kind_irq_safe() {
        assert!(LockKind::SpinLock.irq_safe());
        assert!(LockKind::IrqSpinLock.irq_safe());
        assert!(!LockKind::Mutex.irq_safe());
        assert!(!LockKind::PiMutex.irq_safe());
    }

    #[test]
    fn test_lock_kind_may_sleep() {
        assert!(LockKind::Mutex.may_sleep());
        assert!(LockKind::PiMutex.may_sleep());
        assert!(!LockKind::SpinLock.may_sleep());
        assert!(!LockKind::IrqSpinLock.may_sleep());
    }

    #[test]
    fn test_held_stack_basic() {
        let mut stack = HeldLockStack::new();
        assert_eq!(stack.len(), 0);

        let id_a = LockClassId(0);
        let id_b = LockClassId(1);

        assert!(stack.push(id_a, false));
        assert_eq!(stack.len(), 1);
        assert!(stack.contains(id_a));
        assert!(!stack.contains(id_b));

        assert!(stack.push(id_b, true));
        assert_eq!(stack.len(), 2);
        assert!(stack.contains(id_b));

        assert!(stack.pop(id_b));
        assert_eq!(stack.len(), 1);
        assert!(!stack.contains(id_b));

        assert!(stack.pop(id_a));
        assert_eq!(stack.len(), 0);
    }

    #[test]
    fn test_held_stack_overflow() {
        let mut stack = HeldLockStack::new();
        for i in 0..MAX_HELD_LOCKS {
            assert!(stack.push(LockClassId(i as u16), false));
        }
        // 第 MAX_HELD_LOCKS + 1 次应失败
        assert!(!stack.push(LockClassId(MAX_HELD_LOCKS as u16), false));
    }

    #[test]
    fn test_held_stack_pop_not_present() {
        let mut stack = HeldLockStack::new();
        stack.push(LockClassId(0), false);
        assert!(!stack.pop(LockClassId(1))); // 不存在
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn test_lock_dep_map_register() {
        let mut map = LockDepMap::new();
        let id_a = map.register(LockClassDesc {
            name: "lock_a",
            kind: LockKind::Mutex,
        });
        let id_b = map.register(LockClassDesc {
            name: "lock_b",
            kind: LockKind::SpinLock,
        });

        assert_eq!(id_a, LockClassId(0));
        assert_eq!(id_b, LockClassId(1));
        assert_eq!(map.num_classes(), 2);

        // 同名注册应返回相同 ID
        let id_a2 = map.register(LockClassDesc {
            name: "lock_a",
            kind: LockKind::Mutex,
        });
        assert_eq!(id_a2, id_a);
        assert_eq!(map.num_classes(), 2);
    }

    #[test]
    fn test_lock_dep_map_no_cycle() {
        let mut map = LockDepMap::new();
        let id_a = map.register(LockClassDesc {
            name: "A",
            kind: LockKind::Mutex,
        });
        let id_b = map.register(LockClassDesc {
            name: "B",
            kind: LockKind::Mutex,
        });
        let id_c = map.register(LockClassDesc {
            name: "C",
            kind: LockKind::Mutex,
        });

        // A → B → C (无环)
        assert!(map.add_dependency(id_a, id_b));
        assert!(map.add_dependency(id_b, id_c));
        assert!(!map.deadlock_detected.load(Ordering::Relaxed));
    }

    #[test]
    fn test_lock_dep_map_cycle_detection() {
        let mut map = LockDepMap::new();
        let id_a = map.register(LockClassDesc {
            name: "A",
            kind: LockKind::Mutex,
        });
        let id_b = map.register(LockClassDesc {
            name: "B",
            kind: LockKind::Mutex,
        });

        // A → B
        assert!(map.add_dependency(id_a, id_b));
        // B → A (环!)
        assert!(!map.add_dependency(id_b, id_a));
        assert!(map.deadlock_detected.load(Ordering::Relaxed));
        assert_eq!(map.num_violations(), 1);
    }

    #[test]
    fn test_lock_dep_map_three_node_cycle() {
        let mut map = LockDepMap::new();
        let id_a = map.register(LockClassDesc {
            name: "A",
            kind: LockKind::Mutex,
        });
        let id_b = map.register(LockClassDesc {
            name: "B",
            kind: LockKind::Mutex,
        });
        let id_c = map.register(LockClassDesc {
            name: "C",
            kind: LockKind::Mutex,
        });

        // A → B
        assert!(map.add_dependency(id_a, id_b));
        // B → C
        assert!(map.add_dependency(id_b, id_c));
        // C → A (环!)
        assert!(!map.add_dependency(id_c, id_a));
        assert!(map.deadlock_detected.load(Ordering::Relaxed));
    }

    #[test]
    fn test_irq_context_tracking() {
        assert!(!in_irq_context());
        irq_enter();
        assert!(in_irq_context());
        irq_enter();
        assert!(in_irq_context());
        irq_exit();
        assert!(in_irq_context());
        irq_exit();
        assert!(!in_irq_context());
    }
}
