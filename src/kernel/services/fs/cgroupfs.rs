#![deny(unsafe_code)]
//! cgroupfs — 控制组文件系统
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `services::fs::api`。
//!
//! ## 职责
//!
//! - 提供 cgroup 层级结构 (/sys/fs/cgroup/)
//! - 管理资源限制 (CPU, 内存, IO 等)
//!
//! ## 参考
//!
//! - Linux cgroup 文档: Documentation/admin-guide/cgroup-v2.rst

use crate::framework::sync::IrqSpinLock as Mutex;
use crate::framework::sync::OnceLock;
use crate::framework::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// 最大控制器数
pub const MAX_CONTROLLERS: usize = 4;

/// 最大组数
pub const MAX_GROUPS: usize = 32;

/// 名称最大长度
pub const MAX_NAME_LEN: usize = 32;

/// 值最大长度
pub const MAX_VALUE_LEN: usize = 64;

// ============================================================================
// 控制器
// ============================================================================

/// cgroup 控制器
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupController {
    /// CPU 控制器
    Cpu,
    /// 内存控制器
    Memory,
    /// IO 控制器
    Io,
    /// PIDs 控制器
    Pids,
}

impl CgroupController {
    /// 从名称解析
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "cpu" => Some(Self::Cpu),
            "memory" => Some(Self::Memory),
            "io" => Some(Self::Io),
            "pids" => Some(Self::Pids),
            _ => None,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取控制器名称
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Io => "io",
            Self::Pids => "pids",
        }
    }
}

// ============================================================================
// 节点
// ============================================================================

/// cgroup 节点
#[derive(Debug, Clone, Copy)]
pub struct CgroupNode {
    /// 节点名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
    /// 节点值
    pub value: [u8; MAX_VALUE_LEN],
    /// 值长度
    pub value_len: u8,
}

impl CgroupNode {
    pub const fn new() -> Self {
        Self {
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
            value: [0u8; MAX_VALUE_LEN],
            value_len: 0,
        }
    }

    /// 设置名称
    pub fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_NAME_LEN);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name_len = len as u8;
    }

    /// 获取名称
    pub fn get_name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }

    /// 设置值
    pub fn set_value(&mut self, value: &str) {
        let bytes = value.as_bytes();
        let len = bytes.len().min(MAX_VALUE_LEN);
        self.value[..len].copy_from_slice(&bytes[..len]);
        self.value_len = len as u8;
    }

    /// 获取值
    pub fn get_value(&self) -> &str {
        core::str::from_utf8(&self.value[..self.value_len as usize]).unwrap_or("")
    }
}

// ============================================================================
// 组
// ============================================================================

/// cgroup 组
#[derive(Debug, Clone, Copy)]
pub struct CgroupGroup {
    /// 组名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
    /// 控制器
    pub controller: CgroupController,
    /// 节点
    pub nodes: [CgroupNode; 4],
    /// 节点数量
    pub node_count: u8,
}

impl CgroupGroup {
    pub const fn new(controller: CgroupController) -> Self {
        Self {
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
            controller,
            nodes: [const { CgroupNode::new() }; 4],
            node_count: 0,
        }
    }

    /// 设置名称
    pub fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_NAME_LEN);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name_len = len as u8;
    }

    /// 获取名称
    pub fn get_name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }

    /// 添加节点
    ///
    /// # Errors
    /// 当节点表已满 (`node_count` 达到容量上限) 时返回 `ENOMEM`.
    pub fn add_node(&mut self, node: CgroupNode) -> Result<(), Errno> {
        if self.node_count as usize >= self.nodes.len() {
            return Err(Errno::ENOMEM);
        }
        self.nodes[self.node_count as usize] = node;
        self.node_count += 1;
        Ok(())
    }

    /// 查找节点
    pub fn find_node(&self, name: &str) -> Option<&CgroupNode> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].get_name() == name {
                return Some(&self.nodes[i]);
            }
        }
        None
    }

    /// 更新节点值
    ///
    /// # Errors
    /// 当不存在名为 `name` 的节点时返回 `ENOENT`.
    pub fn update_node(&mut self, name: &str, value: &str) -> Result<(), Errno> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].get_name() == name {
                self.nodes[i].set_value(value);
                return Ok(());
            }
        }
        Err(Errno::ENOENT)
    }
}

// ============================================================================
// cgroup 文件系统
// ============================================================================

/// cgroup 文件系统
pub struct CgroupFs {
    /// 组
    pub groups: [CgroupGroup; MAX_GROUPS],
    /// 组数量
    pub group_count: u32,
}

impl CgroupFs {
    pub const fn new() -> Self {
        Self {
            groups: [const { CgroupGroup::new(CgroupController::Cpu) }; MAX_GROUPS],
            group_count: 0,
        }
    }

    /// 创建组
    ///
    /// # Errors
    /// 当组数量已达上限 (`MAX_GROUPS`) 时返回 `ENOMEM`;
    /// 当已存在同名组时返回 `EEXIST`.
    pub fn create_group(&mut self, name: &str, controller: CgroupController) -> Result<(), Errno> {
        if self.group_count as usize >= MAX_GROUPS {
            return Err(Errno::ENOMEM);
        }

        // 检查是否已存在
        for i in 0..self.group_count as usize {
            if self.groups[i].get_name() == name {
                return Err(Errno::EEXIST);
            }
        }

        let idx = self.group_count as usize;
        self.groups[idx].set_name(name);
        self.groups[idx].controller = controller;
        self.groups[idx].node_count = 0;

        // 添加默认节点
        let mut node = CgroupNode::new();
        node.set_name("cgroup.controllers");
        node.set_value(controller.name());
        let _ = self.groups[idx].add_node(node);

        let mut node = CgroupNode::new();
        node.set_name("cgroup.procs");
        node.set_value("");
        let _ = self.groups[idx].add_node(node);

        self.group_count += 1;
        Ok(())
    }

    /// 删除组
    ///
    /// # Errors
    /// 当不存在名为 `name` 的组时返回 `ENOENT`.
    pub fn delete_group(&mut self, name: &str) -> Result<(), Errno> {
        let mut found = false;
        for i in 0..self.group_count as usize {
            if self.groups[i].get_name() == name {
                found = true;
                // 移动后面的组
                for j in i..(self.group_count as usize - 1) {
                    self.groups[j] = self.groups[j + 1];
                }
                self.group_count -= 1;
                break;
            }
        }
        if found { Ok(()) } else { Err(Errno::ENOENT) }
    }

    /// 查找组
    pub fn find_group(&self, name: &str) -> Option<&CgroupGroup> {
        for i in 0..self.group_count as usize {
            if self.groups[i].get_name() == name {
                return Some(&self.groups[i]);
            }
        }
        None
    }

    /// 读取节点值
    ///
    /// # Errors
    /// 当组为空或不存在指定组/节点时返回 `ENOENT`;
    /// 当 `buf` 长度小于节点值长度时返回 `EINVAL`.
    pub fn read_node(&self, group: &str, node: &str, buf: &mut [u8]) -> Result<usize, Errno> {
        let g = if group.is_empty() {
            // 根组不存在, 返回错误
            return Err(Errno::ENOENT);
        } else {
            self.find_group(group).ok_or(Errno::ENOENT)?
        };

        let n = g.find_node(node).ok_or(Errno::ENOENT)?;
        let value = n.get_value().as_bytes();
        if buf.len() < value.len() {
            return Err(Errno::EINVAL);
        }
        buf[..value.len()].copy_from_slice(value);
        Ok(value.len())
    }

    /// 写入节点值
    ///
    /// # Errors
    /// 当组为空或不存在指定组/节点时返回 `ENOENT`;
    /// 当 `data` 不是合法 UTF-8 时返回 `EINVAL`.
    pub fn write_node(&mut self, group: &str, node: &str, data: &[u8]) -> Result<(), Errno> {
        let g = if group.is_empty() {
            return Err(Errno::ENOENT);
        } else {
            self.groups
                .iter_mut()
                .find(|g| g.get_name() == group)
                .ok_or(Errno::ENOENT)?
        };

        let value = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
        g.update_node(node, value)
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 cgroup 文件系统实例
static CGROUP_FS: OnceLock<Mutex<CgroupFs>> = OnceLock::new();

/// 获取 cgroup 文件系统实例
pub fn get_cgroup_fs() -> &'static Mutex<CgroupFs> {
    CGROUP_FS.get_or_init(|slot| {
        slot.write(Mutex::new(CgroupFs::new()));
    })
}

// ============================================================================
// safe API
// ============================================================================

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 挂载 cgroupfs
///
/// # Errors
/// 当前实现恒返回 `Ok(())`; 内部创建默认组时的错误被忽略.
pub fn mount_cgroupfs() -> Result<(), Errno> {
    let fs = get_cgroup_fs();
    let mut fs = fs.lock();
    // 创建默认组
    let _ = fs.create_group("system", CgroupController::Cpu);
    let _ = fs.create_group("system", CgroupController::Memory);
    let _ = fs.create_group("system", CgroupController::Io);
    let _ = fs.create_group("system", CgroupController::Pids);
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 卸载 cgroupfs
///
/// # Errors
/// 当前实现恒返回 `Ok(())`; 仅清空组数据, 不返回错误.
pub fn umount_cgroupfs() -> Result<(), Errno> {
    // 重置为新实例
    // 注意: OnceLock 无法重置, 这里只是清空数据
    let mut fs = get_cgroup_fs().lock();
    fs.group_count = 0;
    Ok(())
}
