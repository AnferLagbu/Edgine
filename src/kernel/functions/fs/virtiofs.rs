#![deny(unsafe_code)]
#![deny(missing_docs)]
//! virtiofs — virtio 文件系统 (VM 通信)
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `functions::fs::api`。
//!
//! ## 职责
//!
//! - 提供 host/guest 共享文件系统接口
//! - 支持 virtio-fs 协议
//!
//! ## 参考
//!
//! - virtio-fs 文档: docs/virtio-fs.rst

use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::sync::OnceLock;
use crate::privileged::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// 最大节点数
pub const MAX_NODES: usize = 256;

/// 名称最大长度
pub const MAX_NAME_LEN: usize = 32;

// ============================================================================
// 操作类型
// ============================================================================

/// virtiofs 操作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioFsOp {
    /// 获取文件系统属性
    Lookup,
    /// 获取文件属性
    GetAttr,
    /// 设置文件属性
    SetAttr,
    /// 创建目录
    Mkdir,
    /// 删除文件
    Unlink,
    /// 删除目录
    Rmdir,
    /// 重命名
    Rename,
    /// 打开文件
    Open,
    /// 创建文件
    Create,
    /// 读取文件
    Read,
    /// 写入文件
    Write,
    /// 截断文件
    Truncate,
    /// 释放文件
    Release,
    /// 读取目录
    Readdir,
    /// 获取文件系统信息
    StatFs,
}

impl VirtioFsOp {
    /// 从操作码解析
    pub fn from_opcode(opcode: u32) -> Self {
        match opcode {
            3 => Self::GetAttr,
            4 => Self::SetAttr,
            11 => Self::Mkdir,
            12 => Self::Unlink,
            13 => Self::Rmdir,
            14 => Self::Rename,
            16 => Self::Open,
            18 => Self::Create,
            19 => Self::Read,
            20 => Self::Write,
            22 => Self::Truncate,
            24 => Self::Release,
            27 => Self::Readdir,
            31 => Self::StatFs,
            // 未识别操作码 (含 1 = Lookup) 回退为 Lookup
            _ => Self::Lookup,
        }
    }
}

// ============================================================================
// 节点
// ============================================================================

/// virtiofs 节点
#[derive(Debug, Clone, Copy)]
pub struct VirtioFsNode {
    /// 节点 ID (inode)
    pub id: u64,
    /// 父节点 ID
    pub parent_id: u64,
    /// 节点名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
    /// 文件模式
    pub mode: u32,
    /// 链接数
    pub nlink: u32,
    /// 文件大小
    pub size: u64,
}

impl VirtioFsNode {
    /// 构造一个 virtiofs 节点 (链接数为 1, 无名称)。
    pub const fn new(id: u64, parent_id: u64, mode: u32) -> Self {
        Self {
            id,
            parent_id,
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
            mode,
            nlink: 1,
            size: 0,
        }
    }

    /// 设置名称
    pub fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_NAME_LEN);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name_len = crate::functions::intcast::low_u8_from_usize(len);
    }

    /// 获取名称
    pub fn get_name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }
}

// ============================================================================
// virtiofs 文件系统
// ============================================================================

/// virtiofs 文件系统
pub struct VirtioFs {
    /// 节点表
    pub nodes: [VirtioFsNode; MAX_NODES],
    /// 节点数量
    pub node_count: u32,
    /// 下一个可用节点 ID
    pub next_id: u64,
}

impl VirtioFs {
    #[expect(
        clippy::large_stack_arrays,
        reason = "large_stack_arrays: 大栈数组是性能权衡 (避免堆分配); 当前优先 expect"
    )]
    /// 构造空的 virtiofs 实例 (无节点)。
    pub const fn new() -> Self {
        Self {
            nodes: [const { VirtioFsNode::new(0, 0, 0) }; MAX_NODES],
            node_count: 0,
            next_id: 1,
        }
    }

    /// 初始化根节点
    pub fn init(&mut self) {
        self.nodes[0] = VirtioFsNode::new(0, 0, 0o40755);
        self.nodes[0].set_name("/");
        self.node_count = 1;
        self.next_id = 1;
    }

    /// 查找节点
    pub fn find_node(&self, id: u64) -> Option<&VirtioFsNode> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].id == id {
                return Some(&self.nodes[i]);
            }
        }
        None
    }

    /// 创建节点
    ///
    /// # Errors
    /// 当父节点不存在时返回 `ENOENT`; 当节点表已满 (`MAX_NODES`) 时返回 `ENOMEM`.
    pub fn create_node(&mut self, parent_id: u64, name: &str, mode: u32) -> Result<u64, Errno> {
        if self.find_node(parent_id).is_none() {
            return Err(Errno::ENOENT);
        }

        if self.node_count as usize >= MAX_NODES {
            return Err(Errno::ENOMEM);
        }

        let id = self.next_id;
        self.next_id += 1;

        let idx = self.node_count as usize;
        self.nodes[idx] = VirtioFsNode::new(id, parent_id, mode);
        self.nodes[idx].set_name(name);
        self.node_count += 1;

        Ok(id)
    }

    /// 删除节点
    ///
    /// # Errors
    /// 当 `id` 为根节点 (0) 时返回 `EINVAL`; 当节点仍有子节点时返回 `ENOTEMPTY`;
    /// 当节点不存在时返回 `ENOENT`.
    pub fn delete_node(&mut self, id: u64) -> Result<(), Errno> {
        if id == 0 {
            return Err(Errno::EINVAL);
        }

        // 检查是否有子节点
        for i in 0..self.node_count as usize {
            if self.nodes[i].parent_id == id {
                return Err(Errno::ENOTEMPTY);
            }
        }

        let mut found = false;
        for i in 0..self.node_count as usize {
            if self.nodes[i].id == id {
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

    /// 列出子节点 (缓冲区不足时输出会被截断)
    pub fn list_children(&self, parent_id: u64, buf: &mut [u8]) -> usize {
        let mut offset = 0;
        for i in 0..self.node_count as usize {
            if self.nodes[i].parent_id == parent_id {
                let name = self.nodes[i].get_name().as_bytes();
                if offset + name.len() + 1 > buf.len() {
                    break;
                }
                buf[offset..offset + name.len()].copy_from_slice(name);
                offset += name.len();
                buf[offset] = b'\n';
                offset += 1;
            }
        }
        offset
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 virtiofs 文件系统实例
static VIRTIO_FS: OnceLock<Mutex<VirtioFs>> = OnceLock::new();

/// 获取 virtiofs 文件系统实例
pub fn get_virtiofs() -> &'static Mutex<VirtioFs> {
    VIRTIO_FS.get_or_init(|slot| {
        slot.write(Mutex::new(VirtioFs::new()));
    })
}

// ============================================================================
// safe API
// ============================================================================

/// 挂载 virtiofs
pub fn mount_virtiofs() {
    let fs = get_virtiofs();
    fs.lock().init();
}

/// 卸载 virtiofs
pub fn umount_virtiofs() {
    let mut fs = get_virtiofs().lock();
    fs.node_count = 0;
    fs.next_id = 1;
}

/// 处理请求
///
/// # Errors
/// 当 `opcode` 非法时返回 `EINVAL`; 当数据非 UTF-8 或操作对象不存在时返回 `EINVAL`/`ENOENT`;
/// 当操作不被支持时返回 `ENOSYS`.
pub fn handle_request(
    opcode: u32,
    node_id: u64,
    data: &[u8],
    buf: &mut [u8],
) -> Result<usize, Errno> {
    let op = VirtioFsOp::from_opcode(opcode);
    let fs = get_virtiofs();
    let mut fs = fs.lock();

    match op {
        VirtioFsOp::Lookup => {
            let name = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
            // 遍历子节点
            for i in 0..fs.node_count as usize {
                if fs.nodes[i].parent_id == node_id && fs.nodes[i].get_name() == name {
                    if buf.len() < 8 {
                        return Err(Errno::EINVAL);
                    }
                    buf[..8].copy_from_slice(&fs.nodes[i].id.to_le_bytes());
                    return Ok(8);
                }
            }
            Err(Errno::ENOENT)
        }
        VirtioFsOp::GetAttr => {
            if let Some(node) = fs.find_node(node_id) {
                if buf.len() < 32 {
                    return Err(Errno::EINVAL);
                }
                buf[0..4].copy_from_slice(&node.mode.to_le_bytes());
                buf[4..8].copy_from_slice(&node.nlink.to_le_bytes());
                buf[8..16].copy_from_slice(&node.size.to_le_bytes());
                Ok(16)
            } else {
                Err(Errno::ENOENT)
            }
        }
        VirtioFsOp::Mkdir => {
            let name = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
            fs.create_node(node_id, name, 0o40755)?;
            Ok(0)
        }
        VirtioFsOp::Create => {
            let name = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
            fs.create_node(node_id, name, 0o10_0644)?;
            Ok(0)
        }
        VirtioFsOp::Unlink | VirtioFsOp::Rmdir => {
            fs.delete_node(node_id)?;
            Ok(0)
        }
        VirtioFsOp::Readdir => Ok(fs.list_children(node_id, buf)),
        _ => Err(Errno::ENOSYS),
    }
}
