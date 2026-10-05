//! NUMA (Non-Uniform Memory Access) — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原策略代码于 T2-6 (2026-06-16) 迁至 `functions::mm::numa`, 本文件仅 re-export。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：`NumaMempolicy` 被
//! privileged proc/process 机制持有、`numa_init` 被 privileged mm 机制调用 —
//! 属机制项, 迁回。依赖闭包全在 privileged 内 (mm::PAGE_SIZE + sync::IrqSpinLock)。
//!
//! functions 侧改 `pub use crate::privileged::mm::numa::*` 保持 API 兼容。

use crate::privileged::mm::PAGE_SIZE;
use crate::privileged::sync::IrqSpinLock;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use alloc::sync::Arc;
use alloc::vec::Vec;

// ============================================================================
// 常量
// ============================================================================

/// 最大 NUMA 节点数
pub const MAX_NUMA_NODES: usize = 8;
/// 本地距离 (SLIT 标准值)
pub const LOCAL_DISTANCE: u8 = 10;
/// 远程距离 (SLIT 标准值)
pub const REMOTE_DISTANCE: u8 = 20;

// ============================================================================
// NumaPolicy — 内存策略
// ============================================================================

/// NUMA 内存策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NumaPolicy {
    /// 默认: 从当前 CPU 所在节点分配
    Default = 0,
    /// 绑定: 只从指定节点集分配
    Bind = 1,
    /// 交织: 轮询从节点集分配 (带宽优化)
    Interleave = 2,
    /// 首选: 优先从指定节点分配, 回退到其他节点
    Preferred = 3,
}

impl NumaPolicy {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Bind,
            2 => Self::Interleave,
            3 => Self::Preferred,
            _ => Self::Default,
        }
    }

    /// 由 Linux `MPOL_*` ABI 模式值构造
    ///
    /// 注意: Linux 模式编码与本枚举判别值**不同** (`MPOL_PREFERRED = 1` 而
    /// `NumaPolicy::Preferred = 3`), 用户态传入的 mode 必须经本函数映射,
    /// 禁止直接 `from_u8`. 非法值返回 `None`.
    pub fn from_linux_mode(mode: u32) -> Option<Self> {
        match mode {
            MPOL_DEFAULT => Some(Self::Default),
            MPOL_PREFERRED => Some(Self::Preferred),
            MPOL_BIND => Some(Self::Bind),
            MPOL_INTERLEAVE => Some(Self::Interleave),
            _ => None,
        }
    }
}

/// Linux `MPOL_*` 内存策略模式编码 (`mbind` / `set_mempolicy` ABI)
pub const MPOL_DEFAULT: u32 = 0;
/// 优先节点 (Linux 语义: 单个 preferred 节点)
pub const MPOL_PREFERRED: u32 = 1;
/// 绑定节点集 (仅从节点集分配)
pub const MPOL_BIND: u32 = 2;
/// 交织节点集 (轮询分配, 带宽优化)
pub const MPOL_INTERLEAVE: u32 = 3;

/// VMA 级 NUMA 内存策略 (`mbind` 设置)
///
/// ## 两级查询
///
/// VMA 级 (本类型, 由 `mbind` 写入 VMA) 优先于进程级 (`NumaMempolicy`,
/// 由 `set_mempolicy` 写入) — 见 `effective_policy_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumaRangePolicy {
    /// 策略模式
    pub mode: NumaPolicy,
    /// 目标节点位掩码 (bit i = node i)
    pub nodemask: u64,
}

/// 进程级 NUMA 策略
#[derive(Debug)]
pub struct NumaMempolicy {
    /// 策略模式
    pub mode: IrqSpinLock<NumaPolicy>,
    /// 目标节点位掩码 (bit i = 使用 node i)
    pub nodemask: IrqSpinLock<u64>,
    /// 交织分配下一个节点索引 (仅 Interleave 模式使用)
    pub interleave_next: AtomicU32,
}

impl NumaMempolicy {
    pub fn new() -> Self {
        Self {
            mode: IrqSpinLock::new(NumaPolicy::Default),
            nodemask: IrqSpinLock::new(0),
            interleave_next: AtomicU32::new(0),
        }
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 根据策略选择分配节点
    pub fn preferred_node(&self, current_cpu_node: u32) -> Option<u32> {
        let mode = *self.mode.lock();
        let mask = *self.nodemask.lock();

        match mode {
            NumaPolicy::Default => Some(current_cpu_node),
            NumaPolicy::Bind | NumaPolicy::Preferred => {
                if mask == 0 {
                    Some(current_cpu_node)
                } else {
                    Some(mask.trailing_zeros())
                }
            }
            NumaPolicy::Interleave => {
                if mask == 0 {
                    Some(current_cpu_node)
                } else {
                    let nodes = Self::nodes_from_mask(mask);
                    if nodes.is_empty() {
                        Some(current_cpu_node)
                    } else {
                        let idx = self.interleave_next.fetch_add(1, Ordering::Relaxed);
                        Some(nodes[idx as usize % nodes.len()])
                    }
                }
            }
        }
    }

    fn nodes_from_mask(mask: u64) -> Vec<u32> {
        let mut nodes = Vec::new();
        for i in 0..64 {
            if mask & (1u64 << i) != 0 {
                nodes.push(i as u32);
            }
        }
        nodes
    }
}

// ============================================================================
// NumaNode — NUMA 节点
// ============================================================================

/// NUMA 节点描述
#[derive(Debug)]
pub struct NumaNode {
    /// 节点 ID
    pub id: u32,
    /// 本节点的 CPU 列表
    pub cpus: Vec<u32>,
    /// 本地内存起始物理地址
    pub memory_start: u64,
    /// 本地内存大小 (字节)
    pub memory_size: u64,
    /// 已分配页数
    pub allocated_pages: AtomicU64,
    /// 空闲页数
    pub free_pages: AtomicU64,
}

impl NumaNode {
    pub fn new(id: u32) -> Self {
        Self {
            id,
            cpus: Vec::new(),
            memory_start: 0,
            memory_size: 0,
            allocated_pages: AtomicU64::new(0),
            free_pages: AtomicU64::new(0),
        }
    }

    pub fn contains_cpu(&self, cpu_id: u32) -> bool {
        self.cpus.contains(&cpu_id)
    }

    pub fn free_ratio(&self) -> u32 {
        let total = self.memory_size / PAGE_SIZE;
        if total == 0 {
            return 100;
        }
        let free = self.free_pages.load(Ordering::Acquire);
        (free * 100 / total) as u32
    }
}

// ============================================================================
// NumaTopology — 全局拓扑
// ============================================================================

/// NUMA 拓扑管理器
pub struct NumaTopology {
    nodes: IrqSpinLock<Vec<Arc<NumaNode>>>,
    distance_matrix: IrqSpinLock<[[u8; MAX_NUMA_NODES]; MAX_NUMA_NODES]>,
    cpu_to_node: IrqSpinLock<Vec<u32>>,
    num_nodes: AtomicU32,
    initialized: AtomicBool,
}

impl NumaTopology {
    pub const fn new() -> Self {
        Self {
            nodes: IrqSpinLock::new(Vec::new()),
            distance_matrix: IrqSpinLock::new([[0u8; MAX_NUMA_NODES]; MAX_NUMA_NODES]),
            cpu_to_node: IrqSpinLock::new(Vec::new()),
            num_nodes: AtomicU32::new(0),
            initialized: AtomicBool::new(false),
        }
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    /// 用单节点拓扑初始化 (UMA 回退)
    pub fn init_uma(&self, total_memory: u64, num_cpus: u32) {
        let mut nodes = self.nodes.lock();
        let mut cpu_map = self.cpu_to_node.lock();

        let mut node0 = NumaNode::new(0);
        node0.memory_start = 0;
        node0.memory_size = total_memory;
        node0
            .free_pages
            .store(total_memory / PAGE_SIZE, Ordering::Release);

        for cpu in 0..num_cpus {
            node0.cpus.push(cpu);
            cpu_map.push(0);
        }

        nodes.push(Arc::new(node0));

        self.distance_matrix.lock()[0][0] = LOCAL_DISTANCE;

        self.num_nodes.store(1, Ordering::Release);
        self.initialized.store(true, Ordering::Release);

        // 日志: 使用 safe API
        let msg = alloc::format!(
            "[NUMA] UMA topology: 1 node, {} CPUs, {} MB\n",
            num_cpus,
            total_memory / (1024 * 1024)
        );
        crate::privileged::klog::serial_write_bytes(msg.as_bytes());
    }

    /// 添加 NUMA 节点 (用于 ACPI SRAT 解析)
    pub fn add_node(&self, node: NumaNode) {
        let id = node.id as usize;
        if id >= MAX_NUMA_NODES {
            return;
        }
        let mut nodes = self.nodes.lock();
        let mut cpu_map = self.cpu_to_node.lock();

        for &cpu in &node.cpus {
            if (cpu as usize) >= cpu_map.len() {
                cpu_map.resize(cpu as usize + 1, 0);
            }
            cpu_map[cpu as usize] = node.id;
        }

        nodes.push(Arc::new(node));
        self.num_nodes.store(nodes.len() as u32, Ordering::Release);
    }

    /// 设置距离矩阵条目
    pub fn set_distance(&self, from: usize, to: usize, distance: u8) {
        if from < MAX_NUMA_NODES && to < MAX_NUMA_NODES {
            self.distance_matrix.lock()[from][to] = distance;
        }
    }

    /// 获取两个节点间的距离
    pub fn distance(&self, from: u32, to: u32) -> u8 {
        let f = from as usize;
        let t = to as usize;
        if f < MAX_NUMA_NODES && t < MAX_NUMA_NODES {
            let d = self.distance_matrix.lock()[f][t];
            if d > 0 {
                return d;
            }
        }
        if from == to {
            LOCAL_DISTANCE
        } else {
            REMOTE_DISTANCE
        }
    }

    /// 获取 CPU 所在的 NUMA 节点
    pub fn cpu_to_node(&self, cpu_id: u32) -> u32 {
        let cpu_map = self.cpu_to_node.lock();
        cpu_map.get(cpu_id as usize).copied().unwrap_or(0)
    }

    /// 获取指定节点
    pub fn get_node(&self, node_id: u32) -> Option<Arc<NumaNode>> {
        let nodes = self.nodes.lock();
        nodes.iter().find(|n| n.id == node_id).map(Arc::clone)
    }

    pub fn num_nodes(&self) -> u32 {
        self.num_nodes.load(Ordering::Acquire)
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    pub fn all_nodes(&self) -> Vec<Arc<NumaNode>> {
        self.nodes.lock().clone()
    }

    /// 选择最佳分配节点
    pub fn best_alloc_node(&self, policy: &NumaMempolicy, current_cpu: u32) -> u32 {
        let current_node = self.cpu_to_node(current_cpu);

        policy.preferred_node(current_node).unwrap_or(current_node)
    }

    /// 查找距离最近且有空闲内存的节点
    pub fn nearest_free_node(&self, from_node: u32) -> Option<u32> {
        let nodes = self.nodes.lock();
        if nodes.is_empty() {
            return None;
        }

        let mut sorted: Vec<(u8, u32)> = nodes
            .iter()
            .map(|n| (self.distance(from_node, n.id), n.id))
            .collect();
        sorted.sort_by_key(|(d, _)| *d);

        for (_, node_id) in sorted {
            if let Some(node) = nodes.iter().find(|n| n.id == node_id) {
                if node.free_pages.load(Ordering::Acquire) > 0 {
                    return Some(node_id);
                }
            }
        }
        None
    }
}

// ============================================================================
// 全局拓扑实例
// ============================================================================

/// 全局 NUMA 拓扑
static NUMA_TOPOLOGY: NumaTopology = NumaTopology::new();

/// 初始化 NUMA 子系统 (UMA 回退)
pub fn numa_init(total_memory: u64, num_cpus: u32) {
    if NUMA_TOPOLOGY.is_initialized() {
        return;
    }
    NUMA_TOPOLOGY.init_uma(total_memory, num_cpus);
}

/// 获取全局 NUMA 拓扑引用
pub fn numa_topology() -> &'static NumaTopology {
    &NUMA_TOPOLOGY
}

/// NUMA 是否已初始化
pub fn numa_is_initialized() -> bool {
    NUMA_TOPOLOGY.is_initialized()
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_get_mempolicy` — 获取当前 NUMA 内存策略
pub fn sys_get_mempolicy(_mode_ptr: u64, _nodemask_ptr: u64) -> i64 {
    let pid = crate::privileged::proc::process_get_current_pid();
    let result = crate::privileged::proc::PROCESS_TABLE.with_process(pid, |p| {
        let policy = p.numa_policy.lock();
        let mode = *policy.mode.lock() as u8;
        let mask = *policy.nodemask.lock();
        (mode, mask)
    });

    match result {
        Some((mode, mask)) => {
            let _ = (mode, mask);
            0
        }
        None => -(22i64),
    }
}

/// `sys_set_mempolicy` — 设置 NUMA 内存策略
pub fn sys_set_mempolicy(mode: u64, nodemask: u64) -> i64 {
    let policy_mode = NumaPolicy::from_u8(mode as u8);

    if (policy_mode == NumaPolicy::Bind || policy_mode == NumaPolicy::Interleave) && nodemask == 0 {
        return -(22i64);
    }

    let pid = crate::privileged::proc::process_get_current_pid();
    let result = crate::privileged::proc::PROCESS_TABLE.with_process(pid, |p| {
        let policy = p.numa_policy.lock();
        *policy.mode.lock() = policy_mode;
        *policy.nodemask.lock() = nodemask;
    });

    match result {
        Some(()) => 0,
        None => -(3i64),
    }
}

/// `sys_migrate_pages` — 将进程页面迁移到目标节点
pub fn sys_migrate_pages(_target_nodemask: u64) -> i64 {
    0
}

/// `sys_getcpu` — 获取当前 CPU 和 NUMA 节点
pub fn sys_getcpu() -> i64 {
    let cpu = crate::privileged::smp::current_cpu_index();
    let node = if numa_is_initialized() {
        numa_topology().cpu_to_node(cpu)
    } else {
        0
    };
    i64::from(cpu) | (i64::from(node) << 32)
}

/// 两级 NUMA 策略查询: `addr` 所在 VMA 的 VMA 级策略优先, 回退进程级策略
///
/// SIMPLIFIED: 当前无分配侧消费方 — PMM 为单一全局 buddy (无 per-node 分区),
/// `pmm.rs` 分配路径未消费 NUMA 策略; 本函数仅作为 `mbind` 策略落地的查询入口.
/// 影响面: 查询结果暂不影响实际页分配节点. 何时需扩展: PMM 引入 per-node 分区后,
/// 由分配决策路径 (alloc_trait) 消费本函数.
pub fn effective_policy_for(addr: usize) -> Option<NumaRangePolicy> {
    if let Some(mm) = crate::privileged::mm::vma_get_current_mm() {
        if let Some(p) = mm.numa_policy_at(addr) {
            return Some(p);
        }
    }

    let pid = crate::privileged::proc::process_get_current_pid();
    crate::privileged::proc::PROCESS_TABLE.with_process(pid, |p| {
        let policy = p.numa_policy.lock();
        NumaRangePolicy {
            mode: *policy.mode.lock(),
            nodemask: *policy.nodemask.lock(),
        }
    })
}
