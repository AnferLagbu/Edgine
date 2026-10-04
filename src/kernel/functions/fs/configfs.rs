#![deny(unsafe_code)]
//! configfs — 配置文件系统
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `functions::fs::api`。
//!
//! ## 职责
//!
//! - 提供内核对象的配置接口 (/sys/kernel/config/)
//! - 支持动态创建/删除配置项
//!
//! ## 参考
//!
//! - Linux configfs 文档: Documentation/filesystems/configfs.rst

use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::sync::OnceLock;
use crate::privileged::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// 最大目录数
pub const MAX_DIRS: usize = 32;

/// 名称最大长度
pub const MAX_NAME_LEN: usize = 32;

/// 值最大长度
pub const MAX_VALUE_LEN: usize = 128;

// ============================================================================
// 节点
// ============================================================================

/// configfs 节点
#[derive(Debug, Clone, Copy)]
pub struct ConfigNode {
    /// 节点名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
    /// 节点值
    pub value: [u8; MAX_VALUE_LEN],
    /// 值长度
    pub value_len: u8,
    /// 是否可写
    pub writable: bool,
}

impl ConfigNode {
    pub const fn new() -> Self {
        Self {
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
            value: [0u8; MAX_VALUE_LEN],
            value_len: 0,
            writable: true,
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

    /// 读取节点值
    ///
    /// # Errors
    /// 当 `buf` 长度小于节点值长度时返回 `EINVAL`.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        let value = self.get_value().as_bytes();
        if buf.len() < value.len() {
            return Err(Errno::EINVAL);
        }
        buf[..value.len()].copy_from_slice(value);
        Ok(value.len())
    }

    /// 写入节点值
    ///
    /// # Errors
    /// 当节点不可写时返回 `EPERM`;
    /// 当 `data` 不是合法 UTF-8 时返回 `EINVAL`.
    pub fn write(&mut self, data: &[u8]) -> Result<(), Errno> {
        if !self.writable {
            return Err(Errno::EPERM);
        }
        let value = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
        self.set_value(value);
        Ok(())
    }
}

// ============================================================================
// 目录
// ============================================================================

/// configfs 目录
#[derive(Debug, Clone, Copy)]
pub struct ConfigDir {
    /// 目录名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
    /// 节点
    pub nodes: [ConfigNode; 16],
    /// 节点数量
    pub node_count: u8,
}

impl ConfigDir {
    pub const fn new() -> Self {
        Self {
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
            nodes: [const { ConfigNode::new() }; 16],
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
    /// 当节点表已满 (节点数量达到容量上限) 时返回 `ENOMEM`.
    pub fn add_node(&mut self, node: ConfigNode) -> Result<(), Errno> {
        if self.node_count as usize >= self.nodes.len() {
            return Err(Errno::ENOMEM);
        }
        self.nodes[self.node_count as usize] = node;
        self.node_count += 1;
        Ok(())
    }

    /// 查找节点
    pub fn find_node(&self, name: &str) -> Option<&ConfigNode> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].get_name() == name {
                return Some(&self.nodes[i]);
            }
        }
        None
    }

    /// 查找节点 (可变)
    pub fn find_node_mut(&mut self, name: &str) -> Option<&mut ConfigNode> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].get_name() == name {
                return Some(&mut self.nodes[i]);
            }
        }
        None
    }

    /// 删除节点
    ///
    /// # Errors
    /// 当不存在名为 `name` 的节点时返回 `ENOENT`.
    pub fn delete_node(&mut self, name: &str) -> Result<(), Errno> {
        let mut found = false;
        for i in 0..self.node_count as usize {
            if self.nodes[i].get_name() == name {
                found = true;
                for j in i..(self.node_count as usize - 1) {
                    self.nodes[j] = self.nodes[j + 1];
                }
                self.node_count -= 1;
                break;
            }
        }
        if found { Ok(()) } else { Err(Errno::ENOENT) }
    }
}

// ============================================================================
// configfs 文件系统
// ============================================================================

/// configfs 文件系统
pub struct ConfigFs {
    /// 目录
    pub dirs: [ConfigDir; MAX_DIRS],
    /// 目录数量
    pub dir_count: u32,
}

impl ConfigFs {
    #[expect(
        clippy::large_stack_arrays,
        reason = "large_stack_arrays: 大栈数组是性能权衡 (避免堆分配); 当前优先 expect"
    )]
    pub const fn new() -> Self {
        Self {
            dirs: [const { ConfigDir::new() }; MAX_DIRS],
            dir_count: 0,
        }
    }

    /// 创建目录
    ///
    /// # Errors
    /// 当目录数量已达上限 (`MAX_DIRS`) 时返回 `ENOMEM`;
    /// 当已存在同名目录时返回 `EEXIST`.
    pub fn create_dir(&mut self, name: &str) -> Result<(), Errno> {
        if self.dir_count as usize >= MAX_DIRS {
            return Err(Errno::ENOMEM);
        }

        // 检查是否已存在
        for i in 0..self.dir_count as usize {
            if self.dirs[i].get_name() == name {
                return Err(Errno::EEXIST);
            }
        }

        let idx = self.dir_count as usize;
        self.dirs[idx].set_name(name);
        self.dir_count += 1;
        Ok(())
    }

    /// 删除目录
    ///
    /// # Errors
    /// 当不存在名为 `name` 的目录时返回 `ENOENT`.
    pub fn delete_dir(&mut self, name: &str) -> Result<(), Errno> {
        let mut found = false;
        for i in 0..self.dir_count as usize {
            if self.dirs[i].get_name() == name {
                found = true;
                for j in i..(self.dir_count as usize - 1) {
                    self.dirs[j] = self.dirs[j + 1];
                }
                self.dir_count -= 1;
                break;
            }
        }
        if found { Ok(()) } else { Err(Errno::ENOENT) }
    }

    /// 查找目录
    pub fn find_dir(&self, name: &str) -> Option<&ConfigDir> {
        for i in 0..self.dir_count as usize {
            if self.dirs[i].get_name() == name {
                return Some(&self.dirs[i]);
            }
        }
        None
    }

    /// 查找目录 (可变)
    pub fn find_dir_mut(&mut self, name: &str) -> Option<&mut ConfigDir> {
        for i in 0..self.dir_count as usize {
            if self.dirs[i].get_name() == name {
                return Some(&mut self.dirs[i]);
            }
        }
        None
    }

    /// 读取节点
    ///
    /// # Errors
    /// 当目录或节点不存在时返回 `ENOENT`; 当 `buf` 过小时返回 `EINVAL`.
    pub fn read_node(&self, dir: &str, node: &str, buf: &mut [u8]) -> Result<usize, Errno> {
        let d = self.find_dir(dir).ok_or(Errno::ENOENT)?;
        let n = d.find_node(node).ok_or(Errno::ENOENT)?;
        n.read(buf)
    }

    /// 写入节点
    ///
    /// # Errors
    /// 当目录或节点不存在时返回 `ENOENT`; 当节点不可写或 `data` 非法时返回 `EPERM`/`EINVAL`.
    pub fn write_node(&mut self, dir: &str, node: &str, data: &[u8]) -> Result<(), Errno> {
        let d = self.find_dir_mut(dir).ok_or(Errno::ENOENT)?;
        let n = d.find_node_mut(node).ok_or(Errno::ENOENT)?;
        n.write(data)
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 configfs 文件系统实例
static CONFIG_FS: OnceLock<Mutex<ConfigFs>> = OnceLock::new();

/// 获取 configfs 文件系统实例
pub fn get_config_fs() -> &'static Mutex<ConfigFs> {
    CONFIG_FS.get_or_init(|slot| {
        slot.write(Mutex::new(ConfigFs::new()));
    })
}

// ============================================================================
// safe API
// ============================================================================

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 挂载 configfs
///
/// # Errors
/// 当前实现恒返回 `Ok(())`; 内部创建默认目录时的错误被忽略.
pub fn mount_configfs() -> Result<(), Errno> {
    let fs = get_config_fs();
    let mut fs = fs.lock();
    // 创建默认目录
    let _ = fs.create_dir("devices");
    let _ = fs.create_dir("modules");
    let _ = fs.create_dir("groups");
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 卸载 configfs
///
/// # Errors
/// 当前实现恒返回 `Ok(())`; 仅清空目录数据, 不返回错误.
pub fn umount_configfs() -> Result<(), Errno> {
    let mut fs = get_config_fs().lock();
    fs.dir_count = 0;
    Ok(())
}
