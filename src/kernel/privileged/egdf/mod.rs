//! EGDF设备驱动框架 (EGDF Driver Framework) — **API 层**
//!
//! `Edgine` 的统一设备驱动模型 — 唯一的注册/发现/初始化/I/O 入口。
//! 本文件是 `Edgine` 的 **API 层**。
//!
//! ## 调用方契约
//! - `driver::infra::Driver` —— 驱动实现者
//! - `fs::unkfs` —— 通过 `proto_block` 读块设备
//! - `fs::ramfs/devfs` —— 通过 `egdf_register_driver` 暴露设备节点
//! - `net::e1000/virtio` —— 通过 `proto_net` 注册网卡
//! - `proc::session` —— 通过 `user_driver` 暴露用户态驱动接口
//! - `host-tests` —— host 端设备测试桩
//!
//! ## 内部接口
//! - `proto_block/char/input/net` —— 协议族,函数指针表(类 Linux `struct ops`)
//! - `EGDF_DEVICES` —— 全局注册表,spinlock 保护
//!
//! ## 安全约束
//! - `egdf_register_*` 必须在启动早期单线程上下文调用
//! - `egdf_blk_read/write` 等 IO 路径可在中断上下文调用,但调用方负责
//!   buffer 生命周期
//!
//! ## 性能特征
//! - 注册表: `Vec<EGDFDevice>` + spinlock,O(N) 查找(N ≤ 64)
//! - 协议调用: 静态分发,无 vtable 开销
//!
//! - **`EGDFProto`**: 设备协议分类 (Block/Char/Net/Input/Bus/Other)
//! - **`EGDFOps`**: 协议级 I/O 操作表 (函数指针, 零开销)
//! - **`EGDFDevice`**: 统一设备描述符 (含 I/O 能力)
//! - **`BlockDevice` trait**: 块设备统一接口 (推荐路径)
//! - **Driver trait**: 驱动运行时行为的接口契约 (`init/shutdown/is_ready`)
//! - **全局注册表**: `EGDF_DEVICES` 统一管理所有设备
//!
//! ## 架构
//!
//! ```text
//! egdf_register_driver("vga", EGDFProto::Char, None, None, vga_driver_box)
//!   ├── 创建 EGDFDevice 并存入 EGDF_DEVICES
//!   ├── 调用 Driver::init() → 设置 state = Ready
//!   └── 返回 device id
//!
//! egdf_register_block_dev("ata0", None, None, &mut block_dev)
//!   ├── 创建 EGDFDevice + block_dev trait 引用
//!   ├── UNKFS 通过 egdf_blk_read/write → BlockDevice::blk_read/write
//!   └── 0 unsafe
//!
//! egdf_blk_read(drive_idx, sector, buf)
//!   └── EGDF_DEVICES[drive].block_dev.blk_read(...)
//! ```

use super::driver::Driver;
use crate::privileged::error::{KernelError, KernelResult};
use crate::privileged::sync::IrqSpinLock as Mutex;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

// ── 协议模块 ──
pub mod composite;
pub mod devtree;
pub mod proto_block;

// devtree 公共接口 re-export — 避免跨子系统直接访问 egdf::devtree 内部
pub use devtree::*;
pub mod proto_char;
pub mod proto_input;
pub mod proto_net;
pub mod user_driver;

// composite 公共接口 re-export — 避免跨子系统直接访问 egdf::composite 内部
pub use composite::devtree_probe_composites;

// firmware 公共接口 re-export — 避免跨子系统直接访问 egdf::firmware 内部
pub use firmware::*;

// proto_net 公共接口 re-export — 避免跨子系统直接访问 egdf::proto_net 内部
pub use proto_net::NetOps;

// proto_char/proto_input 公共接口 re-export — 避免跨子系统直接访问 egdf::proto_char/proto_input 内部
pub use proto_char::CharOps;
pub use proto_input::InputOps;

// proto_block 公共接口 re-export
pub use proto_block::register_block_device;
pub mod firmware;

// ── BlockDevice Trait (设备框架层定义, driver::block re-export) ──

/// 块设备统一接口, 由所有存储驱动实现.
///
/// 定义在 egdf (设备框架) 而非 driver (具体驱动), 因为:
/// - egdf 是设备框架, 负责定义设备协议
/// - driver 是具体驱动实现, 实现 egdf 定义的 trait
/// - 消除 egdf→driver 循环依赖
pub trait BlockDevice: Send + Sync {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32;
    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32;
    fn blk_is_present(&self) -> bool;
    fn blk_total_sectors(&self) -> u64;
}

// ── 协议类型 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EGDFProto {
    Block,
    Char,
    Net,
    Input,
    Bus,
    Other,
}

impl EGDFProto {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Char => "char",
            Self::Net => "net",
            Self::Input => "input",
            Self::Bus => "bus",
            Self::Other => "other",
        }
    }
}

impl From<super::driver::DeviceType> for EGDFProto {
    fn from(dt: super::driver::DeviceType) -> Self {
        match dt {
            super::driver::DeviceType::Block => Self::Block,
            super::driver::DeviceType::Char => Self::Char,
            super::driver::DeviceType::Network => Self::Net,
            super::driver::DeviceType::Input => Self::Input,
            super::driver::DeviceType::Bus => Self::Bus,
            super::driver::DeviceType::Other => Self::Other,
        }
    }
}

// ── 设备状态 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Uninit,
    Probing,
    Ready,
    Failed,
    Removed,
}

// ── I/O 操作表 ──

/// 协议级 I/O 操作表 (联合体, 按协议类型取对应变体)
pub enum EGDFOps {
    Char(&'static proto_char::CharOps),
    Net(&'static proto_net::NetOps),
    Input(&'static proto_input::InputOps),
}

// ── EGDFDevice ──

/// EGDF设备描述符
///
/// 每个设备在注册表中占用一个条目。`driver_data` 指向内核堆上
/// 的实际驱动结构体, `ops` 提供协议级 I/O 操作表 (Char/Net/Input).
pub struct EGDFDevice {
    pub id: u32,
    pub name: &'static str,
    pub proto: EGDFProto,
    pub state: DeviceState,
    pub io_base: Option<u64>,
    pub irq: Option<u8>,
    pub driver_data: *mut u8,
    /// 驱动所有权标记: `true` 表示 `driver_data` 指向 EGDF 自有的 `DriverObject`
    /// (经 `egdf_register_driver` / `egdf_register_driver_with_ops` 注册),
    /// 由 EGDF 负责 `init`/`shutdown`/`drop`; `false` 表示 `driver_data` 为调用方
    /// 自持的裸驱动指针 (经 `egdf_register` / `egdf_register_with_ops` 注册),
    /// EGDF 不管理其生命周期 (避免 `driver_from_obj` 类型混淆)。
    pub driver_owned: bool,
    pub ops: Option<EGDFOps>,
    /// 块设备 trait 引用
    /// 当 `proto == EGDFProto::Block` 且驱动通过 `register_block_device` 注册时,
    /// `egdf_blk_read/write` 走此字段 (0 unsafe)
    ///
    /// # Mutability
    ///
    /// `&'static mut` 是因为 `BlockDevice::blk_read/blk_write` 需要 `&mut self`.
    /// 通过 `&mut EGDFDevice` 借用得到 (`EGDF_DEVICES` 锁保护),
    /// 不存在数据竞争 (持有锁时此引用唯一).
    pub block_dev: Option<&'static mut dyn BlockDevice>,
}

// SAFETY: EGDFDevice 含一个原始指针 (`driver_data`), 其不
// 拥有内存 —— 内存由驱动方自行管理. 其它字段均为 Copy 类型或
// SAFETY: EGDFDevice 含裸指针, 但通过 Option 包装, 写操作由设备树锁保护.
//         该指针仅在驱动自身锁内解引用.
unsafe impl Send for EGDFDevice {}
// SAFETY: 同上, 设备树锁保护并发访问.
unsafe impl Sync for EGDFDevice {}

impl EGDFDevice {
    #[inline]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    ///
    /// # Safety
    ///
    /// `self.driver_data` 由驱动在 probe 阶段设置. 调用方必须保证
    /// `T` 与 `set_driver_data` 写入时的具体类型一致.
    pub unsafe fn driver_as_mut<T>(&self) -> &mut T {
        unsafe { &mut *(self.driver_data as *mut T) }
    }

    #[inline]
    ///
    /// # Safety
    ///
    /// `self.driver_data` 由驱动在 probe 阶段设置. 调用方必须保证
    /// `T` 与 `set_driver_data` 写入时的具体类型一致.
    pub unsafe fn driver_as_ref<T>(&self) -> &T {
        unsafe { &*(self.driver_data as *const T) }
    }

    pub fn char_ops(&self) -> Option<&'static proto_char::CharOps> {
        match &self.ops {
            Some(EGDFOps::Char(ops)) => Some(ops),
            _ => None,
        }
    }

    pub fn net_ops(&self) -> Option<&'static proto_net::NetOps> {
        match &self.ops {
            Some(EGDFOps::Net(ops)) => Some(ops),
            _ => None,
        }
    }

    pub fn input_ops(&self) -> Option<&'static proto_input::InputOps> {
        match &self.ops {
            Some(EGDFOps::Input(ops)) => Some(ops),
            _ => None,
        }
    }
}

// ── 全局注册表 ──

static NEXT_DEVICE_ID: AtomicU32 = AtomicU32::new(1);

/// 设备注册回调 — 可由 `DevFS` 订阅, 自动创建设备节点
/// E6-9b: EGDF→DevFS 桥接
static DEVICE_REGISTER_CALLBACK: Mutex<Option<fn(&EGDFDevice)>> = Mutex::new(None);

/// 设置设备注册回调 (由 `DevFS` 初始化时调用)
pub fn egdf_set_register_callback(cb: fn(&EGDFDevice)) {
    *DEVICE_REGISTER_CALLBACK.lock() = Some(cb);
}

/// 通知 `DevFS` 新设备已注册 (E6-9b)
///
/// 从 `EGDF_DEVICES` 中获取最后注册的设备并调用回调。
/// 这样避免 `EGDFDevice` 的 move 问题。
fn notify_last_registered() {
    let cb = DEVICE_REGISTER_CALLBACK.lock();
    if let Some(f) = *cb {
        let devices = EGDF_DEVICES.lock();
        if let Some(dev) = devices.last() {
            f(dev);
        }
    }
}

pub static EGDF_DEVICES: Mutex<Vec<EGDFDevice>> = Mutex::new(Vec::new());

// ── 注册函数 ──

pub fn egdf_register(
    name: &'static str,
    proto: EGDFProto,
    io_base: Option<u64>,
    irq: Option<u8>,
    driver_data: *mut u8,
) -> u32 {
    let id = NEXT_DEVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dev = EGDFDevice {
        id,
        name,
        proto,
        state: DeviceState::Ready,
        io_base,
        irq,
        driver_data,
        driver_owned: false,
        ops: None,
        block_dev: None,
    };
    {
        let mut devices = EGDF_DEVICES.lock();
        devices.push(dev);
    }
    // E6-9b: 通知 DevFS 创建设备节点 (从注册表获取引用)
    notify_last_registered();
    id
}

/// 注册带 I/O 操作表的设备
///
/// 这是推荐的注册方式, 使设备同时具备发现和 I/O 能力。
pub fn egdf_register_with_ops(
    name: &'static str,
    proto: EGDFProto,
    io_base: Option<u64>,
    irq: Option<u8>,
    driver_data: *mut u8,
    ops: EGDFOps,
) -> u32 {
    let id = NEXT_DEVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dev = EGDFDevice {
        id,
        name,
        proto,
        state: DeviceState::Ready,
        io_base,
        irq,
        driver_data,
        driver_owned: false,
        ops: Some(ops),
        block_dev: None,
    };
    {
        let mut devices = EGDF_DEVICES.lock();
        devices.push(dev);
    }
    notify_last_registered();
    id
}

/// 注册块设备 (直接传 `&'static mut dyn BlockDevice`)
///
/// 优势:
/// - 0 unsafe (`egdf_blk_read/write` 直接 trait dispatch)
/// - 类型安全 (驱动方必须实现 `BlockDevice` trait, 编译期检查)
///
/// # 生命周期
///
/// `dev` 必须是 `'static` (即 `Box::leak` 或静态分配), `EGDFDevice` 持有
/// `&'static mut dyn BlockDevice` 引用. 设备移除时 EGDF 负责清理.
///
/// # Mutability 原因
///
/// `BlockDevice::blk_read/blk_write` 需要 `&mut self`, 故用 `&'static mut` 而非 `&'static`.
/// 通过 `EGDF_DEVICES` 锁保护, 不存在数据竞争.
pub fn egdf_register_block_dev(
    name: &'static str,
    io_base: Option<u64>,
    irq: Option<u8>,
    dev: &'static mut dyn BlockDevice,
) -> u32 {
    let id = NEXT_DEVICE_ID.fetch_add(1, Ordering::Relaxed);
    let egdf_dev = EGDFDevice {
        id,
        name,
        proto: EGDFProto::Block,
        state: DeviceState::Ready,
        io_base,
        irq,
        driver_data: core::ptr::null_mut(), // 不再使用, 改用 block_dev
        driver_owned: false,
        ops: None, // 不再使用, 改用 block_dev
        block_dev: Some(dev),
    };
    let idx;
    {
        let mut devices = EGDF_DEVICES.lock();
        idx = devices.len() as u32;
        devices.push(egdf_dev);
    }
    notify_last_registered();
    idx
}

// ── 查找函数 ──

pub fn egdf_find_by_id(id: u32) -> Option<usize> {
    EGDF_DEVICES.lock().iter().position(|d| d.id == id)
}

pub fn egdf_find_by_name(name: &str) -> Option<usize> {
    EGDF_DEVICES.lock().iter().position(|d| d.name == name)
}

pub fn egdf_find_by_proto(proto: EGDFProto) -> Option<usize> {
    EGDF_DEVICES.lock().iter().position(|d| d.proto == proto)
}

pub fn egdf_list() -> Vec<(u32, &'static str, EGDFProto, DeviceState)> {
    EGDF_DEVICES
        .lock()
        .iter()
        .map(|d| (d.id, d.name, d.proto, d.state))
        .collect()
}

pub fn egdf_count() -> usize {
    EGDF_DEVICES.lock().len()
}

pub fn egdf_count_by_proto(proto: EGDFProto) -> usize {
    EGDF_DEVICES
        .lock()
        .iter()
        .filter(|d| d.proto == proto)
        .count()
}

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// 查找第一个网络设备, 返回其 `NetOps` + `driver_data` + MAC 地址
///
/// 供 `smoltcp_impl` 使用——协议栈不关心具体驱动类型, 只需要
/// 四个函数指针 (`send/recv/get_mac/irq`) 和 `driver_data`。
pub fn egdf_find_net_device() -> Option<(
    &'static crate::privileged::egdf::proto_net::NetOps,
    *mut u8,
    [u8; 6],
)> {
    let devices = EGDF_DEVICES.lock();
    for dev in devices.iter() {
        if dev.proto != EGDFProto::Net {
            continue;
        }
        if dev.state != DeviceState::Ready {
            continue;
        }
        match &dev.ops {
            Some(EGDFOps::Net(net_ops)) => {
                let mut mac = [0u8; 6];
                net_ops.get_mac(dev.driver_data, &mut mac);
                return Some((net_ops, dev.driver_data, mac));
            }
            _ => continue,
        }
    }
    None
}

pub fn egdf_with_device<F>(id: u32, f: F)
where
    F: FnOnce(&mut EGDFDevice),
{
    let mut devices = EGDF_DEVICES.lock();
    if let Some(dev) = devices.iter_mut().find(|d| d.id == id) {
        f(dev);
    }
}

pub fn egdf_with_device_map<T, F>(id: u32, f: F) -> Option<T>
where
    F: FnOnce(&mut EGDFDevice) -> T,
{
    let mut devices = EGDF_DEVICES.lock();
    devices.iter_mut().find(|d| d.id == id).map(f)
}

pub fn egdf_unregister(id: u32) -> Option<*mut u8> {
    let mut devices = EGDF_DEVICES.lock();
    let pos = devices.iter().position(|d| d.id == id)?;
    let dev = devices.remove(pos);
    Some(dev.driver_data)
}

/// 注销块设备 (墓碑语义, 热插拔移除路径专用)。
///
/// `drive` 是设备在 `EGDF_DEVICES` 中的索引。热插拔移除时**不物理删除**
/// 条目, 而是将其标记为 `DeviceState::Removed` 并释放 `block_dev` 引用:
/// 物理删除会使后续设备的索引整体前移, 导致已下发到上层 (如 UNKFS vdev
/// 映射) 的 `drive` 句柄指向错误设备。墓碑保持索引稳定, 后续 I/O 因状态
/// 非 `Ready` 而安全失败 (`egdf_blk_read/write` 返回 `Busy`)。
///
/// 返回 `true` 表示该索引处存在块设备且已成功标记移除; 索引越界 / 非块
/// 设备 / 已移除时返回 `false`。
pub fn egdf_unregister_block(drive: u8) -> bool {
    let mut devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return false;
    }
    let dev = &mut devices[idx];
    if dev.proto != EGDFProto::Block || dev.state == DeviceState::Removed {
        return false;
    }
    dev.state = DeviceState::Removed;
    dev.block_dev = None;
    true
}

pub fn egdf_set_state(id: u32, state: DeviceState) {
    let mut devices = EGDF_DEVICES.lock();
    if let Some(dev) = devices.iter_mut().find(|d| d.id == id) {
        dev.state = state;
    }
}

// ── 块设备 I/O (统一入口, 替代 block::hdd_*) ──

/// 通过 EGDF 读取块设备扇区
///
/// `drive` 是设备在 `EGDF_DEVICES` 中的索引。
/// 仅对 `EGDFProto::Block` 且携带 `block_dev` 的设备有效。
///
/// 返回值遵循 POSIX 约定: `0` = 成功, `-errno` = 失败 (与 `privileged::error::KernelError` 对齐)。
pub fn egdf_blk_read(drive: u8, sector: u64, buf: &mut [u8]) -> i32 {
    if buf.len() < 512 {
        return KernelError::InvalidArgument.as_i32();
    }
    let mut devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return KernelError::Io.as_i32();
    }
    let dev = &mut devices[idx];
    if dev.proto != EGDFProto::Block {
        return KernelError::Io.as_i32();
    }
    if dev.state != DeviceState::Ready {
        return KernelError::Busy.as_i32();
    }
    if let Some(bd) = dev.block_dev.as_mut() {
        return bd.blk_read(sector, buf);
    }
    KernelError::NotSupported.as_i32()
}

/// 通过 EGDF 写入块设备扇区
pub fn egdf_blk_write(drive: u8, sector: u64, buf: &[u8]) -> i32 {
    if buf.len() < 512 {
        return KernelError::InvalidArgument.as_i32();
    }
    let mut devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return KernelError::Io.as_i32();
    }
    let dev = &mut devices[idx];
    if dev.proto != EGDFProto::Block {
        return KernelError::Io.as_i32();
    }
    if dev.state != DeviceState::Ready {
        return KernelError::Busy.as_i32();
    }
    if let Some(bd) = dev.block_dev.as_mut() {
        return bd.blk_write(sector, buf);
    }
    KernelError::NotSupported.as_i32()
}

/// 通过 EGDF 连续读取多个扇区 (每扇区 512 字节)。
///
/// 与逐扇区调用 [`egdf_blk_read`] 不同, 本函数在**单次** `EGDF_DEVICES`
/// 锁持有期内完成整段读取: 既避免重复加锁, 也避免读取途中设备被墓碑化
/// 而留下半段成功的数据。上层文件系统 (ext2/exFAT) 的元数据读取依赖该语义。
///
/// # Errors
/// `buf` 长度不足以容纳 `count` 个扇区时返回 `InvalidArgument`;
/// `drive` 越界 / 非块设备 / 缺少块设备实现时返回 `Io`;
/// 设备处于非 `Ready` 状态 (已墓碑化) 时返回 `Busy`;
/// 底层设备任一扇区读取失败时返回 `Io`。
pub fn egdf_blk_read_sectors(
    drive: u8,
    start: u64,
    count: u32,
    buf: &mut [u8],
) -> KernelResult<()> {
    if (buf.len() as u64) < u64::from(count) * 512 {
        return Err(KernelError::InvalidArgument);
    }
    let mut devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return Err(KernelError::Io);
    }
    let dev = &mut devices[idx];
    if dev.proto != EGDFProto::Block {
        return Err(KernelError::Io);
    }
    if dev.state != DeviceState::Ready {
        return Err(KernelError::Busy);
    }
    let Some(bd) = dev.block_dev.as_mut() else {
        return Err(KernelError::Io);
    };
    for i in 0..count {
        let offset = i as usize * 512;
        if bd.blk_read(start + u64::from(i), &mut buf[offset..offset + 512]) < 0 {
            return Err(KernelError::Io);
        }
    }
    Ok(())
}

/// 通过 EGDF 连续写入多个扇区 (每扇区 512 字节)。
///
/// 语义与 [`egdf_blk_read_sectors`] 对称: 单次持锁完成整段写入。
///
/// # Errors
/// `buf` 长度不足以提供 `count` 个扇区时返回 `InvalidArgument`;
/// `drive` 越界 / 非块设备 / 缺少块设备实现时返回 `Io`;
/// 设备处于非 `Ready` 状态 (已墓碑化) 时返回 `Busy`;
/// 底层设备任一扇区写入失败时返回 `Io`。
pub fn egdf_blk_write_sectors(drive: u8, start: u64, count: u32, buf: &[u8]) -> KernelResult<()> {
    if (buf.len() as u64) < u64::from(count) * 512 {
        return Err(KernelError::InvalidArgument);
    }
    let mut devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return Err(KernelError::Io);
    }
    let dev = &mut devices[idx];
    if dev.proto != EGDFProto::Block {
        return Err(KernelError::Io);
    }
    if dev.state != DeviceState::Ready {
        return Err(KernelError::Busy);
    }
    let Some(bd) = dev.block_dev.as_mut() else {
        return Err(KernelError::Io);
    };
    for i in 0..count {
        let offset = i as usize * 512;
        if bd.blk_write(start + u64::from(i), &buf[offset..offset + 512]) < 0 {
            return Err(KernelError::Io);
        }
    }
    Ok(())
}

/// 通过 EGDF 检查块设备是否存在
pub fn egdf_blk_is_present(drive: u8) -> bool {
    let devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return false;
    }
    let dev = &devices[idx];
    if dev.proto != EGDFProto::Block {
        return false;
    }
    if dev.state != DeviceState::Ready {
        return false;
    }
    if let Some(bd) = dev.block_dev.as_ref() {
        return bd.blk_is_present();
    }
    false
}

/// 通过 EGDF 获取块设备总扇区数
pub fn egdf_blk_total_sectors(drive: u8) -> u64 {
    let devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return 0;
    }
    let dev = &devices[idx];
    if dev.proto != EGDFProto::Block {
        return 0;
    }
    if let Some(bd) = dev.block_dev.as_ref() {
        return bd.blk_total_sectors();
    }
    0
}

/// 获取块设备名称
pub fn egdf_blk_name(drive: u8) -> Option<&'static str> {
    let devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return None;
    }
    if devices[idx].proto != EGDFProto::Block {
        return None;
    }
    Some(devices[idx].name)
}

/// 获取块设备综合信息 (name, `is_present`, `total_sectors`)
pub fn egdf_blk_info(drive: u8) -> (&'static str, bool, u64) {
    let name = egdf_blk_name(drive).unwrap_or("unknown");
    let present = egdf_blk_is_present(drive);
    let sectors = egdf_blk_total_sectors(drive);
    (name, present, sectors)
}

/// 获取块设备数量
pub fn egdf_blk_count() -> usize {
    egdf_count_by_proto(EGDFProto::Block)
}

/// 枚举所有块设备在 `EGDF_DEVICES` 中的索引 (含已墓碑化的设备)。
///
/// 用于向热插拔状态上报逐设备状态: `drive` 必须与 `egdf_blk_read/write`
/// 使用的索引一致, 因此按全局下标返回而非按块设备重新编号。
pub fn egdf_blk_drives() -> Vec<u8> {
    EGDF_DEVICES
        .lock()
        .iter()
        .enumerate()
        .filter(|(_, d)| d.proto == EGDFProto::Block)
        .map(|(i, _)| i as u8)
        .collect()
}

/// 查询块设备是否已被墓碑化移除。
pub fn egdf_blk_is_removed(drive: u8) -> bool {
    let devices = EGDF_DEVICES.lock();
    let idx = drive as usize;
    if idx >= devices.len() {
        return false;
    }
    let dev = &devices[idx];
    dev.proto == EGDFProto::Block && dev.state == DeviceState::Removed
}

// ── 字符设备 I/O (统一入口) ──

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// 通过 EGDF 向第一个就绪字符设备写入字节
///
/// 遍历 `EGDF_DEVICES` 查找第一个 proto=Char+Ready 且携带 `CharOps` 的设备,
/// 通过其 write 函数指针输出数据。用于内核日志串口输出等。
pub fn egdf_char_write(data: &[u8]) {
    let devices = EGDF_DEVICES.lock();
    for dev in devices.iter() {
        if dev.proto != EGDFProto::Char {
            continue;
        }
        if dev.state != DeviceState::Ready {
            continue;
        }
        match dev.char_ops() {
            Some(ops) => {
                ops.write(dev.driver_data, data);
                return;
            }
            None => continue,
        }
    }
}

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// 通过 EGDF 从第一个就绪字符设备读取字节
pub fn egdf_char_read(buf: &mut [u8]) -> usize {
    let devices = EGDF_DEVICES.lock();
    for dev in devices.iter() {
        if dev.proto != EGDFProto::Char {
            continue;
        }
        if dev.state != DeviceState::Ready {
            continue;
        }
        match dev.char_ops() {
            Some(ops) => return ops.read(dev.driver_data, buf),
            None => continue,
        }
    }
    0
}

// ── 输入设备 I/O (统一入口) ──

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// 通过 EGDF 从第一个就绪输入设备读取一个字符
pub fn egdf_input_read() -> Option<u8> {
    let devices = EGDF_DEVICES.lock();
    for dev in devices.iter() {
        if dev.proto != EGDFProto::Input {
            continue;
        }
        if dev.state != DeviceState::Ready {
            continue;
        }
        match dev.input_ops() {
            Some(ops) => return ops.read_char(dev.driver_data),
            None => continue,
        }
    }
    None
}

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// 通过 EGDF 检查第一个就绪输入设备是否有数据
pub fn egdf_input_has_data() -> bool {
    let devices = EGDF_DEVICES.lock();
    for dev in devices.iter() {
        if dev.proto != EGDFProto::Input {
            continue;
        }
        if dev.state != DeviceState::Ready {
            continue;
        }
        match dev.input_ops() {
            Some(ops) => return ops.has_char(dev.driver_data),
            None => continue,
        }
    }
    false
}

// ── 工具 ──

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub fn box_to_raw<T: ?Sized>(b: Box<T>) -> *mut u8 {
    Box::into_raw(b) as *mut u8
}

// ── Driver trait 集成 ──

struct DriverObject {
    ptr: *mut dyn Driver,
}

impl Drop for DriverObject {
    fn drop(&mut self) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            drop(Box::from_raw(self.ptr));
        }
    }
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub fn egdf_register_driver(
    name: &'static str,
    proto: EGDFProto,
    io_base: Option<u64>,
    irq: Option<u8>,
    mut driver: Box<dyn Driver>,
) -> u32 {
    let _ = driver.init();
    let raw = Box::into_raw(driver);
    let obj = Box::new(DriverObject { ptr: raw });
    let obj_ptr = Box::into_raw(obj) as *mut u8;
    let id = NEXT_DEVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dev = EGDFDevice {
        id,
        name,
        proto,
        state: DeviceState::Ready,
        io_base,
        irq,
        driver_data: obj_ptr,
        driver_owned: true,
        ops: None,
        block_dev: None,
    };
    {
        let mut devices = EGDF_DEVICES.lock();
        devices.push(dev);
    }
    notify_last_registered();
    id
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
/// 注册带 Driver trait 和 I/O 操作表的设备
pub fn egdf_register_driver_with_ops(
    name: &'static str,
    proto: EGDFProto,
    io_base: Option<u64>,
    irq: Option<u8>,
    mut driver: Box<dyn Driver>,
    ops: EGDFOps,
) -> u32 {
    let _ = driver.init();
    let raw = Box::into_raw(driver);
    let obj = Box::new(DriverObject { ptr: raw });
    let obj_ptr = Box::into_raw(obj) as *mut u8;
    let id = NEXT_DEVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dev = EGDFDevice {
        id,
        name,
        proto,
        state: DeviceState::Ready,
        io_base,
        irq,
        driver_data: obj_ptr,
        driver_owned: true,
        ops: Some(ops),
        block_dev: None,
    };
    {
        let mut devices = EGDF_DEVICES.lock();
        devices.push(dev);
    }
    notify_last_registered();
    id
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
fn driver_from_obj<'a>(ptr: *mut u8) -> &'a mut dyn Driver {
    // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
    let obj: &mut DriverObject = unsafe { &mut *(ptr as *mut DriverObject) };
    // SAFETY: `obj` 由调用方保证为有效指针; 只读访问
    unsafe { &mut *obj.ptr }
}

pub fn egdf_init_all() {
    // 注册进程退出清理回调, 解耦 proc→egdf 依赖
    // SAFETY: egdf_process_cleanup 是 'static 函数指针, 在内核运行期间始终有效.
    unsafe {
        crate::privileged::process_cleanup::register_process_cleanup(
            crate::privileged::egdf::user_driver::egdf_process_cleanup,
        );
    }

    let mut devices = EGDF_DEVICES.lock();
    for dev in devices.iter_mut() {
        if dev.state == DeviceState::Uninit && dev.driver_owned && !dev.driver_data.is_null() {
            let driver = driver_from_obj(dev.driver_data);
            let _ = driver.init();
            dev.state = DeviceState::Ready;
        }
    }
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
pub fn egdf_shutdown_all() {
    let mut devices = EGDF_DEVICES.lock();
    for dev in devices.iter_mut() {
        if dev.state == DeviceState::Ready && dev.driver_owned && !dev.driver_data.is_null() {
            {
                let driver = driver_from_obj(dev.driver_data);
                let _ = driver.shutdown();
            }
            dev.state = DeviceState::Failed;
            // SAFETY: `dev.driver_owned` 为真, 保证 `driver_data` 由
            // `egdf_register_driver*` 经 `Box::into_raw(DriverObject)` 写入,
            // 类型一致且归 EGDF 所有; 非空已由上方条件校验。
            unsafe {
                drop(Box::from_raw(dev.driver_data as *mut DriverObject));
            }
            dev.driver_data = core::ptr::null_mut();
        }
    }
}

// ── 单元测试 ──

#[cfg(test)]
mod tests {
    use super::super::driver::DriverError;
    use super::*;

    /// 注册表类用例的互斥锁.
    ///
    /// 用例共享全局 `EGDF_DEVICES`, 且普遍以 `clear()` 开场并按返回的下标
    /// (`idx`) 回查设备; 并行 test runner 下清表/注册会互相错位下标, 造成偶发
    /// 失败 (2026-09-24 UT-06, 口径同 privileged/timer/tick.rs 的 FREQ_TEST_LOCK).
    static EGDF_TEST_LOCK: crate::privileged::sync::IrqSpinLock<()> =
        crate::privileged::sync::IrqSpinLock::new(());

    struct TestDriver {
        name: &'static str,
        /// init() 被调用的观测标志.
        ///
        /// 不能再用 `driver_as_ref::<TestDriver>()` 读自身字段: `driver_data`
        /// 现存的是一次 `Box<DriverObject>` 包装 (见 `egdf_register_driver`),
        /// 按具体驱动类型解引用是类型混淆 (2026-09-24 UT-06 实测修正).
        init_called: &'static core::sync::atomic::AtomicBool,
    }

    impl Driver for TestDriver {
        fn name(&self) -> &'static str {
            self.name
        }
        fn device_type(&self) -> super::super::driver::DeviceType {
            super::super::driver::DeviceType::Other
        }

        fn init(&mut self) -> core::result::Result<(), DriverError> {
            self.init_called
                .store(true, core::sync::atomic::Ordering::Relaxed);
            Ok(())
        }

        fn shutdown(&mut self) -> core::result::Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn test_proto_as_str() {
        assert_eq!(EGDFProto::Block.as_str(), "block");
        assert_eq!(EGDFProto::Net.as_str(), "net");
    }

    #[test]
    fn test_device_state() {
        assert_eq!(DeviceState::Uninit as u8, DeviceState::Uninit as u8);
        assert_ne!(DeviceState::Ready as u8, DeviceState::Failed as u8);
    }

    #[test]
    fn test_register_and_find() {
        let _lock = EGDF_TEST_LOCK.lock();
        let dummy: Box<u32> = Box::new(42u32);
        let raw = box_to_raw(dummy);

        let id = egdf_register("test_dev", EGDFProto::Other, None, None, raw);
        assert!(id > 0);

        assert!(egdf_find_by_name("test_dev").is_some());
        assert_eq!(egdf_count(), 1);

        EGDF_DEVICES.lock().clear();
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            drop(Box::from_raw(raw as *mut u32));
        }
    }

    #[test]
    fn test_register_driver_auto_init() {
        let _lock = EGDF_TEST_LOCK.lock();
        static AUTO_INIT_CALLED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);

        let driver = Box::new(TestDriver {
            name: "test",
            init_called: &AUTO_INIT_CALLED,
        });
        let id = egdf_register_driver("test_drv", EGDFProto::Other, None, None, driver);
        assert!(id > 0);

        egdf_with_device(id, |dev| {
            assert_eq!(dev.state, DeviceState::Ready);
        });
        // 注册即自动 init (见 egdf_register_driver 内的 driver.init() 调用)
        assert!(AUTO_INIT_CALLED.load(core::sync::atomic::Ordering::Relaxed));

        let ptr = egdf_unregister(id).unwrap();
        // SAFETY: `egdf_register_driver` 存入的是 Box<DriverObject> 包装,
        // 其 Drop 负责释放内部 Box<dyn Driver> (详见实现).
        unsafe {
            drop(Box::from_raw(ptr as *mut DriverObject));
        }
    }

    #[test]
    fn test_rust_ffi() {
        assert!(alloc::format!("{:?}", EGDFProto::Char).contains("Char"));
        assert_eq!(EGDFProto::Input.as_str(), "input");
    }

    #[test]
    fn test_blk_read_invalid_drive() {
        let mut buf = [0u8; 512];
        // 越界 drive 返回 -EIO (KernelError::Io), 非裸 -1;
        // 与 test_t4_1_drive_oob_via_trait 的既有断言口径一致 (UT-06 实测修正).
        assert_eq!(egdf_blk_read(255, 0, &mut buf), -5);
    }

    // NetOps 字段类型是 `extern "C" fn` 指针, 闭包不能强转为 C fn, 故用具名函数.
    extern "C" fn mock_net_send(_: *mut u8, _: *const u8, _: u32) -> i32 {
        0
    }
    extern "C" fn mock_net_try_receive(_: *mut u8, _: *mut u8, _: u32) -> i32 {
        0
    }
    extern "C" fn mock_net_get_mac(_: *mut u8, _: *mut [u8; 6]) {}
    extern "C" fn mock_net_handle_irq(_: *mut u8) {}

    #[test]
    fn test_register_with_ops() {
        let _lock = EGDF_TEST_LOCK.lock();
        let dummy: Box<u8> = Box::new(0u8);
        let raw = box_to_raw(dummy);
        let id = egdf_register_with_ops(
            "mock_net",
            EGDFProto::Net,
            None,
            None,
            raw,
            // 使用 Net 变体替代已移除的 Block 变体
            EGDFOps::Net(&crate::privileged::egdf::proto_net::NetOps {
                send: mock_net_send,
                try_receive: mock_net_try_receive,
                get_mac: mock_net_get_mac,
                handle_irq: Some(mock_net_handle_irq),
            }),
        );
        assert!(id > 0);
        let mut devices = EGDF_DEVICES.lock();
        assert!(devices.iter().any(|d| d.id == id && d.ops.is_some()));
        // 复用同一 guard 内 clear: EGDF_DEVICES 为非重入 Mutex,
        // 原写法「guard 存活期间再次 lock()」为自死锁 (guard 的 drop 作用域至函数末尾).
        devices.clear();
        // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
        unsafe {
            drop(Box::from_raw(raw as *mut u8));
        }
    }

    // ==========================================================================
    // BlockDevice trait 注册路径测试
    // ==========================================================================
    //
    // 验证: register_block_device + egdf_register_block_dev 走 trait dispatch
    // 这些测试用 MockBlockDevice (本地 struct) 验证
    // BlockDevice trait 直接被 egdf_blk_read/write 调用.

    struct MockBlockDevice {
        counter: u64,
        last_sector: u64,
        last_written: [u8; 16],
    }

    impl MockBlockDevice {
        const fn new() -> Self {
            Self {
                counter: 0,
                last_sector: 0,
                last_written: [0u8; 16],
            }
        }
    }

    impl BlockDevice for MockBlockDevice {
        fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
            self.counter += 1;
            self.last_sector = sector;
            if buf.len() < 512 {
                return -1;
            }
            // 写一个 magic 模式, 测试用
            for i in 0..16 {
                buf[i] = b'T'; // 标识 "通过 trait 来的数据"
            }
            buf[16] = (sector & 0xFF) as u8;
            0
        }
        fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
            self.counter += 1;
            self.last_sector = sector;
            for i in 0..16.min(buf.len()) {
                self.last_written[i] = buf[i];
            }
            0
        }
        fn blk_is_present(&self) -> bool {
            true
        }
        fn blk_total_sectors(&self) -> u64 {
            2048
        }
    }

    /// 1. 注册路径: register_block_device + egdf_register_block_dev
    #[test]
    fn test_t4_1_register_via_trait() {
        let _lock = EGDF_TEST_LOCK.lock();
        EGDF_DEVICES.lock().clear();
        let mock: &'static mut MockBlockDevice = Box::leak(Box::new(MockBlockDevice::new()));
        let idx = egdf_register_block_dev("trait_blk", None, None, mock);
        assert_eq!(egdf_count_by_proto(EGDFProto::Block), 1);
        let mut devices = EGDF_DEVICES.lock();
        {
            let dev = &devices[idx as usize];
            assert!(dev.block_dev.is_some(), "block_dev 字段应被设置");
            assert!(dev.ops.is_none(), "Block 字段应为空 (block_dev 已替代)");
        }
        // 复用同一 guard 内 clear: EGDF_DEVICES 为非重入 Mutex,
        // 原写法「guard 存活期间再次 lock()」为自死锁.
        devices.clear();
    }

    /// trait dispatch: egdf_blk_read 调 MockBlockDevice.blk_read
    #[test]
    fn test_t4_1_egdf_blk_read_via_trait() {
        let _lock = EGDF_TEST_LOCK.lock();
        EGDF_DEVICES.lock().clear();
        // 以裸指针持有泄漏对象: 注册表取走 `&'static mut` 后仍需读回 mock 的观测字段.
        let mock_ptr: *mut MockBlockDevice = Box::leak(Box::new(MockBlockDevice::new()));
        // SAFETY: mock_ptr 由 Box::leak 产生, 在测试进程生命周期内始终有效且独占.
        let idx = egdf_register_block_dev("trait_blk_read", None, None, unsafe { &mut *mock_ptr });

        let mut buf = [0u8; 512];
        let r = egdf_blk_read(idx as u8, 7, &mut buf);
        assert_eq!(r, 0, "egdf_blk_read 应成功");
        assert_eq!(buf[0], b'T', "数据应来自 MockBlockDevice (trait)");
        assert_eq!(buf[16], 7, "sector 7 应被传到 trait");
        // SAFETY: 同上述; 注册表只持有该对象的可变引用, 此处无并发访问.
        let (counter, last_sector) = unsafe { ((*mock_ptr).counter, (*mock_ptr).last_sector) };
        assert_eq!(counter, 1, "MockBlockDevice.blk_read 应被调用 1 次");
        assert_eq!(last_sector, 7);

        EGDF_DEVICES.lock().clear();
    }

    /// 3. trait dispatch: egdf_blk_write 调 MockBlockDevice.blk_write
    #[test]
    fn test_t4_1_egdf_blk_write_via_trait() {
        let _lock = EGDF_TEST_LOCK.lock();
        EGDF_DEVICES.lock().clear();
        // 理由同 test_t4_1_egdf_blk_read_via_trait.
        let mock_ptr: *mut MockBlockDevice = Box::leak(Box::new(MockBlockDevice::new()));
        // SAFETY: mock_ptr 由 Box::leak 产生, 在测试进程生命周期内始终有效且独占.
        let idx = egdf_register_block_dev("trait_blk_write", None, None, unsafe { &mut *mock_ptr });

        let buf = [b'X'; 512];
        let r = egdf_blk_write(idx as u8, 99, &buf);
        assert_eq!(r, 0, "egdf_blk_write 应成功");
        // SAFETY: 同上述; 注册表只持有该对象的可变引用, 此处无并发访问.
        let (last_sector, first_written) =
            unsafe { ((*mock_ptr).last_sector, (*mock_ptr).last_written[0]) };
        assert_eq!(last_sector, 99);
        assert_eq!(first_written, b'X', "数据应传到 MockBlockDevice");

        EGDF_DEVICES.lock().clear();
    }

    /// 4. trait 分发: egdf_blk_is_present / total_sectors
    #[test]
    fn test_t4_1_egdf_blk_metadata_via_trait() {
        let _lock = EGDF_TEST_LOCK.lock();
        EGDF_DEVICES.lock().clear();
        let mock: &'static mut MockBlockDevice = Box::leak(Box::new(MockBlockDevice::new()));
        let idx = egdf_register_block_dev("trait_blk_meta", None, None, mock);

        assert!(egdf_blk_is_present(idx as u8));
        assert_eq!(egdf_blk_total_sectors(idx as u8), 2048);

        EGDF_DEVICES.lock().clear();
    }

    /// 优先级: block_dev 路径
    #[test]
    fn test_t4_1_block_dev_takes_priority() {
        let _lock = EGDF_TEST_LOCK.lock();
        EGDF_DEVICES.lock().clear();
        let mock: &'static mut MockBlockDevice = Box::leak(Box::new(MockBlockDevice::new()));
        let idx = egdf_register_block_dev("priority_blk", None, None, mock);

        let mut buf = [0u8; 512];
        egdf_blk_read(idx as u8, 1, &mut buf);
        assert_eq!(buf[0], b'T', "block_dev 路径应返回 'T'");
        EGDF_DEVICES.lock().clear();
    }

    /// 7. 边界: buf.len() < 512 应返回 -EINVAL (无论 block_dev 路径)
    #[test]
    fn test_t4_1_buf_too_small_via_trait() {
        let _lock = EGDF_TEST_LOCK.lock();
        EGDF_DEVICES.lock().clear();
        // 理由同 test_t4_1_egdf_blk_read_via_trait.
        let mock_ptr: *mut MockBlockDevice = Box::leak(Box::new(MockBlockDevice::new()));
        // SAFETY: mock_ptr 由 Box::leak 产生, 在测试进程生命周期内始终有效且独占.
        let idx = egdf_register_block_dev("small_buf_blk", None, None, unsafe { &mut *mock_ptr });

        let mut small = [0u8; 256];
        let r = egdf_blk_read(idx as u8, 0, &mut small);
        assert_eq!(r, -22); // -EINVAL
        // SAFETY: 同上述; 注册表只持有该对象的可变引用, 此处无并发访问.
        let counter = unsafe { (*mock_ptr).counter };
        assert_eq!(counter, 0, "Mock 不应被调用 (校验前置)");

        EGDF_DEVICES.lock().clear();
    }

    /// 8. 边界: drive OOB 应返回 -EIO (无 panic)
    #[test]
    fn test_t4_1_drive_oob_via_trait() {
        let _lock = EGDF_TEST_LOCK.lock();
        EGDF_DEVICES.lock().clear();
        let mut buf = [0u8; 512];
        assert_eq!(egdf_blk_read(255, 0, &mut buf), -5); // -EIO
        assert_eq!(egdf_blk_write(255, 0, &buf), -5);
        assert!(!egdf_blk_is_present(255));
        assert_eq!(egdf_blk_total_sectors(255), 0);
    }
}
