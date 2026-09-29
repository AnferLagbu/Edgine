#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 设备文件系统 (`DevFS`) — services 策略实现
//!
//! ## 阶段 4b 归属收敛 (当前权威)
//!
//! 自阶段 4b 起 DevFS 实装归 `services::fs` (依 Minimalism 准则下沉),
//! framework 侧不再持有设备表。依赖闭包: IrqSpinLock/OnceLock 经
//! framework safe API, Inode trait 归 `services::fs::inode`, 0 unsafe。
//!
//! ## DECISION-J 归属反转记录 (2026-09-13) — 已被阶段 4b 取代
//!
//! 实现曾于 E6-7 (2026-06-10) 迁至 `services::fs::devfs`, framework 侧仅
//! re-export。当时按"机制持有的数据结构归 framework"统一判据反转, 将
//! DevFS 设备表 (被 framework VFS 挂载机制内联消费) 迁回 framework; 阶段
//! 4b 依 Minimalism 准则再次下沉 services, 上述反转已失效。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 所有硬件交互通过 framework safe API
//! - **类型安全**: 设备类型用 `DevKind` 枚举, 而非裸 `u8`
//! - **完整实现**: 包含设备注册/IO/目录读取, 不再依赖 framework 实现
//!
//! E6-7: `DevFS` 策略提取

use core::sync::atomic::{AtomicU32, Ordering};

use crate::framework::sync::IrqSpinLock as Mutex;

// ============================================================================
// 常量
// ============================================================================

/// 最大设备数
pub const DEVFS_MAX_DEVICES: usize = 16;
/// 设备名最大长度
pub const DEVFS_MAX_NAME: usize = 32;

// ============================================================================
// 设备类型
// ============================================================================

/// 设备类型 (强类型枚举, 替代裸 `u8`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DevKind {
    /// 空设备 /dev/null
    Null = 0,
    /// 零字节源 /dev/zero
    Zero = 1,
    /// 控制台 /dev/console
    Console = 2,
    /// TTY /dev/tty
    Tty = 3,
    /// Credo 能力 (PWM) 接口 /dev/credo
    Credo = 4,
    /// 块设备 (如 /dev/sda)
    Block = 5,
    /// 字符设备 (如 /dev/serial0)
    Char = 6,
    /// 网络设备 (如 /dev/eth0)
    Net = 7,
    /// 输入设备 (如 /dev/input/kbd)
    Input = 8,
}

impl DevKind {
    /// 从 `u8` 解析 (容忍未知值, 返回 `None`)
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Null),
            1 => Some(Self::Zero),
            2 => Some(Self::Console),
            3 => Some(Self::Tty),
            4 => Some(Self::Credo),
            5 => Some(Self::Block),
            6 => Some(Self::Char),
            7 => Some(Self::Net),
            8 => Some(Self::Input),
            _ => None,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 是否为虚拟设备 (内核内部实现)
    pub fn is_virtual(&self) -> bool {
        matches!(
            self,
            Self::Null | Self::Zero | Self::Console | Self::Tty | Self::Credo
        )
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 是否为物理设备 (由 Chitin 驱动提供)
    pub fn is_physical(&self) -> bool {
        matches!(self, Self::Block | Self::Char | Self::Net | Self::Input)
    }
}

// ============================================================================
// 内部辅助
// ============================================================================

fn write_u32_dec(buf: &mut [u8], mut off: usize, mut val: u32) -> usize {
    if val == 0 {
        if off < buf.len() {
            buf[off] = b'0';
            off += 1;
        }
        return off;
    }
    let mut digits = [0u8; 10];
    let mut i = 0;
    while val > 0 {
        digits[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        if off < buf.len() {
            buf[off] = digits[j];
            off += 1;
        }
    }
    off
}

// ============================================================================
// 设备条目
// ============================================================================

/// 设备条目
#[derive(Debug, Clone, Copy)]
pub struct DevfsDevice {
    pub name: [u8; DEVFS_MAX_NAME],
    pub dev_type: u8,
    pub used: bool,
}

impl DevfsDevice {
    pub const fn new() -> Self {
        Self {
            name: [0; DEVFS_MAX_NAME],
            dev_type: 0,
            used: false,
        }
    }
}

// ============================================================================
// DevfsData
// ============================================================================

/// `DevFS` 核心数据
pub struct DevfsData {
    devices: Mutex<[DevfsDevice; DEVFS_MAX_DEVICES]>,
    device_count: AtomicU32,
}

// DevfsData 自动 Send + Sync: IrqSpinLock<T> (framework) 已实现 unsafe impl Send/Sync,
// AtomicU32 也是 Send + Sync, 无需手动实现.

impl DevfsData {
    pub const fn new() -> Self {
        Self {
            devices: Mutex::new([const { DevfsDevice::new() }; DEVFS_MAX_DEVICES]),
            device_count: AtomicU32::new(0),
        }
    }

    fn set_name(device: &mut DevfsDevice, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(DEVFS_MAX_NAME - 1);
        device.name[..len].copy_from_slice(&bytes[..len]);
        device.name[len] = 0;
    }

    /// 注册设备到 devfs, 使其可被 `open` 打开.
    ///
    /// # Errors
    /// 当同名设备已注册时返回 `AlreadyExists`; 当设备表已满时返回 `NoSpace`.
    pub fn register_device(&self, name: &str, dev_type: u8) -> KernelResult<()> {
        let mut devices = self.devices.lock();
        for device in devices.iter() {
            if device.used {
                let end = device
                    .name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(DEVFS_MAX_NAME);
                let existing = core::str::from_utf8(&device.name[..end]).unwrap_or("");
                if existing == name {
                    return Err(KernelError::AlreadyExists);
                }
            }
        }
        for device in devices.iter_mut() {
            if !device.used {
                Self::set_name(device, name);
                device.dev_type = dev_type;
                device.used = true;
                self.device_count.fetch_add(1, Ordering::SeqCst);
                return Ok(());
            }
        }
        Err(KernelError::NoSpace)
    }

    /// 注销设备, 释放设备表项.
    ///
    /// # Errors
    /// 当不存在名为 `name` 的已注册设备时返回 `FileNotFound`.
    pub fn unregister_device(&self, name: &str) -> KernelResult<()> {
        let mut devices = self.devices.lock();
        for device in devices.iter_mut() {
            if device.used {
                let end = device
                    .name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(DEVFS_MAX_NAME);
                let existing = core::str::from_utf8(&device.name[..end]).unwrap_or("");
                if existing == name {
                    device.used = false;
                    device.dev_type = 0;
                    self.device_count.fetch_sub(1, Ordering::SeqCst);
                    return Ok(());
                }
            }
        }
        Err(KernelError::FileNotFound)
    }

    pub fn mount(&self, _path: &str) -> i32 {
        // E6-9a: 不再硬编码设备, 改为通过 register_device 注册
        // 标准虚拟设备由启动流程显式调用 register_standard() 注册
        self.device_count.store(0, Ordering::SeqCst);
        0
    }

    pub fn open(&self, path: &str) -> Option<(u32, u8)> {
        let dev_name = path.trim_start_matches('/');

        let devices = self.devices.lock();
        for device in devices.iter() {
            if device.used {
                let end = device
                    .name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(DEVFS_MAX_NAME);
                let name = core::str::from_utf8(&device.name[..end]).unwrap_or("");
                if name == dev_name {
                    return Some((u32::from(device.dev_type), device.dev_type));
                }
            }
        }
        None
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn read(&self, dev_type: u8, buf: &mut [u8]) -> i32 {
        match DevKind::from_u8(dev_type) {
            Some(DevKind::Null) => 0,
            Some(DevKind::Zero) => {
                buf.fill(0);
                buf.len() as i32
            }
            Some(DevKind::Console | DevKind::Tty) => 0,
            Some(DevKind::Credo) => {
                let pwm = crate::framework::credo::session::get_current_pwm();
                let euid = crate::framework::credo::session::get_euid();
                let uid = crate::framework::credo::session::get_current_uid();
                if pwm != 0 {
                    let mut off = 0;
                    let blen = buf.len();
                    if off < blen {
                        buf[off] = b'O';
                        off += 1;
                    }
                    if off < blen {
                        buf[off] = b'K';
                        off += 1;
                    }
                    if off < blen {
                        buf[off] = b' ';
                        off += 1;
                    }
                    for &b in b"pwm=0x" {
                        if off < blen {
                            buf[off] = b;
                            off += 1;
                        }
                    }
                    let hex = b"0123456789ABCDEF";
                    for shift in (0..64).rev().step_by(4) {
                        let nibble = ((pwm >> shift) & 0xF) as usize;
                        if off < blen {
                            buf[off] = hex[nibble];
                            off += 1;
                        }
                    }
                    for &b in b" uid=" {
                        if off < blen {
                            buf[off] = b;
                            off += 1;
                        }
                    }
                    off = write_u32_dec(buf, off, uid);
                    for &b in b" euid=" {
                        if off < blen {
                            buf[off] = b;
                            off += 1;
                        }
                    }
                    off = write_u32_dec(buf, off, euid);
                    if off < blen {
                        buf[off] = b'\n';
                        off += 1;
                    }
                    off as i32
                } else {
                    let msg = b"ERR not_authenticated\n";
                    let len = msg.len().min(buf.len());
                    buf[..len].copy_from_slice(&msg[..len]);
                    len as i32
                }
            }
            Some(DevKind::Block | DevKind::Char | DevKind::Net | DevKind::Input) => {
                // E6-9a: 物理设备 I/O 路由待 E6-9b (Chitin 桥接) 实现
                -1
            }
            None => -1,
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn write(&self, dev_type: u8, buf: &[u8]) -> i32 {
        match DevKind::from_u8(dev_type) {
            Some(DevKind::Null | DevKind::Zero) => buf.len() as i32,
            Some(DevKind::Console | DevKind::Tty) => {
                // services 层不能使用 klog_info! (含 unsafe), 改用 safe 串口输出
                crate::framework::klog::serial_write_bytes(buf);
                buf.len() as i32
            }
            Some(DevKind::Credo) => {
                let input = core::str::from_utf8(buf).unwrap_or("");
                let input = input.trim_end_matches(['\n', '\r', '\0']);
                let mut parts = input.splitn(2, '\n');
                let note = parts.next().unwrap_or("").trim();
                let password = parts.next().unwrap_or("").trim();
                if note.is_empty() || password.is_empty() {
                    return KernelError::InvalidArgument.as_i32();
                }
                crate::framework::credo::session::login(note, password)
                    .map_or(KernelError::PermissionDenied.as_i32(), |_pwm| {
                        buf.len() as i32
                    })
            }
            Some(DevKind::Block | DevKind::Char | DevKind::Net | DevKind::Input) => {
                // E6-9a: 物理设备 I/O 路由待 E6-9b (Chitin 桥接) 实现
                -1
            }
            None => -1,
        }
    }

    pub fn readdir(&self, index: usize) -> Option<([u8; 32], u8)> {
        let devices = self.devices.lock();
        let mut count = 0;

        for device in devices.iter() {
            if device.used {
                if count == index {
                    let mut name = [0u8; 32];
                    let end = device.name.iter().position(|&b| b == 0).unwrap_or(32);
                    name[..end].copy_from_slice(&device.name[..end]);
                    return Some((name, device.dev_type));
                }
                count += 1;
            }
        }
        None
    }

    pub fn device_count(&self) -> u32 {
        self.device_count.load(Ordering::SeqCst)
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 `DevFS` 数据
pub static DEVFS_DATA: DevfsData = DevfsData::new();

/// 初始化全局 `DevFS` 并注册标准虚拟设备
pub fn init() {
    register_standard();
}

/// 初始化 `DevFS` 并订阅 Chitin 设备注册回调 (E6-9b)
///
/// Chitin 驱动注册新设备时, `DevFS` 自动创建对应设备节点。
pub fn init_with_chitin_bridge() {
    register_standard();
    crate::framework::chitin::chitin_set_register_callback(on_chitin_device_registered);
}

/// Chitin 设备注册回调 — 自动创建 `DevFS` 设备节点 (E6-9b)
fn on_chitin_device_registered(dev: &crate::framework::chitin::ChitinDevice) {
    let kind = match dev.proto {
        crate::framework::chitin::ChitinProto::Block => DevKind::Block,
        crate::framework::chitin::ChitinProto::Char => DevKind::Char,
        crate::framework::chitin::ChitinProto::Net => DevKind::Net,
        crate::framework::chitin::ChitinProto::Input => DevKind::Input,
        // Bus/Other 不创建 DevFS 节点
        _ => return,
    };
    // I-20: register_device 现在返回 KernelResult<()>, 失败仅记录
    // (设备可能已存在, Chitin 重启场景), 不阻断 chitin 回调链.
    let _ = DEVFS_DATA.register_device(dev.name, kind as u8);
}

// ============================================================================
// 安全设备文件描述符
// ============================================================================

/// `DevFS` 文件描述符
#[derive(Debug, Clone, Copy)]
pub struct DevFile {
    /// 内部节点索引
    pub index: u32,
    /// 设备类型
    pub kind: DevKind,
}

impl DevFile {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 设备名 (如 "null", "zero", "console")
    /// 物理设备无固定名称, 返回 "device"
    pub fn name(&self) -> &'static str {
        match self.kind {
            DevKind::Null => "null",
            DevKind::Zero => "zero",
            DevKind::Console => "console",
            DevKind::Tty => "tty",
            DevKind::Credo => "credo",
            DevKind::Block | DevKind::Char | DevKind::Net | DevKind::Input => "device",
        }
    }
}

// ============================================================================
// 安全 DevFS 代理
// ============================================================================

/// `DevFS` 安全代理 (services 层)。
pub struct SafeDevFs {
    inner: &'static DevfsData,
}

impl SafeDevFs {
    /// 创建全局 `DevFS` 代理
    pub fn new() -> Self {
        Self { inner: &DEVFS_DATA }
    }

    /// 注册设备
    ///
    /// # Errors
    /// 当同名设备已存在时返回 `AlreadyExists`; 当设备表已满时返回 `NoSpace`.
    pub fn register(&self, name: &str, kind: DevKind) -> Result<(), KernelError> {
        self.inner.register_device(name, kind as u8)
    }

    /// 注销设备
    ///
    /// # Errors
    /// 当设备未注册时返回 `FileNotFound`.
    pub fn unregister(&self, name: &str) -> Result<(), KernelError> {
        self.inner.unregister_device(name)
    }

    /// 打开设备
    ///
    /// # Errors
    /// 当设备不存在时返回 `FileNotFound`; 当设备类型无效时返回 `InvalidArgument`.
    pub fn open(&self, path: &str) -> Result<DevFile, KernelError> {
        match self.inner.open(path) {
            Some((index, dev_type)) => {
                let kind = DevKind::from_u8(dev_type).ok_or(KernelError::InvalidArgument)?;
                Ok(DevFile { index, kind })
            }
            None => Err(KernelError::FileNotFound),
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 从设备读
    ///
    /// # Errors
    /// 当底层设备读操作失败时返回 `Io`.
    pub fn read(&self, dev: &DevFile, buf: &mut [u8]) -> Result<usize, KernelError> {
        let rc = self.inner.read(dev.kind as u8, buf);
        if rc < 0 {
            Err(KernelError::Io)
        } else {
            Ok(rc as usize)
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 向设备写
    ///
    /// # Errors
    /// 当底层设备写操作失败时返回 `Io`.
    pub fn write(&self, dev: &DevFile, buf: &[u8]) -> Result<usize, KernelError> {
        let rc = self.inner.write(dev.kind as u8, buf);
        if rc < 0 {
            Err(KernelError::Io)
        } else {
            Ok(rc as usize)
        }
    }

    /// 读目录项
    pub fn readdir(&self, index: usize) -> Option<(alloc::string::String, DevKind)> {
        let (raw_name, dev_type) = self.inner.readdir(index)?;
        let kind = DevKind::from_u8(dev_type)?;
        let end = raw_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(raw_name.len());
        let name = alloc::string::String::from_utf8_lossy(&raw_name[..end]).into_owned();
        Some((name, kind))
    }

    /// 设备总数
    pub fn device_count(&self) -> u32 {
        self.inner.device_count()
    }
}

impl Default for SafeDevFs {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 全局实例 (Once)
// ============================================================================

// I-16: 替换 spin::Once → 项目自研 OnceCell (framework::sync::OnceLock 别名)
use crate::framework::sync::OnceLock as OnceCell;
static GLOBAL_DEVFS: OnceCell<SafeDevFs> = OnceCell::new();

/// 初始化全局 `DevFS`
pub fn init_global() {
    let _ = GLOBAL_DEVFS.get_or_init(|slot| {
        slot.write(SafeDevFs::new());
    });
}

/// 获取全局 `DevFS` 引用
///
/// # Panics
/// 当 `init_global()` 尚未被调用、全局实例未初始化时发生 panic (内部使用 `expect`).
pub fn global() -> &'static SafeDevFs {
    GLOBAL_DEVFS
        .get()
        .expect("devfs::global() called before init_global()")
}

// ============================================================================
// 便利函数
// ============================================================================

/// 注册设备到全局 `DevFS`
///
/// # Errors
/// 错误条件与 [`SafeDevFs::register`] 相同.
pub fn register(name: &str, kind: DevKind) -> Result<(), KernelError> {
    global().register(name, kind)
}

/// 打开全局 `DevFS` 设备
///
/// # Errors
/// 错误条件与 [`SafeDevFs::open`] 相同.
pub fn open(path: &str) -> Result<DevFile, KernelError> {
    global().open(path)
}

/// 设备名最大长度
pub const fn max_name_len() -> usize {
    DEVFS_MAX_NAME
}

// ============================================================================
// 标准设备预注册
// ============================================================================

/// 注册所有标准设备 (null/zero/console/tty/credo)
pub fn register_standard() {
    let _ = register("null", DevKind::Null);
    let _ = register("zero", DevKind::Zero);
    let _ = register("console", DevKind::Console);
    let _ = register("tty", DevKind::Tty);
    let _ = register("credo", DevKind::Credo);
}

// ============================================================================
// DevFs Inode — 设备文件 Inode 实现
// ============================================================================

use crate::services::fs::inode::Inode;
use alloc::sync::Arc;

/// 设备文件 Inode — `DevFS` 的 Inode 实现
pub struct DevFsInode {
    dev_type: u8,
    mount_idx: u32,
}

impl DevFsInode {
    pub fn new(dev_type: u8, mount_idx: u32) -> Self {
        Self {
            dev_type,
            mount_idx,
        }
    }
}

impl Inode for DevFsInode {
    fn read(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        let result = DEVFS_DATA.read(self.dev_type, buf);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn write(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        let result = DEVFS_DATA.write(self.dev_type, buf);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn stat(&self, _pwm: u64) -> KernelResult<VfsStat> {
        Ok(VfsStat {
            node_id: u32::from(self.dev_type),
            mode: 0o20666,
            file_type: crate::framework::fs::VfsFileType::Dev.as_u8(),
            perm: 0o666,
            ..VfsStat::default()
        })
    }

    fn truncate(&self, _size: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn seek(
        &self,
        _offset: i64,
        _whence: crate::services::fs::VfsSeekWhence,
        _current_offset: u64,
    ) -> KernelResult<u64> {
        Err(KernelError::InvalidArgument)
    }

    fn is_dir(&self) -> bool {
        false
    }

    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        // DevFS: 设备文件, 无时间戳
        Ok(())
    }

    fn node_id(&self) -> u32 {
        u32::from(self.dev_type)
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

// ============================================================================
// FileSystem trait 实现 (E6-9c: VFS 分发接入)
// ============================================================================

use crate::services::fs::{FileSystem, KernelError, KernelResult, VfsDirEntry, VfsStat};

impl FileSystem for DevfsData {
    fn name(&self) -> &'static str {
        "devfs"
    }

    fn fs_init(&self) -> KernelResult<()> {
        Ok(())
    }

    fn fs_mount(&self, path: &str) -> KernelResult<()> {
        if self.mount(path) != 0 {
            return Err(KernelError::Io);
        }
        // E6-9a: mount 不再硬编码设备, 由 init() 或 init_with_chitin_bridge() 注册
        Ok(())
    }

    fn fs_open(&self, rel_path: &str, _flags: u32, _pwm: u64) -> KernelResult<Arc<dyn Inode>> {
        match self.open(rel_path) {
            Some((_index, dev_type)) => Ok(Arc::new(DevFsInode::new(dev_type, 0))),
            None => Err(KernelError::FileNotFound),
        }
    }

    fn fs_close(&self, _handle: u32) -> KernelResult<()> {
        Ok(())
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    fn fs_read(
        &self,
        _handle: u32,
        _offset: u64,
        buf: &mut [u8],
        _pwm: u64,
    ) -> KernelResult<usize> {
        // DevFS read 需要 dev_type, handle 即为 dev_type
        let result = self.read(_handle as u8, buf);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    fn fs_write(&self, _handle: u32, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        let result = self.write(_handle as u8, buf);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_stat(&self, rel_path: &str, _pwm: u64) -> KernelResult<VfsStat> {
        match self.open(rel_path) {
            Some((index, dev_type)) => Ok(VfsStat {
                node_id: index,
                mode: 0o20666, // 字符设备, rw-rw-rw-
                size: 0,
                uid: 0,
                gid: 0,
                atime: 0,
                mtime: 0,
                ctime: 0,
                owner_pwm: 0,
                group_pwm: 0,
                perm: 0o666,
                file_type: dev_type,
                sensitivity: 0,
            }),
            None => Err(KernelError::FileNotFound),
        }
    }

    fn fs_chmod(&self, _rel_path: &str, _mode: u16, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::PermissionDenied)
    }

    fn fs_chown(
        &self,
        _rel_path: &str,
        _owner_pwm: u64,
        _group_pwm: u64,
        _pwm: u64,
    ) -> KernelResult<()> {
        Err(KernelError::PermissionDenied)
    }

    fn fs_mkdir(&self, _rel_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_unlink(&self, _rel_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::PermissionDenied)
    }

    fn fs_rmdir(&self, _rel_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_rename(&self, _old_path: &str, _new_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_readdir(&self, _handle: u32, offset: u64, entry: &mut VfsDirEntry) -> KernelResult<bool> {
        match self.readdir(offset as usize) {
            Some((raw_name, dev_type)) => {
                let end = raw_name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(raw_name.len());
                let len = end.min(entry.name.len());
                entry.name[..len].copy_from_slice(&raw_name[..len]);
                // 剩余部分填零
                entry.name[len..].fill(0);
                entry.file_type = dev_type;
                entry.node = offset as u32;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    // L4 重构: 扩展方法实现 (override trait 默认实现)
    fn fs_resolve_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
    ) -> Option<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        Some(alloc::sync::Arc::new(DevFsInode::new(
            inode_id as u8,
            mount_idx,
        )))
    }
}

// ============================================================================
// 单元测试 (仅纯逻辑, `cargo test --features host-test --lib` 唯一一份)
// ============================================================================

// 4c-B2: 由 framework/tests/test_devfs.rs 迁入。用例一律使用局部 `DevfsData::new()`
// 实例, 不触碰全局 `DEVFS_DATA`, 故天然并行安全、顺序无关; 仅
// `test_devfs_register_standard` 覆盖全局入口, 采用幂等注册 + 只读 open 断言。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_devfs_mount() {
        let devfs = DevfsData::new();
        assert_eq!(devfs.mount("/dev"), 0, "devfs mount failed");
        // E6-9a: mount 不再硬编码设备, 需显式注册标准设备
        assert!(
            devfs.register_device("null", DevKind::Null as u8).is_ok(),
            "register null"
        );
        assert!(
            devfs.register_device("zero", DevKind::Zero as u8).is_ok(),
            "register zero"
        );
        assert!(
            devfs
                .register_device("console", DevKind::Console as u8)
                .is_ok(),
            "register console"
        );
        assert!(
            devfs.register_device("tty", DevKind::Tty as u8).is_ok(),
            "register tty"
        );
        assert!(
            devfs.register_device("credo", DevKind::Credo as u8).is_ok(),
            "register credo"
        );
        assert_eq!(devfs.device_count(), 5, "expected 5 standard devices");
    }

    #[test]
    fn test_devfs_open_default_devices() {
        let devfs = DevfsData::new();
        assert!(devfs.register_device("null", DevKind::Null as u8).is_ok());
        assert!(devfs.register_device("zero", DevKind::Zero as u8).is_ok());
        assert!(
            devfs
                .register_device("console", DevKind::Console as u8)
                .is_ok()
        );
        assert!(devfs.register_device("tty", DevKind::Tty as u8).is_ok());
        assert!(devfs.open("null").is_some(), "should open null");
        assert!(devfs.open("zero").is_some(), "should open zero");
        assert!(devfs.open("console").is_some(), "should open console");
        assert!(devfs.open("tty").is_some(), "should open tty");
        assert!(
            devfs.open("nonexistent").is_none(),
            "should not open nonexistent device"
        );
    }

    #[test]
    fn test_devfs_read_null() {
        let devfs = DevfsData::new();
        let mut buf = [0xAAu8; 16];
        let n = devfs.read(0, &mut buf);
        assert_eq!(n, 0, "null device should return 0 bytes");
    }

    #[test]
    fn test_devfs_read_zero() {
        let devfs = DevfsData::new();
        let mut buf = [0xFFu8; 16];
        let n = devfs.read(1, &mut buf);
        assert_eq!(n, 16, "zero device should fill buffer");
        assert_eq!(buf, [0u8; 16], "zero device should fill with zeros");
    }

    #[test]
    fn test_devfs_register_device() {
        let devfs = DevfsData::new();
        let count_before = devfs.device_count();
        let result = devfs.register_device("testdev", 10);
        // I-20: register_device 改 KernelResult, 0 → Ok(()), -1 → Err(_)
        assert!(result.is_ok(), "register_device should succeed");
        assert_eq!(
            devfs.device_count(),
            count_before + 1,
            "device count should increase"
        );
        assert!(
            devfs.open("testdev").is_some(),
            "should open newly registered device"
        );
    }

    #[test]
    fn test_devfs_unregister_device() {
        let devfs = DevfsData::new();
        let _ = devfs.register_device("tempdev", 20);
        let count_before = devfs.device_count();
        let result = devfs.unregister_device("tempdev");
        // I-20: unregister_device 改 KernelResult
        assert!(result.is_ok(), "unregister_device should succeed");
        assert_eq!(
            devfs.device_count(),
            count_before - 1,
            "device count should decrease"
        );
        assert!(
            devfs.open("tempdev").is_none(),
            "should not open unregistered device"
        );
    }

    #[test]
    fn test_devfs_register_duplicate() {
        let devfs = DevfsData::new();
        assert!(devfs.register_device("null", 0).is_ok());
        // I-20: 重复注册从 `== -1` 改为 AlreadyExists (KernelError 变体)
        let result = devfs.register_device("null", 0);
        assert!(
            matches!(result, Err(KernelError::AlreadyExists)),
            "registering duplicate should return AlreadyExists"
        );
    }

    #[test]
    fn test_devfs_unregister_nonexistent() {
        let devfs = DevfsData::new();
        // I-20: 注销不存在从 `== -1` 改为 NotFound
        let result = devfs.unregister_device("nonexistent_dev");
        assert!(
            matches!(result, Err(KernelError::FileNotFound)),
            "unregistering nonexistent should return NotFound"
        );
    }

    #[test]
    fn test_devfs_readdir() {
        let devfs = DevfsData::new();
        assert!(devfs.register_device("null", 0).is_ok());
        let first = devfs.readdir(0);
        assert!(first.is_some(), "readdir(0) should return a device");
        let beyond = devfs.readdir(DEVFS_MAX_DEVICES + 10);
        assert!(beyond.is_none(), "readdir beyond count should return None");
    }

    /// 全局 `register_standard()` 入口 — 幂等注册 + 只读 open 顺序无关断言。
    #[test]
    fn test_devfs_register_standard() {
        init_global();
        // 幂等: 重复注册返回 AlreadyExists, 由 register_standard 内部忽略
        register_standard();
        let g = global();
        for name in ["null", "zero", "console", "tty", "credo"] {
            assert!(g.open(name).is_ok(), "global devfs should open {name}");
        }
    }
}
