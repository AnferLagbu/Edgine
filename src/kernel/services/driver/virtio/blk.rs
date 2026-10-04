#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! `VirtIO` 块设备驱动 — services 层 (Phase 2.1.3)
//!
//! 通过 [`crate::framework::driver::virtio::VirtioMmioDevice`] (framework
//! transport 机制) 提供 100% safe 的块设备初始化与配置路径。
//! `VirtQueue` 操作通过 framework 层安全 API 完成。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 所有 MMIO 读/写通过 framework `VirtioMmioDevice` 安全代理
//! - **请求格式**: 定义 `BlkRequest` / 状态码 / 配置偏移, 供 framework I/O 路径使用
//! - **初始化序列**: Reset → Ack → Driver → Feature Negotiate → `Features_OK` → Queue Setup → `Driver_OK`
//!
//! ## 与 framework 的分工
//!
//! - **services (本文件)**: 初始化序列, 特性协商, 配置空间读取, 请求格式定义
//! - **framework**: `VirtioMmioDevice` (MMIO transport 机制) + `VirtQueue` 分配与 DMA 缓冲区管理
//!
//! 评估日期: 2026-06-04
//! Phase 2.1.3 任务: VirtIO-blk 块设备迁移

use crate::framework::driver::virtio::queue::{DmaBuffer, VirtQueue};
use crate::framework::driver::virtio::{VIRTIO_F_VERSION_1, VIRTIO_ID_BLOCK, VirtioMmioDevice};
use crate::slog_info;
use crate::slog_warn;

// ============================================================================
// VirtIO-blk 常量
// ============================================================================

/// 块设备扇区大小 (字节)
pub const BLK_SECTOR_SIZE: usize = 512;

// ── Feature bits ──

/// `VIRTIO_BLK_F_SIZE_MAX`: 驱动可指定最大缓冲区大小
pub const VIRTIO_BLK_F_SIZE_MAX: u64 = 1 << 1;
/// `VIRTIO_BLK_F_SEG_MAX`: 最大 scatter-gather 段数
pub const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
/// `VIRTIO_BLK_F_GEOMETRY`: 几何信息 (cylinders/heads/sectors)
pub const VIRTIO_BLK_F_GEOMETRY: u64 = 1 << 4;
/// `VIRTIO_BLK_F_BLK_SIZE`: 块大小可配置
pub const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
/// `VIRTIO_BLK_F_TOPOLOGY`: 拓扑信息 (topology, aligned I/O)
pub const VIRTIO_BLK_F_TOPOLOGY: u64 = 1 << 10;
/// `VIRTIO_BLK_F_CONFIG_WCE`: 可配置 write-back 缓存
pub const VIRTIO_BLK_F_CONFIG_WCE: u64 = 1 << 11;

// ── 请求类型 ──

/// 读请求 (设备 → 驱动)
pub const VIRTIO_BLK_T_IN: u32 = 0;
/// 写请求 (驱动 → 设备)
pub const VIRTIO_BLK_T_OUT: u32 = 1;
/// 刷新请求
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;

// ── 状态码 ──

/// 请求成功
pub const VIRTIO_BLK_S_OK: u8 = 0;
/// I/O 错误
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
/// 不支持的请求
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

// ── 配置空间偏移 (相对于 0x100) ──

/// `capacity_lo`: 容量低 32 位 (512 字节扇区)
pub const BLK_CONFIG_CAPACITY_LO: usize = 0x00;
/// `capacity_hi`: 容量高 32 位
pub const BLK_CONFIG_CAPACITY_HI: usize = 0x04;
/// `size_max`: 最大缓冲区大小
pub const BLK_CONFIG_SIZE_MAX: usize = 0x08;
/// `seg_max`: 最大段数
pub const BLK_CONFIG_SEG_MAX: usize = 0x0C;
/// `geometry_cylinders`: 几何 — 偏移 0x10 包含 cylinders(u16) + heads(u8) + sectors(u8)
pub const BLK_CONFIG_GEOM_CYL: usize = 0x10;
/// `blk_size`: 块大小
pub const BLK_CONFIG_BLK_SIZE: usize = 0x14;

// ============================================================================
// 请求结构体
// ============================================================================

/// `VirtIO` 块请求头 (小端, 与设备 DMA 格式一致).
///
/// 描述符链布局:
///   desc`[0]`: `BlkRequest` (8 字节, 设备读)
///   desc`[1]`: 数据缓冲区 (IN 时设备写, OUT 时设备读)
///   desc`[2]`: 状态字节 (1 字节, 设备写)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlkRequest {
    /// 请求类型: 0=读, 1=写
    pub req_type: u32,
    /// 保留字段
    pub reserved: u32,
    /// LBA 扇区号 (小端)
    pub sector: u64,
}

impl BlkRequest {
    /// 创建读请求
    pub fn read(lba: u64) -> Self {
        Self {
            req_type: VIRTIO_BLK_T_IN.to_le(),
            reserved: 0,
            sector: lba.to_le(),
        }
    }

    /// 创建写请求
    pub fn write(lba: u64) -> Self {
        Self {
            req_type: VIRTIO_BLK_T_OUT.to_le(),
            reserved: 0,
            sector: lba.to_le(),
        }
    }

    /// 创建刷新请求
    pub fn flush() -> Self {
        Self {
            req_type: VIRTIO_BLK_T_FLUSH.to_le(),
            reserved: 0,
            sector: 0,
        }
    }

    /// 请求头大小 (字节)
    pub const fn header_size() -> usize {
        core::mem::size_of::<Self>()
    }
}

// ============================================================================
// 安全驱动逻辑
// ============================================================================

/// `VirtIO` 块设备安全驱动 (services 层, 0 unsafe)。
///
/// 封装 `VirtIO` 块设备的初始化序列与配置读取, 通过 framework `VirtioMmioDevice`
/// 安全代理访问 MMIO。DMA 缓冲区管理与 `VirtQueue` 操作保留在 framework 层。
///
/// ## 初始化流程
///
/// 1. `VirtioBlkDriver::new(device)` — 验证设备 ID, 执行初始化序列
/// 2. 读取配置空间获取容量信息
/// 3. framework: 分配 `VirtQueue` 并配置
/// 4. `set_driver_ok()` — 设备进入 live 状态
pub struct VirtioBlkDriver {
    /// MMIO 设备传输代理 (framework transport 机制)
    device: VirtioMmioDevice,
    /// 以 512 字节扇区为单位的总容量
    capacity_sectors: u64,
    /// 设备支持的特性位
    negotiated_features: u64,
    /// `VirtQueue` (单请求队列, 块设备通常仅 vq0)
    vq: VirtQueue,
}

impl VirtioBlkDriver {
    /// 创建并初始化 `VirtIO` 块设备驱动。
    ///
    /// 验证设备 ID 为块设备, 执行完整初始化序列 (Reset → Ack → Driver → Feature → `Features_OK`)。
    /// 返回 `Some(VirtioBlkDriver)` 表示设备就绪, 可继续配置 `VirtQueue`。
    ///
    /// # 参数
    /// - `device`: 已探测到的 `VirtIO` 设备 (`device_id` 必须为 `VIRTIO_ID_BLOCK`)
    pub fn new(device: VirtioMmioDevice) -> Option<Self> {
        if device.device_id() != VIRTIO_ID_BLOCK {
            slog_warn!(
                Driver,
                "virtio-blk: 期望 device_id={}, 实际={}",
                VIRTIO_ID_BLOCK,
                device.device_id()
            );
            return None;
        }

        slog_info!(
            Driver,
            "virtio-blk: 初始化 at {:#x} (version={})",
            device.mmio_base(),
            device.version()
        );

        // Step 1: Reset
        device.reset();

        // Step 2: ACKNOWLEDGE
        device.ack();

        // Step 3: DRIVER
        device.set_driver();

        // Step 4: 特性协商
        let dev_features = device.device_features();
        slog_info!(Driver, "virtio-blk: 设备特性={:#018x}", dev_features);

        // 驱动请求的特性: VERSION_1 + 基础块设备特性
        let mut driver_features = VIRTIO_F_VERSION_1;
        // 可选: 协商 blk_size 如果设备支持
        if dev_features & VIRTIO_BLK_F_BLK_SIZE != 0 {
            driver_features |= VIRTIO_BLK_F_BLK_SIZE;
        }
        // 可选: 协商 TOPOLOGY 如果设备支持
        if dev_features & VIRTIO_BLK_F_TOPOLOGY != 0 {
            driver_features |= VIRTIO_BLK_F_TOPOLOGY;
        }

        device.set_driver_features(driver_features);
        slog_info!(Driver, "virtio-blk: 驱动特性={:#018x}", driver_features);

        // Step 5: FEATURES_OK
        if !device.features_ok() {
            slog_warn!(Driver, "virtio-blk: FEATURES_OK 被拒绝");
            return None;
        }

        // 分配 VirtQueue (块设备通常仅 vq0)
        let is_legacy = device.is_legacy();
        let vq = VirtQueue::new(is_legacy)?;

        // 读取配置空间: 容量
        let cap_lo = u64::from(device.read_config32(BLK_CONFIG_CAPACITY_LO));
        let cap_hi = u64::from(device.read_config32(BLK_CONFIG_CAPACITY_HI));
        let capacity = cap_lo | (cap_hi << 32);

        slog_info!(
            Driver,
            "virtio-blk: 容量={} 扇区 ({} MB)",
            capacity,
            // 整数除法 (消除内核侧浮点, 见 docs/plan/aarch64-kernel-fp-free.md)
            capacity * 512 / (1024 * 1024)
        );

        Some(Self {
            device,
            capacity_sectors: capacity,
            negotiated_features: driver_features,
            vq,
        })
    }

    /// 设置 `DRIVER_OK` (设备进入 live 状态).
    ///
    /// 必须在所有 virtqueue 配置完成后调用.
    pub fn set_driver_ok(&self) {
        self.device.set_driver_ok();
        slog_info!(Driver, "virtio-blk: DRIVER_OK 已设置");
    }

    /// 完成设备初始化并进入 live (§6.4 直接方案 B: 注册前调用).
    ///
    /// 等价于 framework VirtioBlk::new 的收尾: 配置 vq0 MMIO 寄存器
    /// (desc/avail/used 物理地址) + 设置 DRIVER_OK.
    pub fn finalize(&mut self) {
        let vq_index = 0u16;
        self.device.select_queue(vq_index);
        self.device.setup_queue_addrs(
            self.vq.desc_paddr(),
            self.vq.avail_paddr(),
            self.vq.used_paddr(),
        );
        self.device.set_queue_ready();
        self.set_driver_ok();
    }

    /// 获取 MMIO 设备引用 (用于 `VirtQueue` 配置).
    pub fn device(&self) -> &VirtioMmioDevice {
        &self.device
    }

    /// 获取以扇区为单位的总容量.
    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// 获取协商后的特性位.
    pub fn negotiated_features(&self) -> u64 {
        self.negotiated_features
    }

    /// 检查是否协商了指定特性.
    pub fn has_feature(&self, feature: u64) -> bool {
        self.negotiated_features & feature != 0
    }

    // ── 配置空间读取 (安全, 通过 VirtioMmioDevice) ──

    /// 读取块大小 (仅当 `VIRTIO_BLK_F_BLK_SIZE` 被协商时有效).
    pub fn block_size(&self) -> u32 {
        self.device.read_config32(BLK_CONFIG_BLK_SIZE)
    }

    /// 读取最大缓冲区大小 (仅当 `VIRTIO_BLK_F_SIZE_MAX` 被协商时有效).
    pub fn size_max(&self) -> u32 {
        self.device.read_config32(BLK_CONFIG_SIZE_MAX)
    }

    /// 读取最大 scatter-gather 段数 (仅当 `VIRTIO_BLK_F_SEG_MAX` 被协商时有效).
    pub fn seg_max(&self) -> u32 {
        self.device.read_config32(BLK_CONFIG_SEG_MAX)
    }

    /// 读取几何信息 (cylinders, heads, sectors).
    ///
    /// VirtIO-blk 配置空间偏移 0x10: cylinders(u16) + heads(u8) + sectors(u8)
    /// 通过 `read_config32` 读取对齐的 32 位, 再拆分.
    pub fn geometry(&self) -> BlkGeometry {
        // 偏移 0x10 包含 cylinders(u16) + heads(u8) + sectors(u8), 共 4 字节
        let raw = self.device.read_config32(BLK_CONFIG_GEOM_CYL);
        let cylinders = (raw & 0xFFFF) as u16;
        let heads = ((raw >> 16) & 0xFF) as u8;
        let sectors = ((raw >> 24) & 0xFF) as u8;
        BlkGeometry {
            cylinders,
            heads,
            sectors,
        }
    }

    // ── 中断处理 ──

    /// 读取并应答中断状态 (写 1 清除).
    pub fn ack_interrupt(&self) -> u32 {
        let status = self.device.interrupt_status();
        if status != 0 {
            self.device.ack_interrupt_mask(status);
        }
        status
    }

    // ── 队列配置辅助 (通过 VirtioMmioDevice MMIO) ──

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 配置指定 virtqueue 的 MMIO 寄存器.
    ///
    /// # 参数
    /// - `vq_index`: virtqueue 索引 (blk 通常为 0)
    /// - `desc_paddr`: 描述符表物理地址
    /// - `avail_paddr`: available ring 物理地址
    /// - `used_paddr`: used ring 物理地址
    ///
    /// # 返回
    /// - `Ok(max_size)`: 队列最大尺寸
    /// - `Err(())`: 配置失败
    ///
    /// # Errors
    /// 当底层 virtio 设备队列配置失败时返回 `Err(())`.
    pub fn setup_queue(
        &self,
        vq_index: u16,
        desc_paddr: u64,
        avail_paddr: u64,
        used_paddr: u64,
    ) -> Result<u32, ()> {
        self.device.select_queue(vq_index);
        let max_size = self.device.queue_num_max();
        slog_info!(Driver, "virtio-blk: vq{} max_size={}", vq_index, max_size);
        self.device
            .setup_queue_addrs(desc_paddr, avail_paddr, used_paddr);
        self.device.set_queue_ready();
        slog_info!(Driver, "virtio-blk: vq{} ready", vq_index);
        Ok(max_size)
    }

    /// 通知设备: 指定队列有新描述符.
    pub fn notify(&self, vq_index: u16) {
        self.device.notify_queue(vq_index);
    }

    // ── 数据路径: 块读写 ──

    /// 读取单个扇区 (512 字节) 到调用方缓冲区.
    ///
    /// 使用链式描述符: 请求头 → 数据缓冲区 → 状态字节.
    /// DMA 缓冲区通过 framework `DmaBuffer` 分配.
    ///
    /// # Errors
    /// 当 `buf` 长度小于扇区大小 (`BLK_SECTOR_SIZE`) 或底层 I/O 执行失败时返回 `Err(())`.
    pub fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        if buf.len() < BLK_SECTOR_SIZE {
            return Err(());
        }
        self.do_sector_io(lba, VIRTIO_BLK_T_IN, buf)
    }

    /// 从调用方缓冲区写入单个扇区 (512 字节).
    ///
    /// # Errors
    /// 当 `buf` 长度小于扇区大小 (`BLK_SECTOR_SIZE`) 或底层 I/O 执行失败时返回 `Err(())`.
    pub fn write_sector(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        if buf.len() < BLK_SECTOR_SIZE {
            return Err(());
        }
        // 写操作: 将数据拷贝到 DMA, 设备读取
        self.do_sector_io_write(lba, buf)
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 执行读扇区 I/O (设备 → DMA → buf).
    fn do_sector_io(&mut self, lba: u64, req_type: u32, buf: &mut [u8]) -> Result<(), ()> {
        let req_size = BlkRequest::header_size();
        let data_size = BLK_SECTOR_SIZE;
        let status_size = 1;
        let total_dma = req_size + data_size + status_size;

        let mut dma = if let Some(b) = DmaBuffer::new(total_dma) {
            b
        } else {
            slog_warn!(Driver, "virtio-blk: DMA 缓冲区分配失败");
            return Err(());
        };
        let dma_phys = dma.phys_addr();

        // 构造请求头
        dma.write_u32(0, req_type.to_le());
        dma.write_u32(4, 0);
        dma.write_u64(8, lba.to_le());

        self.submit_and_wait(dma_phys, req_size, data_size, status_size, req_type, &dma)?;

        // 读操作: 从 DMA 缓冲区拷贝到调用方
        let mut tmp = [0u8; BLK_SECTOR_SIZE];
        dma.read_slice(req_size, &mut tmp);
        let copy_len = BLK_SECTOR_SIZE.min(buf.len());
        buf[..copy_len].copy_from_slice(&tmp[..copy_len]);
        Ok(())
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 执行写扇区 I/O (buf → DMA → 设备).
    fn do_sector_io_write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        let req_size = BlkRequest::header_size();
        let data_size = BLK_SECTOR_SIZE;
        let status_size = 1;
        let total_dma = req_size + data_size + status_size;

        let mut dma = if let Some(b) = DmaBuffer::new(total_dma) {
            b
        } else {
            slog_warn!(Driver, "virtio-blk: DMA 缓冲区分配失败");
            return Err(());
        };
        let dma_phys = dma.phys_addr();

        // 构造请求头
        dma.write_u32(0, VIRTIO_BLK_T_OUT.to_le());
        dma.write_u32(4, 0);
        dma.write_u64(8, lba.to_le());

        // 复制写入数据到 DMA 缓冲区
        let copy_len = data_size.min(buf.len());
        dma.write_slice(req_size, &buf[..copy_len]);

        self.submit_and_wait(
            dma_phys,
            req_size,
            data_size,
            status_size,
            VIRTIO_BLK_T_OUT,
            &dma,
        )
    }

    /// 提交描述符链并等待设备完成.
    ///
    /// 描述符链: [请求头] → [数据] → [状态字节].
    /// 返回 Ok(status) 或 `Err(status_byte)`.
    fn submit_and_wait(
        &mut self,
        dma_phys: u64,
        req_size: usize,
        data_size: usize,
        status_size: usize,
        req_type: u32,
        dma: &DmaBuffer,
    ) -> Result<(), ()> {
        // ── 准备描述符链 ──
        let desc_req = self.vq.prepare_desc(dma_phys, req_size as u32, false);
        let desc_data = self.vq.prepare_desc(
            dma_phys + req_size as u64,
            data_size as u32,
            req_type == VIRTIO_BLK_T_IN,
        );
        let desc_status = self.vq.prepare_desc(
            dma_phys + (req_size + data_size) as u64,
            status_size as u32,
            true,
        );

        if desc_req == 0xFFFF || desc_data == 0xFFFF || desc_status == 0xFFFF {
            if desc_status != 0xFFFF {
                self.vq.reclaim_desc(desc_status);
            }
            if desc_data != 0xFFFF {
                self.vq.reclaim_desc(desc_data);
            }
            if desc_req != 0xFFFF {
                self.vq.reclaim_desc(desc_req);
            }
            slog_warn!(Driver, "virtio-blk: 描述符耗尽");
            return Err(());
        }

        // 链接: req → data → status
        self.vq.link_desc(desc_req, desc_data);
        self.vq.link_desc(desc_data, desc_status);

        // 提交并通知设备
        self.vq.submit(desc_req);
        self.vq.commit_and_kick();
        self.device.notify_queue(0);

        // ── 等待完成 (轮询 used ring) ──
        loop {
            if let Some((_id, _len)) = self.vq.pop_used() {
                let status = dma.read_byte(req_size + data_size);

                self.vq.reclaim_desc(desc_status);
                self.vq.reclaim_desc(desc_data);
                self.vq.reclaim_desc(desc_req);

                if status != VIRTIO_BLK_S_OK {
                    if status == VIRTIO_BLK_S_IOERR {
                        slog_warn!(Driver, "virtio-blk: I/O 错误");
                    }
                    return Err(());
                }
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }
}

/// 块设备几何信息.
#[derive(Debug, Clone, Copy)]
pub struct BlkGeometry {
    /// 柱面数
    pub cylinders: u16,
    /// 磁头数
    pub heads: u8,
    /// 每磁道扇区数
    pub sectors: u8,
}

// ============================================================================
// BlockDevice trait 实现 (§6.4 直接方案 B: services 权威注册)
// ============================================================================

impl crate::framework::egdf::BlockDevice for VirtioBlkDriver {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
        match self.read_sector(sector, buf) {
            Ok(()) => 0,
            Err(()) => -1,
        }
    }

    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
        match self.write_sector(sector, buf) {
            Ok(()) => 0,
            Err(()) => -1,
        }
    }

    fn blk_is_present(&self) -> bool {
        true
    }

    fn blk_total_sectors(&self) -> u64 {
        self.capacity_sectors
    }
}
