//! EGDF设备树 (EGDF Device Tree)
//!
//! 构建内核设备拓扑层次结构，支持：
//! - 总线发现 (PCI, platform bus, virtio-mmio)
//! - compatible 字符串匹配 (驱动绑定)
//! - 设备属性 (reg, interrupts, clocks, gpios 等)
//! - 父子层级关系
//!
//! ## 与现有 EGDF 的关系
//!
//! 本模块在现有扁平 `EGDF_DEVICES` Vec 之上增加设备树层级:
//! - `EGDFNode` 代表树中的一个设备节点
//! - `EGDF_ROOT` 是根节点 (platform bus)
//! - 注册时同时添加到 `EGDF_DEVICES` (兼容已有查询) 和树中
//!
//! ## 示例
//!
//! ```text
//! root (platform bus)
//! ├── pci0 (PCI host bridge)      compatible: "pci-host-bridge"
//! │   ├── virtio-blk0             compatible: "virtio,blk"
//! │   └── e1000                   compatible: "intel,e1000"
//! ├── serial0                     compatible: "ns16550a"
//! └── timer                       compatible: "arm,armv8-timer"
//! ```

use super::{DeviceState, EGDFProto, egdf_register};
use crate::framework::sync::IrqSpinLock as Mutex;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyValue {
    U32(u32),
    U64(u64),
    String(&'static str),
    Bool(bool),
    U32Array(&'static [u32]),
    U64Array(&'static [u64]),
}

impl PropertyValue {
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::U32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&'static str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Property {
    pub name: &'static str,
    pub value: PropertyValue,
}

pub type NodeId = u32;

#[derive(Debug, Clone)]
pub struct EGDFNode {
    pub id: NodeId,
    pub name: &'static str,
    pub compatible: Vec<&'static str>,
    pub proto: EGDFProto,
    pub properties: BTreeMap<&'static str, PropertyValue>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub device_id: Option<u32>,
    pub state: DeviceState,
    pub user_mapped: Option<u32>,
    pub firmware: Option<super::firmware::FirmwareBlob>,
}

impl EGDFNode {
    pub fn new(id: NodeId, name: &'static str, proto: EGDFProto) -> Self {
        Self {
            id,
            name,
            compatible: Vec::new(),
            proto,
            properties: BTreeMap::new(),
            parent: None,
            children: Vec::new(),
            device_id: None,
            state: DeviceState::Uninit,
            user_mapped: None,
            firmware: None,
        }
    }

    pub fn add_prop(&mut self, name: &'static str, value: PropertyValue) {
        self.properties.insert(name, value);
    }

    pub fn get_prop(&self, name: &str) -> Option<&PropertyValue> {
        self.properties.get(name)
    }

    pub fn set_compatible(&mut self, compat: Vec<&'static str>) {
        self.compatible = compat;
    }

    pub fn matches_compatible(&self, needle: &str) -> bool {
        self.compatible.contains(&needle)
    }
}

// SAFETY: EGDFNode 访问由 DEV_TREE 锁 (Mutex) 串行化.
// 无需 UnsafeCell, 因为我们已用 Mutex 保护的集合.
unsafe impl Send for EGDFNode {}
unsafe impl Sync for EGDFNode {}

static NEXT_NODE_ID: AtomicU32 = AtomicU32::new(1);
static ROOT_NODE_ID: AtomicU32 = AtomicU32::new(0);

pub(crate) struct DevTree {
    pub(crate) nodes: Vec<EGDFNode>,
}

pub(crate) static DEV_TREE: Mutex<DevTree> = Mutex::new(DevTree { nodes: Vec::new() });

fn devtree_init_impl() {
    let mut tree = DEV_TREE.lock();
    let root_id = NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed);
    let mut root = EGDFNode::new(root_id, "platform", EGDFProto::Bus);
    root.set_compatible(vec!["simple-bus"]);
    root.state = DeviceState::Ready;
    tree.nodes.push(root);
    ROOT_NODE_ID.store(root_id, Ordering::Release);
}

pub fn devtree_root_id() -> NodeId {
    ROOT_NODE_ID.load(Ordering::Acquire)
}

/// 创建新的设备树节点
pub(crate) fn devtree_create_node_impl(
    name: &'static str,
    proto: EGDFProto,
    parent_id: Option<NodeId>,
) -> Option<NodeId> {
    let mut tree = DEV_TREE.lock();
    let id = NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed);

    let mut node = EGDFNode::new(id, name, proto);
    node.parent = parent_id;

    if let Some(pid) = parent_id {
        if let Some(parent) = tree.nodes.iter_mut().find(|n| n.id == pid) {
            parent.children.push(id);
        } else {
            return None;
        }
    } else {
        let root = ROOT_NODE_ID.load(Ordering::Acquire);
        if root != 0 {
            if let Some(root_node) = tree.nodes.iter_mut().find(|n| n.id == root) {
                root_node.children.push(id);
                node.parent = Some(root);
            }
        }
    }

    tree.nodes.push(node);
    Some(id)
}

pub fn devtree_add_prop(node_id: NodeId, name: &'static str, value: PropertyValue) {
    let mut tree = DEV_TREE.lock();
    if let Some(node) = tree.nodes.iter_mut().find(|n| n.id == node_id) {
        node.add_prop(name, value);
    }
}

pub fn devtree_set_compatible(node_id: NodeId, compat: Vec<&'static str>) {
    let mut tree = DEV_TREE.lock();
    if let Some(node) = tree.nodes.iter_mut().find(|n| n.id == node_id) {
        node.set_compatible(compat);
    }
}

pub fn devtree_set_state(node_id: NodeId, state: DeviceState) {
    let mut tree = DEV_TREE.lock();
    if let Some(node) = tree.nodes.iter_mut().find(|n| n.id == node_id) {
        node.state = state;
    }
}

pub fn devtree_set_user_mapped(node_id: NodeId, pid: u32) {
    let mut tree = DEV_TREE.lock();
    if let Some(node) = tree.nodes.iter_mut().find(|n| n.id == node_id) {
        node.user_mapped = Some(pid);
    }
}

pub fn devtree_clear_user_mapped(node_id: NodeId) {
    let mut tree = DEV_TREE.lock();
    if let Some(node) = tree.nodes.iter_mut().find(|n| n.id == node_id) {
        node.user_mapped = None;
    }
}

/// 清除指定 PID 在所有设备节点上的绑定标记
/// 在进程退出时调用, 防止 `user_mapped` 残留导致设备节点无法重新绑定
pub fn devtree_clear_user_mapped_by_pid(pid: u32) {
    let mut tree = DEV_TREE.lock();
    for node in &mut tree.nodes {
        if node.user_mapped == Some(pid) {
            node.user_mapped = None;
        }
    }
}

pub fn devtree_get_user_mapped(node_id: NodeId) -> Option<u32> {
    let tree = DEV_TREE.lock();
    tree.nodes
        .iter()
        .find(|n| n.id == node_id)
        .and_then(|n| n.user_mapped)
}

/// 按 compatible 字符串查找节点
pub fn devtree_find_compatible(compat: &str) -> Option<NodeId> {
    let tree = DEV_TREE.lock();
    tree.nodes
        .iter()
        .find(|n| n.matches_compatible(compat))
        .map(|n| n.id)
}

/// 按名称查找节点
pub fn devtree_find_by_name(name: &str) -> Option<NodeId> {
    let tree = DEV_TREE.lock();
    tree.nodes.iter().find(|n| n.name == name).map(|n| n.id)
}

/// 获取节点的所有直接子节点
pub fn devtree_children(node_id: NodeId) -> Vec<NodeId> {
    let tree = DEV_TREE.lock();
    tree.nodes
        .iter()
        .find(|n| n.id == node_id)
        .map_or_default(|n| n.children.clone())
}

/// 获取节点
pub fn devtree_get_node(node_id: NodeId) -> Option<EGDFNode> {
    let tree = DEV_TREE.lock();
    tree.nodes.iter().find(|n| n.id == node_id).cloned()
}

/// 将设备树节点绑定到 `EGDFDevice` (注册到全局设备表)
pub fn devtree_bind_device(
    node_id: NodeId,
    io_base: Option<u64>,
    irq: Option<u8>,
    driver_data: *mut u8,
) -> Option<u32> {
    let mut tree = DEV_TREE.lock();
    let node = tree.nodes.iter_mut().find(|n| n.id == node_id)?;

    let device_id = egdf_register(node.name, node.proto, io_base, irq, driver_data);
    node.device_id = Some(device_id);
    node.state = DeviceState::Ready;

    Some(device_id)
}

/// 遍历设备树 (DFS)，对每个节点调用回调
pub fn devtree_walk<F>(mut f: F)
where
    F: FnMut(&EGDFNode),
{
    let tree = DEV_TREE.lock();
    let root_id = ROOT_NODE_ID.load(Ordering::Acquire);
    if root_id == 0 {
        return;
    }

    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    fn walk_children<F: FnMut(&EGDFNode)>(nodes: &[EGDFNode], id: NodeId, f: &mut F) {
        if let Some(node) = nodes.iter().find(|n| n.id == id) {
            f(node);
            for child_id in &node.children {
                walk_children(nodes, *child_id, f);
            }
        }
    }

    walk_children(&tree.nodes, root_id, &mut f);
}

/// 从设备树节点读取 "reg" 属性的地址部分
/// reg 格式: `<addr size>` 对, 返回第一个 addr
pub fn devtree_read_addr(node_id: NodeId) -> Option<u64> {
    let tree = DEV_TREE.lock();
    let node = tree.nodes.iter().find(|n| n.id == node_id)?;
    node.get_prop("reg").and_then(PropertyValue::as_u64)
}

/// 从设备树节点读取 "interrupts" 属性
pub fn devtree_read_irq(node_id: NodeId) -> Option<u32> {
    let tree = DEV_TREE.lock();
    let node = tree.nodes.iter().find(|n| n.id == node_id)?;
    node.get_prop("interrupts").and_then(PropertyValue::as_u32)
}

pub fn devtree_count() -> usize {
    DEV_TREE.lock().nodes.len()
}

fn devtree_print_impl() {
    let tree = DEV_TREE.lock();
    let root_id = ROOT_NODE_ID.load(Ordering::Acquire);

    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    fn print_node(nodes: &[EGDFNode], id: NodeId, depth: usize) {
        if let Some(node) = nodes.iter().find(|n| n.id == id) {
            let _ = depth;
            crate::serial_println!("[{}] {} (proto={:?})", node.id, node.name, node.proto);
            for child_id in &node.children {
                print_node(nodes, *child_id, depth + 1);
            }
        }
    }

    if root_id != 0 {
        print_node(&tree.nodes, root_id, 0);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn devtree_print() {
    devtree_print_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn devtree_init() {
    devtree_init_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn devtree_create_node(name: *const u8, proto: u32, parent_id: u32) -> u32 {
    let name_str = if name.is_null() {
        "unknown"
    } else {
        // SAFETY: name 来自 FFI 的 C 字符串; 调用方保证其以 NUL 结尾,
        // 且在函数执行期间持续有效.
        unsafe {
            let bytes = core::ffi::CStr::from_ptr(name as *const core::ffi::c_char);
            bytes.to_str().unwrap_or("unknown")
        }
    };

    // 泄漏字符串以获得 `'static` 生命周期 —— 内核设备名采用此做法.
    let static_name: &'static str = name_str;

    let proto = match proto {
        1 => EGDFProto::Block,
        2 => EGDFProto::Char,
        3 => EGDFProto::Net,
        4 => EGDFProto::Input,
        5 => EGDFProto::Bus,
        _ => EGDFProto::Other,
    };

    let parent = if parent_id == 0 {
        None
    } else {
        Some(parent_id)
    };

    devtree_create_node_impl(static_name, proto, parent).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_devtree_create_and_find() {
        devtree_init();
        let node_id = devtree_create_node_impl("test", EGDFProto::Char, None).unwrap();
        assert!(node_id > 0);

        devtree_set_compatible(node_id, vec!["vendor,test-device"]);
        let found = devtree_find_compatible("vendor,test-device");
        assert_eq!(found, Some(node_id));

        // UT-07 (2026-09-26): 注册侧 devtree 组独有断言迁入 —
        // devtree_get_node 可达性 (源侧原仅经 find_compatible 间接验证).
        assert!(devtree_get_node(node_id).is_some());
    }

    #[test]
    fn test_devtree_hierarchy() {
        devtree_init();
        let root = devtree_root_id();
        let child = devtree_create_node_impl("child", EGDFProto::Char, Some(root)).unwrap();
        let grandchild =
            devtree_create_node_impl("grandchild", EGDFProto::Char, Some(child)).unwrap();

        let children = devtree_children(child);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], grandchild);
    }

    #[test]
    fn test_devtree_properties() {
        devtree_init();
        let node = devtree_create_node_impl("prop_test", EGDFProto::Block, None).unwrap();
        devtree_add_prop(node, "reg", PropertyValue::U64(0xF0000000));
        devtree_add_prop(node, "interrupts", PropertyValue::U32(42));

        assert_eq!(devtree_read_addr(node), Some(0xF0000000));
        assert_eq!(devtree_read_irq(node), Some(42));
    }
}
