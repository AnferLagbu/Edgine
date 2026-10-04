#![deny(unsafe_code)]
//! 拓扑感知级联策略 — functions/freg/ 业务层
//!
//! ## 职责
//!
//! 给定域的依赖关系 (parent ↔ child), 决定级联恢复方向:
//!
//! - **`BottomUp`** (自底向上): 子域先恢复, 父域后恢复 (默认)
//! - **`TopDown`** (自顶向下): 父域先恢复, 子域后恢复 (用于父域配置错误场景)
//! - **Isolated** (隔离): 失败域不动, 仅恢复依赖者
//!
//! ## 与 `privileged::freg::manager::cascade_rollback` 区别
//!
//! - privileged 层: BFS 拓扑遍历 + undo 回滚 (机制)
//! - functions 层: 业务策略 — 哪些域需要级联, 哪些需要隔离, 哪些不参与
//!
//! ## @SAFE
//!
//! 本文件不含 `unsafe`. 拓扑由 functions 层静态定义.

use super::attribution::FaultAttribution;
use super::recovery_policy::{FaultSignal, RecoveryAction, RecoveryPolicy};

/// 级联方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeDirection {
    /// 自底向上 (子先父后) — 默认
    BottomUp,
    /// 自顶向下 (父先子后) — 用于配置错误
    TopDown,
    /// 隔离失败域, 不级联
    Isolated,
}

/// 域节点
#[derive(Debug, Clone, Copy)]
pub struct DomainNode {
    pub id: u64,
    pub name: &'static str,
    pub parent: Option<u64>,
    pub children_count: u32,
}

/// 拓扑描述
///
/// ```text
/// Domain { id: 1, name: "pmm",  parent: None, children_count: 0 }
/// Domain { id: 2, name: "unkfs", parent: Some(1), children_count: 2 }
/// Domain { id: 3, name: "net",  parent: Some(1), children_count: 0 }
/// ```
pub struct DomainTopology {
    pub nodes: [DomainNode; MAX_TOPOLOGY_DOMAINS],
    pub count: usize,
}

pub const MAX_TOPOLOGY_DOMAINS: usize = 16;

impl DomainTopology {
    pub const fn new() -> Self {
        const EMPTY: DomainNode = DomainNode {
            id: 0,
            name: "",
            parent: None,
            children_count: 0,
        };
        Self {
            nodes: [EMPTY; MAX_TOPOLOGY_DOMAINS],
            count: 0,
        }
    }

    /// 添加域节点
    ///
    /// 返回 false 表示拓扑已满或 ID 已存在
    pub fn add(&mut self, id: u64, name: &'static str, parent: Option<u64>) -> bool {
        if self.count >= MAX_TOPOLOGY_DOMAINS {
            return false;
        }
        // 检查 ID 重复
        for n in self.nodes.iter().take(self.count) {
            if n.id == id {
                return false;
            }
        }
        // 更新 parent 的 children_count
        if let Some(pid) = parent {
            for n in self.nodes.iter_mut().take(self.count) {
                if n.id == pid {
                    n.children_count += 1;
                    break;
                }
            }
        }
        self.nodes[self.count] = DomainNode {
            id,
            name,
            parent,
            children_count: 0,
        };
        self.count += 1;
        true
    }

    /// 查找域节点
    pub fn find(&self, id: u64) -> Option<&DomainNode> {
        self.nodes.iter().take(self.count).find(|n| n.id == id)
    }

    /// 计算 `BottomUp` 顺序的恢复队列
    pub fn bottom_up_order(&self, root_id: u64) -> CascadeQueue {
        // 简化实现: 返回根 + 全部子节点 (实际应 BFS)
        let mut q = CascadeQueue::new();
        if let Some(root) = self.find(root_id) {
            q.push(root.id);
        }
        // 收集 children
        for n in self.nodes.iter().take(self.count) {
            if n.parent == Some(root_id) {
                q.push(n.id);
            }
        }
        q
    }

    /// 计算 `TopDown` 顺序的恢复队列
    pub fn top_down_order(&self, root_id: u64) -> CascadeQueue {
        let mut q = CascadeQueue::new();
        if let Some(root) = self.find(root_id) {
            q.push(root.id);
            for n in self.nodes.iter().take(self.count) {
                if n.parent == Some(root_id) {
                    q.push(n.id);
                }
            }
        }
        q
    }
}

/// 级联队列
#[derive(Debug, Clone, Copy)]
pub struct CascadeQueue {
    pub order: [u64; MAX_TOPOLOGY_DOMAINS],
    pub count: usize,
}

impl CascadeQueue {
    pub const fn new() -> Self {
        Self {
            order: [0; MAX_TOPOLOGY_DOMAINS],
            count: 0,
        }
    }
    pub fn push(&mut self, id: u64) {
        if self.count < MAX_TOPOLOGY_DOMAINS {
            self.order[self.count] = id;
            self.count += 1;
        }
    }
}

/// 级联策略决策
pub struct CascadePolicy;

impl CascadePolicy {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 给定故障域 + 拓扑, 决定级联方向
    pub fn direction(
        attribution: &FaultAttribution,
        topo: &DomainTopology,
        failed_id: u64,
    ) -> CascadeDirection {
        match attribution {
            FaultAttribution::Tcb { .. } => {
                // TCB 故障 → 隔离 (防止级联)
                CascadeDirection::Isolated
            }
            FaultAttribution::CrossLayer { .. } => {
                // 跨层故障 → 隔离失败域
                CascadeDirection::Isolated
            }
            FaultAttribution::Service {
                recoverable: false, ..
            } => CascadeDirection::Isolated,
            FaultAttribution::Service { .. } | FaultAttribution::Unknown => {
                // 服务域故障 → 根据子节点数量决策
                topo.find(failed_id)
                    .map_or(CascadeDirection::BottomUp, |node| {
                        if node.children_count == 0 {
                            // 叶子节点 → 自底向上
                            CascadeDirection::BottomUp
                        } else {
                            // 内部节点 → 自顶向下 (父先恢复可让子节点重新连接)
                            CascadeDirection::TopDown
                        }
                    })
            }
        }
    }

    /// 编排级联恢复
    ///
    /// 返回: 实际执行的恢复动作 + 队列
    pub fn orchestrate(signal: &FaultSignal, topo: &DomainTopology) -> CascadePlan {
        let action = RecoveryPolicy::decide(signal);
        let direction = Self::direction(&signal.attribution, topo, signal.domain_id());
        let queue = match direction {
            CascadeDirection::BottomUp => topo.bottom_up_order(signal.domain_id()),
            CascadeDirection::TopDown => topo.top_down_order(signal.domain_id()),
            CascadeDirection::Isolated => CascadeQueue::new(),
        };
        CascadePlan {
            action,
            direction,
            queue,
        }
    }
}

impl FaultSignal {
    /// 从 attribution 提取 `domain_id` (Service 分支)
    pub fn domain_id(&self) -> u64 {
        match self.attribution {
            FaultAttribution::Service { domain_id, .. } => domain_id,
            _ => 0,
        }
    }
}

/// 级联计划
#[derive(Debug, Clone, Copy)]
pub struct CascadePlan {
    pub action: RecoveryAction,
    pub direction: CascadeDirection,
    pub queue: CascadeQueue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::freg::attribution::TcbModule;

    fn build_topology() -> DomainTopology {
        let mut topo = DomainTopology::new();
        topo.add(1, "pmm", None);
        topo.add(2, "unkfs", Some(1));
        topo.add(3, "net", Some(1));
        topo.add(4, "vfs", Some(2));
        topo
    }

    #[test]
    fn topology_add_basic() {
        let mut topo = DomainTopology::new();
        assert!(topo.add(1, "pmm", None));
        assert!(topo.add(2, "unkfs", Some(1)));
        assert!(topo.add(3, "net", Some(1)));
        assert_eq!(topo.count, 3);
    }

    #[test]
    fn topology_add_duplicate_rejected() {
        let mut topo = DomainTopology::new();
        assert!(topo.add(1, "pmm", None));
        assert!(!topo.add(1, "dup", None));
    }

    #[test]
    fn topology_add_full_rejected() {
        let mut topo = DomainTopology::new();
        for i in 0..MAX_TOPOLOGY_DOMAINS {
            assert!(topo.add(i as u64, "x", None));
        }
        assert!(!topo.add(99, "y", None));
    }

    #[test]
    fn topology_parent_children_count() {
        let topo = build_topology();
        let pmm = topo.find(1).unwrap();
        assert_eq!(pmm.children_count, 2); // unkfs + net
    }

    #[test]
    fn topology_find() {
        let topo = build_topology();
        assert!(topo.find(2).is_some());
        assert!(topo.find(99).is_none());
    }

    #[test]
    fn bottom_up_order() {
        let topo = build_topology();
        let q = topo.bottom_up_order(1);
        assert_eq!(q.count, 3);
        assert_eq!(q.order[0], 1);
    }

    #[test]
    fn top_down_order() {
        let topo = build_topology();
        let q = topo.top_down_order(2);
        assert_eq!(q.count, 2);
        assert_eq!(q.order[0], 2);
    }

    #[test]
    fn direction_tcb_isolated() {
        let topo = build_topology();
        let attr = FaultAttribution::Tcb {
            module: TcbModule::FREG,
        };
        let dir = CascadePolicy::direction(&attr, &topo, 2);
        assert_eq!(dir, CascadeDirection::Isolated);
    }

    #[test]
    fn direction_service_leaf_bottomup() {
        let topo = build_topology();
        let attr = FaultAttribution::Service {
            domain_id: 4,
            recoverable: true,
        };
        let dir = CascadePolicy::direction(&attr, &topo, 4);
        assert_eq!(dir, CascadeDirection::BottomUp);
    }

    #[test]
    fn direction_service_internal_topdown() {
        let topo = build_topology();
        let attr = FaultAttribution::Service {
            domain_id: 1,
            recoverable: true,
        };
        let dir = CascadePolicy::direction(&attr, &topo, 1);
        assert_eq!(dir, CascadeDirection::TopDown);
    }

    #[test]
    fn orchestrate_full_plan() {
        let topo = build_topology();
        let signal = FaultSignal::service(2, 3, 0, 0, 100);
        let plan = CascadePolicy::orchestrate(&signal, &topo);
        assert_eq!(plan.action, RecoveryAction::FREGSoftReset);
        assert_eq!(plan.direction, CascadeDirection::TopDown);
        assert_eq!(plan.queue.count, 2);
    }
}
