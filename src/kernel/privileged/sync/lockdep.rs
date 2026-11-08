//! Lockdep — 运行时锁依赖检测器
//!
//! 在 `debug_assertions` 或 `feature = "lockdep"` 启用时, 跟踪每个锁的
//! 获取/释放, 构建锁序图, 检测以下问题 (标号与 `acquire` / `release` 内注释逐字对应):
//!
//! - **检查 1 · 中断上下文睡眠**: 在硬中断中获取 `Mutex` / `PiMutex` (会 yield).
//!   门控关闭的构建里只剩这一条发射路径
//! - **检查 2 · 递归获取非递归锁**: 同一 CPU 对**同一物理实例**重复 lock; 同类
//!   不同实例 (如 per-CPU 数组锁成对持有) 是合法的
//! - **检查 2.5 · 跨上下文睡眠**: 进程上下文持有「曾在中断上下文获取过的锁」时
//!   再取睡眠锁 —— 只发 WARNING, 不计入违规数
//! - **检查 3 · AB-BA 死锁**: 持有 L1 时获取 L2, 而依赖图上已存在 L2 → L1 路径
//! - **release 侧 · 释放未持有的锁**: 当前持有栈中没有该实例
//!
//! 检查 1/2/3 与 release 侧那条计入 `num_violations()`; 检查 2.5 只发报告.
//!
//! ## 覆盖面有损时的处置 (A-1 前置 ①/③ 已裁: 显式失败, 不 panic)
//!
//! 两处容量截断**不静默**: 类表满 (`MAX_LOCK_CLASSES`) 丢弃注册、持有栈满
//! (`MAX_HELD_LOCKS`) 丢失跟踪, 都累加 [`LockdepStats`] 的粘滞失效读数 (单调不减),
//! 并发一条报告. 锁的语义与返回值一律不受影响 (不 panic、不回退), 失效只体现在读数上
//! —— 启动末段读数行打印它, 门禁按「硬零」判红 (静态侧另有容量对账判据, 见
//! `scripts/audit_deadlock_matrix.py`).
//!
//! ## 架构
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ LockDepMap (全局, IrqSpinLock 守护)                        │
//! │  ├── classes: [LockClassEntry; MAX_LOCK_CLASSES]           │
//! │  ├── adjacency: [[u8; MAX_LOCK_CLASSES]; MAX_LOCK_CLASSES] │
//! │  │     值域 0=无边 / 1=已验证 / 2=新边, BFS 只跑新边       │
//! │  └── class_count, stats: LockdepStats                      │
//! ├────────────────────────────────────────────────────────────┤
//! │ HeldLockStack (per-CPU 一格, 关中断独占本格)               │
//! │  ├── entries: [HeldLockEntry; MAX_HELD_LOCKS]              │
//! │  │     每条含 class_id (名) + instance (物理地址) + in_irq │
//! │  └── depth                                                 │
//! ├────────────────────────────────────────────────────────────┤
//! │ 检测入口 (五把同步原语的 lock/unlock 路径调用)             │
//! │  ├── register_class(desc) -> LockClassId                   │
//! │  ├── acquire(class_id, instance, irq_context) -> bool      │
//! │  └── release(class_id, instance)                           │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## 性能
//!
//! - 门控关闭 (`--no-default-features` 的 `release` 构建; A-1 起出货构建默认开启):
//!   五把同步原语的调用点整体不编译, 运行时零检查开销. 但**体积不省**: 模块本体仍
//!   参与编译 (内部只有 per-CPU 持栈与依赖图段被同一掩码 cfg 掉), 且链接为
//!   `--whole-archive` 且无 `--gc-sections`, 归档目标文件全量入镜像 —— `nm` 在门控
//!   关闭的 release 归档里看得到 `lockdep::acquire` 的 `T` 符号. 故关闭门控只省下
//!   调用, 不省下镜像占用
//! - 门控开启: 每次有名锁获取只进入 **一次** `LOCK_DEP_MAP` 全局临界区,
//!   记账路径 **零堆分配** (持有栈快照用定长栈数组), 依赖图 BFS 只在
//!   **首次观察到新边** 时执行 (已知边直接跳过)
//! - 违规打印一律延迟到临界区之后 (`PendingReports`), 不在持有全局 map
//!   guard 时写串口
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

#[cfg(any(debug_assertions, feature = "lockdep"))]
use crate::privileged::constants::limits::MAX_HELD_LOCKS;
use crate::privileged::constants::limits::MAX_LOCK_CLASSES;
use crate::privileged::sync::IrqSpinLock;

/// 邻接矩阵中 "已验证无环" 的标记位 (避免重复 BFS)
#[cfg(any(debug_assertions, feature = "lockdep"))]
const DEPENDENCY_VERIFIED: u8 = 1;

/// 邻接矩阵中 "新边, 需要检测" 的标记位
#[cfg(any(debug_assertions, feature = "lockdep"))]
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
    #[must_use]
    pub fn irq_safe(self) -> bool {
        matches!(self, Self::SpinLock | Self::IrqSpinLock)
    }

    /// 是否为睡眠锁 (获取时可能 yield)
    #[must_use]
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

/// lockdep 记账计数 (M6: 让每项低开销化改动的增益可归因)
///
/// 只在 `LOCK_DEP_MAP` guard 内更新, 故为非原子字段; 读取走 [`stats`]。
///
/// 末两项是**观测面失效读数** (A-1 前置 ①/③): 容量截断不再静默 —— 满表丢弃注册与
/// 持栈溢出各累加一个粘滞计数 (单调不减, 不 panic、不改写锁语义), 由启动末段读数行
/// 与门禁硬零断言消费; 任一非零即「本轮 lockdep 覆盖面有损」。
#[derive(Debug, Clone, Copy)]
pub struct LockdepStats {
    /// 进入记账的有名锁获取次数
    pub acquire_ops: u64,
    /// acquire 记账进入 `LOCK_DEP_MAP` 全局临界区的次数 (低开销化判据: 应恒等于
    /// `acquire_ops`; 每 op 多次即临界区未合并。release 只在违规时取锁, 不计入)
    pub map_sections: u64,
    /// AB-BA 检查遍历的持有条目数 (随持有深度线性增长)
    pub edge_attempts: u64,
    /// 首次出现的新边数 —— 即真正触发 BFS 的次数
    pub new_edges: u64,
    /// 类表满 (`MAX_LOCK_CLASSES`) 时被丢弃的注册次数 —— 每丢一次, 该锁类全程脱离检测
    /// (本构建的 `register_class` 调用点仍可命中, 与门控无关)
    pub class_table_dropped: u64,
    /// 持有栈满 (`MAX_HELD_LOCKS`) 的次数 —— 每次溢出, 该锁自本次获取起不再被跟踪
    /// (仅门控激活的构建可能非零: 门控关闭的构建不维护持有栈, 也就没有可丢失的覆盖)
    pub held_stack_overflowed: u64,
}

impl LockdepStats {
    /// 全零快照 (门控关闭构建下 [`stats`] 的返回值)
    pub const ZERO: Self = Self {
        acquire_ops: 0,
        map_sections: 0,
        edge_attempts: 0,
        new_edges: 0,
        class_table_dropped: 0,
        held_stack_overflowed: 0,
    };
}

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
    /// 是否已检测到死锁 (仅置位 + 打印, 不 panic; 处置由调用方与观测面决定)
    deadlock_detected: AtomicBool,
    /// 检测到的死锁计数
    violation_count: AtomicU32,
    /// 记账计数
    stats: LockdepStats,
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
            stats: LockdepStats::ZERO,
        }
    }

    /// 注册锁类, 返回 `LockClassId`
    fn register(&mut self, desc: LockClassDesc) -> LockClassId {
        // 查找是否已注册同名锁类
        for (i, class) in self.classes[..self.class_count].iter().enumerate() {
            if class.used && class.name == desc.name {
                return LockClassId(crate::privileged::glue::low_u16_from_usize(i));
            }
        }

        // 新注册
        if self.class_count >= MAX_LOCK_CLASSES {
            // 显式失败 (不静默): 粘滞计数 + 告警. 返回 INVALID 后 acquire/release
            // 对该 ID 短路 —— 锁本身照常工作, 丢的是检测覆盖, 故必须留下可读的账.
            self.stats.class_table_dropped += 1;
            lockdep_log!(
                "lockdep: MAX_LOCK_CLASSES ({}) exceeded, skipping '{}' (dropped={})",
                MAX_LOCK_CLASSES,
                desc.name,
                self.stats.class_table_dropped
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
        LockClassId(crate::privileged::glue::low_u16_from_usize(id))
    }

    /// 检查并添加依赖边 (from → to)
    ///
    /// 返回 true = 添加成功 (无环), false = 检测到环 (死锁风险)
    #[cfg(any(debug_assertions, feature = "lockdep"))]
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

        // BFS 只对**新边**执行 (已知边在上面短路) —— 稳态成本与 BFS 无关。
        self.stats.new_edges += 1;

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
    #[cfg(any(debug_assertions, feature = "lockdep"))]
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

/// 持有锁栈条目 (仅 lockdep 激活时参与编译)
#[cfg(any(debug_assertions, feature = "lockdep"))]
#[derive(Debug, Clone, Copy)]
struct HeldLockEntry {
    /// 锁类 ID (名字, 用于依赖图 ABBA)
    class_id: LockClassId,
    /// 锁实例地址 (物理锁指针, 用于持有栈/递归判定/release pop)
    instance: usize,
    /// 获取时是否在中断上下文
    in_irq: bool,
}

#[cfg(any(debug_assertions, feature = "lockdep"))]
impl Default for HeldLockEntry {
    fn default() -> Self {
        Self {
            class_id: LockClassId::INVALID,
            instance: 0,
            in_irq: false,
        }
    }
}

/// Per-CPU 持有锁栈 (每 CPU 一格; 仅 lockdep 激活时参与编译)
#[cfg(any(debug_assertions, feature = "lockdep"))]
struct HeldLockStack {
    entries: [HeldLockEntry; MAX_HELD_LOCKS],
    depth: usize,
}

#[cfg(any(debug_assertions, feature = "lockdep"))]
impl HeldLockStack {
    const fn new() -> Self {
        Self {
            entries: [HeldLockEntry {
                class_id: LockClassId::INVALID,
                instance: 0,
                in_irq: false,
            }; MAX_HELD_LOCKS],
            depth: 0,
        }
    }

    /// 压入一个锁 (记录类 + 实例)
    ///
    /// 溢出返回 false 且**不打印** —— 发射由调用方排入 `PendingReports`
    /// (M5: 不在任何全局锁临界区内写串口)。
    fn push(&mut self, class_id: LockClassId, instance: usize, in_irq: bool) -> bool {
        if self.depth >= MAX_HELD_LOCKS {
            return false;
        }
        self.entries[self.depth] = HeldLockEntry {
            class_id,
            instance,
            in_irq,
        };
        self.depth += 1;
        true
    }

    /// 弹出指定实例 (按实例地址精确匹配; LIFO 从栈顶向下)
    fn pop(&mut self, class_id: LockClassId, instance: usize) -> bool {
        for i in (0..self.depth).rev() {
            if self.entries[i].instance == instance && self.entries[i].class_id == class_id {
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

    /// 查找栈中是否已持有指定实例 (递归获取判定: 同一物理锁重入)
    fn contains_instance(&self, instance: usize) -> bool {
        self.entries[..self.depth]
            .iter()
            .any(|e| e.instance == instance)
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

/// Per-CPU 持有锁栈数组 (每 CPU 一格; 仅 lockdep 激活时占用, 避免 `MAX_CPUS` 格常驻 boot BSS).
///
/// 访问纪律: 每次 [`with_current_stack`] 先 `disable_interrupts` 再操作本 CPU 格,
/// 与 `IrqSpinLock` 同一语义 —— 普通 `SpinLock` 的 lockdep acquire 可能在中断开启时
/// 发生, 若不关中断, 同核 ISR 可重入同一格造成 `&mut` 别名 UB; 关中断后本核无并发,
/// 跨核访问的是互不重叠的格.
#[cfg(any(debug_assertions, feature = "lockdep"))]
struct PerCpuHeldStacks {
    slots: [core::cell::UnsafeCell<HeldLockStack>; crate::privileged::config::MAX_CPUS],
}

// SAFETY: 每个 `slots[cpu]` 仅由其所属 CPU 在关中断临界区内独占访问 (见
// `PerCpuHeldStacks` 文档), 跨 CPU 访问不重叠的格, 不存在数据竞争与 `&mut` 别名;
// 由此整体可安全 `Sync` (I2: 每格仅属主 CPU 可见其持有状态).
#[cfg(any(debug_assertions, feature = "lockdep"))]
unsafe impl Sync for PerCpuHeldStacks {}

/// Per-CPU 持有锁栈全局数组 (const 初始化, 仅 lockdep 激活时占用).
#[cfg(any(debug_assertions, feature = "lockdep"))]
static HELD_LOCK_STACKS: PerCpuHeldStacks = PerCpuHeldStacks {
    slots: [const { core::cell::UnsafeCell::new(HeldLockStack::new()) };
        crate::privileged::config::MAX_CPUS],
};

/// 在当前 CPU 的持有锁栈上执行闭包, 关中断保证本核独占 (返回 `None` 表示 CPU id 越界,
/// 调用方保守跳过检测).
#[cfg(any(debug_assertions, feature = "lockdep"))]
fn with_current_stack<R>(f: impl FnOnce(&mut HeldLockStack) -> R) -> Option<R> {
    let cpu = crate::privileged::smp::get_current_cpu() as usize;
    if cpu >= crate::privileged::config::MAX_CPUS {
        return None;
    }
    let prev = crate::privileged::sync::disable_interrupts();
    // SAFETY: 已屏蔽本 CPU 中断 → 本核无并发访问 `slots[cpu]`; 其他 CPU 访问不同格,
    // 无 `&mut` 别名 (见 `PerCpuHeldStacks` Sync 契约).
    let r = f(unsafe { &mut *HELD_LOCK_STACKS.slots[cpu].get() });
    crate::privileged::sync::restore_interrupts(prev);
    Some(r)
}

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
// 违规发射队列 (M5: 打印延迟到全局锁临界区之外)
// ============================================================================

/// 单次记账可积压的发射数: AB-BA 报告上限 = 持有深度, 另加睡眠锁告警与
/// 持有栈溢出各一条 (release 路径每 op 至多一条)。
#[cfg(any(debug_assertions, feature = "lockdep"))]
const PENDING_CAPACITY: usize = MAX_HELD_LOCKS + 2;

/// 门控关闭的构建只剩检查 1 一条发射路径。
#[cfg(not(any(debug_assertions, feature = "lockdep")))]
const PENDING_CAPACITY: usize = 1;

/// 延迟发射的违规记录 (载荷全 `Copy` ⇒ 队列是栈上定长数组, 零堆分配)
#[derive(Clone, Copy)]
enum PendingReport {
    /// 空槽 (队列按前缀密排, 首个 `Idle` 即结束)
    Idle,
    /// 检查 1: 中断上下文获取睡眠锁
    SleepInIrq { name: &'static str, kind: LockKind },
    /// 检查 2.5: 持有 IRQ 上下文锁时获取睡眠锁
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    SleepWhileHeldInIrq { name: &'static str },
    /// 检查 3: AB-BA 环
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    Circular {
        from: &'static str,
        to: &'static str,
    },
    /// 检查 2: 递归获取非递归锁
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    Recursive { name: &'static str },
    /// 释放了当前上下文未持有的锁
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    ReleasedNotHeld { name: &'static str },
    /// 持有栈溢出 (该锁本轮起不再被跟踪)
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    HeldStackOverflow { depth: usize },
}

impl PendingReport {
    /// 在 `LOCK_DEP_MAP` guard 释放之后发射。
    fn emit(self) {
        match self {
            Self::Idle => {}
            Self::SleepInIrq { name, kind } => lockdep_log!(
                "lockdep VIOLATION: acquiring sleep lock '{}' ({:?}) in IRQ context!",
                name,
                kind
            ),
            #[cfg(any(debug_assertions, feature = "lockdep"))]
            Self::SleepWhileHeldInIrq { name } => lockdep_log!(
                "lockdep WARNING: acquiring sleep lock '{}' while holding IRQ-context lock",
                name
            ),
            #[cfg(any(debug_assertions, feature = "lockdep"))]
            Self::Circular { from, to } => lockdep_log!(
                "lockdep DEADLOCK: circular dependency detected: {} → {} (AB-BA)",
                from,
                to
            ),
            #[cfg(any(debug_assertions, feature = "lockdep"))]
            Self::Recursive { name } => lockdep_log!(
                "lockdep VIOLATION: recursive acquire of non-recursive lock '{}'!",
                name
            ),
            #[cfg(any(debug_assertions, feature = "lockdep"))]
            Self::ReleasedNotHeld { name } => lockdep_log!(
                "lockdep VIOLATION: releasing lock '{}' not held by current context!",
                name
            ),
            #[cfg(any(debug_assertions, feature = "lockdep"))]
            Self::HeldStackOverflow { depth } => {
                lockdep_log!("lockdep: held lock stack overflow (depth={})", depth);
            }
        }
    }
}

/// 栈上定长发射队列。
struct PendingReports {
    items: [PendingReport; PENDING_CAPACITY],
    len: usize,
}

impl PendingReports {
    const fn new() -> Self {
        Self {
            items: [PendingReport::Idle; PENDING_CAPACITY],
            len: 0,
        }
    }

    /// SIMPLIFIED: 队列满即丢弃该条报告; 影响面 = 同一次 acquire 内违规条数超过
    /// `PENDING_CAPACITY` 时少打一行 (`violation_count` 与 `deadlock_detected`
    /// 不受影响, 不丢判定); 何时需扩展 = 单 op 可能贡献多于容量条报告时
    /// (当前上限 = 持有深度 + 2, 与容量同式, 实际不可达)。
    fn record(&mut self, report: PendingReport) {
        if self.len < PENDING_CAPACITY {
            self.items[self.len] = report;
            self.len += 1;
        }
    }

    fn emit_all(self) {
        for report in self.items {
            if matches!(report, PendingReport::Idle) {
                break;
            }
            report.emit();
        }
    }
}

// ============================================================================
// 公开 API
// ============================================================================

/// 注册锁类
///
/// 返回 `LockClassId`, 后续 acquire/release 使用此 ID。
/// 同名锁类只注册一次 (幂等)。类表满 (`MAX_LOCK_CLASSES`) 时返回
/// [`LockClassId::INVALID`], 而 acquire/release 对该 ID 一律短路放行 ——
/// 即注册失败只丢失观测, 不影响锁本身; 该丢失**不静默**: 每次都累加
/// [`LockdepStats::class_table_dropped`] 粘滞读数 (判据见模块文档).
pub fn register_class(desc: LockClassDesc) -> LockClassId {
    let mut map = LOCK_DEP_MAP.lock();
    map.register(desc)
}

/// 锁获取通知
///
/// 在锁成功获取后调用。一次全局临界区内依次做:
/// - **检查 1**: 中断上下文获取睡眠锁
/// - **检查 2**: 递归获取非递归锁 (按 `instance` 判同一物理实例)
/// - **检查 2.5**: 持有中断上下文锁时获取睡眠锁 (只发 WARNING)
/// - **检查 3**: 与当前持有锁的依赖关系 (AB-BA 检测, 只对新边跑 BFS)
///
/// # 参数
/// - `class_id`: 锁类 ID (名字, 依赖图用)
/// - `instance`: 锁实例地址 (物理锁指针, 持有栈/递归/pop 用)
/// - `irq_context`: 是否在中断上下文中获取 (由调用方判断)
///
/// # 返回
/// - `true`: 放行 (含仅命中检查 2.5 / 3 的情形 —— 环检测不改写返回值,
///   违规只体现在报告与 `num_violations()` 上)
/// - `false`: 命中检查 1 或检查 2 —— 纯诊断信号, 锁的真实持有状态不受影响,
///   五把原语的调用点也一律不据此回退
pub fn acquire(class_id: LockClassId, instance: usize, irq_context: bool) -> bool {
    // 门控关闭 (release, 无 lockdep feature) 时下方 per-CPU 持有栈段被 cfg 掉,
    // instance 仅在该段使用 → 显式消费避免 unused 警告.
    #[cfg(not(any(debug_assertions, feature = "lockdep")))]
    let _ = instance;
    if class_id == LockClassId::INVALID {
        return true;
    }

    // M2: 检查 1/2/2.5/3 与压栈收进**同一次** `LOCK_DEP_MAP` 临界区 (原形态是
    // 1 + 持有深度 次全局加锁); M1: 遍历持有栈直读切片, 不再 `to_vec()` 堆分配;
    // M5: 违规只登记进栈上队列, 临界区释放后才发射。
    let mut reports = PendingReports::new();
    let mut proceed = true;
    {
        let mut map = LOCK_DEP_MAP.lock();
        map.stats.acquire_ops += 1;
        map.stats.map_sections += 1;
        let kind = map.class_kind(class_id);

        // 检查 1: 中断上下文获取睡眠锁
        if let Some(k) = kind
            && irq_context
            && k.may_sleep()
        {
            map.violation_count.fetch_add(1, Ordering::Relaxed);
            reports.record(PendingReport::SleepInIrq {
                name: map.class_name(class_id),
                kind: k,
            });
            proceed = false;
        }

        // 检查 2/2.5/3 + 压栈: 操作当前 CPU 的 per-CPU 持有锁栈 (仅 lockdep 激活时).
        #[cfg(any(debug_assertions, feature = "lockdep"))]
        if proceed {
            let stack_verdict = with_current_stack(|stack| {
                // 检查 2: 递归获取非递归锁 — 同一物理实例重入才算递归
                // (不同实例同类 — 如 per-CPU cfs_rq 成对持有 — 是合法的, 不算递归).
                if stack.contains_instance(instance) {
                    return false;
                }
                // 检查 2.5: 持有 IRQ 上下文锁时获取 sleep lock → 潜在死锁
                if !irq_context
                    && stack.any_in_irq()
                    && let Some(k) = kind
                    && k.may_sleep()
                {
                    reports.record(PendingReport::SleepWhileHeldInIrq {
                        name: map.class_name(class_id),
                    });
                }
                // 检查 3: AB-BA 依赖检测 — 对持有栈中每个锁添加 "held → new" 依赖边.
                // 跳过同类边 (同一 class, 不同 instance: per-CPU 数组锁成对持有, 此类边
                // 无跨类顺序信息, 且会构成 has_path 自环假阳性).
                for entry in stack.held_classes() {
                    if entry.class_id == class_id {
                        continue;
                    }
                    map.stats.edge_attempts += 1;
                    if !map.add_dependency(entry.class_id, class_id) {
                        // 不中断记账: 其余边继续登记, 全部报告一起在 guard 外打印
                        reports.record(PendingReport::Circular {
                            from: map.class_name(entry.class_id),
                            to: map.class_name(class_id),
                        });
                    }
                }
                // 压入持有栈 (溢出登记报告 + 粘滞失效计数, 打印仍在 guard 外)
                if !stack.push(class_id, instance, irq_context) {
                    // 显式失败 (不静默): 该锁自此不在持有栈里 —— 后续同类锁的递归/
                    // AB-BA 判定都看不到它, 覆盖面有损, 必须留下可读的账.
                    map.stats.held_stack_overflowed += 1;
                    reports.record(PendingReport::HeldStackOverflow { depth: stack.len() });
                }
                true
            });
            // Some(false): 检测到递归获取非递归锁 → 违规; None (CPU 越界): 保守放行
            if stack_verdict == Some(false) {
                map.violation_count.fetch_add(1, Ordering::Relaxed);
                reports.record(PendingReport::Recursive {
                    name: map.class_name(class_id),
                });
                proceed = false;
            }
        }
    }
    reports.emit_all();
    proceed
}

/// 锁释放通知
///
/// 在锁释放前调用。按 (`class_id`, `instance`) 从当前 CPU 的持有栈中移除;
/// 栈中没有该实例即「释放未持有的锁」, 计入 `num_violations()`.
/// 门控关闭的构建里本函数只消费参数, 不进 `LOCK_DEP_MAP`.
pub fn release(class_id: LockClassId, instance: usize) {
    // 门控关闭的 boot (两门皆关): 不持有锁栈, 仅消费参数避免 unused.
    #[cfg(not(any(debug_assertions, feature = "lockdep")))]
    let _ = (class_id, instance);

    #[cfg(any(debug_assertions, feature = "lockdep"))]
    {
        if class_id == LockClassId::INVALID {
            return;
        }
        let popped = with_current_stack(|stack| stack.pop(class_id, instance));
        // Some(false): 释放了当前上下文未持有的锁 → 违规
        if popped == Some(false) {
            let mut reports = PendingReports::new();
            {
                let map = LOCK_DEP_MAP.lock();
                map.violation_count.fetch_add(1, Ordering::Relaxed);
                reports.record(PendingReport::ReleasedNotHeld {
                    name: map.class_name(class_id),
                });
            }
            reports.emit_all();
        }
    }
}

/// 查询当前持有锁深度
#[must_use]
pub fn held_depth() -> usize {
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    {
        with_current_stack(|stack| stack.len()).unwrap_or(0)
    }
    #[cfg(not(any(debug_assertions, feature = "lockdep")))]
    {
        0
    }
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

/// 读取记账计数 (`LOCK_DEP_MAP` guard 内快照)
///
/// 低开销化的归因入口: `map_sections == acquire_ops` 即「每 op 单次全局临界区」
/// 成立; `new_edges` 远小于 `edge_attempts` 即「BFS 只对新边执行」成立。
///
/// 覆盖面归因入口: `class_table_dropped` / `held_stack_overflowed` 是**粘滞失效读数**
/// (只增不减), 任一非零即本轮检测覆盖有损 —— 启动末段读数行与门禁硬零断言消费它们。
#[must_use]
pub fn stats() -> LockdepStats {
    LOCK_DEP_MAP.lock().stats
}

/// 打印当前锁依赖状态 (调试用)
pub fn dump_state() {
    lockdep_log!("=== Lockdep State Dump ===");
    {
        let map = LOCK_DEP_MAP.lock();
        lockdep_log!("Registered classes: {}", map.num_classes());
        lockdep_log!("Violations: {}", map.num_violations());
        lockdep_log!("Stats: {:?}", map.stats);
        lockdep_log!(
            "Deadlock detected: {}",
            map.deadlock_detected.load(Ordering::Relaxed)
        );
    }

    // 当前 CPU 持有锁栈 (仅 lockdep 激活时). 定长栈快照 —— 与记账路径同一口径:
    // 不在持有栈的借用期内做堆分配.
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    {
        let held = with_current_stack(|stack| {
            let mut snap = [(LockClassId::INVALID, false); MAX_HELD_LOCKS];
            for (i, entry) in stack.held_classes().iter().enumerate() {
                snap[i] = (entry.class_id, entry.in_irq);
            }
            (stack.len(), snap)
        });
        if let Some((depth, snap)) = held {
            lockdep_log!("Held locks (depth={}):", depth);
            for &(class_id, in_irq) in snap.iter().take(depth) {
                let name = LOCK_DEP_MAP.lock().class_name(class_id);
                lockdep_log!("  {} (irq={})", name, in_irq);
            }
        }
    }

    {
        let map = LOCK_DEP_MAP.lock();
        lockdep_log!("Dependency edges:");
        for i in 0..map.class_count {
            for j in 0..map.class_count {
                if map.adjacency[i][j] != 0 {
                    lockdep_log!("  {} → {}", map.classes[i].name, map.classes[j].name,);
                }
            }
        }
    }
    lockdep_log!("=== End Lockdep Dump ===");
}

// ============================================================================
// 单元测试 (host 端)
// ============================================================================

// P-43 甲: 本模块的被测符号 (`HeldLockStack` / `MAX_HELD_LOCKS` /
// `LockDepMap::add_dependency`) 全在 `any(debug_assertions, feature = "lockdep")`
// 掩码下, 故测试侧必须同掩; 否则 release 测试构建 (debug_assertions 关闭且未开
// lockdep) 会引用不存在的符号而编译失败. 该掩码下 release 维的用例数为 0,
// 不代表 lockdep 覆盖已达成 — 口径登记在 net-e2e-tcp-echo-flakiness.md §6 P-43.
#[cfg(all(test, any(debug_assertions, feature = "lockdep")))]
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
        let inst_a = 0x1000usize;
        let inst_b = 0x2000usize;

        assert!(stack.push(id_a, inst_a, false));
        assert_eq!(stack.len(), 1);
        assert!(stack.contains_instance(inst_a));
        assert!(!stack.contains_instance(inst_b));

        assert!(stack.push(id_b, inst_b, true));
        assert_eq!(stack.len(), 2);
        assert!(stack.contains_instance(inst_b));

        assert!(stack.pop(id_b, inst_b));
        assert_eq!(stack.len(), 1);
        assert!(!stack.contains_instance(inst_b));

        assert!(stack.pop(id_a, inst_a));
        assert_eq!(stack.len(), 0);
    }

    #[test]
    fn test_held_stack_overflow() {
        let mut stack = HeldLockStack::new();
        for i in 0..MAX_HELD_LOCKS {
            assert!(stack.push(LockClassId(i as u16), i, false));
        }
        // 第 MAX_HELD_LOCKS + 1 次应失败
        assert!(!stack.push(LockClassId(MAX_HELD_LOCKS as u16), MAX_HELD_LOCKS, false));
    }

    #[test]
    fn test_held_stack_pop_not_present() {
        let mut stack = HeldLockStack::new();
        stack.push(LockClassId(0), 0x10, false);
        assert!(!stack.pop(LockClassId(1), 0x20)); // 实例不存在
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

    /// M6 记账: `new_edges` 只统计**首次出现**的边 —— 已知边在 `add_dependency`
    /// 开头短路, 不进 BFS。这条即「稳态成本与 BFS 无关」的可执行判据.
    #[test]
    fn test_stats_new_edges_count_only_unseen_pairs() {
        let mut map = LockDepMap::new();
        let id_a = map.register(LockClassDesc {
            name: "A",
            kind: LockKind::Mutex,
        });
        let id_b = map.register(LockClassDesc {
            name: "B",
            kind: LockKind::Mutex,
        });
        assert_eq!(map.stats.new_edges, 0);

        assert!(map.add_dependency(id_a, id_b));
        assert_eq!(map.stats.new_edges, 1);

        // 同一对再来一次: 已知边短路, 既不记新边也不进 BFS.
        assert!(map.add_dependency(id_a, id_b));
        assert_eq!(map.stats.new_edges, 1);

        // 成环的边: 确实进了一次 BFS (回滚不影响它被计入).
        assert!(!map.add_dependency(id_b, id_a));
        assert_eq!(map.stats.new_edges, 2);
    }

    /// M5 发射队列: 满即丢该条报告, 不 panic、不越界.
    #[test]
    fn test_pending_reports_overflow_drops_extra() {
        let mut queue = PendingReports::new();
        for i in 0..PENDING_CAPACITY {
            queue.record(PendingReport::HeldStackOverflow { depth: i });
        }
        queue.record(PendingReport::HeldStackOverflow { depth: 0xFFFF });
        assert_eq!(queue.len, PENDING_CAPACITY);
    }

    /// A-127b ①: 类表满的丢弃必须留下粘滞读数 —— 显式失败, 不静默、不 panic.
    ///
    /// 白盒: 直接把 `class_count` 填到上限, 免得为凑 64 个 `&'static str` 类名造无关
    /// 夹具 (表满分支只看 `class_count`, 与条目 `used` 无关).
    #[test]
    fn test_register_full_table_counts_as_dropped() {
        let mut map = LockDepMap::new();
        map.class_count = MAX_LOCK_CLASSES;
        let before = map.stats.class_table_dropped;

        // 满表 → INVALID, 且读数 +1 (锁语义不受影响, 丢的是检测覆盖)
        assert_eq!(
            map.register(LockClassDesc {
                name: "full_table_probe_a",
                kind: LockKind::SpinLock,
            }),
            LockClassId::INVALID
        );
        assert_eq!(map.class_count, MAX_LOCK_CLASSES, "满表注册不得改写类表");
        assert_eq!(map.stats.class_table_dropped, before + 1);

        // 再来一次: 计数逐次累加 (粘滞, 只增不减)
        assert_eq!(
            map.register(LockClassDesc {
                name: "full_table_probe_b",
                kind: LockKind::SpinLock,
            }),
            LockClassId::INVALID
        );
        assert_eq!(map.stats.class_table_dropped, before + 2);
    }

    /// A-127b ③: 持有栈溢出的丢失同样必须留下粘滞读数.
    ///
    /// 判据取 `>= +1` 而非相等: 持有栈变动全部发生在 `LOCK_DEP_MAP` 临界区内, 故
    /// 本线程连压 `MAX_HELD_LOCKS + 1` 个不同实例时第 9 次必然落空 (别的线程只会让
    /// 它更早落空), 而别的线程若也撞满栈会额外贡献读数 —— 相等判据会被无关流量打断.
    #[test]
    fn test_acquire_held_stack_overflow_counts_as_dropped() {
        let id = register_class(LockClassDesc {
            name: "held_overflow_probe",
            kind: LockKind::SpinLock,
        });
        let before = stats().held_stack_overflowed;

        // 实例键取未映射低位页 (与 0x5A00 探针同族), 不会撞上真实锁地址
        let mut instances = [0usize; MAX_HELD_LOCKS + 1];
        for (i, slot) in instances.iter_mut().enumerate() {
            *slot = 0x5B00 + i;
            assert!(acquire(id, *slot, false));
        }
        let after = stats().held_stack_overflowed;
        assert!(
            after >= before + 1,
            "持有栈溢出未累加粘滞读数 (before={before}, after={after})"
        );

        // 逐一归还: 满栈那一条没进持有栈, release 会对它报一条 released_not_held
        // (host-test 下 klog 为 no-op, 且无用例断言全局违规数为零) —— 不影响本判据.
        for instance in instances {
            release(id, instance);
        }
    }

    /// M2: 一次 acquire 记账恰好进入一次 `LOCK_DEP_MAP` 全局临界区.
    ///
    /// 「恰好一次」由全局等式 `map_sections == acquire_ops` 执法 —— 两计数器在同一条
    /// 临界区内背靠背自增, 任一完整快照下必然相等; 若某次 op 多进了一次临界区, 等式即
    /// 破裂. 本测试线程的增量只判 `>= 1`: 计数器是全局的, 并行用例 (含持栈溢出探针)
    /// 的 acquire 会落进同一个采样窗口, 写死 `== 1` 会把无关流量读成失败 (P-43 口径).
    #[test]
    fn test_acquire_enters_map_once_per_op() {
        let id = register_class(LockClassDesc {
            name: "stats_acquire_probe",
            kind: LockKind::SpinLock,
        });
        let before = stats();
        assert!(acquire(id, 0x5A00, false));
        release(id, 0x5A00);
        let after = stats();
        let acquired = after.acquire_ops - before.acquire_ops;
        let sections = after.map_sections - before.map_sections;
        assert!(
            acquired >= 1,
            "本线程的 acquire 未被计入 (acquired={acquired})"
        );
        assert_eq!(
            acquired, sections,
            "采样窗口内 acquire 与临界区增量不等: 每 op 单次临界区已失效"
        );
        assert_eq!(after.map_sections, after.acquire_ops);
    }
}
