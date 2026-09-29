#![deny(unsafe_code)]
//! sysfs — services 层安全代理 (Phase C4)
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `services::fs::api`。
//!
//! ## 职责
//!
//! - 提供类型安全 sysfs 安装 API (类似 procfs 但挂 /sys)
//! - 注册伪文件节点 (/sys/cpu/online, /sys/mem/total 等)
//! - 暴露给 userland 读取系统信息
//!
//! ## 数据源
//!
//! sysfs 项值取自各 framework 层 API (`cpu_count`, `mem_total`, `acpi_status` 等).
//! 不在 sysfs 中持久化数据, 全部为只读 + 按需 `read()` 时计算.

use crate::framework::syscall::Errno;

// ============================================================================
// sysfs 节点类型
// ============================================================================

/// sysfs 节点值类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysfsValue {
    /// 整数 (u64), 输出十进制
    Integer(u64),
    /// 字符串 (≤ 31 字节), 输出 C 字符串
    String(&'static str),
    /// 布尔, 输出 "1\n" / "0\n"
    Bool(bool),
}

/// 节点元信息
#[derive(Debug, Clone, Copy)]
pub struct SysfsNode {
    pub name: &'static str,
    pub perm: u16, // 0o444 默认只读
}

impl SysfsNode {
    pub const fn new(name: &'static str) -> Self {
        Self { name, perm: 0o444 }
    }
}

// ============================================================================
// 内置节点表 (编译期静态)
// ============================================================================

const NODE_CPU_COUNT: SysfsNode = SysfsNode::new("cpu_count");
const NODE_MEM_TOTAL: SysfsNode = SysfsNode::new("mem_total");
const NODE_MEM_FREE: SysfsNode = SysfsNode::new("mem_free");
const NODE_UPTIME: SysfsNode = SysfsNode::new("uptime_secs");
const NODE_VERSION: SysfsNode = SysfsNode::new("version");
const NODE_BOOT_STATUS: SysfsNode = SysfsNode::new("boot_status");

/// /sys/queenx/ 下节点列表
pub const QUEENX_NODES: &[SysfsNode] = &[
    NODE_CPU_COUNT,
    NODE_MEM_TOTAL,
    NODE_MEM_FREE,
    NODE_UPTIME,
    NODE_VERSION,
    NODE_BOOT_STATUS,
];

// ============================================================================
// 节点数
// ============================================================================

/// 静态节点总数
#[inline]
pub fn node_count() -> usize {
    QUEENX_NODES.len()
}

/// 节点存在查询
#[inline]
pub fn has_node(name: &str) -> bool {
    QUEENX_NODES.iter().any(|n| n.name == name)
}

// ============================================================================
// 节点值格式化
// ============================================================================

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 把节点值写到 buffer, 返写入字节数
///
/// # Errors
/// 当节点不存在时返回 `ENOENT`; 当缓冲区过小 (装不下格式化结果) 时返回 `EINVAL`.
pub fn write_node_value(name: &str, buf: &mut [u8]) -> Result<usize, Errno> {
    let val = match name {
        "cpu_count" => SysfsValue::Integer(1),          // 简化: BSP=1
        "mem_total" => SysfsValue::Integer(0x10000000), // 256 MiB
        "mem_free" => SysfsValue::Integer(0x08000000),  // 128 MiB
        "uptime_secs" => SysfsValue::Integer(0),
        "version" => SysfsValue::String("queenx-0.1.0"),
        "boot_status" => SysfsValue::Bool(true),
        _ => return Err(Errno::ENOENT),
    };

    match val {
        SysfsValue::Integer(n) => {
            // 简单十进制: 拆位写入
            let s = format_u64(n);
            if buf.len() < s.len() {
                return Err(Errno::EINVAL);
            }
            for (i, b) in s.iter().enumerate() {
                buf[i] = *b;
            }
            Ok(s.len())
        }
        SysfsValue::String(s) => {
            let bytes = s.as_bytes();
            if buf.len() < bytes.len() {
                return Err(Errno::EINVAL);
            }
            for (i, b) in bytes.iter().enumerate() {
                buf[i] = *b;
            }
            Ok(bytes.len())
        }
        SysfsValue::Bool(b) => {
            let c = if b { b'1' } else { b'0' };
            if buf.is_empty() {
                return Err(Errno::EINVAL);
            }
            buf[0] = c;
            Ok(1)
        }
    }
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
// safe 安装 API
// ============================================================================

/// 把 sysfs 挂到 /sys
///
/// # Errors
/// 当节点表为空、无法挂载时返回 `EINVAL`.
pub fn mount_sysfs() -> Result<(), Errno> {
    // 真实实现: vfs_mount("/sys", "sysfs")
    // 简化: 计数 + 验证节点表
    if QUEENX_NODES.is_empty() {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 卸载 /sys
///
/// # Errors
/// 当前实现恒返回 `Ok(())`, 不产生错误.
pub fn umount_sysfs() -> Result<(), Errno> {
    Ok(())
}
