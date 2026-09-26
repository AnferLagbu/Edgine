//! VirtIO MMIO 传输层
//!
//! 实现 VirtIO 1.0 MMIO 传输, 用于设备发现与初始化.
//! 在 QEMU virt 平台 (aarch64 与 x86_64 -M virt) 上使用.
//!
//! MMIO 寄存器布局 (每个设备占据 0x200 字节区域):
//!
//! | Offset | Name            | Width | Description                       |
//! |--------|-----------------|-------|-----------------------------------|
//! | 0x000  | MagicValue      | R     | 0x74726976 ("virt")               |
//! | 0x004  | Version         | R     | 0x2 for VirtIO 1.0               |
//! | 0x008  | DeviceID        | R     | 2=blk, 1=net, etc.               |
//! | 0x00c  | VendorID        | R     | 0x554d4551 ("QEUM")              |
//! | 0x010  | DeviceFeatures  | R     | Bits 0..31 of device features     |
//! | 0x014  | DeviceFeaturesSel| W    | Selects which 32-bit feature word |
//! | 0x020  | DriverFeatures  | W     | Bits 0..31 of driver features    |
//! | 0x024  | DriverFeaturesSel| W    | Selects which 32-bit feature word |
//! | 0x030  | QueueSel        | W     | Select virtqueue                  |
//! | 0x034  | QueueNumMax     | R     | Max size of selected queue        |
//! | 0x038  | QueueNum        | W     | Set size of selected queue        |
//! | 0x040  | QueueReady      | RW    | Mark queue as ready               |
//! | 0x050  | QueueNotify     | W     | Notify device of new descriptors  |
//! | 0x060  | InterruptStatus | R     | Interrupt reason                  |
//! | 0x064  | InterruptACK    | W     | Acknowledge interrupt             |
//! | 0x070  | Status          | RW    | Device status                     |
//! | 0x080  | QueueDescLow    | W     | Descriptor table phys addr `[31:0]` |
//! | 0x084  | QueueDescHigh   | W     | Descriptor table phys addr `[63:32]`|
//! | 0x090  | QueueDriverLow  | W     | Available ring phys addr `[31:0]`   |
//! | 0x094  | QueueDriverHigh | W     | Available ring phys addr `[63:32]`  |
//! | 0x0a0  | QueueDeviceLow  | W     | Used ring phys addr `[31:0]`        |
//! | 0x0a4  | QueueDeviceHigh | W     | Used ring phys addr `[63:32]`       |
//! | 0x0fc  | ConfigGeneration| R     | Config change counter             |
//! | 0x100+ | Config          | RW    | Device-specific configuration     |
//!
//! QEMU virt aarch64 将 virtio-mmio 设备放置在 0x0a000000 起始地址,
//! 每个设备之间步长 0x200 字节.

pub mod queue;

use crate::framework::iomem::IoMem;
use crate::framework::mm::PhysAddr;
use crate::klog_info;
use crate::klog_warn;

// ── MMIO register offsets ──

const MAGIC_VALUE: usize = 0x000;
const VERSION: usize = 0x004;
const DEVICE_ID: usize = 0x008;
const VENDOR_ID: usize = 0x00c;
const DEVICE_FEATURES: usize = 0x010;
const DEVICE_FEATURES_SEL: usize = 0x014;
const DRIVER_FEATURES: usize = 0x020;
const DRIVER_FEATURES_SEL: usize = 0x024;
const QUEUE_SEL: usize = 0x030;
const QUEUE_NUM_MAX: usize = 0x034;
const QUEUE_READY: usize = 0x044;
const QUEUE_PFN: usize = 0x040; // 传统模式 (Legacy): QueuePFN (队列物理页号)
const QUEUE_NOTIFY: usize = 0x050;
const INTERRUPT_STATUS: usize = 0x060;
const INTERRUPT_ACK: usize = 0x064;
const STATUS: usize = 0x070;
const QUEUE_DESC_LOW: usize = 0x080;
const QUEUE_DESC_HIGH: usize = 0x084;
const QUEUE_DRIVER_LOW: usize = 0x090;
const QUEUE_DRIVER_HIGH: usize = 0x094;
const QUEUE_DEVICE_LOW: usize = 0x0a0;
const QUEUE_DEVICE_HIGH: usize = 0x0a4;

// ── Register magic ──

const VIRTIO_MAGIC: u32 = 0x74726976;

// ── 设备状态位 ──

const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_NEEDS_RESET: u32 = 0x40;
const STATUS_FAILED: u32 = 0x80;

// ── Device IDs ──

pub const VIRTIO_ID_BLOCK: u32 = 2;
pub const VIRTIO_ID_NET: u32 = 1;
pub const VIRTIO_ID_GPU: u32 = 16;

// ── MMIO region ──

/// QEMU virt (aarch64) 上 virtio-mmio 区域的基地址.
/// 在 `x86_64` QEMU microvm 上可能不同.
pub const VIRTIO_MMIO_BASE: u64 = 0x0a00_0000;
/// virtio-mmio 设备之间的步长 (0x200 字节).
pub const VIRTIO_MMIO_STRIDE: u64 = 0x200;
/// 要探测的 virtio-mmio 设备最大数量.
pub const VIRTIO_MMIO_MAX_DEVICES: u32 = 32;

// ── Feature bits ──

/// 通用特性: `VIRTIO_F_VERSION_1` (必须确认以符合规范)
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// 通过 MMIO 传输发现的 virtio 设备.
pub struct VirtioMmioDevice {
    /// MMIO 区域句柄 (安全访问代理).
    pub iomem: IoMem,
    /// 设备 ID (如 2 表示块设备).
    pub device_id: u32,
    /// 设备支持的 virtqueue 数量.
    /// 块设备通常为 1.
    pub queue_count: u32,
    /// 厂商 ID (`VendorID` 寄存器).
    pub vendor_id: u32,
    /// VirtIO 版本 (1=过渡/旧版, 2=现代).
    pub version: u32,
}

impl VirtioMmioDevice {
    /// 从设备的 MMIO 空间读取 32 位寄存器.
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn read32(&self, offset: usize) -> u32 {
        self.iomem.read_u32(offset)
    }

    /// 向设备的 MMIO 空间写入 32 位寄存器.
    #[inline(always)]
    fn write32(&self, offset: usize, val: u32) {
        self.iomem.write_u32(offset, val);
    }

    /// 读取跨 Low/High 寄存器的 64 位值.
    fn read64(&self, low_off: usize, high_off: usize) -> u64 {
        let lo = u64::from(self.read32(low_off));
        let hi = u64::from(self.read32(high_off));
        lo | (hi << 32)
    }

    /// 写入跨 Low/High 寄存器的 64 位值.
    fn write64(&self, low_off: usize, high_off: usize, val: u64) {
        self.write32(low_off, (val & 0xFFFF_FFFF) as u32);
        self.write32(high_off, (val >> 32) as u32);
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 探测给定 MMIO 基址的设备是否为合法的 virtio 设备.
    pub fn probe(mmio_base: u64) -> Option<Self> {
        // 为 MMIO 区域创建 IoMem (每个设备 0x200 字节)
        let iomem = match IoMem::from_pci_bar(PhysAddr::new(mmio_base), 0x200, "virtio-mmio") {
            Ok(m) => m,
            Err(_) => return None,
        };

        let magic = iomem.read_u32(MAGIC_VALUE);
        if magic != VIRTIO_MAGIC {
            return None;
        }

        let version = iomem.read_u32(VERSION);
        // QEMU virt 使用 VirtIO 1.0 (version 2) 或过渡版 (version 1)
        if version != 1 && version != 2 {
            return None;
        }

        let device_id = iomem.read_u32(DEVICE_ID);
        if device_id == 0 {
            return None; // 此槽位无设备
        }

        let vendor_id = iomem.read_u32(VENDOR_ID);

        klog_info!(
            Driver,
            "virtio: found device id={} vendor={:#x} at {:#x}",
            device_id,
            vendor_id,
            mmio_base
        );

        let queue_count = if device_id == VIRTIO_ID_BLOCK { 1 } else { 2 };

        Some(Self {
            iomem,
            device_id,
            queue_count,
            vendor_id,
            version,
        })
    }

    // ── 设备查询 ──

    /// 设备 ID (1=net, 2=blk, ...).
    pub fn device_id(&self) -> u32 {
        self.device_id
    }

    /// VirtIO 版本 (1=过渡/旧版, 2=现代).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// 是否为传统模式 (version == 1).
    pub fn is_legacy(&self) -> bool {
        self.version == 1
    }

    /// MMIO 基地址.
    pub fn mmio_base(&self) -> u64 {
        self.iomem.phys().as_u64()
    }

    // ── 设备状态机 (Status 寄存器) ──

    /// 读 Status 寄存器.
    pub fn status(&self) -> u32 {
        self.read32(STATUS)
    }

    /// 写 Status 寄存器.
    pub fn set_status(&self, val: u32) {
        self.write32(STATUS, val);
    }

    /// 重置设备 (status=0).
    pub fn reset(&self) {
        self.write32(STATUS, 0);
        // 内存屏障: 确保设备观察到 reset
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }

    /// 进入 ACKNOWLEDGE 状态.
    pub fn ack(&self) {
        self.write32(STATUS, STATUS_ACKNOWLEDGE);
    }

    /// 进入 DRIVER 状态.
    pub fn set_driver(&self) {
        self.write32(STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    }

    /// 进入 FEATURES_OK 状态, 返回设备是否接受特性协商.
    pub fn features_ok(&self) -> bool {
        self.write32(
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        self.read32(STATUS) & STATUS_FEATURES_OK != 0
    }

    // ── 特性协商 ──

    /// 读设备特性 (全 64 位: sel=0 低 32 位 + sel=1 高 32 位).
    pub fn device_features(&self) -> u64 {
        self.write32(DEVICE_FEATURES_SEL, 0);
        let lo = u64::from(self.read32(DEVICE_FEATURES));
        self.write32(DEVICE_FEATURES_SEL, 1);
        let hi = u64::from(self.read32(DEVICE_FEATURES));
        lo | (hi << 32)
    }

    /// 写驱动特性 (全 64 位).
    pub fn set_driver_features(&self, features: u64) {
        self.write32(DRIVER_FEATURES_SEL, 1);
        self.write32(DRIVER_FEATURES, (features >> 32) as u32);
        self.write32(DRIVER_FEATURES_SEL, 0);
        self.write32(DRIVER_FEATURES, features as u32);
    }

    // ── Virtqueue 细粒度配置 ──

    /// 选择 virtqueue 索引 (后续 `QUEUE_NUM_MAX` 等作用于该队列).
    pub fn select_queue(&self, vq_index: u16) {
        self.write32(QUEUE_SEL, u32::from(vq_index));
    }

    /// 读选中队列的最大尺寸.
    pub fn queue_num_max(&self) -> u32 {
        self.read32(QUEUE_NUM_MAX)
    }

    /// 标记选中队列为 ready.
    pub fn set_queue_ready(&self) {
        self.write32(QUEUE_READY, 1);
    }

    /// 通知设备: 选中队列有新描述符.
    pub fn notify_queue(&self, vq_index: u16) {
        self.write32(QUEUE_NOTIFY, u32::from(vq_index));
    }

    /// 设置选中队列的描述符/avail/used 物理地址 (modern 模式).
    pub fn setup_queue_addrs(&self, desc_paddr: u64, avail_paddr: u64, used_paddr: u64) {
        self.write64(QUEUE_DESC_LOW, QUEUE_DESC_HIGH, desc_paddr);
        self.write64(QUEUE_DRIVER_LOW, QUEUE_DRIVER_HIGH, avail_paddr);
        self.write64(QUEUE_DEVICE_LOW, QUEUE_DEVICE_HIGH, used_paddr);
    }

    /// 设置选中队列的 PFN (legacy 模式).
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn setup_queue_legacy(&self, paddr: u64) {
        let pfn = (paddr >> 12) as u32;
        self.write32(QUEUE_PFN, pfn);
    }

    // ── 中断 ──

    /// 读中断状态寄存器.
    pub fn interrupt_status(&self) -> u32 {
        self.read32(INTERRUPT_STATUS)
    }

    /// 按掩码应答中断 (写 1 清除对应位).
    pub fn ack_interrupt_mask(&self, mask: u32) {
        self.write32(INTERRUPT_ACK, mask);
    }

    /// 初始化设备:
    /// 1. Reset
    /// 2. Acknowledge
    /// 3. Negotiate features
    /// 4. Set `DRIVER_OK`
    /// # Errors
    /// 设备驱动状态初始化失败时返回 Err。
    pub fn init(&self) -> Result<(), ()> {
        // Step 1: Reset
        self.write32(STATUS, 0);
        // 确保设备观察到重置
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Step 2: ACKNOWLEDGE
        self.write32(STATUS, STATUS_ACKNOWLEDGE);

        // Step 3: DRIVER
        self.write32(STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // Step 4: 特性协商
        // 读取设备特性
        self.write32(DEVICE_FEATURES_SEL, 0);
        let _dev_features_lo = self.read32(DEVICE_FEATURES);
        self.write32(DEVICE_FEATURES_SEL, 1);
        let _dev_features_hi = self.read32(DEVICE_FEATURES);

        // 确认 VIRTIO_F_VERSION_1
        self.write32(DRIVER_FEATURES_SEL, 1);
        self.write32(DRIVER_FEATURES, (VIRTIO_F_VERSION_1 >> 32) as u32);
        self.write32(DRIVER_FEATURES_SEL, 0);
        self.write32(DRIVER_FEATURES, 0);

        // Step 5: FEATURES_OK
        self.write32(
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );

        // 验证 FEATURES_OK 已被接受
        let status = self.read32(STATUS);
        if status & STATUS_FAILED != 0 {
            klog_warn!(
                Driver,
                "virtio: device FAILED at {:#x}",
                self.iomem.phys().as_u64()
            );
            return Err(());
        }
        if status & STATUS_NEEDS_RESET != 0 {
            klog_warn!(
                Driver,
                "virtio: device NEEDS_RESET at {:#x}",
                self.iomem.phys().as_u64()
            );
            return Err(());
        }
        if status & STATUS_FEATURES_OK == 0 {
            klog_warn!(
                Driver,
                "virtio: FEATURES_OK rejected at {:#x}",
                self.iomem.phys().as_u64()
            );
            return Err(());
        }

        Ok(())
    }

    /// 设置 `DRIVER_OK` (设备进入 live). 必须在所有 virtqueue 配置完成后调用.
    pub fn set_driver_ok(&self) {
        self.write32(
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
    }

    /// 通知设备新的描述符已在 virtqueue 上可用.
    pub fn notify(&self, vq_index: u16) {
        self.write32(QUEUE_NOTIFY, u32::from(vq_index));
    }

    /// 从设备特定配置空间读取 (偏移相对于 0x100).
    pub fn read_config32(&self, offset: usize) -> u32 {
        self.read32(0x100 + offset)
    }

    pub fn read_config64(&self, offset: usize) -> u64 {
        self.read64(0x100 + offset, 0x100 + offset + 4)
    }

    /// I-42: 读取中断状态并写 ACK 寄存器 (`VirtIO` MMIO 规范要求).
    /// 驱动必须在处理完中断后调用此方法, 否则设备不会产生新中断.
    pub fn ack_interrupt(&self) {
        let status = self.read32(INTERRUPT_STATUS);
        self.write32(INTERRUPT_ACK, status);
    }
}
