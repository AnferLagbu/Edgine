#![deny(unsafe_code)]
//! systree — 动态系统树
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 services::fs::api。
//!
//! ## 职责
//!
//! - 提供 Tree + Node + Attr 三元组
//! - 节点支持属性读写回调
//! - 用于内核对象的运行时配置和状态暴露
//!
//! ## 参考
//!
//! - Linux sysfs 文档: Documentation/filesystems/sysfs.rst
//! - Linux kobject 文档: Documentation/core-api/kobject.rst

use alloc::vec::Vec;

use crate::framework::sync::IrqSpinLock as Mutex;
use crate::framework::sync::OnceLock;
use crate::framework::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// 最大树数
pub const MAX_TREES: usize = 8;

/// 最大节点数
pub const MAX_NODES: usize = 256;

/// 最大属性数
pub const MAX_ATTRS: usize = 16;

/// 名称最大长度
pub const MAX_NAME_LEN: usize = 32;

/// 值最大长度
pub const MAX_VALUE_LEN: usize = 128;

// ============================================================================
// 属性回调
// ============================================================================

/// 属性读取回调类型
pub type AttrReadFn = fn(&mut [u8]) -> Result<usize, Errno>;

/// 属性写入回调类型
pub type AttrWriteFn = fn(&[u8]) -> Result<(), Errno>;

// ============================================================================
// 属性
// ============================================================================

/// 属性
#[derive(Debug, Clone, Copy)]
pub struct SystreeAttr {
    /// 属性名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
    /// 文件权限
    pub perm: u16,
    /// 读取回调
    pub read_fn: Option<AttrReadFn>,
    /// 写入回调
    pub write_fn: Option<AttrWriteFn>,
}

impl SystreeAttr {
    pub const fn new() -> Self {
        Self {
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
            perm: 0o644,
            read_fn: None,
            write_fn: None,
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

    /// 读取属性值
    ///
    /// # Errors
    /// 当属性未注册读取回调函数时返回 `EIO`.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        self.read_fn.map_or(Err(Errno::EIO), |f| f(buf))
    }

    /// 写入属性值
    ///
    /// # Errors
    /// 当属性未注册写入回调函数时返回 `EIO`.
    pub fn write(&self, data: &[u8]) -> Result<(), Errno> {
        self.write_fn.map_or(Err(Errno::EIO), |f| f(data))
    }
}

// ============================================================================
// 节点
// ============================================================================

/// 节点
#[derive(Debug, Clone, Copy)]
pub struct SystreeNode {
    /// 节点 ID
    pub id: u32,
    /// 父节点 ID (0 = 根)
    pub parent_id: u32,
    /// 节点名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
    /// 属性
    pub attrs: [SystreeAttr; MAX_ATTRS],
    /// 属性数量
    pub attr_count: u8,
}

impl SystreeNode {
    pub const fn new(id: u32, parent_id: u32) -> Self {
        Self {
            id,
            parent_id,
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
            attrs: [const { SystreeAttr::new() }; MAX_ATTRS],
            attr_count: 0,
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

    /// 添加属性
    ///
    /// # Errors
    /// 当属性表已满 (`MAX_ATTRS`) 时返回 `ENOMEM`.
    pub fn add_attr(&mut self, attr: SystreeAttr) -> Result<(), Errno> {
        if self.attr_count as usize >= MAX_ATTRS {
            return Err(Errno::ENOMEM);
        }
        self.attrs[self.attr_count as usize] = attr;
        self.attr_count += 1;
        Ok(())
    }

    /// 查找属性
    pub fn find_attr(&self, name: &str) -> Option<&SystreeAttr> {
        for i in 0..self.attr_count as usize {
            if self.attrs[i].get_name() == name {
                return Some(&self.attrs[i]);
            }
        }
        None
    }

    /// 查找属性 (可变)
    pub fn find_attr_mut(&mut self, name: &str) -> Option<&mut SystreeAttr> {
        for i in 0..self.attr_count as usize {
            if self.attrs[i].get_name() == name {
                return Some(&mut self.attrs[i]);
            }
        }
        None
    }

    /// 删除属性
    ///
    /// # Errors
    /// 当不存在名为 `name` 的属性时返回 `ENOENT`.
    pub fn delete_attr(&mut self, name: &str) -> Result<(), Errno> {
        let mut found = false;
        for i in 0..self.attr_count as usize {
            if self.attrs[i].get_name() == name {
                found = true;
                for j in i..(self.attr_count as usize - 1) {
                    self.attrs[j] = self.attrs[j + 1];
                }
                self.attr_count -= 1;
                break;
            }
        }
        if found { Ok(()) } else { Err(Errno::ENOENT) }
    }
}

// ============================================================================
// 树
// ============================================================================

/// 系统树
pub struct Systree {
    /// 节点表 (字段级堆化: 构造时按 `MAX_NODES` 预置容量)
    pub nodes: Vec<SystreeNode>,
    /// 节点数量
    pub node_count: u32,
    /// 下一个可用节点 ID
    pub next_id: u32,
}

impl Systree {
    /// 构造系统树, 并预置 `MAX_NODES` 个空节点.
    ///
    /// 在 `get_systree()` 的 `OnceLock` 初始化闭包内调用 (堆已就绪),
    /// 逐元素 collect 构造, 避免 BSS 常驻大数组与栈上大临时对象.
    pub fn new() -> Self {
        Self {
            nodes: (0..MAX_NODES).map(|_| SystreeNode::new(0, 0)).collect(),
            node_count: 0,
            next_id: 1,
        }
    }

    /// 初始化根节点
    pub fn init(&mut self) {
        self.nodes[0] = SystreeNode::new(0, 0);
        self.nodes[0].set_name("/");
        self.node_count = 1;
        self.next_id = 1;
    }

    /// 创建节点
    ///
    /// # Errors
    /// 当父节点不存在时返回 `ENOENT`; 当节点表已满 (`MAX_NODES`) 时返回 `ENOMEM`.
    pub fn create_node(&mut self, parent_id: u32, name: &str) -> Result<u32, Errno> {
        if self.find_node(parent_id).is_none() {
            return Err(Errno::ENOENT);
        }

        if self.node_count as usize >= MAX_NODES {
            return Err(Errno::ENOMEM);
        }

        let id = self.next_id;
        self.next_id += 1;

        let idx = self.node_count as usize;
        self.nodes[idx] = SystreeNode::new(id, parent_id);
        self.nodes[idx].set_name(name);
        self.node_count += 1;

        Ok(id)
    }

    /// 删除节点
    ///
    /// # Errors
    /// 当 `id` 为根节点 (0) 时返回 `EINVAL`; 当节点仍有子节点时返回 `ENOTEMPTY`;
    /// 当节点不存在时返回 `ENOENT`.
    pub fn delete_node(&mut self, id: u32) -> Result<(), Errno> {
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

    /// 查找节点
    pub fn find_node(&self, id: u32) -> Option<&SystreeNode> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].id == id {
                return Some(&self.nodes[i]);
            }
        }
        None
    }

    /// 查找节点 (可变)
    pub fn find_node_mut(&mut self, id: u32) -> Option<&mut SystreeNode> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].id == id {
                return Some(&mut self.nodes[i]);
            }
        }
        None
    }

    /// 按名称查找节点
    pub fn find_node_by_name(&self, parent_id: u32, name: &str) -> Option<&SystreeNode> {
        for i in 0..self.node_count as usize {
            if self.nodes[i].parent_id == parent_id && self.nodes[i].get_name() == name {
                return Some(&self.nodes[i]);
            }
        }
        None
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 列出子节点
    ///
    /// # Errors
    /// 当前实现不返回错误; 当缓冲区不足时输出会被截断.
    pub fn list_children(&self, parent_id: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut offset = 0;
        for i in 0..self.node_count as usize {
            if u64::from(self.nodes[i].parent_id) == parent_id {
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
        Ok(offset)
    }

    /// 读取属性
    ///
    /// # Errors
    /// 当节点或属性不存在时返回 `ENOENT`; 当属性未注册读取回调时返回 `EIO`.
    pub fn read_attr(&self, node_id: u32, attr_name: &str, buf: &mut [u8]) -> Result<usize, Errno> {
        let node = self.find_node(node_id).ok_or(Errno::ENOENT)?;
        let attr = node.find_attr(attr_name).ok_or(Errno::ENOENT)?;
        attr.read(buf)
    }

    /// 写入属性
    ///
    /// # Errors
    /// 当节点或属性不存在时返回 `ENOENT`; 当属性未注册写入回调时返回 `EIO`.
    pub fn write_attr(&mut self, node_id: u32, attr_name: &str, data: &[u8]) -> Result<(), Errno> {
        let node = self.find_node_mut(node_id).ok_or(Errno::ENOENT)?;
        let attr = node.find_attr(attr_name).ok_or(Errno::ENOENT)?;
        attr.write(data)
    }

    /// 解析路径并查找节点
    pub fn resolve_path(&self, path: &str) -> Option<&SystreeNode> {
        if path == "/" || path.is_empty() {
            return Some(&self.nodes[0]);
        }

        let mut current = &self.nodes[0];
        let mut start = 0;
        let bytes = path.as_bytes();

        while start < bytes.len() {
            // 跳过前导 '/'
            while start < bytes.len() && bytes[start] == b'/' {
                start += 1;
            }
            if start >= bytes.len() {
                break;
            }

            // 找到下一个 '/'
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'/' {
                end += 1;
            }

            // 解析当前部分
            let part = core::str::from_utf8(&bytes[start..end]).ok()?;
            let found = self.find_node_by_name(current.id, part)?;
            current = found;

            start = end;
        }

        Some(current)
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局系统树实例
static SYSTREE: OnceLock<Mutex<Systree>> = OnceLock::new();

/// 获取系统树实例
pub fn get_systree() -> &'static Mutex<Systree> {
    SYSTREE.get_or_init(|slot| {
        slot.write(Mutex::new(Systree::new()));
    })
}

// ============================================================================
// 属性读写辅助函数
// ============================================================================

/// 读取整数属性
///
/// # Errors
/// 当 `buf` 长度小于十进制表示长度时返回 `EINVAL`.
pub fn read_int_attr(value: u64, buf: &mut [u8]) -> Result<usize, Errno> {
    let s = format_u64(value);
    // 找到有效数据的结束位置
    let len = s.iter().position(|&b| b == 0).unwrap_or(20);
    if buf.len() < len {
        return Err(Errno::EINVAL);
    }
    buf[..len].copy_from_slice(&s[..len]);
    Ok(len)
}

/// 写入整数属性
///
/// # Errors
/// 当 `data` 不是合法 UTF-8 或无法解析为 `u64` 时返回 `EINVAL`.
pub fn write_int_attr(data: &[u8]) -> Result<u64, Errno> {
    let s = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
    let s = s.trim();
    s.parse::<u64>().map_err(|_| Errno::EINVAL)
}

/// u64 → ASCII 十进制 (无 alloc)
fn format_u64(mut n: u64) -> [u8; 20] {
    if n == 0 {
        return [b'0'; 20];
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    // 左移填充
    let mut out = [b'0'; 20];
    let mut j = 0;
    while i < 20 {
        out[j] = buf[i];
        j += 1;
        i += 1;
    }
    out
}

// ============================================================================
// safe API
// ============================================================================

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 挂载 systree
///
/// # Errors
/// 当前实现恒返回 `Ok(())`; 仅初始化根节点, 不返回错误.
pub fn mount_systree() -> Result<(), Errno> {
    let tree = get_systree();
    tree.lock().init();
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 卸载 systree
///
/// # Errors
/// 当前实现恒返回 `Ok(())`; 仅清空节点数据, 不返回错误.
pub fn umount_systree() -> Result<(), Errno> {
    let mut tree = get_systree().lock();
    tree.node_count = 0;
    tree.next_id = 1;
    Ok(())
}

/// 创建节点
///
/// # Errors
/// 错误条件与 [`Systree::create_node`] 相同, 参见其 `# Errors` 段.
pub fn create_node(parent_id: u32, name: &str) -> Result<u32, Errno> {
    get_systree().lock().create_node(parent_id, name)
}

/// 删除节点
///
/// # Errors
/// 错误条件与 [`Systree::delete_node`] 相同, 参见其 `# Errors` 段.
pub fn delete_node(id: u32) -> Result<(), Errno> {
    get_systree().lock().delete_node(id)
}

/// 查找节点
pub fn find_node(id: u32) -> Option<SystreeNode> {
    get_systree().lock().find_node(id).copied()
}

/// 按路径查找节点
pub fn resolve_path(path: &str) -> Option<SystreeNode> {
    get_systree().lock().resolve_path(path).copied()
}

/// 读取属性
///
/// # Errors
/// 错误条件与 [`Systree::read_attr`] 相同, 参见其 `# Errors` 段.
pub fn read_attr(node_id: u32, attr_name: &str, buf: &mut [u8]) -> Result<usize, Errno> {
    get_systree().lock().read_attr(node_id, attr_name, buf)
}

/// 写入属性
///
/// # Errors
/// 错误条件与 [`Systree::write_attr`] 相同, 参见其 `# Errors` 段.
pub fn write_attr(node_id: u32, attr_name: &str, data: &[u8]) -> Result<(), Errno> {
    get_systree().lock().write_attr(node_id, attr_name, data)
}

/// 处理请求
///
/// # Errors
/// 当数据非 UTF-8 或操作数非法时返回 `EINVAL`;
/// 当查找/读取/写入的对象不存在时返回 `ENOENT`;
/// 当 `opcode` 不被支持时返回 `ENOSYS`.
pub fn handle_request(
    opcode: u32,
    node_id: u64,
    data: &[u8],
    buf: &mut [u8],
) -> Result<usize, Errno> {
    let tree = get_systree();
    let mut tree = tree.lock();

    match opcode {
        1 => {
            // Lookup
            let name = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
            let node = tree.find_node_by_name(node_id as u32, name);
            if let Some(n) = node {
                if buf.len() < 4 {
                    return Err(Errno::EINVAL);
                }
                buf[..4].copy_from_slice(&n.id.to_le_bytes());
                Ok(4)
            } else {
                Err(Errno::ENOENT)
            }
        }
        27 => {
            // Readdir
            tree.list_children(node_id, buf)
        }
        19 => {
            // Read attr
            let name = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
            tree.read_attr(node_id as u32, name, buf)
        }
        20 => {
            // Write attr
            let name = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
            tree.write_attr(node_id as u32, name, data)?;
            Ok(0)
        }
        _ => Err(Errno::ENOSYS),
    }
}
